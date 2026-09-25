use crate::models::domain::message::{ChatMessage, Role, ToolCallRecord};
use crate::models::ports::agent::Agent;
use crate::models::ports::provider::LlmProvider;
use crate::models::ports::speech_energy::SpeechEnergy;
use crate::models::ports::voice_input::{SpeculativeSignal, VoiceInput};
use crate::models::ports::voice_output::VoiceOutput;
use crate::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use crate::models::services::instant_activation::InstantActivation;
use crate::prompts::SYSTEM_PROMPT;
use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::security::ports::event_log::EventLog;
use crate::shared::domain::agent::{AgentRequest, AgentStreamEvent, WorkflowEvent, WorkflowState};
use crate::shared::services::print_output::PrintOutput;
use crate::shared::services::stdin_input::StdinInput;
use crate::user_data::domain::session::SessionMessage;
use crate::user_data::ports::session_storage::SessionStorage;
use anyhow::Result;
use futures::StreamExt as _;
use std::io::{self, Write};
use std::sync::Arc;
use uuid::Uuid;

// ── Voice helpers ─────────────────────────────────────────────────────────────

/// Voice mode always uses the "chat" role. The LLM handles tool routing
/// natively via MCP — no pre-classification needed.
fn resolve_voice_role(_message: &str) -> String {
    "chat".to_string()
}

/// The bare tool name, stripping any MCP server prefix
/// (`giap-weather__get_current_weather` -> `get_current_weather`).
///
/// MCP tool ids are `<server>__<tool>` (see `parse_tool_name` in
/// pond-mcp-server). The egress tracker (#113) already records the bare name via
/// `set_current_tool`, so recording it here too keeps the activity feed
/// consistent between a tool's `tool.call` event and its `egress.http` events.
fn bare_tool_name(tool: &str) -> String {
    match tool.split_once("__") {
        Some((_, bare)) if !bare.is_empty() => bare.to_string(),
        _ => tool.to_string(),
    }
}

/// Voice-loop control phrases live in `pond-voice` so the desktop shell — a
/// separate cargo workspace that cannot see `pond-core` — matches the same
/// list. It was duplicated there, and a phrase added here did not work there.
use pond_voice::control::{classify as classify_voice_command, VoiceCommand};

use crate::user_data::domain::profile::ProfileScope;
use pond_voice::control::is_control_phrase as is_dismissal_or_exit_phrase;

/// Truncate a tool-result payload to the NDJSON contract's 2000-char cap.
///
/// The contract specifies `content` is "truncated to 2000 chars". We count
/// Unicode scalar values (chars), not bytes, and cut on a char boundary so the
/// serialized JSON is always valid. Sub-cap payloads are returned unchanged.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Consecutive microphone failures tolerated before voice mode gives up.
///
/// With [`MIC_RETRY_BACKOFF_MS`] rising linearly, this is roughly a minute of
/// a genuinely absent device.
const MIC_RETRY_BUDGET: u32 = 10;
/// Base backoff between microphone retries; multiplied by the attempt number.
const MIC_RETRY_BACKOFF_MS: u64 = 1_000;

fn truncate_tool_result(mut content: String) -> String {
    // Find the byte offset of the (MAX+1)-th char. `char_indices().nth(N)`
    // early-exits after N+1 chars, so sub-cap payloads pay at most a bounded
    // scan and never a full `chars().count()`; the caller owns the String, so we
    // truncate in place with zero extra allocation on either branch.
    if let Some((byte_idx, _)) = content.char_indices().nth(TOOL_RESULT_MAX_CHARS) {
        content.truncate(byte_idx);
    }
    content
}

// ── PAI-7 P6: speaking first ─────────────────────────────────────────────────

/// Local wall-clock time of day, `[0, 1440)` minutes past local midnight.
///
/// A newtype rather than an `(u32, u32)` pair, because `time_tick.rs` already
/// records what two positional `u32`s cost inside a timer loop: they swap
/// silently. `LocalTimeOfDay::new` is the only constructor and it refuses
/// anything outside a real clock face, so a caller cannot hand the gate minute
/// 90 and have the window quietly answer "not quiet hours".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTimeOfDay(u16);

impl LocalTimeOfDay {
    /// `None` for an hour outside `0..=23` or a minute outside `0..=59`.
    pub fn new(hour: u32, minute: u32) -> Option<Self> {
        if hour > 23 || minute > 59 {
            return None;
        }
        Some(Self((hour * 60 + minute) as u16))
    }

    /// Minutes past local midnight.
    pub fn minutes(self) -> u16 {
        self.0
    }
}

/// Why the pond stayed quiet. Every variant is a refusal; there is no variant
/// meaning "spoke anyway".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechRefusal {
    /// The settings read failed, so nothing is known about consent. Silence.
    SettingsUnreadable,
    /// `unprompted_speech_enabled` is off. The default, and the common case.
    NotEnabled,
    /// Inside the quiet-hours window. PAI-7 invariant 6: absolute.
    QuietHours,
    /// The quiet-hours bounds could not be parsed, so the window is treated as
    /// covering everything. A separate variant from [`Self::QuietHours`]
    /// because one of them is the user's choice and the other is a broken row
    /// somebody has to fix.
    QuietHoursUnreadable,
    /// This notification's category is not one the household enabled.
    CategoryNotEnabled,
    /// A turn is in flight. PAI-7 3.4: never mid-conversation.
    MidConversation,
    /// The utterance is addressed to `Guest`. PAI-7 invariant 5.
    GuestSession,
    /// The utterance is addressed to `Household`, which is not an address.
    /// PAI-7 invariant 4 -- speaking to the room is the broadcast this
    /// workstream exists to avoid.
    NotAddressedToAMember,
    /// The member it is for is not present. Speaking into an empty room is
    /// worse than not speaking: nobody is helped and somebody else may hear it.
    MemberNotPresent,
    /// There is nothing to say.
    NothingToSay,
}

impl SpeechRefusal {
    /// Short, stable label for structured logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SettingsUnreadable => "settings_unreadable",
            Self::NotEnabled => "not_enabled",
            Self::QuietHours => "quiet_hours",
            Self::QuietHoursUnreadable => "quiet_hours_unreadable",
            Self::CategoryNotEnabled => "category_not_enabled",
            Self::MidConversation => "mid_conversation",
            Self::GuestSession => "guest_session",
            Self::NotAddressedToAMember => "not_addressed_to_a_member",
            Self::MemberNotPresent => "member_not_present",
            Self::NothingToSay => "nothing_to_say",
        }
    }
}

/// What the pond did when it considered speaking first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpromptedSpeech {
    /// Spoken aloud.
    Spoken,
    /// Not spoken, for this reason.
    Refused(SpeechRefusal),
}

/// One thing the pond is considering saying without having been asked.
///
/// Every input the decision depends on is a field here, and none of them is an
/// `Option` the gate could fill in for itself. That is deliberate: PAI-5 P7 and
/// PAI-1 P5 were both one ordering mistake from shipping a widening default
/// reached by construction order, and the shape that prevents it is a struct
/// the caller cannot finish building without having answered every question.
pub struct UnpromptedUtterance<'a> {
    /// Who this is for. Only [`ProfileScope::Owner`] can be spoken to.
    pub audience: &'a ProfileScope,
    /// `Notification.category` -- `alert` / `info` / `action_required`.
    pub category: &'a str,
    /// What would be said.
    pub text: &'a str,
    /// Members the pond currently believes are here, from PAI-7 P2's presence
    /// events. An empty slice is an empty room, which is a refusal.
    pub present_members: &'a [String],
    /// The local wall clock, read by the caller from the same clock the rules
    /// engine evaluates its time windows against.
    pub now: LocalTimeOfDay,
    /// Whether a turn is currently being served.
    pub turn_in_flight: bool,
}

/// `true` when `now` falls inside the `[start, end)` quiet window.
///
/// Wraps midnight when `start > end`, which is the normal case and the one the
/// `22:00` / `07:00` default takes. `None` when either bound is not `HH:MM`.
///
/// **The malformed answer is deliberately the opposite of the rules engine's.**
/// `schedule.rs :: in_time_window` answers `false` for a malformed bound, and
/// that is correct there: its window says when a rule MAY fire, so false means
/// the rule does not fire. This window says when the pond MUST NOT speak, so
/// false would mean it speaks. Both are the same principle -- on unreadable
/// input, do less -- and they are opposite booleans because the two windows
/// mean opposite things. Returning `Option` rather than a bare `bool` is what
/// keeps a reader from "fixing" one to match the other.
///
/// # Equal bounds are decided here, on the parsed times
///
/// A zero-length window is indistinguishable from "no quiet hours at all", and
/// the narrowing reading of an ambiguous setting is the quiet one: switching
/// quiet hours off is what `unprompted_speech_enabled` is for.
///
/// That decision was originally the caller's, taken as `start == end` on the
/// two setting strings, and it was wrong -- `%H` accepts an unpadded hour, so
/// `"9:00"` and `"09:00"` are one instant written two ways. Compared as text
/// they read as `start < end`, take the non-wrapping arm as
/// `now >= 9:00 && now < 9:00`, and answer NEVER QUIET: the exact inversion of
/// what the household asked for, produced by how they spelled it. Both bounds
/// are free text out of the settings table, so both spellings are reachable.
/// Deciding it after the parse is what makes the two spellings one answer.
fn quiet_hours_cover(start: &str, end: &str, now: LocalTimeOfDay) -> Option<bool> {
    let parse = |s: &str| {
        let t = chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()?;
        LocalTimeOfDay::new(chrono::Timelike::hour(&t), chrono::Timelike::minute(&t))
    };
    let (start, end) = (parse(start)?, parse(end)?);
    Some(if start == end {
        // Zero length, so quiet all day. See the note above.
        true
    } else if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    })
}

/// Whether `category` is one the household enabled for speech.
///
/// Splits on commas, trims, and lowercases. An empty list enables nothing, a
/// blank entry is not a category, and an unrecognised entry matches nothing --
/// so every way of getting the setting wrong ends in less speech rather than
/// more.
fn category_is_speakable(enabled: &str, category: &str) -> bool {
    let wanted = category.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return false;
    }
    enabled
        .split(',')
        .map(|c| c.trim().to_ascii_lowercase())
        .any(|c| !c.is_empty() && c == wanted)
}

/// Decide whether the pond may say this, unasked.
///
/// `settings` is `None` when the read FAILED. That is a refusal and not a
/// fallback to [`Settings::default`], even though the default would also be
/// silent today: the default is a value somebody may change, and a gate that
/// launders an unreadable store through it would start speaking the day
/// somebody flipped that default, with nothing in this function to review.
///
/// **Quiet hours are checked before consent, presence and category**, so no
/// combination of the other inputs can produce speech inside the window.
/// Invariant 6 says quiet hours are absolute, and "absolute" is a statement
/// about ordering as much as about the condition.
pub fn decide_unprompted_speech(
    settings: Option<&crate::user_data::domain::settings::Settings>,
    utterance: &UnpromptedUtterance<'_>,
) -> UnpromptedSpeech {
    use UnpromptedSpeech::Refused;

    let Some(settings) = settings else {
        return Refused(SpeechRefusal::SettingsUnreadable);
    };

    // 1. Quiet hours, first and unconditionally.
    let start = settings.quiet_hours_start.trim();
    let end = settings.quiet_hours_end.trim();
    match quiet_hours_cover(start, end, utterance.now) {
        None => return Refused(SpeechRefusal::QuietHoursUnreadable),
        // Equal bounds describe a zero-length window, and a zero-length window
        // is indistinguishable from "no quiet hours at all". Taking it as
        // silence-all-day is the narrowing reading: switching quiet hours OFF
        // is what `unprompted_speech_enabled` is for.
        Some(_) if start == end => return Refused(SpeechRefusal::QuietHours),
        Some(true) => return Refused(SpeechRefusal::QuietHours),
        Some(false) => {}
    }

    // 2. Consent.
    if !settings.unprompted_speech_enabled {
        return Refused(SpeechRefusal::NotEnabled);
    }

    // 3. Not while somebody is talking to it.
    if utterance.turn_in_flight {
        return Refused(SpeechRefusal::MidConversation);
    }

    // 4. Category.
    if !category_is_speakable(&settings.unprompted_speech_categories, utterance.category) {
        return Refused(SpeechRefusal::CategoryNotEnabled);
    }

    // 5. Addressed to a member, and only a member.
    let member = match utterance.audience {
        ProfileScope::Owner(id) => id,
        ProfileScope::Guest => return Refused(SpeechRefusal::GuestSession),
        ProfileScope::Household => return Refused(SpeechRefusal::NotAddressedToAMember),
    };

    // 6. Present. PAI-7 P2's presence is keyed on when somebody last SPOKE, so
    //    this is "the pond has recent evidence this member is here", never a
    //    claim about the room.
    if !utterance.present_members.iter().any(|p| p == member) {
        return Refused(SpeechRefusal::MemberNotPresent);
    }

    if utterance.text.trim().is_empty() {
        return Refused(SpeechRefusal::NothingToSay);
    }

    UnpromptedSpeech::Spoken
}

/// The tone that plays while the assistant is working, stopped exactly once.
///
/// It is the only feedback between the request and the answer, so it has two
/// jobs that pull in opposite directions: it must not stop early, and it must
/// never outlive the turn. A tone still playing after the assistant has gone
/// quiet is the worst of the failure modes — nothing is coming, and the sound
/// says something is.
///
/// A plain flag could not promise the second half. Every early return in the
/// turn — the `?` on a stream error most of all — skipped the stop and left
/// the tone thread running for the life of the process. Tying the stop to the
/// guard's scope means the compiler places it on paths nobody remembered to
/// write.
struct WorkingTone {
    output: Arc<dyn VoiceOutput>,
    stopped: bool,
}

impl WorkingTone {
    /// `enabled` is `settings.voice_thinking_tone_enabled`. When it is false the
    /// guard is still constructed and still runs `stop()` on drop: stopping a
    /// tone that never started is a no-op on every `VoiceOutput`, and building
    /// the disabled case out of the same guard means switching the tone back on
    /// cannot reintroduce a path where it outlives the turn.
    fn start(output: Arc<dyn VoiceOutput>, enabled: bool) -> Self {
        if enabled {
            output.start_thinking_tone();
        }
        Self {
            output,
            stopped: false,
        }
    }

    /// Stop the tone now — the answer has started. Idempotent, because the
    /// first speakable sentence can arrive down several different paths.
    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.output.stop_thinking_tone();
        }
    }
}

impl Drop for WorkingTone {
    fn drop(&mut self) {
        self.stop();
    }
}

// TTS text handling lives in the `pond-voice` leaf crate so the desktop shell
// (a separate cargo workspace, no pond-core) shares one implementation instead
// of carrying its own port. Re-exported below to keep call sites unchanged.
use pond_voice::text::{split_sentences, strip_markdown_for_speech};

/// Re-exported for backwards compatibility: this was `pub` here before the
/// move, so removing the path would be a breaking change to pond-core's API.
pub use pond_voice::text::filter_thinking;

/// Derive a short, deterministic session title from a user message.
///
/// Takes the first ~6 whitespace-separated words, trims surrounding
/// punctuation/quotes, and caps the result at 60 chars. Returns an empty
/// string when the input has no usable words (caller skips the update).
///
/// This is the no-LLM fallback used by `ChatService::ensure_session_title`
/// so sessions always get a human-readable title even when no provider is
/// attached (the common HTTP-handler path).
fn derive_title_from_text(text: &str) -> String {
    const MAX_WORDS: usize = 6;
    const MAX_CHARS: usize = 60;

    let cleaned = text.trim().trim_matches('"').trim_matches('\'');
    let title = cleaned
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim().trim_matches('"').trim_matches('\'').trim();

    if title.chars().count() <= MAX_CHARS {
        title.to_string()
    } else {
        // Cap at MAX_CHARS on a char boundary and add an ellipsis.
        let truncated: String = title.chars().take(MAX_CHARS).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Domain Service: ChatService
///
/// Orchestrates the Wait → Listen → Thinking → Speak workflow loop.
/// Also persists messages to session storage for conversation history.
///
/// All inference is routed through the `Agent` port (GooseAdapter in production).
/// An optional `LlmProvider` may be attached solely for session title generation.
///
/// Input is abstracted via the `VoiceInput` port.  The default is
/// `StdinInput` (reads from stdin).  Override with `with_voice_input()`.
///
/// `Clone` is cheap — every field is an `Arc`, `Option<Arc>`, `String`, or a
/// small `Clone` value. The Q2-26 speculative path clones the service into a
/// spawned task so a provisional transcript can start inference early.
#[derive(Clone)]
pub struct ChatService {
    agent: Arc<dyn Agent>,
    provider: Option<Arc<dyn LlmProvider>>,
    voice_input: Arc<dyn VoiceInput>,
    voice_output: Arc<dyn VoiceOutput>,
    speech_energy: Arc<dyn SpeechEnergy>,
    /// Wake-word detector.  Defaults to `InstantActivation` (keyboard / stdin mode).
    /// All detectors implement `StreamingWakeWordDetector`; `run_loop` always calls
    /// `wait_for_activation_with_audio()` so captured command audio is available for
    /// the one-breath flow when the detector supports it.
    wake_word_detector: Arc<dyn StreamingWakeWordDetector>,
    session_id: String,
    session_storage: Arc<dyn SessionStorage>,
    /// System prompt sent to the LLM on every completion call.
    /// Defaults to `SYSTEM_PROMPT`; override with `with_system_prompt()`.
    system_prompt: String,
    /// Optional Answer Reviewer — adversarial post-inference quality gate.
    answer_reviewer: Option<Arc<dyn crate::models::ports::answer_reviewer::AnswerReviewer>>,
    /// Optional unified activity log. When set, `persist_assistant_turn` records
    /// one Agent event, one Inference event (when token usage is known), and one
    /// Tool event per tool call — so the activity feed reflects chat activity,
    /// not just Auth/Network. `None` in tests and the CLI path.
    event_log: Option<Arc<dyn EventLog>>,
    /// Optional workflow-event sink. When set (e.g. the `--json-events` NDJSON
    /// writer), `emit_event` forwards every event to it in addition to tracing.
    /// `None` is zero-cost — `emit_event` only ever traces. `Arc` keeps clone
    /// cheap so the Q2-26 speculative task (which clones the service) carries
    /// the same sink and streams `Token` events from the spawned job.
    event_sink: Option<WorkflowEventSink>,
    /// Whether `run_loop` prints its human-facing banners/prompts to stdout.
    /// Defaults to `true` (the interactive terminal experience). Set `false`
    /// in `--json-events` mode so stdout carries NOTHING but NDJSON lines —
    /// human diagnostics still go to stderr via `eprintln!`/tracing.
    stdout_diagnostics: bool,
    /// Optional per-turn telemetry sink. When set, confirmed voice turns
    /// record a `TurnMetrics` row exactly like the REST path does.
    telemetry: Option<Arc<dyn crate::security::ports::telemetry::TelemetryPort>>,
    /// Model identifier for telemetry rows (the CLI knows `--model`).
    model_name: Option<String>,
    /// Whose turns these are. See [`with_profile_scope`](Self::with_profile_scope).
    profile_scope: ProfileScope,
    /// Whether the ambient working tone plays while inference runs. Mirrors
    /// `settings.voice_thinking_tone_enabled`; the composition root reads the
    /// setting and passes it via [`with_thinking_tone`](Self::with_thinking_tone).
    /// Defaults TRUE to match the settings default, so a caller that predates
    /// the switch behaves the way the pond did before it existed.
    thinking_tone: bool,
    /// PAI-5 P6. Whether reasoning text may be written to storage at all.
    /// FALSE unless [`with_thinking`](Self::with_thinking) says otherwise, so a
    /// handler that never heard of this feature persists nothing.
    persist_thinking: bool,
    /// Reasoning passages accumulated during the turn in flight, drained by
    /// `persist_assistant_turn`.
    ///
    /// Interior mutability, and the reason is the whole reason this seam works:
    /// the assistant message's id is minted INSIDE `persist_assistant_turn`
    /// (`Uuid::new_v4()`), so a handler cannot key these rows to it. The
    /// handler therefore hands over the TEXT as it streams and this service
    /// does the keying. Widening `persist_assistant_turn`'s signature was the
    /// alternative; it would have moved every caller for a value only one of
    /// them has.
    thinking_blocks: Arc<std::sync::Mutex<Vec<String>>>,
}

/// Result of one non-persisting agent stream: the streamed/spoken text plus
/// the usage and performance stats carried by the agent's `Done` event.
#[derive(Debug)]
struct TurnOutcome {
    text: String,
    usage: Option<crate::models::ports::provider::UsageStats>,
    stats: Option<crate::shared::domain::turn_stats::TurnStats>,
    total_latency_ms: u64,
}

struct BargeInWatch {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for BargeInWatch {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// A pluggable workflow-event observer. `run_chat`'s `--json-events` mode wires
/// an NDJSON stdout writer here; the desktop shell parses those lines. Kept as a
/// bare `Fn` so `pond-core` stays framework-free.
pub type WorkflowEventSink = Arc<dyn Fn(&WorkflowEvent) + Send + Sync>;

impl ChatService {
    pub fn new(
        agent: Arc<dyn Agent>,
        session_id: String,
        session_storage: Arc<dyn SessionStorage>,
    ) -> Self {
        Self {
            agent,
            provider: None,
            voice_input: Arc::new(StdinInput::new()),
            voice_output: Arc::new(PrintOutput),
            speech_energy: Arc::new(crate::models::ports::speech_energy::NoEnergy),
            wake_word_detector: Arc::new(InstantActivation),
            session_id,
            session_storage,
            system_prompt: SYSTEM_PROMPT.to_string(),
            answer_reviewer: None,
            event_log: None,
            event_sink: None,
            stdout_diagnostics: true,
            telemetry: None,
            model_name: None,
            profile_scope: ProfileScope::Household,
            thinking_tone: true,
            persist_thinking: false,
            thinking_blocks: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Play (or suppress) the ambient working tone for this service's turns.
    ///
    /// Takes the setting rather than being an opt-out marker method, for the
    /// same reason as [`with_thinking`](Self::with_thinking): the call site
    /// reads as "whatever the household chose", and switching the tone off does
    /// not depend on a composition root remembering to stop calling something.
    pub fn with_thinking_tone(mut self, enabled: bool) -> Self {
        self.thinking_tone = enabled;
        self
    }

    /// PAI-5 P6. Allow this turn's reasoning text to be persisted.
    ///
    /// Takes the setting rather than being an opt-in marker method so the call
    /// site reads as "whatever the user chose", and so turning the setting off
    /// does not depend on a handler remembering to stop calling something.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.persist_thinking = enabled;
        self
    }

    /// Offer one reasoning passage from the turn in flight.
    ///
    /// **The gate is here, not at the call site.** Handlers call this
    /// unconditionally from their `AgentStreamEvent::Thinking` arm; if
    /// `persist_thinking` is false the text is dropped on the floor and never
    /// enters this process's heap for longer than the call. Putting the `if` in
    /// the handler would have meant two copies of a privacy decision in two
    /// stream loops, and PAI-5's own recorded failure was a gate whose second
    /// input nobody tested.
    ///
    /// `&self` on purpose: the SSE handlers hold the service inside an
    /// `async_stream!` block where a `&mut` borrow across an await point is
    /// exactly what does not compile.
    pub fn record_thinking(&self, block: impl Into<String>) {
        if !self.persist_thinking {
            return;
        }
        let block = block.into();
        if block.trim().is_empty() {
            return;
        }
        if let Ok(mut buf) = self.thinking_blocks.lock() {
            buf.push(block);
        }
    }

    /// Attach the unified activity log so `persist_assistant_turn` records
    /// Agent / Inference / Tool events for each turn. Handlers that omit this
    /// simply record nothing — best-effort, never fatal.
    pub fn with_event_log(mut self, event_log: Arc<dyn EventLog>) -> Self {
        self.event_log = Some(event_log);
        self
    }

    /// Attach a telemetry sink so confirmed voice turns record `TurnMetrics`.
    pub fn with_telemetry(
        mut self,
        telemetry: Arc<dyn crate::security::ports::telemetry::TelemetryPort>,
    ) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Set the model identifier used in telemetry rows.
    pub fn with_model_name(mut self, model_name: impl Into<String>) -> Self {
        self.model_name = Some(model_name.into());
        self
    }

    /// The scope this session's turns are attributed to.
    ///
    /// Set by the handler from the same resolution that fills
    /// `AgentRequest.profile_scope`, so a turn is answered under the identity
    /// it was asked under. Defaults to `Household`, which is what every path
    /// did before PAI-1.
    ///
    /// It no longer decides who a memory is written for: extraction left the
    /// turn, and the batch engine resolves that from the session's persisted
    /// identity instead. Which is why `resolve_turn_scope` now writes that
    /// identity back -- the two answers have to be the same answer.
    pub fn with_profile_scope(mut self, scope: ProfileScope) -> Self {
        self.profile_scope = scope;
        self
    }

    /// Say something the user did not ask for -- or, far more often, decline to
    /// (PAI-7 P6).
    ///
    /// **This is the only door.** Every other `voice_output.speak()` in this
    /// file is downstream of a user utterance, and that is what makes the gate
    /// meaningful: a second unprompted speaking path would not be gated by
    /// having this one, so if one is ever added it belongs here rather than
    /// beside it.
    ///
    /// `settings` is `None` when the settings read failed. The gate refuses on
    /// that rather than falling back to a default -- see
    /// [`decide_unprompted_speech`].
    ///
    /// Returns the decision rather than a `Result`, because "the pond stayed
    /// quiet" is not an error and typing it as one invites a caller to retry it.
    /// A synthesis or playback failure IS logged, and still reports
    /// [`UnpromptedSpeech::Spoken`]: the decision to speak was taken and
    /// carried out: whether the speaker worked is the audio stack's problem,
    /// and reporting it as a refusal would make a broken speaker look like a
    /// privacy gate doing its job.
    pub async fn speak_unprompted(
        &self,
        settings: Option<&crate::user_data::domain::settings::Settings>,
        utterance: &UnpromptedUtterance<'_>,
    ) -> UnpromptedSpeech {
        let decision = decide_unprompted_speech(settings, utterance);
        match &decision {
            UnpromptedSpeech::Refused(reason) => {
                tracing::debug!(
                    reason = reason.as_str(),
                    category = %utterance.category,
                    "declined to speak unprompted"
                );
            }
            UnpromptedSpeech::Spoken => {
                if let Err(e) = self.voice_output.speak(utterance.text).await {
                    tracing::warn!(error = %e, "unprompted speech failed to play");
                }
            }
        }
        decision
    }

    /// Attach an Answer Reviewer for post-inference adversarial quality review.
    pub fn with_answer_reviewer(
        mut self,
        reviewer: Arc<dyn crate::models::ports::answer_reviewer::AnswerReviewer>,
    ) -> Self {
        self.answer_reviewer = Some(reviewer);
        self
    }

    /// Access the LLM provider (if set) for constructing tool agents etc.
    pub fn provider_ref(&self) -> &Option<Arc<dyn LlmProvider>> {
        &self.provider
    }

    /// Attach a real LLM provider. When set, `chat_once` calls the provider
    /// with the full conversation history instead of the echo agent.
    pub fn with_provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Override the input source.  Defaults to `StdinInput`.
    pub fn with_voice_input(mut self, input: Arc<dyn VoiceInput>) -> Self {
        self.voice_input = input;
        self
    }

    /// Override the voice output.  Defaults to `PrintOutput` (stdout).
    pub fn with_voice_output(mut self, output: Arc<dyn VoiceOutput>) -> Self {
        self.voice_output = output;
        self
    }

    pub fn with_speech_energy(mut self, energy: Arc<dyn SpeechEnergy>) -> Self {
        self.speech_energy = energy;
        self
    }

    fn watch_for_barge_in(&self) -> Option<BargeInWatch> {
        use pond_voice::barge;

        // Nothing to listen with. Starting the poll anyway would wake a task
        // ten times a second for the length of every reply to be told there
        // is no microphone, which it already knows.
        if self.speech_energy.is_inert() {
            return None;
        }

        let energy = self.speech_energy.clone();
        let voice_output = self.voice_output.clone();

        let handle = tokio::spawn(async move {
            let mut gate = barge::BargeIn::while_speaking();
            let poll = std::time::Duration::from_millis(barge::POLL_MS);
            let mut warned_inert = false;

            loop {
                tokio::time::sleep(poll).await;

                let Some(rms) = energy.recent_rms(barge::WINDOW_MS) else {
                    gate.reset();
                    if !warned_inert {
                        tracing::debug!("barge-in inert: the microphone is not open for this turn");
                        warned_inert = true;
                    }
                    continue;
                };

                if gate.on_rms(rms) {
                    tracing::debug!(rms, "barge-in: the user spoke over the reply");
                    voice_output.stop_speaking();
                    return;
                }
            }
        });

        Some(BargeInWatch { handle })
    }

    /// Set the wake-word detector.  Defaults to `InstantActivation` (no wait).
    ///
    /// All detectors implement `StreamingWakeWordDetector`.  `run_loop` always calls
    /// `wait_for_activation_with_audio()`, so detectors that capture command audio
    /// (e.g. `WhisperKeywordDetector`) enable the one-breath flow automatically.
    pub fn with_wake_word_detector(mut self, detector: Arc<dyn StreamingWakeWordDetector>) -> Self {
        self.wake_word_detector = detector;
        self
    }

    /// Override the system prompt sent to the LLM.
    ///
    /// Use `pond_core::prompts::build_system_prompt()` to build a personalised
    /// prompt from `Settings`.  The default is the static `SYSTEM_PROMPT` constant.
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }

    /// Attach a workflow-event sink. Every `emit_event` call forwards to it in
    /// addition to tracing. Used by `pond-server chat --json-events` to write
    /// the NDJSON contract to stdout.
    ///
    /// The sink is invoked synchronously from the loop, so keep it cheap
    /// (a buffered line write + flush). It is cloned into the speculative task,
    /// so `Token` deltas from a speculative stream reach it too.
    pub fn with_event_sink(mut self, sink: WorkflowEventSink) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Control whether `run_loop` prints human-facing banners/prompts to stdout.
    ///
    /// Pass `false` in `--json-events` mode so stdout carries only NDJSON lines;
    /// diagnostics continue to reach stderr (`eprintln!`) and tracing.
    pub fn with_stdout_diagnostics(mut self, enabled: bool) -> Self {
        self.stdout_diagnostics = enabled;
        self
    }

    /// Single-shot chat (useful for tests and non-interactive callers).
    ///
    /// All inference is routed through the `Agent` port (GooseAdapter in production).
    /// Goose manages conversation history and context compaction internally.
    /// Our `SessionStorage` is used only for the REST API's history/listing endpoints.
    pub async fn chat_once(&self, message: String) -> Result<String> {
        // Persist the user message first
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Always route through the Agent port (GooseAdapter in production), which manages
        // its own history, system prompt, and MCP tools internally.
        // The optional `self.provider` is kept solely for session title generation.
        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: resolve_voice_role(&message),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
            // Whatever the caller set. `chat_once` serves BOTH the
            // non-streaming REST /chat handler and the voice loop, so a
            // hardcoded Household here made one endpoint disagree with
            // /chat/stream about the same speaker. The builder decides.
            profile_scope: self.profile_scope.clone(),
            // Voice has no speaker identification, so there is no member
            // whose preferences these would be.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        };
        let response_text = self.agent.chat(request).await?.text;

        // Persist the assistant response
        let assistant_msg = ChatMessage::assistant(response_text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Auto-generate a session title after the first exchange
        self.maybe_generate_title(&message, &response_text).await;

        Ok(response_text)
    }

    /// Auto-generate a title for the session after the very first exchange.
    ///
    /// Only fires when:
    ///   1. An `LlmProvider` is available (title generation needs an LLM)
    ///   2. The session has no title yet
    ///   3. This is the first user+assistant pair (2 messages total)
    ///
    /// The title is generated by sending the user message and assistant
    /// response to the LLM with the shared [`TITLE_SYSTEM_PROMPT`], then
    /// stored with [`set_generated_title`] so its provenance is recorded.
    ///
    /// It uses the same prompt and the same normalisation as the idle
    /// re-titling service, deliberately: two paths naming the same thing by
    /// different rules is how one of them ends up producing titles the other
    /// would reject.
    ///
    /// **This names a conversation from its first two messages**, which is all
    /// there is at the time — so the name is a guess about where the
    /// conversation is going. Recording it as `model` rather than `user` is
    /// what lets the idle pass correct that guess later, once the conversation
    /// has actually gone somewhere.
    ///
    /// Failures are logged but never bubble up — title generation is
    /// best-effort and must never break the chat flow.
    ///
    /// [`TITLE_SYSTEM_PROMPT`]: crate::shared::services::session_title::TITLE_SYSTEM_PROMPT
    /// [`set_generated_title`]: crate::user_data::ports::session_storage::SessionStorage::set_generated_title
    async fn maybe_generate_title(&self, user_text: &str, assistant_text: &str) {
        use crate::shared::services::session_title::{normalise_title, TITLE_SYSTEM_PROMPT};
        // Only generate if we have an LLM provider
        let provider = match &self.provider {
            Some(p) => p,
            None => return,
        };

        // Check if session already has a title
        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

        // Check if this is the first exchange (exactly 2 messages: user + assistant).
        // Fetch only 3 to avoid loading the entire history just for a count check.
        //
        // The id of the newest message is kept: it is how far the resulting name
        // reaches, and without it the idle pass cannot tell whether the
        // conversation has since outgrown its title. A failed read now returns
        // rather than falling through — previously it generated a title anyway,
        // which cannot record an honest reach, and the deterministic fallback
        // still names the session either way.
        let through_message_id = match self
            .session_storage
            .get_messages_paginated(&self.session_id, 3, 0)
            .await
        {
            Ok(msgs) if msgs.len() == 2 => msgs[1].id.clone(),
            _ => return,
        };

        // Build context for the title generation LLM call
        let context = format!("User: {}\nAssistant: {}", user_text, assistant_text);
        let messages = vec![ChatMessage::user(&context)];

        match provider.complete(TITLE_SYSTEM_PROMPT, messages).await {
            Ok(response) => {
                // The shared normaliser, not a local trim: it enforces the same
                // ten-word ceiling, strips the same decorations, and refuses
                // prose rather than truncating it into a confident-looking
                // fragment. A local copy would let this path emit titles the
                // re-titling service would have rejected.
                let Some(title) = normalise_title(&response.content) else {
                    tracing::debug!(
                        session_id = %self.session_id,
                        "title generation returned nothing usable — leaving it to the fallback"
                    );
                    return;
                };

                // `set_generated_title`, not `update_title`: the latter records
                // a human rename and would put this session permanently beyond
                // the reach of the idle pass, freezing a name guessed from two
                // messages for the life of the conversation.
                if let Err(e) = self
                    .session_storage
                    .set_generated_title(&self.session_id, &title, &through_message_id)
                    .await
                {
                    tracing::warn!(
                        session_id = %self.session_id,
                        error = %e,
                        "Failed to save LLM-generated session title"
                    );
                } else {
                    tracing::info!(
                        session_id = %self.session_id,
                        title = %title,
                        "Auto-generated session title (LLM)"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %self.session_id,
                    error = %e,
                    "LLM title generation failed (non-fatal)"
                );
            }
        }
    }

    /// Persist the user side of a turn. Call before starting the agent stream
    /// so the message is saved even if the stream errors out.
    pub async fn persist_user_message(&self, message: &str) -> Result<()> {
        self.persist_user_message_with_images(message, Vec::new())
            .await
            .map(|_| ())
    }

    /// Persist the user side of a turn along with its image attachments
    /// (phase F2).
    ///
    /// The storage adapter decides where the bytes land; this service only has
    /// to stop dropping them. Attachment order is the order given.
    /// Returns the id of the row written.
    ///
    /// The caller needs it because this row is committed BEFORE inference
    /// starts, so a turn that is cancelled or dies before it says anything
    /// leaves a question with no answer behind it. Only whoever holds the id
    /// can take it back.
    pub async fn persist_user_message_with_images(
        &self,
        message: &str,
        images: Vec<crate::models::domain::message::ImageAttachment>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let sm = SessionMessage::new(
            id.clone(),
            self.session_id.clone(),
            ChatMessage::user_with_images(message, images),
        );
        self.session_storage
            .add_message(self.session_id.clone(), sm)
            .await?;
        Ok(id)
    }

    /// Persist the assistant side of a turn. Call after the agent stream drains.
    /// `tool_results` is raw JSON strings (one per tool call, in call order).
    /// `usage` is `(prompt_tokens, completion_tokens)`; pass `None` if unavailable.
    pub async fn persist_assistant_turn(
        &self,
        tool_results: Vec<String>,
        assistant_text: &str,
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
    ) -> Result<()> {
        // Each entry is JSON carrying `tool`, `tool_call_id` and `arguments`
        // (built in the chat handler). Parsed ONCE here: the same fields feed
        // the activity events, the tool rows' `tool_call_id`, and the assistant
        // row's tool-call records.
        //
        // An entry that does not parse is skipped rather than fatal, and still
        // persists its content -- a malformed blob must not lose the result.
        let parsed: Vec<Option<serde_json::Value>> = tool_results
            .iter()
            .map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .collect();

        let field = |v: &Option<serde_json::Value>, key: &str| -> Option<String> {
            v.as_ref()
                .and_then(|v| v.get(key))
                .and_then(|t| t.as_str())
                .map(str::to_string)
        };

        let tool_names: Vec<String> = parsed
            .iter()
            .filter_map(|v| field(v, "tool").map(|t| bare_tool_name(&t)))
            .collect();

        // The calls this turn made, linked to the results below by id.
        //
        // Written because nothing wrote them: across a real installation's whole
        // history there were 508 assistant rows, 0 with tool calls, against 488
        // tool rows -- every stored result an orphan with no record of who asked
        // for it. The read path in the REST API has always served both fields.
        let tool_calls: Vec<ToolCallRecord> = parsed
            .iter()
            .filter_map(|v| {
                Some(ToolCallRecord {
                    id: field(v, "tool_call_id")?,
                    name: field(v, "tool")?,
                    arguments: field(v, "arguments").unwrap_or_else(|| "{}".to_string()),
                })
            })
            .collect();

        for (content, v) in tool_results.into_iter().zip(parsed.iter()) {
            let sm = SessionMessage::new(
                Uuid::new_v4().to_string(),
                self.session_id.clone(),
                ChatMessage::tool_result(content, field(v, "tool_call_id").unwrap_or_default()),
            );
            self.session_storage
                .add_message(self.session_id.clone(), sm)
                .await?;
        }
        let assistant_id = Uuid::new_v4().to_string();
        let sm = SessionMessage::new(
            assistant_id.clone(),
            self.session_id.clone(),
            ChatMessage::assistant_with_tool_calls(assistant_text, tool_calls),
        )
        .with_token_counts(usage.map(|(p, _)| p), usage.map(|(_, c)| c));
        self.session_storage
            .add_message(self.session_id.clone(), sm)
            .await?;

        // PAI-5 P6. AFTER the assistant row commits, never before: the
        // `session_thinking` rows carry a foreign key onto it, and writing them
        // first would either fail or -- on a connection without
        // `PRAGMA foreign_keys` -- leave reasoning pointing at a message that
        // does not exist.
        //
        // Best-effort, deliberately. This is a UI convenience; a storage error
        // here must not cost the user the assistant turn that is already
        // committed above.
        self.persist_thinking_blocks(&assistant_id).await;

        if let Some((prompt, completion)) = usage {
            if prompt > 0 || completion > 0 {
                let _ = self
                    .session_storage
                    .increment_usage(&self.session_id, prompt, completion, model_name)
                    .await;
            }
        }

        // Ensure the session has a title. Handlers build ChatService without a
        // provider, so the LLM-based `maybe_generate_title` never fires; without
        // this fallback every session stays `title = null`. This derives a
        // cheap, deterministic title from the first user message — no LLM call.
        self.ensure_session_title().await;

        // Record the turn's activity in the unified log (best-effort). Uses the
        // tool names already carried in `tool_results` and the token usage, so
        // the activity feed reflects chat activity, not just Auth/Network.
        self.record_turn_activity(&tool_names, usage, model_name)
            .await;

        Ok(())
    }

    /// Drain the turn's reasoning passages into storage, keyed to the assistant
    /// row they produced.
    ///
    /// The buffer is drained whether or not the write succeeds, and drained
    /// even when the gate is off (where it is always empty, because
    /// `record_thinking` refuses to fill it). Both matter for the same reason:
    /// a `ChatService` that outlived one turn -- the terminal voice loop keeps
    /// one for the life of the process -- must not attach turn 1's reasoning to
    /// turn 2's answer.
    async fn persist_thinking_blocks(&self, assistant_message_id: &str) {
        let blocks: Vec<String> = match self.thinking_blocks.lock() {
            Ok(mut buf) => std::mem::take(&mut *buf),
            Err(poisoned) => {
                // A poisoned lock means a panic happened while holding it. Take
                // what is there and clear it; leaving stale text behind is the
                // worse failure of the two.
                let mut buf = poisoned.into_inner();
                std::mem::take(&mut *buf)
            }
        };
        if blocks.is_empty() || !self.persist_thinking {
            return;
        }
        if let Err(e) = self
            .session_storage
            .add_thinking(&self.session_id, assistant_message_id, &blocks)
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                "failed to persist reasoning text (turn itself is saved): {e}"
            );
        }
    }

    /// Append the Agent / Inference / Tool events for one completed turn.
    ///
    /// Best-effort: a failed append is logged and swallowed — observability must
    /// never fail a turn (same contract as the auth-event and egress emitters).
    /// Metadata only (counts, model, tool names); message content already lives
    /// in `session_messages`, so these events stay `Internal`.
    async fn record_turn_activity(
        &self,
        tool_names: &[String],
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
    ) {
        let Some(event_log) = &self.event_log else {
            return;
        };

        let mut events = Vec::with_capacity(2 + tool_names.len());

        // The turn itself.
        events.push(
            Event::new(EventCategory::Agent, "agent.turn")
                .attr("tool_count", tool_names.len() as i64)
                .session(&self.session_id)
                .sensitivity(PrivacySensitivity::Internal),
        );

        // The inference, when token usage was reported (chat_stream path).
        if let Some((prompt, completion)) = usage {
            let mut ev = Event::new(EventCategory::Inference, "inference.completion")
                .attr("prompt_tokens", prompt as i64)
                .attr("completion_tokens", completion as i64)
                .session(&self.session_id)
                .sensitivity(PrivacySensitivity::Internal);
            if let Some(model) = model_name {
                ev = ev.attr("model", model);
            }
            events.push(ev);
        }

        // One per tool call.
        for tool in tool_names {
            events.push(
                Event::new(EventCategory::Tool, "tool.call")
                    .attr("tool", tool.as_str())
                    .session(&self.session_id)
                    .sensitivity(PrivacySensitivity::Internal),
            );
        }

        for event in events {
            if let Err(e) = event_log.append(event).await {
                tracing::warn!(error = %e, "failed to record turn activity event");
            }
        }
    }

    /// Set a session title if one is not already present, deriving it
    /// deterministically from the first user message (first ~6 words).
    ///
    /// This is the reliable fallback for the common path where no
    /// `LlmProvider` is attached (all HTTP handlers). It is best-effort:
    /// failures are logged, never bubbled, and it runs inside the
    /// persistence owner so the ChatService contract is preserved.
    async fn ensure_session_title(&self) {
        // Skip if a title already exists (either set here previously or by the
        // LLM path). A missing session is treated as "no title" — the update
        // below is a no-op for a non-existent row.
        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

        // Find the first user message to derive a title from.
        let first_user_text = match self
            .session_storage
            .get_messages_paginated(&self.session_id, 20, 0)
            .await
        {
            Ok(msgs) => msgs
                .into_iter()
                .find(|m| m.message.role == Role::User)
                .map(|m| m.message.content),
            Err(_) => None,
        };

        let Some(text) = first_user_text else {
            return;
        };

        let title = derive_title_from_text(&text);
        if title.is_empty() {
            return;
        }

        // `set_derived_title`, not `update_title`: the latter now records a
        // human rename and puts the session permanently beyond the reach of
        // the idle re-titling job. This is the machine's own guess at a name
        // and is explicitly the thing that job exists to improve on.
        if let Err(e) = self
            .session_storage
            .set_derived_title(&self.session_id, &title)
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                error = %e,
                "Failed to save derived session title"
            );
        } else {
            tracing::info!(
                session_id = %self.session_id,
                title = %title,
                "Derived session title (deterministic fallback)"
            );
        }
    }

    /// Streaming chat — routes through the Agent, chunks TTS by sentence,
    /// AND persists the turn (user + assistant) to session storage.
    ///
    /// Differences from `chat_once`:
    /// - Calls `agent.chat_stream()` so text arrives token-by-token.
    /// - Speaks each completed sentence immediately (low-latency TTS).
    /// - Announces MCP tool calls with a short spoken phrase before execution.
    /// - Speaking happens *inside* this method; callers must NOT call
    ///   `voice_output.speak()` on the returned text.
    ///
    /// This is the persisting entry point used for a *confirmed* transcript.
    /// The Q2-26 speculative path must NOT call this — it calls
    /// `stream_response_inner` (no persistence) so a provisional transcript
    /// that later turns out wrong never lands a phantom turn in
    /// `pond_system.db`. `run_loop` persists the confirmed turn exactly once
    /// via `persist_confirmed_turn`.
    pub async fn chat_stream_once(&self, message: String) -> Result<String> {
        let fired_at = std::time::Instant::now();
        // Persist user message
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Stream + speak (no persistence inside).
        let outcome = self
            .stream_response_inner(message.clone(), fired_at)
            .await?;

        // Persist the assistant response and generate a title if needed.
        self.persist_assistant_response(&outcome.text, outcome.usage.as_ref())
            .await?;
        self.maybe_generate_title(&message, &outcome.text).await;
        self.record_turn_outcome(&outcome).await;
        Ok(outcome.text)
    }

    /// Streams a response through the Agent and speaks it, returning the full
    /// assistant text. Performs **no persistence** — the caller owns that.
    ///
    /// Used directly by the Q2-26 speculative path (which must be able to run
    /// on a provisional transcript and be discarded without side effects) and
    /// by `chat_stream_once` (which wraps it with persistence). `fired_at`
    /// timestamps when inference was kicked off, purely for TTFT telemetry.
    async fn stream_response_inner(
        &self,
        message: String,
        fired_at: std::time::Instant,
    ) -> Result<TurnOutcome> {
        // The LLM handles tool routing natively via MCP — no pre-classification needed.
        // This path is only ever reached via run_loop (the voice CLI loop),
        // so voice_mode is unconditionally true here — this gets the TTS-friendly
        // prompt and suppressed thinking that desktop's voice path already gets.
        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: true,
            canvas_mode: false,
            // As above -- the builder decides. Voice callers leave it at the
            // Household default because there is no speaker identification
            // (PAI-1 section 3.8); the REST handlers set a resolved scope.
            profile_scope: self.profile_scope.clone(),
            // Voice has no speaker identification, so there is no member
            // whose preferences these would be.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        };

        // Clear any interrupt left over from the previous turn. Exactly once
        // per turn, before any speech: interrupt state is a property of the
        // turn, and clearing it per-utterance made a barge-in stop one sentence
        // and then let the rest of the reply play out.
        self.voice_output.begin_utterance();

        // ── Working tone ──────────────────────────────────────────────────
        // The one signal that the request was heard and is being worked on.
        // It starts before inference and runs until the first real sentence
        // is ready to speak.
        //
        // A spoken filler ("Skimming the surface.") used to play first. It
        // said the same thing the tone says, but took a full synthesis and
        // playback to say it — delaying the answer to announce that the
        // answer was coming. One signal, and the cheaper one.
        //
        // Households that would rather have silence here switch it off with
        // `voice_thinking_tone_enabled`; the guard is built either way.
        let mut tone = WorkingTone::start(self.voice_output.clone(), self.thinking_tone);

        let mut stream = self.agent.chat_stream(request).await?;
        let mut turn_usage: Option<crate::models::ports::provider::UsageStats> = None;
        let mut turn_stats: Option<crate::shared::domain::turn_stats::TurnStats> = None;
        let mut full_text = String::new();
        let mut sentence_buf = String::new();
        let mut spoken_first = false;
        let mut barge_in: Option<BargeInWatch> = None;
        let mut thought_filter = crate::models::services::thought_filter::ThoughtFilter::new();

        // Pipelined TTS: synthesize the next sentence while the current one plays.
        // `pending_audio` holds WAV bytes ready for playback while we synthesize ahead.
        // `first_sentence_spoken` gates clause-boundary speak() for the first sentence
        // so audio starts before sentence 2's synthesis completes.
        let mut pending_audio: Option<Vec<u8>> = None;
        let mut first_sentence_spoken = false;

        macro_rules! speak_pipelined {
            ($self:expr, $text:expr, $pending:expr, $first_spoken:expr) => {{
                let text = $text;
                if !*$first_spoken {
                    // First sentence: speak() with clause-boundary splitting so the
                    // user hears audio immediately rather than waiting for S2 synth.
                    *$first_spoken = true;
                    if let Err(e) = $self.voice_output.speak(&text).await {
                        tracing::warn!("TTS failed: {}", e);
                    }
                } else {
                    // Subsequent sentences: synthesize-ahead pipeline.
                    match $self.voice_output.synthesize(&text).await {
                        Ok(Some(new_wav)) => {
                            if let Some(prev) = $pending.take() {
                                if let Err(e) = $self.voice_output.play_audio(prev).await {
                                    tracing::warn!("TTS playback failed: {}", e);
                                }
                            }
                            *$pending = Some(new_wav);
                        }
                        _ => {
                            if let Some(prev) = $pending.take() {
                                if let Err(e) = $self.voice_output.play_audio(prev).await {
                                    tracing::warn!("TTS playback failed: {}", e);
                                }
                            }
                            if let Err(e) = $self.voice_output.speak(&text).await {
                                tracing::warn!("TTS failed: {}", e);
                            }
                        }
                    }
                }
            }};
        }

        while let Some(event_result) = stream.next().await {
            match event_result? {
                AgentStreamEvent::ToolCall { id, tool, .. } => {
                    // Visible to the UI, silent to the ear. Tool use is part
                    // of working on the request, and the working tone already
                    // says that — narrating each step ("Checking the
                    // weather.") interrupted the tone to repeat it, and on a
                    // multi-tool turn the user heard a stream of announcements
                    // before hearing a single word of the actual answer.
                    //
                    // The tone deliberately keeps playing here.
                    self.emit_event(WorkflowEvent::ToolCall {
                        tool: tool.clone(),
                        id: id.clone(),
                    });
                }
                AgentStreamEvent::Text { content } => {
                    // Strip all thinking/reasoning tags — not meant for TTS or transcript.
                    let content = thought_filter.push(&content);
                    if content.is_empty() {
                        continue;
                    }

                    if !spoken_first {
                        // From-fire, user-perceived first-text latency (includes
                        // quip/tone time). The engine-level TTFT arrives in the
                        // Done event's TurnStats and is what the summary prints.
                        tracing::debug!(
                            "[Q2-26 TTFT] {}ms from-fire",
                            fired_at.elapsed().as_millis()
                        );
                        self.emit_event(WorkflowEvent::StateChanged {
                            state: WorkflowState::Speak,
                        });
                        spoken_first = true;
                    }
                    // Stream the (thought-filtered) delta to the event sink so the
                    // desktop caption feed updates token-by-token. Emitted before
                    // buffering so a partial that never completes a sentence still
                    // reaches the UI.
                    self.emit_event(WorkflowEvent::Token {
                        content: content.clone(),
                    });
                    full_text.push_str(&content);
                    sentence_buf.push_str(&content);

                    let (sentences, remainder) = split_sentences(&sentence_buf);
                    sentence_buf = remainder;
                    for sentence in sentences {
                        let spoken = strip_markdown_for_speech(&sentence);
                        if spoken.is_empty() {
                            continue;
                        }
                        tone.stop();
                        // Start barge-in mic monitoring before first TTS playback.
                        // If the user speaks during TTS, the listener sets the
                        // interrupt flag and playback stops immediately.
                        if barge_in.is_none() {
                            barge_in = self.watch_for_barge_in();
                        }
                        speak_pipelined!(
                            self,
                            spoken,
                            &mut pending_audio,
                            &mut first_sentence_spoken
                        );
                    }
                }
                AgentStreamEvent::Done { usage, stats, .. } => {
                    turn_usage = usage;
                    turn_stats = stats;
                    // Flush any tail held back by the thought filter. Also append to
                    // full_text so the returned + persisted message includes the
                    // withheld lookahead bytes — otherwise the tail is spoken but
                    // dropped from history. (#153)
                    let tail = thought_filter.flush();
                    if !tail.is_empty() {
                        // Emit the tail as a Token too, so the desktop caption/
                        // transcript (built solely from `Token` events) matches the
                        // text that is spoken and persisted — otherwise the UI bubble
                        // ends short of the reply. Mirror the streamed-delta path:
                        // ensure Speak has fired first so Token never precedes it.
                        if !spoken_first {
                            self.emit_event(WorkflowEvent::StateChanged {
                                state: WorkflowState::Speak,
                            });
                            spoken_first = true;
                        }
                        self.emit_event(WorkflowEvent::Token {
                            content: tail.clone(),
                        });
                        full_text.push_str(&tail);
                        sentence_buf.push_str(&tail);
                    }
                    // Flush any remaining buffer
                    let remainder = sentence_buf.trim().to_string();
                    if !remainder.is_empty() {
                        let spoken = strip_markdown_for_speech(&remainder);
                        if !spoken.is_empty() {
                            tone.stop();
                            speak_pipelined!(
                                self,
                                spoken,
                                &mut pending_audio,
                                &mut first_sentence_spoken
                            );
                        }
                    }
                    sentence_buf.clear();
                    break;
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow::anyhow!("Agent stream error: {}", content));
                }
                AgentStreamEvent::ToolResult { id, tool, content } => {
                    // Surface the tool result to the event sink (NDJSON).
                    // Not spoken — informational only. Truncate to the contract's
                    // 2000-char cap so a huge tool payload cannot bloat one line.
                    let content = truncate_tool_result(content);
                    self.emit_event(WorkflowEvent::ToolResult { tool, id, content });
                }
                AgentStreamEvent::Status { .. }
                | AgentStreamEvent::Thinking { .. }
                | AgentStreamEvent::ReviewStatus { .. }
                | AgentStreamEvent::ReviewRevision { .. }
                // The engine's cap message itself arrives as Text and IS spoken;
                // this structured marker is for clients that can offer a
                // continue affordance, which a voice turn cannot.
                | AgentStreamEvent::TurnLimitReached { .. }
                // PAI-6 P6. A voice turn has no tree to draw, and narrating a
                // delegation ("the researcher is calling recall_memories") is
                // the same mistake the ToolCall arm above already refuses:
                // announcements the user hears instead of an answer. It is also
                // deliberately not appended to `full_text` here or anywhere —
                // that string is what gets persisted, and invariant 4 keeps a
                // subagent's activity out of the parent's history.
                | AgentStreamEvent::SubagentProgress { .. } => {
                    // Not spoken during streaming — informational only
                }
            }
        }

        // Play any remaining synthesized audio
        if let Some(last) = pending_audio.take() {
            if let Err(e) = self.voice_output.play_audio(last).await {
                tracing::warn!("TTS final playback failed: {}", e);
            }
        }

        // Flush thought filter tail if stream ended without Done. Same as the Done
        // branch, the tail must also reach full_text so the persisted message is
        // complete on this path too — the second, previously-unfixed flush. (#153)
        let tail = thought_filter.flush();
        if !tail.is_empty() {
            // Emit the tail as a Token too (see the Done branch): keep the caption
            // feed in sync with the spoken/persisted text on the no-Done path.
            if !spoken_first {
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Speak,
                });
            }
            self.emit_event(WorkflowEvent::Token {
                content: tail.clone(),
            });
            full_text.push_str(&tail);
            sentence_buf.push_str(&tail);
        }
        // Flush anything left if stream ended without Done
        let remainder = sentence_buf.trim().to_string();
        if !remainder.is_empty() {
            let spoken = strip_markdown_for_speech(&remainder);
            if !spoken.is_empty() {
                tone.stop();
                if let Err(e) = self.voice_output.speak(&spoken).await {
                    tracing::warn!("TTS final flush failed: {}", e);
                }
            }
        }

        // A turn that produced no speakable text still has to stop the tone.
        tone.stop();

        // Stop watching the microphone: all TTS for this turn is done, so
        // there is nothing left to interrupt. Dropping the watch is what stops
        // it, hence the explicit `drop` rather than letting it fall out of
        // scope — the ordering relative to the tone above is deliberate.
        drop(barge_in.take());

        // No persistence here — the caller (chat_stream_once for a confirmed
        // transcript, or run_loop's persist_confirmed_turn for the reused
        // speculative result) owns writing this turn to session storage.
        Ok(TurnOutcome {
            text: full_text,
            usage: turn_usage,
            stats: turn_stats,
            total_latency_ms: fired_at.elapsed().as_millis() as u64,
        })
    }

    /// Record a completed turn's usage + performance: session token totals,
    /// an optional `TurnMetrics` row, and the console summary line. Failures
    /// are logged, never fatal — the reply was already delivered.
    async fn record_turn_outcome(&self, outcome: &TurnOutcome) {
        if let Some(usage) = &outcome.usage {
            if let Err(e) = self
                .session_storage
                .increment_usage(
                    &self.session_id,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    self.model_name.as_deref(),
                )
                .await
            {
                tracing::warn!("failed to increment session usage: {e}");
            }
        }
        if let (Some(telemetry), Some(stats)) = (&self.telemetry, &outcome.stats) {
            let turn_number = telemetry
                .get_turns(&self.session_id)
                .await
                .map(|v| v.len() as u32)
                .unwrap_or(0)
                + 1;
            let metrics = crate::security::domain::turn_metrics::TurnMetrics {
                session_id: self.session_id.clone(),
                turn_number,
                prompt_tokens: stats.prompt_tokens,
                completion_tokens: stats.completion_tokens,
                ttft_ms: stats.ttft_ms.unwrap_or(outcome.total_latency_ms),
                total_latency_ms: outcome.total_latency_ms,
                tool_name: None,
                tool_latency_ms: None,
                tool_cache_hit: None,
                context_utilization_pct: stats.context_pct().unwrap_or(0.0),
                model_name: self.model_name.clone().unwrap_or_default(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                prefill_ms: stats.prefill_ms,
                model_load_ms: stats.model_load_ms,
                decode_tok_per_sec: stats.decode_tok_per_sec,
                prefill_tok_per_sec: stats.prefill_tok_per_sec,
                prefilled_tokens: Some(stats.prefilled_tokens),
                reused_prefix_tokens: stats.reused_prefix_tokens,
                context_limit_tokens: stats.context_limit_tokens,
                reasoning_tokens: stats.reasoning_tokens,
                reengagements: Some(stats.reengagements),
                inference_count: Some(stats.inference_count),
            };
            if let Err(e) = telemetry.record_turn(metrics).await {
                tracing::debug!("failed to record voice turn metrics: {e}");
            }
        }
        self.print_turn_summary(outcome);
    }

    /// One clean console line per turn — the "inference = summaries" contract.
    /// Suppressed in `--json-events` mode (stdout is NDJSON-only there).
    fn print_turn_summary(&self, outcome: &TurnOutcome) {
        if !self.stdout_diagnostics {
            return;
        }
        let Some(stats) = &outcome.stats else {
            return;
        };
        let mut parts: Vec<String> = Vec::new();
        if let Some(ttft) = stats.ttft_ms {
            parts.push(format!("ttft {ttft}ms"));
        }
        // The prompt and the WORK are different numbers, and printing the prompt
        // beside a prefill duration read as a throughput that was never measured.
        match (stats.prefill_ms, stats.prefill_tok_per_sec) {
            (Some(prefill), Some(rate)) => parts.push(format!(
                "prefill {} of {} tok in {:.1}s ({:.0} tok/s)",
                stats.prefilled_tokens,
                stats.prompt_tokens,
                prefill as f32 / 1000.0,
                rate
            )),
            // No rate, for either of the two reasons `finalize_rates` withholds
            // one: nothing was decoded, or it took no measurable time. Print
            // what was actually decoded rather than assuming the first --
            // hardcoding 0 here claimed "all cached" for a turn that had
            // prefilled tokens and a sub-millisecond prefill, which is a lie in
            // the one line this exists to make honest.
            (Some(prefill), None) => parts.push(format!(
                "prefill {} of {} tok in {:.1}s{}",
                stats.prefilled_tokens,
                stats.prompt_tokens,
                prefill as f32 / 1000.0,
                if stats.prefilled_tokens == 0 {
                    " (all cached)"
                } else {
                    ""
                }
            )),
            _ => parts.push(format!("prompt {} tok", stats.prompt_tokens)),
        }
        if let Some(reused) = stats.reused_prefix_tokens {
            parts.push(format!("reused {reused} tok"));
        }
        if let (Some(decode), Some(rate)) = (stats.decode_ms, stats.decode_tok_per_sec) {
            parts.push(format!(
                "decode {} tok in {:.1}s ({:.1} tok/s)",
                stats.completion_tokens,
                decode as f32 / 1000.0,
                rate
            ));
        }
        // PAI-5 P2. Its own part, next to decode rather than folded into it —
        // this number is GIAP's count of the thinking channel, not the engine's,
        // and printing it inside the decode figure would imply the engine
        // reported it. Absent when nothing counted; "0" when nothing was thought.
        if let Some(reasoning) = stats.reasoning_tokens {
            parts.push(format!("reasoning {reasoning} tok"));
        }
        if let (Some(used), Some(limit)) = (stats.context_used_tokens, stats.context_limit_tokens) {
            parts.push(format!(
                "ctx {used}/{limit} ({:.0}%)",
                stats.context_pct().unwrap_or(0.0)
            ));
        }
        if let Some(load) = stats.model_load_ms {
            if load > 0 {
                parts.push(format!("load {:.1}s", load as f32 / 1000.0));
            }
        }
        if stats.inference_count > 1 {
            parts.push(format!("{} inferences", stats.inference_count));
        }
        if !parts.is_empty() {
            println!("  [turn] {}", parts.join(" | "));
        }
    }

    /// Persist a single assistant message to session storage. Split out of
    /// `chat_stream_once` so the Q2-26 speculative path can persist the
    /// assistant turn *after* the transcript is confirmed, not during the
    /// speculative stream.
    async fn persist_assistant_response(
        &self,
        full_text: &str,
        usage: Option<&crate::models::ports::provider::UsageStats>,
    ) -> Result<()> {
        let assistant_msg = ChatMessage::assistant(full_text.to_string());
        let assistant_id = Uuid::new_v4().to_string();
        let session_msg =
            SessionMessage::new(assistant_id.clone(), self.session_id.clone(), assistant_msg)
                .with_token_counts(
                    usage.map(|u| u.prompt_tokens),
                    usage.map(|u| u.completion_tokens),
                )
                // PAI-5 P2. Carried only where the caller hands over a whole
                // `UsageStats`. `persist_assistant_turn`'s `(prompt, completion)` tuple
                // — which is what `/chat/stream` passes — cannot express it, so rows
                // written by that route keep NULL rather than a wrong zero.
                .with_reasoning_tokens(usage.and_then(|u| u.reasoning_tokens));
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;
        // PAI-5 P6. The terminal voice loop keeps ONE `ChatService` for the life
        // of the process, so this drain is not optional even though no caller on
        // this path currently records anything: an undrained buffer would carry
        // turn 1's reasoning onto turn 2's row the moment one did.
        self.persist_thinking_blocks(&assistant_id).await;
        Ok(())
    }

    /// Persist a confirmed voice turn (user message + assistant response) that
    /// was produced by a speculative stream, and generate a title if needed.
    ///
    /// The Q2-26 speculative path runs `stream_response_inner` (no
    /// persistence) on a *provisional* transcript. Once `run_loop` confirms
    /// the final transcript matches, it calls this to write exactly one turn —
    /// keyed to the confirmed transcript — preserving the single-source-of-
    /// truth invariant (`pond_system.db` is authoritative; ChatService is the
    /// sole persistence owner).
    async fn persist_confirmed_turn(
        &self,
        confirmed_message: &str,
        response_text: &str,
        usage: Option<&crate::models::ports::provider::UsageStats>,
    ) -> Result<()> {
        let user_msg = ChatMessage::user(confirmed_message.to_string());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        self.persist_assistant_response(response_text, usage)
            .await?;
        // Prefer an LLM-summarized title when a provider is attached; otherwise
        // (the live GooseAdapter voice path builds ChatService WITHOUT a
        // provider) fall back to the deterministic first-message-derived title
        // so the chat sidebar shows a readable topic, never a raw session id.
        // `ensure_session_title` is guarded on `title.is_none()`, so it no-ops
        // when the LLM path already set one.
        self.maybe_generate_title(confirmed_message, response_text)
            .await;
        self.ensure_session_title().await;
        Ok(())
    }

    /// Q2-26: listen for the next utterance while *speculatively* starting the
    /// LLM response as soon as a provisional transcript is available, to hide
    /// whisper + first-token latency inside the end-of-speech silence wait.
    ///
    /// Returns `(confirmed_transcript, speculative)` where `speculative`, when
    /// present, is a still-running job that streamed a response for the
    /// **provisional** transcript `spec_transcript`. The job runs
    /// `stream_response_inner`, which performs **no persistence** — so if the
    /// provisional transcript turns out wrong, discarding the job leaves no
    /// phantom turn in `pond_system.db`.
    ///
    /// The caller (`run_loop`) must:
    ///   - race the job against the wake-word interrupt (barge-in parity), and
    ///   - only commit (persist) the job's result if `spec_transcript` equals
    ///     the `confirmed_transcript`; otherwise discard it and process the
    ///     confirmed transcript through the normal persisting path.
    ///
    /// A `SpeculativeSignal::Ready` for an empty or dismissal/exit phrase never
    /// starts a job — those are intercepted by `run_loop` before the LLM sees
    /// them, and a speculative call would bypass that.
    #[allow(clippy::type_complexity)]
    async fn listen_with_speculative_chat(
        &self,
    ) -> Result<(
        Option<String>,
        Option<(String, tokio::task::JoinHandle<Result<TurnOutcome>>)>,
    )> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SpeculativeSignal>();
        let callback: Box<dyn Fn(SpeculativeSignal) + Send + Sync> = Box::new(move |signal| {
            let _ = tx.send(signal);
        });

        let listen_fut = self.voice_input.listen_with_speculative(callback);
        tokio::pin!(listen_fut);

        // `(spec_transcript, handle)` — the transcript the job was fired on is
        // retained so `run_loop` can confirm it matches the final transcript
        // before persisting anything.
        let mut speculative: Option<(String, tokio::task::JoinHandle<Result<TurnOutcome>>)> = None;

        loop {
            tokio::select! {
                result = &mut listen_fut => {
                    let transcript = result?;
                    return Ok((transcript, speculative.take()));
                }
                Some(signal) = rx.recv() => {
                    match signal {
                        SpeculativeSignal::Ready(text)
                            if !text.is_empty() && !is_dismissal_or_exit_phrase(&text) =>
                        {
                            // Fire the LLM early on the provisional transcript.
                            // NOTE: uses stream_response_inner (NO persistence),
                            // so an incorrect provisional transcript can be
                            // discarded without leaving a phantom turn.
                            let svc = self.clone();
                            let spec_text = text.clone();
                            let fired_at = std::time::Instant::now();
                            let handle = tokio::spawn(async move {
                                svc.stream_response_inner(spec_text, fired_at).await
                            });
                            speculative = Some((text, handle));
                        }
                        // Empty / dismissal / exit — let run_loop handle it normally.
                        SpeculativeSignal::Ready(_) => {}
                        SpeculativeSignal::Invalidated => {
                            // The provisional transcript covered a too-short
                            // clip (speech resumed). Abort the job and silence
                            // any audio it may have started. No persistence
                            // happened, so nothing to roll back.
                            if let Some((_, handle)) = speculative.take() {
                                handle.abort();
                                self.voice_output.stop_speaking();
                                self.voice_output.stop_thinking_tone();
                            }
                        }
                    }
                }
            }
        }
    }

    /// Run the interactive workflow loop.
    ///
    /// State machine:
    ///   Wait → Listen → Thinking → Speak → (back to Wait)
    ///
    /// Input is obtained via the `VoiceInput` port (stdin by default).
    /// Wait for the wake word, surviving a microphone that comes and goes.
    ///
    /// Retries with a backoff rather than failing the session. Gives up only
    /// after [`MIC_RETRY_BUDGET`] consecutive failures, which at this backoff
    /// is roughly a minute of a genuinely absent device — long enough to
    /// outlast anything transient, short enough that a permanently missing
    /// microphone still reports itself instead of retrying in silence forever.
    ///
    /// Each failure is reported once on the console, because a voice assistant
    /// that has quietly stopped listening is indistinguishable from one that
    /// is listening and hearing nothing.
    async fn wait_for_activation_resiliently(&self) -> Result<WakeWordActivation> {
        let mut last_error = None;

        for attempt in 1..=MIC_RETRY_BUDGET {
            match self
                .wake_word_detector
                .wait_for_activation_with_audio()
                .await
            {
                Ok(activation) => return Ok(activation),
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        budget = MIC_RETRY_BUDGET,
                        "microphone unavailable: {e}"
                    );
                    if self.stdout_diagnostics {
                        println!("  microphone unavailable ({e}) — retrying");
                    }
                    last_error = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(
                        MIC_RETRY_BACKOFF_MS * attempt as u64,
                    ))
                    .await;
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("microphone unavailable"))
            .context("the microphone did not become available; voice mode cannot continue"))
    }

    pub async fn run_loop(&self) -> Result<()> {
        // First interaction always requires the wake word.
        // After that, conversational turn-taking: Goose listens for the user's
        // next turn directly after speaking, no wake word needed.
        // If the user doesn't speak (empty transcription), fall back to wake word.
        let mut first_turn = true;

        // Pre-captured input from a wake-word interrupt.  When set, the next
        // loop iteration skips the listen/wake-word phase and processes this
        // text directly — but still wrapped in `tokio::select!` so it remains
        // interruptible.
        let mut pending_input: Option<String> = None;

        // Human-facing stdout print, suppressed in `--json-events` mode so
        // stdout carries NOTHING but NDJSON lines. Diagnostics still reach
        // stderr (`eprintln!`) and tracing regardless of this flag.
        macro_rules! diag {
            ($($arg:tt)*) => {
                if self.stdout_diagnostics {
                    println!($($arg)*);
                }
            };
        }
        macro_rules! diag_inline {
            ($($arg:tt)*) => {
                if self.stdout_diagnostics {
                    print!($($arg)*);
                    let _ = io::stdout().flush();
                }
            };
        }

        loop {
            // `speculative`, when present, is a (provisional_transcript, job)
            // pair: an LLM response already streaming for a provisional
            // transcript (Q2-26). It is reused ONLY if the confirmed transcript
            // matches; otherwise it is discarded without persisting anything.
            let (input, speculative) = if let Some(text) = pending_input.take() {
                // Interrupt gave us pre-captured text — skip listen phase.
                // Emit events so the UI/state machine stays consistent.
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                (Some(text), None)
            } else if first_turn {
                // ── Wait for wake word ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Wait,
                });
                // A detector that does not wait has nothing to announce.
                let prompt = self.wake_word_detector.activation_prompt();
                if !prompt.is_empty() {
                    diag!("\n  {prompt}");
                }

                // A microphone that fails to open must not end the session.
                //
                // This `?` used to be fatal, so a transient device error —
                // another process taking the input device, a Bluetooth
                // headset switching profile, a USB mic re-enumerating — killed
                // voice mode outright with "The requested stream configuration
                // is not supported by the device" and no way back short of
                // restarting. Devices come and go; an assistant that waits for
                // one to come back is worth more than one that exits correctly.
                let activation = match self.wait_for_activation_resiliently().await {
                    Ok(a) => a,
                    Err(e) => {
                        self.emit_event(WorkflowEvent::Exit {
                            reason: "microphone_unavailable".to_string(),
                        });
                        return Err(e);
                    }
                };

                // ── Listen (one-breath or fresh recording) ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                diag_inline!("  {}", self.voice_input.prompt());

                if let Some(wav) = activation.captured_audio {
                    self.voice_input.prime_with_captured(wav);
                }

                self.listen_with_speculative_chat().await?
            } else {
                // ── Conversational turn — listen without wake word ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                diag!("\n  listening");

                self.listen_with_speculative_chat().await?
            };

            // Any early-return path (no speech / dismissal / exit) must abort a
            // live speculative job so it stops speaking and never persists.
            // Helper: aborts + silences the speculative job if one is running.
            let abort_speculative =
                |spec: Option<(String, tokio::task::JoinHandle<Result<TurnOutcome>>)>| {
                    if let Some((_, handle)) = spec {
                        handle.abort();
                        self.voice_output.stop_speaking();
                        self.voice_output.stop_thinking_tone();
                    }
                };

            let input = match input {
                None if first_turn => {
                    // Stdin EOF — exit the loop
                    abort_speculative(speculative);
                    self.emit_event(WorkflowEvent::Exit {
                        reason: "stdin_eof".to_string(),
                    });
                    diag!("\n  end of input");
                    break;
                }
                None => {
                    // Conversational mode, no speech — reset to wake word
                    abort_speculative(speculative);
                    diag!("  nothing heard");
                    first_turn = true;
                    continue;
                }
                Some(text) if text.is_empty() => {
                    // Empty transcription — fall back to wake word mode
                    abort_speculative(speculative);
                    if !first_turn {
                        diag!("  nothing heard");
                    }
                    first_turn = true;
                    continue;
                }
                Some(text) => text,
            };

            // ── Voice-loop control (dismissal / exit) — single source of truth ──
            match classify_voice_command(&input) {
                // Dismissal / sleep → speak farewell, return to wake word.
                VoiceCommand::Dismissal => {
                    abort_speculative(speculative);
                    let farewell = "Until next time. Just say my name when you need me.";
                    diag!("  {}", farewell);
                    if let Err(e) = self.voice_output.speak(farewell).await {
                        tracing::warn!("TTS farewell failed: {}", e);
                    }
                    first_turn = true;
                    continue;
                }
                // Hard exit → terminate the voice loop entirely.
                VoiceCommand::Exit => {
                    abort_speculative(speculative);
                    let farewell = "Goodbye! I'll be here whenever you need me.";
                    diag!("  {}", farewell);
                    if let Err(e) = self.voice_output.speak(farewell).await {
                        tracing::warn!("TTS farewell failed: {}", e);
                    }
                    self.emit_event(WorkflowEvent::Exit {
                        reason: "dismissed".to_string(),
                    });
                    break;
                }
                VoiceCommand::Normal => {}
            }

            // Conversation is active — subsequent turns skip the wake word
            first_turn = false;

            // Confirmed user utterance — surface to the event sink (NDJSON
            // `transcript`) before the LLM starts. Legacy UserInput retained
            // for the tracing hook.
            self.emit_event(WorkflowEvent::Transcript {
                text: input.clone(),
            });
            self.emit_event(WorkflowEvent::UserInput(input.clone()));

            // ── Thinking → Speak (streaming), with wake-word interrupt ──
            self.emit_event(WorkflowEvent::StateChanged {
                state: WorkflowState::Thinking,
            });

            // ── Q2-26 phantom-turn gate ─────────────────────────────────────
            // A speculative job (if any) streamed a response for a *provisional*
            // transcript with NO persistence. Reuse it ONLY if the confirmed
            // transcript matches; otherwise discard it and fall through to the
            // normal persisting path on the confirmed transcript. This
            // guarantees exactly ONE persisted turn per utterance, always keyed
            // to the confirmed transcript (single-source-of-truth invariant).
            let reusable_speculative = match speculative {
                Some((spec_transcript, handle)) if spec_transcript == input => Some(handle),
                Some((_, handle)) => {
                    // Mismatch: provisional transcript was wrong. Abort the job
                    // and silence any audio it started — nothing was persisted.
                    handle.abort();
                    self.voice_output.stop_speaking();
                    self.voice_output.stop_thinking_tone();
                    None
                }
                None => None,
            };

            // Race the agent response against the wake word detector.
            // If the user says the wake word during inference or TTS playback,
            // interrupt immediately: stop TTS, drop the stream, and process
            // the new speech as a fresh request.
            //
            // `chat_handle` is either the reusable speculative job (already
            // streaming, NO persistence) or a fresh non-persisting stream.
            // Either way it is raced against the wake interrupt identically, so
            // barge-in works the same. Persistence happens AFTER it completes,
            // via persist_confirmed_turn — so exactly one confirmed turn lands.
            let input_for_task = input.clone();
            let chat_handle = reusable_speculative.unwrap_or_else(|| {
                let svc = self.clone();
                let msg = input_for_task;
                let fired_at = std::time::Instant::now();
                tokio::spawn(async move { svc.stream_response_inner(msg, fired_at).await })
            });

            // ── InstantActivation race guard ─────────────────────────────────
            // The wake-word interrupt race is ONLY correct for detectors that
            // actually wait for real audio. `InstantActivation` (stdin /
            // --no-wake-word / whisper-load-failure fallback) resolves instantly
            // and would win the race before any turn could complete, aborting
            // EVERY turn. When the detector cannot interrupt, await the turn
            // directly — no race, no phantom abort. Real streaming detectors
            // keep the barge-in race below.
            if !self.wake_word_detector.supports_interruption() {
                let chat_result = chat_handle.await;
                if !self.finalize_confirmed_turn(chat_result, &input).await {
                    first_turn = true;
                }
                continue;
            }

            // Race the agent response against the wake word detector.
            // If the user says the wake word during inference or TTS playback,
            // interrupt immediately: stop TTS, drop the stream, and process
            // the new speech as a fresh request.
            //
            // `chat_handle` is either the reusable speculative job (already
            // streaming, NO persistence) or a fresh non-persisting stream.
            // Either way it is raced against the wake interrupt identically, so
            // barge-in works the same. Persistence happens AFTER it completes,
            // via persist_confirmed_turn — so exactly one confirmed turn lands.
            let wake_fut = self.wake_word_detector.wait_for_activation_with_audio();

            tokio::pin!(chat_handle);
            tokio::pin!(wake_fut);

            tokio::select! {
                chat_result = &mut chat_handle => {
                    // Normal completion — agent finished before any interrupt.
                    if !self.finalize_confirmed_turn(chat_result, &input).await {
                        first_turn = true;
                    }
                }
                wake_result = &mut wake_fut => {
                    // Wake word detected during inference/TTS — INTERRUPT
                    diag!("\n  interrupted");

                    // Stop any in-progress TTS playback and background listeners
                    self.voice_output.stop_speaking();
                    self.voice_output.stop_thinking_tone();

                    // `chat_handle` is a spawned task (fresh or reused speculative).
                    // Aborting it propagates the same drop-based cancellation into
                    // Goose's spawn_blocking inference, causing TokenAction::Stop
                    // within one token cycle. Crucially, the interrupted task ran
                    // `stream_response_inner` (NO persistence), so an interrupted
                    // response never leaves a partial turn in pond_system.db —
                    // persistence only happens on normal completion above.
                    // No TurnComplete is emitted either — the turn was never
                    // committed.
                    chat_handle.abort();

                    // Capture the user's new speech (wake word may include trailing audio).
                    // Instead of processing inline (which would be non-interruptible),
                    // stash the text in `pending_input` and `continue` the loop so
                    // the next iteration wraps it in tokio::select! again.
                    match wake_result {
                        Ok(activation) => {
                            self.emit_event(WorkflowEvent::StateChanged {
                                state: WorkflowState::Listen,
                            });
                            diag_inline!("  {}", self.voice_input.prompt());

                            if let Some(wav) = activation.captured_audio {
                                self.voice_input.prime_with_captured(wav);
                            }

                            match self.voice_input.listen().await {
                                Ok(Some(new_text)) if !new_text.is_empty() => {
                                    // Stash for the next loop iteration (interruptible path)
                                    pending_input = Some(new_text);
                                }
                                _ => {
                                    // No speech after interrupt — return to wake word mode
                                    diag!("  nothing heard");
                                    first_turn = true;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Wake word interrupt error: {}", e);
                            first_turn = true;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Finalize a completed (non-interrupted) chat turn: persist it exactly
    /// once, then emit the terminal events. Shared by both the raced and the
    /// non-raced (`supports_interruption() == false`) paths so persistence and
    /// event emission never drift between them.
    ///
    /// Returns `true` when the loop should stay in conversational mode, `false`
    /// on a stream/join error (the caller then resets to wake-word mode).
    ///
    /// On a stream/join error NOTHING is persisted. On a *persistence* failure the
    /// reply was already fully streamed and spoken to the user, so we do NOT reset
    /// to wake-word mode (that would kick the user out mid-conversation for a reply
    /// they just heard): we log + emit an `Error` for observability, SKIP
    /// `TurnComplete` (persistence is that event's contract — the turn is absent
    /// from history), and return `true` to keep the loop alive. `TurnComplete` is
    /// still emitted exactly once, and only when the turn actually persisted.
    async fn finalize_confirmed_turn(
        &self,
        chat_result: std::result::Result<Result<TurnOutcome>, tokio::task::JoinError>,
        input: &str,
    ) -> bool {
        match chat_result {
            Ok(Ok(outcome)) => {
                let response_text = outcome.text.clone();
                // Persist the confirmed turn exactly once (user + assistant),
                // keyed to the confirmed transcript.
                if let Err(e) = self
                    .persist_confirmed_turn(input, &response_text, outcome.usage.as_ref())
                    .await
                {
                    // Persistence failed (e.g. transient SQLITE_BUSY from serve +
                    // child both writing the WAL). The reply is already spoken, so
                    // surface the error but keep the conversation going — no
                    // TurnComplete (nothing landed in history), no wake-word reset.
                    tracing::warn!("Failed to persist confirmed turn: {}", e);
                    self.emit_event(WorkflowEvent::Error {
                        message: format!("failed to persist turn: {e}"),
                    });
                    return true;
                }
                self.record_turn_outcome(&outcome).await;
                self.emit_event(WorkflowEvent::AgentOutput(response_text));
                self.emit_event(WorkflowEvent::TurnComplete {
                    session_id: self.session_id.clone(),
                });
                true
            }
            Ok(Err(e)) => {
                eprintln!("  ❌ Error: {}", e);
                self.emit_event(WorkflowEvent::Error {
                    message: e.to_string(),
                });
                false
            }
            Err(join_err) => {
                eprintln!("  ❌ Error: {}", join_err);
                self.emit_event(WorkflowEvent::Error {
                    message: join_err.to_string(),
                });
                false
            }
        }
    }

    /// Emit a workflow event: trace it, then forward to the optional sink
    /// (e.g. the `--json-events` NDJSON writer). `None` sink is zero-cost.
    fn emit_event(&self, event: WorkflowEvent) {
        match &event {
            WorkflowEvent::StateChanged { state } => {
                tracing::debug!("Workflow state: {}", state);
            }
            WorkflowEvent::UserInput(text) => {
                tracing::debug!("User input: {}", text);
            }
            WorkflowEvent::AgentOutput(text) => {
                tracing::debug!("Agent output: {}", text);
            }
            WorkflowEvent::Exit { reason } => {
                tracing::debug!("Workflow exit requested: {}", reason);
            }
            WorkflowEvent::Ready { session_id } => {
                tracing::debug!(session_id = %session_id, "Voice session ready");
            }
            WorkflowEvent::Warmup { state } => {
                tracing::debug!("Prefix warm-up: {}", state);
            }
            WorkflowEvent::Transcript { text } => {
                tracing::debug!("Transcript: {}", text);
            }
            WorkflowEvent::Token { .. } => {
                // High-frequency — do not trace per-token.
            }
            WorkflowEvent::ToolCall { tool, id } => {
                tracing::debug!(tool = %tool, id = %id, "Tool call");
            }
            WorkflowEvent::ToolResult { tool, id, .. } => {
                tracing::debug!(tool = %tool, id = %id, "Tool result");
            }
            WorkflowEvent::TurnComplete { session_id } => {
                tracing::debug!(session_id = %session_id, "Turn complete");
            }
            WorkflowEvent::Error { message } => {
                tracing::debug!("Workflow error: {}", message);
            }
            WorkflowEvent::AudioLevel { .. } => {
                // High-frequency — do not trace per-level (same as Token).
            }
        }

        if let Some(sink) = &self.event_sink {
            sink(&event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::mocks::mock_provider::MockProvider;
    use crate::shared::mocks::mock_agent::MockAgent;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn chat_once_returns_echo() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        let result = service.chat_once("Hello!".to_string()).await.unwrap();
        assert_eq!(result, "Echo: Hello!");
    }

    // ── activity events (Agent / Inference / Tool) ───────────────────────

    use crate::security::domain::event::EventCategory;
    use crate::security::mocks::mock_event_log::MockEventLog;

    /// A tool_result JSON string in the shape the chat handler builds.
    fn tool_result(tool: &str) -> String {
        serde_json::json!({ "tool_call_id": "id", "tool": tool, "content": "ok" }).to_string()
    }

    #[test]
    fn bare_tool_name_strips_the_mcp_prefix() {
        assert_eq!(
            bare_tool_name("giap-weather__get_current_weather"),
            "get_current_weather"
        );
        assert_eq!(bare_tool_name("ext-filesystem__read_file"), "read_file");
        // No prefix — unchanged.
        assert_eq!(bare_tool_name("save_memory"), "save_memory");
        // Degenerate: trailing separator, keep the original rather than empty.
        assert_eq!(bare_tool_name("weird__"), "weird__");
    }

    async fn service_with_log(session: &str) -> (ChatService, Arc<MockEventLog>) {
        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session(session.to_string()).await.unwrap();
        let log = Arc::new(MockEventLog::default());
        let service = ChatService::new(Arc::new(MockAgent::new()), session.to_string(), storage)
            .with_event_log(log.clone());
        (service, log)
    }

    #[tokio::test]
    async fn turn_emits_agent_inference_and_one_tool_event_each() {
        let (service, log) = service_with_log("sess-a").await;

        service
            .persist_assistant_turn(
                // Prefixed MCP ids as the stream delivers them; events record
                // the bare names.
                vec![
                    tool_result("giap-weather__get_current_weather"),
                    tool_result("giap-memory__save_memory"),
                ],
                "here you go",
                Some((120, 34)),
                Some("gemma-4-E2B-it-Q4_K_M"),
            )
            .await
            .unwrap();

        let events = log.all();
        let by_cat = |c: EventCategory| events.iter().filter(|e| e.category == c).count();
        assert_eq!(by_cat(EventCategory::Agent), 1);
        assert_eq!(by_cat(EventCategory::Inference), 1);
        assert_eq!(by_cat(EventCategory::Tool), 2);

        // Everything is Internal and carries the session id.
        assert!(events
            .iter()
            .all(|e| e.privacy_sensitivity == PrivacySensitivity::Internal
                && e.session_id.as_deref() == Some("sess-a")));

        let agent = events
            .iter()
            .find(|e| e.category == EventCategory::Agent)
            .unwrap();
        assert_eq!(agent.action, "agent.turn");
        assert_eq!(agent.attributes.get("tool_count"), Some(&2_i64.into()));

        let inference = events
            .iter()
            .find(|e| e.category == EventCategory::Inference)
            .unwrap();
        assert_eq!(
            inference.attributes.get("prompt_tokens"),
            Some(&120_i64.into())
        );
        assert_eq!(
            inference.attributes.get("completion_tokens"),
            Some(&34_i64.into())
        );
        assert_eq!(
            inference.attributes.get("model"),
            Some(&"gemma-4-E2B-it-Q4_K_M".into())
        );

        let tools: Vec<_> = events
            .iter()
            .filter(|e| e.category == EventCategory::Tool)
            .filter_map(|e| e.attributes.get("tool").cloned())
            .collect();
        assert!(tools.contains(&"get_current_weather".into()));
        assert!(tools.contains(&"save_memory".into()));
    }

    /// No token usage (the agent_chat_stream path) → an Agent event but no
    /// Inference event, since there is nothing to report.
    #[tokio::test]
    async fn turn_without_usage_emits_no_inference_event() {
        let (service, log) = service_with_log("sess-b").await;

        service
            .persist_assistant_turn(vec![], "hi", None, None)
            .await
            .unwrap();

        let events = log.all();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.category == EventCategory::Agent)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.category == EventCategory::Inference)
                .count(),
            0
        );
    }

    /// Without an event log attached, persistence still succeeds and records
    /// nothing — the feature is opt-in and never fatal.
    #[tokio::test]
    async fn turn_without_event_log_records_nothing_and_succeeds() {
        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("sess-c".to_string()).await.unwrap();
        let service = ChatService::new(Arc::new(MockAgent::new()), "sess-c".to_string(), storage);

        service
            .persist_assistant_turn(vec![tool_result("noop")], "ok", Some((1, 1)), None)
            .await
            .expect("persistence must not fail without an event log");
    }

    // ── truncate_tool_result (NDJSON 2000-char cap) ──────────────────────

    #[test]
    fn truncate_tool_result_leaves_sub_cap_payload_unchanged() {
        let s = "small result".to_string();
        assert_eq!(truncate_tool_result(s), "small result");
    }

    #[test]
    fn truncate_tool_result_returns_exactly_cap_chars_unchanged() {
        // Exactly TOOL_RESULT_MAX_CHARS chars — must not be truncated.
        let s = "x".repeat(TOOL_RESULT_MAX_CHARS);
        let out = truncate_tool_result(s.clone());
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
        assert_eq!(out, s);
    }

    #[test]
    fn truncate_tool_result_caps_oversized_payload_at_char_boundary() {
        let s = "y".repeat(TOOL_RESULT_MAX_CHARS + 500);
        let out = truncate_tool_result(s);
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
    }

    #[test]
    fn truncate_tool_result_cuts_on_utf8_boundary_not_mid_codepoint() {
        // Multi-byte chars straddling the cap must not corrupt the string.
        // "é" is 2 bytes; a naive byte cut at 2000 could split one.
        let s = "é".repeat(TOOL_RESULT_MAX_CHARS + 10);
        let out = truncate_tool_result(s);
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
        // Still valid UTF-8 (would panic on a mid-codepoint truncate).
        assert!(out.chars().all(|c| c == 'é'));
    }

    // ── derive_title_from_text / ensure_session_title (DEF-7) ────────────

    #[test]
    fn derive_title_takes_first_six_words() {
        let title = derive_title_from_text("What is the capital of France exactly?");
        assert_eq!(title, "What is the capital of France");
    }

    #[test]
    fn derive_title_trims_quotes_and_whitespace() {
        assert_eq!(derive_title_from_text("  \"hello world\"  "), "hello world");
        assert_eq!(derive_title_from_text("'single quoted'"), "single quoted");
    }

    #[test]
    fn derive_title_empty_for_blank_input() {
        assert_eq!(derive_title_from_text(""), "");
        assert_eq!(derive_title_from_text("   "), "");
    }

    #[test]
    fn derive_title_caps_long_single_word() {
        let long = "a".repeat(100);
        let title = derive_title_from_text(&long);
        // 60 chars + ellipsis
        assert!(title.chars().count() <= 61);
        assert!(title.ends_with('…'));
    }

    #[tokio::test]
    async fn persist_assistant_turn_sets_deterministic_title() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service
            .persist_user_message("How do I reset my password on the router?")
            .await
            .unwrap();
        service
            .persist_assistant_turn(vec![], "Here's how...", None, None)
            .await
            .unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert_eq!(session.title.as_deref(), Some("How do I reset my password"));
    }

    #[tokio::test]
    async fn persist_assistant_turn_preserves_existing_title() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "kept-title".to_string();
        storage.create_session(session_id.clone()).await.unwrap();
        storage
            .update_title(&session_id, "My custom title".to_string())
            .await
            .unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.persist_user_message("hello there").await.unwrap();
        service
            .persist_assistant_turn(vec![], "hi", None, None)
            .await
            .unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert_eq!(session.title.as_deref(), Some("My custom title"));
    }

    // ── is_dismissal_or_exit_phrase / classify_voice_command ─────────────

    #[test]
    fn dismissal_phrases_are_detected() {
        assert_eq!(classify_voice_command("goodbye"), VoiceCommand::Dismissal);
        assert_eq!(classify_voice_command("Stop."), VoiceCommand::Dismissal);
        assert_eq!(
            classify_voice_command("  STOP LISTENING  "),
            VoiceCommand::Dismissal
        );
        assert!(is_dismissal_or_exit_phrase("goodbye"));
        assert!(is_dismissal_or_exit_phrase("Stop."));
    }

    #[test]
    fn exit_phrases_are_classified_as_exit() {
        assert_eq!(classify_voice_command("exit"), VoiceCommand::Exit);
        assert_eq!(classify_voice_command("quit!"), VoiceCommand::Exit);
        assert!(is_dismissal_or_exit_phrase("exit"));
        assert!(is_dismissal_or_exit_phrase("quit!"));
    }

    #[test]
    fn non_dismissal_text_is_not_flagged() {
        assert_eq!(
            classify_voice_command("what's the weather like"),
            VoiceCommand::Normal
        );
        assert_eq!(
            classify_voice_command("stop and think about this"),
            VoiceCommand::Normal
        );
        assert_eq!(classify_voice_command(""), VoiceCommand::Normal);
        assert!(!is_dismissal_or_exit_phrase("what's the weather like"));
        assert!(!is_dismissal_or_exit_phrase("stop and think about this"));
        assert!(!is_dismissal_or_exit_phrase(""));
    }

    // ── listen_with_speculative_chat (Q2-26) ─────────────────────────────

    /// Drives `listen_with_speculative`'s callback through a scripted
    /// sequence of signals (yielding briefly after each so the caller's
    /// `tokio::select!` loop gets a chance to react), then resolves with
    /// `final_transcript` — mirroring how `record_mono_f32_vad` behaves for
    /// a confirmed recording.
    struct ScriptedSpeculativeVoiceInput {
        signals: Vec<SpeculativeSignal>,
        final_transcript: Option<String>,
    }

    #[async_trait::async_trait]
    impl VoiceInput for ScriptedSpeculativeVoiceInput {
        async fn listen(&self) -> Result<Option<String>> {
            Ok(self.final_transcript.clone())
        }

        async fn listen_with_speculative(
            &self,
            on_speculative: Box<dyn Fn(SpeculativeSignal) + Send + Sync>,
        ) -> Result<Option<String>> {
            for sig in &self.signals {
                on_speculative(sig.clone());
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            Ok(self.final_transcript.clone())
        }

        fn prompt(&self) -> &str {
            "> "
        }
    }

    #[tokio::test]
    async fn speculative_chat_runs_and_is_reused_when_confirmed() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let voice_input = Arc::new(ScriptedSpeculativeVoiceInput {
            signals: vec![SpeculativeSignal::Ready("hello".to_string())],
            final_transcript: Some("hello".to_string()),
        });

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(voice_input);

        let (transcript, speculative) = service.listen_with_speculative_chat().await.unwrap();
        assert_eq!(transcript, Some("hello".to_string()));

        let (spec_transcript, handle) = speculative
            .expect("confirmed, non-dismissal transcript must yield a speculative handle");
        assert_eq!(
            spec_transcript, "hello",
            "the handle must carry the provisional transcript for the confirm-vs-spec gate"
        );
        let response = handle.await.unwrap().unwrap();
        assert_eq!(response.text, "Echo: hello");

        // The speculative stream persists NOTHING — the run_loop gate owns
        // persistence after confirmation. Storage must still be empty here.
        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "speculative stream must not persist; found {} messages",
            msgs.len()
        );
    }

    #[tokio::test]
    async fn speculative_chat_invalidated_on_resumed_speech_yields_no_handle() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let voice_input = Arc::new(ScriptedSpeculativeVoiceInput {
            signals: vec![
                SpeculativeSignal::Ready("hello".to_string()),
                SpeculativeSignal::Invalidated,
            ],
            final_transcript: Some("hello there".to_string()),
        });

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(voice_input);

        let (transcript, speculative) = service.listen_with_speculative_chat().await.unwrap();
        assert_eq!(transcript, Some("hello there".to_string()));
        assert!(
            speculative.is_none(),
            "a false pause must abort the speculative job, not hand it back"
        );
    }

    #[tokio::test]
    async fn speculative_chat_skips_dismissal_phrases() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let voice_input = Arc::new(ScriptedSpeculativeVoiceInput {
            signals: vec![SpeculativeSignal::Ready("goodbye".to_string())],
            final_transcript: Some("goodbye".to_string()),
        });

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(voice_input);

        let (transcript, speculative) = service.listen_with_speculative_chat().await.unwrap();
        assert_eq!(transcript, Some("goodbye".to_string()));
        assert!(
            speculative.is_none(),
            "dismissal phrases must never get a speculative chat call"
        );
    }

    // ── Phantom-turn persistence invariant (Q2-26, data integrity) ───────

    #[tokio::test]
    async fn confirmed_turn_persists_exactly_once_user_and_assistant() {
        // The persisting entry point (chat_stream_once) writes exactly two
        // messages: the user turn and the assistant turn. This is the ground
        // truth the speculative path must match — no more, no less.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service
            .chat_stream_once("what's the weather".to_string())
            .await
            .unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "exactly one user + one assistant message must persist per turn"
        );
    }

    #[tokio::test]
    async fn stream_response_inner_persists_nothing() {
        // The non-persisting core used by the speculative path must never
        // touch storage — that is what makes discarding a wrong provisional
        // transcript safe (no phantom turn).
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        let response = service
            .stream_response_inner("hello".to_string(), std::time::Instant::now())
            .await
            .unwrap();
        assert_eq!(response.text, "Echo: hello");

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "stream_response_inner must not persist; found {} messages",
            msgs.len()
        );
    }

    #[tokio::test]
    async fn persist_confirmed_turn_writes_confirmed_transcript_only() {
        // Simulates the run_loop gate: a speculative stream ran on a provisional
        // transcript ("weath"), but the CONFIRMED transcript ("what's the
        // weather") is what gets persisted — never the provisional one. Exactly
        // one user + one assistant message, keyed to the confirmed text.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());

        // Provisional stream (no persistence).
        let response = service
            .stream_response_inner("weath".to_string(), std::time::Instant::now())
            .await
            .unwrap();

        // Confirmed transcript differs — commit the CONFIRMED one.
        service
            .persist_confirmed_turn(
                "what's the weather",
                &response.text,
                response.usage.as_ref(),
            )
            .await
            .unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(msgs.len(), 2, "exactly one user + one assistant turn");
        assert_eq!(
            msgs[0].message.content, "what's the weather",
            "the persisted user turn must be the CONFIRMED transcript, never the provisional one"
        );
    }

    #[tokio::test]
    async fn chat_stream_once_sets_voice_mode_true() {
        // chat_stream_once is only ever reached via run_loop, the voice CLI
        // loop — regression test for Q2-23 (voice_mode was hardcoded false,
        // silently dropping the TTS-friendly prompt + thinking suppression
        // that desktop's voice path already gets).
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent.clone(), session_id.clone(), storage.clone());
        service
            .chat_stream_once("Hello!".to_string())
            .await
            .unwrap();

        let request = agent.last_request().expect("agent should have been called");
        assert!(
            request.voice_mode,
            "chat_stream_once must set voice_mode: true so the agent renders \
             the <voice-mode> prompt section"
        );
    }

    #[tokio::test]
    async fn chat_persists_messages_to_storage() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service
            .chat_once("First message".to_string())
            .await
            .unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 2); // User message + Assistant response
        assert_eq!(messages[0].message.content, "First message");
        assert!(messages[1].message.content.contains("First message"));
    }

    #[tokio::test]
    async fn chat_messages_persist_across_iterations() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());

        // First iteration
        service.chat_once("Message 1".to_string()).await.unwrap();

        // Second iteration
        service.chat_once("Message 2".to_string()).await.unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 4); // 2 iterations × 2 messages each
        assert_eq!(messages[0].message.content, "Message 1");
        assert_eq!(messages[2].message.content, "Message 2");
    }

    #[tokio::test]
    async fn chat_with_provider_auto_generates_title() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-test".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service =
            ChatService::new(agent, session_id.clone(), storage.clone()).with_provider(provider);

        // First message → triggers title generation
        service
            .chat_once("What is the weather?".to_string())
            .await
            .unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        // MockProvider returns "Mock response to: ..." which becomes the title
        assert!(
            session.title.is_some(),
            "Title should be auto-generated after first exchange"
        );
        let title = session.title.unwrap();
        assert!(!title.is_empty(), "Title should not be empty");
    }

    #[tokio::test]
    async fn title_not_regenerated_on_second_message() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-stable".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service =
            ChatService::new(agent, session_id.clone(), storage.clone()).with_provider(provider);

        // First message → generates title
        service.chat_once("Hello".to_string()).await.unwrap();
        let first_title = storage
            .get_session(&session_id)
            .await
            .unwrap()
            .title
            .clone();

        // Second message → should NOT overwrite title
        service.chat_once("How are you?".to_string()).await.unwrap();
        let second_title = storage
            .get_session(&session_id)
            .await
            .unwrap()
            .title
            .clone();

        assert_eq!(
            first_title, second_title,
            "Title should not change after first generation"
        );
    }

    /// The first-exchange title is a guess made from two messages, and it must
    /// be recorded as the machine's guess.
    ///
    /// Marking it `user` — which is what writing it through `update_title`
    /// does — freezes that guess for the life of the conversation, because the
    /// idle pass refuses to touch anything a person named. A chat that opens
    /// about milk and becomes about bitcoin would keep the milk name forever,
    /// and nothing in the interface would explain why.
    #[tokio::test]
    async fn a_first_exchange_title_is_recorded_as_the_models_guess() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-provenance".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service =
            ChatService::new(agent, session_id.clone(), storage.clone()).with_provider(provider);
        service.chat_once("Hello".to_string()).await.unwrap();

        let (source, through) = storage.get_title_provenance(&session_id).await.unwrap();
        assert_eq!(
            source.as_deref(),
            Some("model"),
            "a title the model wrote must not masquerade as one a person typed"
        );
        assert!(
            through.is_some(),
            "the reach must be recorded, or the idle pass cannot tell when the \
             conversation has outgrown this name"
        );
    }

    /// Whatever the model says, the stored title obeys the same ceiling the
    /// re-titling service enforces. Before both paths shared a normaliser this
    /// one kept 80 characters of whatever came back.
    #[tokio::test]
    async fn a_first_exchange_title_obeys_the_shared_ten_word_ceiling() {
        use crate::shared::services::session_title::MAX_TITLE_WORDS;

        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-ceiling".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service =
            ChatService::new(agent, session_id.clone(), storage.clone()).with_provider(provider);
        service.chat_once("Hello".to_string()).await.unwrap();

        if let Some(title) = storage.get_session(&session_id).await.unwrap().title {
            assert!(
                title.split_whitespace().count() <= MAX_TITLE_WORDS,
                "stored title {title:?} exceeds the ceiling"
            );
        }
    }

    #[tokio::test]
    async fn no_title_without_provider() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "no-provider".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        // No provider → title stays None
        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.chat_once("Hello".to_string()).await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert!(
            session.title.is_none(),
            "No title should be set without a provider"
        );
    }

    #[tokio::test]
    async fn with_voice_input_builder_compiles() {
        use crate::shared::services::stdin_input::StdinInput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service = ChatService::new(agent, session_id, storage)
            .with_voice_input(Arc::new(StdinInput::new()));
    }

    #[tokio::test]
    async fn with_voice_output_builder_compiles() {
        use crate::shared::services::print_output::PrintOutput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service =
            ChatService::new(agent, session_id, storage).with_voice_output(Arc::new(PrintOutput));
    }

    // ── InstantActivation race fix + event sink (terminal-voice-in-desktop) ──
    //
    // These are the CI-visible regressions for the class of bug where the
    // run_loop select! interrupt race fired before every turn could complete
    // whenever the wired detector resolved instantly (InstantActivation).
    // pond-server is check-only in CI, so this coverage lives in pond-core.

    /// Plays a scripted list of utterances, then `None` (EOF) so `run_loop`
    /// exits cleanly. Mirrors the pond-server pipeline test's double.
    struct ScriptedListenInput {
        script: std::sync::Mutex<std::collections::VecDeque<Option<String>>>,
    }

    impl ScriptedListenInput {
        fn new(lines: impl IntoIterator<Item = &'static str>) -> Self {
            let mut deque: std::collections::VecDeque<Option<String>> =
                lines.into_iter().map(|s| Some(s.to_string())).collect();
            deque.push_back(None); // trailing None → end-of-input (stdin EOF)
            Self {
                script: std::sync::Mutex::new(deque),
            }
        }
    }

    #[async_trait::async_trait]
    impl VoiceInput for ScriptedListenInput {
        async fn listen(&self) -> Result<Option<String>> {
            Ok(self.script.lock().unwrap().pop_front().flatten())
        }
    }

    /// Captures every string passed to `speak()`.
    #[derive(Default)]
    struct CapturingSpeak {
        spoken: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl VoiceOutput for CapturingSpeak {
        async fn speak(&self, text: &str) -> Result<()> {
            self.spoken.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    /// Counts thinking-tone starts and stops. `speak` is irrelevant here.
    #[derive(Default)]
    struct CountingTone {
        starts: std::sync::atomic::AtomicUsize,
        stops: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl VoiceOutput for CountingTone {
        async fn speak(&self, _text: &str) -> Result<()> {
            Ok(())
        }
        fn start_thinking_tone(&self) {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn stop_thinking_tone(&self) {
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl CountingTone {
        fn counts(&self) -> (usize, usize) {
            (
                self.starts.load(std::sync::atomic::Ordering::SeqCst),
                self.stops.load(std::sync::atomic::Ordering::SeqCst),
            )
        }
    }

    /// The switch is the whole feature: `voice_thinking_tone_enabled = false`
    /// must reach `start_thinking_tone` and stop it being called at all. A tone
    /// that starts and is immediately stopped is not "off" -- it is a click.
    #[test]
    fn a_disabled_tone_never_starts() {
        let out = Arc::new(CountingTone::default());
        {
            let _tone = WorkingTone::start(out.clone(), false);
        }
        assert_eq!(
            out.counts().0,
            0,
            "the tone started despite the setting being off"
        );
    }

    /// The half that was deleted in 0136f8c5 and is being restored: with the
    /// setting ON the tone must actually play. Asserting the count rather than
    /// "no panic" is the point -- the regression this guards was a silent
    /// no-op, which every looser assertion would have passed.
    #[test]
    fn an_enabled_tone_starts_once_and_stops_once() {
        let out = Arc::new(CountingTone::default());
        {
            let mut tone = WorkingTone::start(out.clone(), true);
            assert_eq!(out.counts(), (1, 0), "tone did not start when enabled");
            // The first speakable sentence can arrive down several paths, so
            // stop() is called more than once in practice.
            tone.stop();
            tone.stop();
            assert_eq!(out.counts(), (1, 1), "stop() is not idempotent");
        }
        assert_eq!(
            out.counts(),
            (1, 1),
            "Drop stopped an already-stopped tone a second time"
        );
    }

    /// Dropping without an explicit `stop()` -- the `?`-on-stream-error path
    /// that motivated the guard -- must still silence the tone.
    #[test]
    fn dropping_the_guard_stops_a_running_tone() {
        let out = Arc::new(CountingTone::default());
        {
            let _tone = WorkingTone::start(out.clone(), true);
        }
        assert_eq!(
            out.counts(),
            (1, 1),
            "an early return left the tone playing for the life of the process"
        );
    }

    /// A thread-safe collector for the event sink.
    #[derive(Clone, Default)]
    struct EventCollector {
        events: Arc<std::sync::Mutex<Vec<WorkflowEvent>>>,
    }

    impl EventCollector {
        fn sink(&self) -> WorkflowEventSink {
            let events = self.events.clone();
            Arc::new(move |ev: &WorkflowEvent| {
                events.lock().unwrap().push(ev.clone());
            })
        }

        fn ndjson_events(&self) -> Vec<WorkflowEvent> {
            // Only the events that map to an NDJSON contract line.
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.to_ndjson().is_some())
                .cloned()
                .collect()
        }
    }

    /// Session storage whose `add_message` always fails — simulates a transient
    /// persist failure (e.g. SQLITE_BUSY from serve + child WAL contention).
    /// Everything else delegates to an in-memory store so setup/reads work.
    struct FailingAddStorage {
        inner: InMemorySessionStorage,
    }

    impl FailingAddStorage {
        fn new() -> Self {
            Self {
                inner: InMemorySessionStorage::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::user_data::ports::session_storage::SessionStorage for FailingAddStorage {
        async fn create_session(
            &self,
            session_id: String,
        ) -> std::result::Result<
            crate::user_data::domain::session::Session,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.create_session(session_id).await
        }
        async fn get_session(
            &self,
            session_id: &str,
        ) -> std::result::Result<
            crate::user_data::domain::session::Session,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.get_session(session_id).await
        }
        async fn add_message(
            &self,
            _session_id: String,
            _message: crate::user_data::domain::session::SessionMessage,
        ) -> std::result::Result<
            crate::user_data::domain::session::SessionMessage,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            Err(
                crate::user_data::ports::session_storage::SessionStorageError::StorageError(
                    "simulated SQLITE_BUSY".to_string(),
                ),
            )
        }
        async fn get_messages(
            &self,
            session_id: &str,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::SessionMessage>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.get_messages(session_id).await
        }
        async fn update_title(
            &self,
            session_id: &str,
            title: String,
        ) -> std::result::Result<(), crate::user_data::ports::session_storage::SessionStorageError>
        {
            self.inner.update_title(session_id, title).await
        }
        async fn delete_session(
            &self,
            session_id: &str,
        ) -> std::result::Result<(), crate::user_data::ports::session_storage::SessionStorageError>
        {
            self.inner.delete_session(session_id).await
        }
        async fn list_sessions(
            &self,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::Session>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.list_sessions().await
        }
        async fn get_messages_paginated(
            &self,
            session_id: &str,
            limit: usize,
            offset: usize,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::SessionMessage>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner
                .get_messages_paginated(session_id, limit, offset)
                .await
        }
        async fn get_recent_messages(
            &self,
            session_id: &str,
            limit: usize,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::SessionMessage>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.get_recent_messages(session_id, limit).await
        }
        async fn count_messages(
            &self,
            session_id: &str,
        ) -> std::result::Result<u64, crate::user_data::ports::session_storage::SessionStorageError>
        {
            self.inner.count_messages(session_id).await
        }
        async fn first_user_message(
            &self,
            session_id: &str,
        ) -> std::result::Result<
            Option<String>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.first_user_message(session_id).await
        }
    }

    /// A microphone that fails and then recovers must not end the session.
    ///
    /// This killed a live session: another process took the input device, the
    /// detector could not open a stream, and the `?` propagated straight out
    /// of `run_loop` — "The requested stream configuration is not supported by
    /// the device", process gone. Devices come and go; the loop waits.
    #[tokio::test]
    async fn a_microphone_that_fails_then_recovers_does_not_end_the_session() {
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicU32, Ordering};

        /// Fails `fail_times` times, then activates.
        struct FlakyMic {
            attempts: AtomicU32,
            fail_times: u32,
        }

        #[async_trait]
        impl StreamingWakeWordDetector for FlakyMic {
            async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
                if self.attempts.fetch_add(1, Ordering::SeqCst) < self.fail_times {
                    return Err(anyhow::anyhow!(
                        "The requested stream configuration is not supported by the device."
                    ));
                }
                Ok(WakeWordActivation {
                    captured_audio: None,
                })
            }
            fn supports_interruption(&self) -> bool {
                false
            }
        }

        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "flaky-mic".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let output = Arc::new(CapturingSpeak::default());
        let svc = ChatService::new(agent, session_id, storage)
            .with_voice_input(Arc::new(ScriptedListenInput::new(["what time is it"])))
            .with_voice_output(output.clone())
            .with_wake_word_detector(Arc::new(FlakyMic {
                attempts: AtomicU32::new(0),
                fail_times: 2,
            }));

        svc.run_loop()
            .await
            .expect("a recoverable device error must not end the loop");

        let spoken = output.spoken.lock().unwrap().clone();
        assert!(
            spoken.iter().any(|s| s.contains("what time is it")),
            "the turn must run once the microphone came back; got: {spoken:?}"
        );
    }

    /// A microphone that never comes back must still report itself, rather
    /// than retrying in silence for the life of the process.
    #[tokio::test(start_paused = true)]
    async fn a_microphone_that_never_recovers_reports_instead_of_hanging() {
        use async_trait::async_trait;

        struct DeadMic;

        #[async_trait]
        impl StreamingWakeWordDetector for DeadMic {
            async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
                Err(anyhow::anyhow!("no audio input device found"))
            }
            fn supports_interruption(&self) -> bool {
                false
            }
        }

        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "dead-mic".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let svc =
            ChatService::new(agent, session_id, storage).with_wake_word_detector(Arc::new(DeadMic));

        let err = svc
            .run_loop()
            .await
            .expect_err("a permanently absent microphone must surface");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("microphone"),
            "the error must name the microphone: {msg}"
        );
    }

    #[tokio::test]
    async fn run_loop_completes_turn_under_instant_activation() {
        // REGRESSION (InstantActivation race): with the default InstantActivation
        // detector (stdin / --no-wake-word), run_loop used to abort every turn
        // via the interrupt race. The turn must now complete and reach speak().
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "instant-race".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let output = Arc::new(CapturingSpeak::default());
        let input = Arc::new(ScriptedListenInput::new(["what is the capital of France"]));

        // Default wake-word detector is InstantActivation (supports_interruption=false).
        let svc = ChatService::new(agent, session_id, storage)
            .with_voice_input(input)
            .with_voice_output(output.clone());

        svc.run_loop().await.unwrap();

        let spoken = output.spoken.lock().unwrap().clone();
        assert!(
            spoken.iter().any(|s| s.contains("what is the capital")),
            "the turn must complete and reach speak(); got: {spoken:?}"
        );
    }

    #[tokio::test]
    async fn run_loop_persists_exactly_one_turn_under_instant_activation() {
        // The completed turn must persist exactly one user + one assistant
        // message — no phantom turns, no double-persist.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "instant-persist".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let input = Arc::new(ScriptedListenInput::new(["hello there"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()));

        svc.run_loop().await.unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "exactly one user + one assistant message must persist"
        );
    }

    #[tokio::test]
    async fn run_loop_derives_deterministic_session_title_without_provider() {
        // The live GooseAdapter voice path builds ChatService WITHOUT a
        // provider, so the LLM title path never runs. persist_confirmed_turn
        // must still derive a readable deterministic title from the first
        // utterance so the chat sidebar never shows a raw session id.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-voice-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let input = Arc::new(ScriptedListenInput::new([
            "how do I reset the router password",
        ]));
        // No .with_provider() — mirrors the live voice path.
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()));

        svc.run_loop().await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert_eq!(
            session.title.as_deref(),
            Some("how do I reset the router"),
            "voice turn must derive a deterministic title without a provider"
        );
    }

    #[tokio::test]
    async fn run_loop_emits_contract_event_sequence_for_a_turn() {
        // Asserts the NDJSON event sequence for a single scripted turn:
        // state changes in order, transcript exactly once, tokens streamed,
        // turn_complete exactly once on completion, exit stdin_eof at the end.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "seq-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let input = Arc::new(ScriptedListenInput::new(["tell me a joke"]));
        let svc = ChatService::new(agent, session_id.clone(), storage)
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        svc.run_loop().await.unwrap();

        let events = collector.ndjson_events();

        // The turn's states must appear in order Wait → Listen → Thinking →
        // Speak. (After the turn the loop keeps cycling Wait/Listen while the
        // script drains to EOF, so assert the ordered prefix, not equality.)
        let states: Vec<WorkflowState> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::StateChanged { state } => Some(*state),
                _ => None,
            })
            .collect();
        let turn_states = &states[..4.min(states.len())];
        assert_eq!(
            turn_states,
            [
                WorkflowState::Wait,
                WorkflowState::Listen,
                WorkflowState::Thinking,
                WorkflowState::Speak,
            ],
            "the turn's state transitions must be Wait→Listen→Thinking→Speak; got {states:?}"
        );
        // Thinking must precede Speak, and both occur exactly once for one turn.
        assert_eq!(
            states
                .iter()
                .filter(|s| **s == WorkflowState::Thinking)
                .count(),
            1,
            "exactly one Thinking for one turn"
        );
        assert_eq!(
            states
                .iter()
                .filter(|s| **s == WorkflowState::Speak)
                .count(),
            1,
            "exactly one Speak for one turn"
        );

        // Exactly one transcript, carrying the confirmed utterance.
        let transcripts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Transcript { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(transcripts, vec!["tell me a joke"], "one transcript event");

        // At least one token streamed.
        let token_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::Token { .. }))
            .count();
        assert!(
            token_count >= 1,
            "tokens must be streamed; got {token_count}"
        );

        // Exactly one turn_complete on completion.
        let turn_complete_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 1,
            "exactly one turn_complete on completion"
        );

        // Ready is NOT emitted by run_loop (the CLI emits it after model load);
        // exit(stdin_eof) is the last contract event.
        match events.last() {
            Some(WorkflowEvent::Exit { reason }) => assert_eq!(reason, "stdin_eof"),
            other => panic!("last event must be exit(stdin_eof); got {other:?}"),
        }
    }

    #[tokio::test]
    async fn thought_filter_tail_is_emitted_as_token_and_matches_persisted_text() {
        // REGRESSION (#153, event stream): when ThoughtFilter's lookahead holds
        // back the final bytes of a response, the flushed tail is appended to the
        // persisted/spoken text but was NOT emitted as a Token — so the desktop
        // caption (built solely from Token events) ended short of the reply.
        //
        // The response text here ends in a partial sentinel prefix ("<end_of_tu"),
        // which the filter withholds in Normal state until flush(). MockAgent
        // echoes the user message, so we drive the tail deterministically via the
        // utterance. The invariant we lock: the concatenation of all Token event
        // contents equals the persisted assistant message text.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "tail-token-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let input = Arc::new(ScriptedListenInput::new(["the code is 42<end_of_tu"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        svc.run_loop().await.unwrap();

        let events = collector.events.lock().unwrap().clone();

        // Concatenate every Token event's content (in emit order).
        let streamed: String = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Token { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();

        // The persisted assistant message is the ground truth for the reply text.
        let msgs = storage.get_messages(&session_id).await.unwrap();
        let assistant = msgs
            .iter()
            .find(|m| m.message.role == crate::models::domain::message::Role::Assistant)
            .expect("assistant turn must persist");

        assert_eq!(
            streamed, assistant.message.content,
            "the streamed Token events must reconstruct the full persisted reply, \
             including the ThoughtFilter tail flushed at stream end"
        );
        // Sanity: the reply's trailing bytes (withheld by the filter's lookahead
        // and released only at flush) reached the Token stream.
        assert!(
            streamed.ends_with("<end_of_tu"),
            "the withheld tail must reach the Token stream; got {streamed:?}"
        );
    }

    #[tokio::test]
    async fn no_tail_is_withheld_when_the_reply_ends_on_ordinary_text() {
        // The sibling #153 never got. That fix guaranteed the withheld tail is
        // released at stream END; it left the filter withholding a fixed 16
        // bytes on every push regardless of content, so the caption trailed
        // generation for the whole reply and froze mid-word whenever generation
        // slowed. A reply that ends on ordinary text must now leave the filter
        // with nothing to flush at all.
        //
        // Deliberately the mirror of the test above: same harness, same
        // assertion on reconstruction, but an utterance whose tail CANNOT begin
        // a marker. Together they pin both halves -- an ambiguous tail is still
        // held, an unambiguous one never is.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "no-tail-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let utterance = "the kettle is on";
        let input = Arc::new(ScriptedListenInput::new([utterance]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        svc.run_loop().await.unwrap();

        let events = collector.events.lock().unwrap().clone();
        let tokens: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Token { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        let assistant = msgs
            .iter()
            .find(|m| m.message.role == crate::models::domain::message::Role::Assistant)
            .expect("assistant turn must persist");

        let streamed: String = tokens.concat();
        assert_eq!(
            streamed, assistant.message.content,
            "the streamed Token events must reconstruct the full persisted reply"
        );
        // The load-bearing half: the first Token already carries the reply's
        // opening bytes. Under the old fixed lookahead the first 16 bytes were
        // withheld, so a reply this short emitted NOTHING until flush.
        assert!(
            !tokens.is_empty(),
            "an ordinary reply must produce at least one Token before flush"
        );
        // The load-bearing assertion. MockAgent delivers this reply as a single
        // chunk, so with no holdback the FIRST Token is the whole reply and
        // flush contributes nothing. Under the old fixed 16-byte lookahead the
        // first Token would have been the reply minus its last 16 bytes, with
        // the remainder arriving only at flush -- i.e. two Tokens, the first
        // one truncated mid-word.
        assert_eq!(
            tokens[0], assistant.message.content,
            "the first Token must carry the whole single-chunk reply, not a \
             lookahead-truncated prefix"
        );
        assert_eq!(
            tokens.len(),
            1,
            "flush must contribute no Token when the reply ends on ordinary \
             text; got {tokens:?}"
        );
    }

    #[tokio::test]
    async fn finalize_persist_failure_keeps_loop_alive_and_skips_turn_complete() {
        // REGRESSION: a transient persist failure (SQLITE_BUSY from serve + child
        // WAL contention) used to hard-fail the turn — resetting to wake-word mode
        // mid-conversation for a reply the user already heard. Now finalize must:
        //   - emit an Error event (observability),
        //   - NOT emit TurnComplete (persistence is that event's contract),
        //   - return true so the loop stays in conversational mode.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(FailingAddStorage::new());
        let session_id = "persist-fail-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let svc = ChatService::new(agent, session_id.clone(), storage)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        // Drive finalize_confirmed_turn directly with a successfully-streamed reply
        // whose persistence will fail (add_message always errors).
        let stayed_conversational = svc
            .finalize_confirmed_turn(
                Ok(Ok(TurnOutcome {
                    text: "the answer is 42".to_string(),
                    usage: None,
                    stats: None,
                    total_latency_ms: 0,
                })),
                "what is the answer",
            )
            .await;

        assert!(
            stayed_conversational,
            "persist failure must NOT reset to wake-word mode — the reply was already spoken"
        );

        let events = collector.events.lock().unwrap().clone();

        let error_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::Error { .. }))
            .count();
        assert_eq!(error_count, 1, "exactly one Error event on persist failure");

        let turn_complete_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 0,
            "no TurnComplete when the turn failed to persist"
        );
    }

    /// A detector that supports interruption and interrupts immediately, to
    /// prove the interrupt path (interruptible detector) emits ZERO
    /// turn_complete and persists NOTHING.
    struct AlwaysInterruptDetector;

    #[async_trait::async_trait]
    impl StreamingWakeWordDetector for AlwaysInterruptDetector {
        async fn wait_for_activation_with_audio(
            &self,
        ) -> Result<crate::models::ports::wake_word::WakeWordActivation> {
            // First call (Wait phase): return quickly so the loop enters Listen.
            // The interrupt race then re-enters here and wins immediately.
            Ok(crate::models::ports::wake_word::WakeWordActivation {
                captured_audio: None,
            })
        }
        fn supports_interruption(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn interrupted_turn_persists_nothing_and_emits_no_turn_complete() {
        // With an interruptible detector that fires instantly, the in-flight
        // turn is aborted: NO persistence, NO turn_complete. This is the
        // exactly-once / none-on-interrupt invariant on the interrupt path.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "interrupt-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        // Listen returns text on the first turn, then None so the loop can end
        // after the interrupt stashes/discards.
        let input = Arc::new(ScriptedListenInput::new(["a long question"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_wake_word_detector(Arc::new(AlwaysInterruptDetector))
            .with_event_sink(collector.sink());

        // MockAgent sleeps 300ms before streaming, so the instant wake future
        // wins the race deterministically.
        svc.run_loop().await.unwrap();

        // Nothing persisted — the interrupted turn never committed.
        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "an interrupted turn must persist nothing; found {} messages",
            msgs.len()
        );

        // No turn_complete emitted for the interrupted turn.
        let turn_complete_count = collector
            .ndjson_events()
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 0,
            "an interrupted turn must NOT emit turn_complete"
        );
    }

    // ── PAI-7 P6: speaking first ──────────────────────────────────────────

    use crate::user_data::domain::settings::Settings;

    fn at(hour: u32, minute: u32) -> LocalTimeOfDay {
        LocalTimeOfDay::new(hour, minute).expect("a real clock face")
    }

    /// Speech switched on, quiet hours at their shipped default.
    fn speech_on() -> Settings {
        Settings {
            unprompted_speech_enabled: true,
            ..Default::default()
        }
    }

    fn liz() -> ProfileScope {
        ProfileScope::Owner("liz".to_string())
    }

    struct Utt {
        audience: ProfileScope,
        category: String,
        text: String,
        present: Vec<String>,
        now: LocalTimeOfDay,
        turn_in_flight: bool,
    }

    impl Utt {
        /// The case that SHOULD speak: Liz is here, it is the afternoon, an
        /// alert, nobody mid-turn. Every test below is this minus one thing.
        fn speakable() -> Self {
            Self {
                audience: liz(),
                category: "alert".into(),
                text: "The freezer has been above -15 for an hour.".into(),
                present: vec!["liz".into()],
                now: at(14, 30),
                turn_in_flight: false,
            }
        }
        fn as_utterance(&self) -> UnpromptedUtterance<'_> {
            UnpromptedUtterance {
                audience: &self.audience,
                category: &self.category,
                text: &self.text,
                present_members: &self.present,
                now: self.now,
                turn_in_flight: self.turn_in_flight,
            }
        }
    }

    fn decide(settings: Option<&Settings>, u: &Utt) -> UnpromptedSpeech {
        decide_unprompted_speech(settings, &u.as_utterance())
    }

    /// The positive case, and the vacuity control for every refusal test in
    /// this section: without it they would all pass against a gate whose body
    /// was `Refused(NotEnabled)`.
    #[test]
    fn an_enabled_present_member_outside_quiet_hours_is_spoken_to() {
        assert_eq!(
            decide(Some(&speech_on()), &Utt::speakable()),
            UnpromptedSpeech::Spoken
        );
    }

    /// PAI-7 invariant 6. "Absolute" is a claim about ORDERING as much as about
    /// the condition, so this asserts the reason is quiet hours rather than
    /// merely that nothing was said -- a gate that refused for some other
    /// reason first would pass a "did it stay quiet" assertion while leaving
    /// the window overridable by whatever it checked first.
    #[test]
    fn quiet_hours_refuse_before_anything_else_can_permit() {
        let mut spoke_outside = 0;
        let mut checked = 0;
        for category in ["alert", "info", "action_required"] {
            for audience in [liz(), ProfileScope::Household, ProfileScope::Guest] {
                for present in [vec![], vec!["liz".to_string()]] {
                    for turn_in_flight in [false, true] {
                        for enabled in [false, true] {
                            let settings = Settings {
                                unprompted_speech_enabled: enabled,
                                unprompted_speech_categories: "alert,info,action_required".into(),
                                ..Default::default()
                            };
                            let mut u = Utt::speakable();
                            u.audience = audience.clone();
                            u.category = category.into();
                            u.present = present.clone();
                            u.turn_in_flight = turn_in_flight;

                            // 02:00 is inside the shipped 22:00-07:00 window.
                            u.now = at(2, 0);
                            checked += 1;
                            assert_eq!(
                                decide(Some(&settings), &u),
                                UnpromptedSpeech::Refused(SpeechRefusal::QuietHours),
                                "inside quiet hours nothing may permit speech, and the reason \
                                 must be the window itself: category={category} \
                                 audience={audience:?} present={present:?} \
                                 turn_in_flight={turn_in_flight} enabled={enabled}"
                            );

                            // The vacuity control, and it is load-bearing: the
                            // sweep above proves nothing unless the SAME
                            // combinations can speak when the clock moves.
                            u.now = at(14, 30);
                            if decide(Some(&settings), &u) == UnpromptedSpeech::Spoken {
                                spoke_outside += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 72, "the sweep must cover the whole matrix");
        assert!(
            spoke_outside > 0,
            "vacuity control: no combination in this matrix speaks even outside quiet hours, so \
             the assertions above hold for some other reason"
        );
    }

    /// A failed settings read is silence, and NOT a fall back to
    /// `Settings::default()` -- even though today's default is also silent.
    /// The default is a value somebody can change; an unreadable store is not
    /// consent, and laundering one through the other would start speaking the
    /// day that default moved, with nothing in the gate to review.
    #[test]
    fn an_unreadable_settings_read_is_silence_rather_than_a_default() {
        assert_eq!(
            decide(None, &Utt::speakable()),
            UnpromptedSpeech::Refused(SpeechRefusal::SettingsUnreadable)
        );
    }

    #[test]
    fn the_pond_says_nothing_until_a_household_switches_it_on() {
        assert_eq!(
            decide(Some(&Settings::default()), &Utt::speakable()),
            UnpromptedSpeech::Refused(SpeechRefusal::NotEnabled),
            "the shipped default is off, so a pond that upgrades into this release is quiet"
        );
    }

    /// PAI-7 invariants 4 and 5. `Household` is refused for the same reason a
    /// targeted notification is never broadcast: speaking into the room is
    /// addressed to nobody and heard by everybody.
    #[test]
    fn only_a_named_member_is_ever_spoken_to() {
        for (audience, expected) in [
            (ProfileScope::Guest, SpeechRefusal::GuestSession),
            (
                ProfileScope::Household,
                SpeechRefusal::NotAddressedToAMember,
            ),
        ] {
            let mut u = Utt::speakable();
            u.audience = audience.clone();
            assert_eq!(
                decide(Some(&speech_on()), &u),
                UnpromptedSpeech::Refused(expected),
                "{audience:?} is not somebody to speak to"
            );
        }
    }

    /// Speaking into an empty room is worse than saying nothing: nobody is
    /// helped, and whoever IS in the room is not the person it was for.
    #[test]
    fn a_member_the_pond_cannot_see_is_not_spoken_to() {
        for present in [vec![], vec!["jerry".to_string()]] {
            let mut u = Utt::speakable();
            u.present = present.clone();
            assert_eq!(
                decide(Some(&speech_on()), &u),
                UnpromptedSpeech::Refused(SpeechRefusal::MemberNotPresent),
                "presence {present:?} does not include the member this was for"
            );
        }
    }

    #[test]
    fn nothing_is_said_over_the_top_of_a_turn_in_flight() {
        let mut u = Utt::speakable();
        u.turn_in_flight = true;
        assert_eq!(
            decide(Some(&speech_on()), &u),
            UnpromptedSpeech::Refused(SpeechRefusal::MidConversation)
        );
    }

    /// The shipped list is `alert` alone, so the category that carries every
    /// completed scheduled task is exactly the one that stays silent.
    #[test]
    fn a_category_the_household_did_not_enable_is_not_spoken() {
        for category in ["info", "action_required", "", "  ", "ALERTS"] {
            let mut u = Utt::speakable();
            u.category = category.into();
            assert_eq!(
                decide(Some(&speech_on()), &u),
                UnpromptedSpeech::Refused(SpeechRefusal::CategoryNotEnabled),
                "category {category:?} is not in the shipped list"
            );
        }
        // Case and surrounding space are not a different category.
        let mut u = Utt::speakable();
        u.category = " Alert ".into();
        assert_eq!(decide(Some(&speech_on()), &u), UnpromptedSpeech::Spoken);
    }

    /// The fail-closed direction, and the one that is the OPPOSITE of the rules
    /// engine's. `schedule.rs :: in_time_window` answers `false` for a
    /// malformed bound, because there false means a rule does not fire. Here
    /// false would mean the pond speaks, so a malformed bound is quiet.
    #[test]
    fn quiet_hours_that_cannot_be_read_mean_quiet_rather_than_no_window() {
        for (start, end) in [
            ("sunset", "07:00"),
            ("22:00", "dawn"),
            ("", ""),
            ("25:00", "07:00"),
            ("22:00", "07:61"),
            ("10pm", "7am"),
        ] {
            let settings = Settings {
                unprompted_speech_enabled: true,
                quiet_hours_start: start.into(),
                quiet_hours_end: end.into(),
                ..Default::default()
            };
            assert_eq!(
                decide(Some(&settings), &Utt::speakable()),
                UnpromptedSpeech::Refused(SpeechRefusal::QuietHoursUnreadable),
                "quiet hours {start:?}..{end:?} are unreadable, so the pond stays quiet"
            );
        }
    }

    /// A zero-length window is indistinguishable from "no quiet hours", and the
    /// narrowing reading of an ambiguous setting is the quiet one. Turning
    /// quiet hours off is what `unprompted_speech_enabled` is for.
    #[test]
    fn equal_quiet_bounds_are_quiet_all_day_rather_than_never() {
        let settings = Settings {
            unprompted_speech_enabled: true,
            quiet_hours_start: "00:00".into(),
            quiet_hours_end: "00:00".into(),
            ..Default::default()
        };
        for hour in [0, 6, 12, 18, 23] {
            let mut u = Utt::speakable();
            u.now = at(hour, 0);
            assert_eq!(
                decide(Some(&settings), &u),
                UnpromptedSpeech::Refused(SpeechRefusal::QuietHours),
                "{hour}:00 with equal bounds"
            );
        }
    }

    #[test]
    fn the_quiet_window_wraps_midnight_and_its_edges_are_half_open() {
        let quiet_at = |h: u32, m: u32| {
            quiet_hours_cover("22:00", "07:00", at(h, m)).expect("well-formed bounds")
        };
        assert!(quiet_at(22, 0), "the window includes its start");
        assert!(quiet_at(23, 59));
        assert!(quiet_at(0, 0), "and carries across midnight");
        assert!(quiet_at(6, 59));
        assert!(!quiet_at(7, 0), "and excludes its end");
        assert!(!quiet_at(14, 30));
        assert!(!quiet_at(21, 59));

        // A window that does not wrap is read the same way round.
        let daytime = |h: u32| quiet_hours_cover("09:00", "17:00", at(h, 0)).unwrap();
        assert!(!daytime(8));
        assert!(daytime(9));
        assert!(daytime(16));
        assert!(!daytime(17));
    }

    /// The same defect as the test above, reached by a route the string
    /// comparison could not see. `"9:00"` and `"09:00"` are the same instant and
    /// two different strings, and `%H` accepts both -- so a zero-length window
    /// decided on the raw text reads as `start < end`, gives the non-wrapping
    /// arm `now >= 9:00 && now < 9:00`, and answers NEVER QUIET. That is a
    /// scope-widening default reached by spelling: the household asked for the
    /// window the other test pins and got the opposite of it.
    ///
    /// Both bounds are free text. `sqlite_settings::apply_key` stores them
    /// verbatim on purpose (a parse there would have to pick a value for a
    /// malformed row) so `PUT /api/v1/settings` can put any of these pairs in
    /// the table, and every one of them is a plausible thing to type.
    #[test]
    fn two_spellings_of_one_time_are_still_a_zero_length_window() {
        for (start, end) in [
            ("9:00", "09:00"),
            ("09:00", "9:00"),
            ("22:00", "22:0"),
            ("07:05", "7:5"),
        ] {
            let settings = Settings {
                unprompted_speech_enabled: true,
                quiet_hours_start: start.into(),
                quiet_hours_end: end.into(),
                ..Default::default()
            };
            for hour in [0, 9, 14, 22] {
                let mut u = Utt::speakable();
                u.now = at(hour, 30);
                assert_eq!(
                    decide(Some(&settings), &u),
                    UnpromptedSpeech::Refused(SpeechRefusal::QuietHours),
                    "{start:?}..{end:?} is the same instant twice, so it is the zero-length \
                     window `equal_quiet_bounds_are_quiet_all_day_rather_than_never` pins -- \
                     but compared as TEXT it reads as a window that never covers anything, and \
                     the pond talks at {hour}:30 in a house that asked it not to"
                );
            }
        }
    }

    /// The vacuity control for the test above: a window whose bounds really are
    /// two different instants must still be read as a window, so the assertions
    /// there are about equal times spelled differently and not about a
    /// `quiet_hours_cover` that answers "quiet" to everything.
    #[test]
    fn an_unpadded_bound_is_still_read_as_the_time_it_names() {
        // "9:00".."17:00" -- the same window as the padded spelling, and the
        // one the wrap test pins with `09:00`.
        assert_eq!(quiet_hours_cover("9:00", "17:00", at(8, 59)), Some(false));
        assert_eq!(quiet_hours_cover("9:00", "17:00", at(9, 0)), Some(true));
        assert_eq!(quiet_hours_cover("9:00", "17:00", at(16, 59)), Some(true));
        assert_eq!(quiet_hours_cover("9:00", "17:00", at(17, 0)), Some(false));
    }

    #[test]
    fn a_time_of_day_off_the_clock_face_cannot_be_constructed() {
        assert!(LocalTimeOfDay::new(24, 0).is_none());
        assert!(LocalTimeOfDay::new(0, 60).is_none());
        assert_eq!(at(23, 59).minutes(), 23 * 60 + 59);
        // Vacuity control: the constructor really does accept the edges it
        // should, so the two `is_none` assertions above are about the range and
        // not about a constructor that refuses everything.
        assert!(LocalTimeOfDay::new(23, 59).is_some());
        assert!(LocalTimeOfDay::new(0, 0).is_some());
    }

    #[test]
    fn every_speech_refusal_has_its_own_log_label() {
        let labels = [
            SpeechRefusal::SettingsUnreadable.as_str(),
            SpeechRefusal::NotEnabled.as_str(),
            SpeechRefusal::QuietHours.as_str(),
            SpeechRefusal::QuietHoursUnreadable.as_str(),
            SpeechRefusal::CategoryNotEnabled.as_str(),
            SpeechRefusal::MidConversation.as_str(),
            SpeechRefusal::GuestSession.as_str(),
            SpeechRefusal::NotAddressedToAMember.as_str(),
            SpeechRefusal::MemberNotPresent.as_str(),
            SpeechRefusal::NothingToSay.as_str(),
        ];
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "two refusals sharing a label make two different failures indistinguishable in the \
             one place anybody looks: {labels:?}"
        );
    }

    /// The behavioural half, and the one that matters: the gate is only worth
    /// anything if a refusal never reaches the speaker. This drives the real
    /// `ChatService` with a capturing `VoiceOutput`, so it fails if
    /// `speak_unprompted` is ever rearranged to speak first and decide after.
    #[tokio::test]
    async fn no_refusal_ever_reaches_the_speaker_and_the_allowed_case_does() {
        async fn service_with(output: Arc<CapturingSpeak>) -> ChatService {
            let agent = Arc::new(MockAgent::new());
            let storage = Arc::new(InMemorySessionStorage::new());
            storage.create_session("s1".to_string()).await.unwrap();
            ChatService::new(agent, "s1".to_string(), storage).with_voice_output(output)
        }

        let night = Settings {
            unprompted_speech_enabled: true,
            quiet_hours_start: "22:00".into(),
            quiet_hours_end: "07:00".into(),
            ..Default::default()
        };
        let broken_window = Settings {
            unprompted_speech_enabled: true,
            quiet_hours_start: "sunset".into(),
            ..Default::default()
        };

        // Every refusal shape, each built as the speakable case minus one thing.
        let mut guest = Utt::speakable();
        guest.audience = ProfileScope::Guest;
        let mut household = Utt::speakable();
        household.audience = ProfileScope::Household;
        let mut absent = Utt::speakable();
        absent.present = vec![];
        let mut mid_turn = Utt::speakable();
        mid_turn.turn_in_flight = true;
        let mut wrong_category = Utt::speakable();
        wrong_category.category = "info".into();
        let mut at_night = Utt::speakable();
        at_night.now = at(2, 0);
        let mut silent = Utt::speakable();
        silent.text = "   ".into();

        let plain = Utt::speakable();
        let off = Settings::default();
        let cases: Vec<(&str, Option<&Settings>, &Utt)> = vec![
            ("settings unreadable", None, &plain),
            ("not enabled", Some(&off), &plain),
            ("quiet hours", Some(&night), &at_night),
            ("quiet hours unreadable", Some(&broken_window), &plain),
            ("category", Some(&night), &wrong_category),
            ("mid conversation", Some(&night), &mid_turn),
            ("guest", Some(&night), &guest),
            ("household", Some(&night), &household),
            ("absent", Some(&night), &absent),
            ("nothing to say", Some(&night), &silent),
        ];

        for (name, settings, utt) in cases {
            let output = Arc::new(CapturingSpeak::default());
            let service = service_with(output.clone()).await;
            let decision = service
                .speak_unprompted(settings, &utt.as_utterance())
                .await;
            assert!(
                matches!(decision, UnpromptedSpeech::Refused(_)),
                "{name} must be refused, got {decision:?}"
            );
            assert!(
                output.spoken.lock().unwrap().is_empty(),
                "{name} reached the speaker: {:?}",
                output.spoken.lock().unwrap()
            );
        }

        // And the control: the allowed case really does reach it, so the ten
        // assertions above are about the gate and not about a `speak_unprompted`
        // that never speaks.
        let output = Arc::new(CapturingSpeak::default());
        let service = service_with(output.clone()).await;
        let allowed = Utt::speakable();
        assert_eq!(
            service
                .speak_unprompted(Some(&night), &allowed.as_utterance())
                .await,
            UnpromptedSpeech::Spoken
        );
        assert_eq!(*output.spoken.lock().unwrap(), vec![allowed.text.clone()]);
    }
}
