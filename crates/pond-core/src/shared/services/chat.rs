use crate::models::domain::message::{ChatMessage, Role, ToolCallRecord};
use crate::models::ports::agent::Agent;
use crate::models::ports::provider::LlmProvider;
use crate::models::ports::speech_energy::{MicHold, SpeechEnergy};
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
use crate::shared::services::voice_control::{StopReason, VoiceControl};
use crate::user_data::domain::session::SessionMessage;
use crate::user_data::ports::session_storage::SessionStorage;
use anyhow::Result;
use futures::StreamExt as _;
use std::io::{self, Write};
use std::sync::Arc;
use uuid::Uuid;

// ── Voice helpers ─────────────────────────────────────────────────────────────

/// Always "chat": the LLM routes tools natively via MCP, so nothing is pre-classified.
fn resolve_voice_role(_message: &str) -> String {
    "chat".to_string()
}

/// Strips the MCP `<server>__` prefix, matching the bare name the egress tracker records.
fn bare_tool_name(tool: &str) -> String {
    match tool.split_once("__") {
        Some((_, bare)) if !bare.is_empty() => bare.to_string(),
        _ => tool.to_string(),
    }
}

/// The row history stores for a tool result that carries `keep`: its stub as `content`, marked
/// `transient`, so neither the result's text nor its card is ever written. `None` keeps `row`.
fn kept_tool_row(row: &serde_json::Value) -> Option<String> {
    let keep = row.get("keep")?.as_str()?.trim();
    if keep.is_empty() {
        return None;
    }
    let mut kept = row.as_object()?.clone();
    kept.remove("keep");
    kept.insert("content".to_string(), serde_json::Value::from(keep));
    kept.insert("transient".to_string(), serde_json::Value::Bool(true));
    Some(serde_json::Value::Object(kept).to_string())
}

/// Control phrases live in `pond-voice` so the desktop shell (separate workspace) shares them.
use pond_voice::control::{classify as classify_voice_command, VoiceCommand};

use crate::user_data::domain::profile::ProfileScope;
use pond_voice::control::is_control_phrase as is_dismissal_or_exit_phrase;

/// The NDJSON contract's cap on tool-result `content`, in chars, not bytes.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Consecutive mic failures before voice mode gives up; ~1 minute at the linear backoff.
const MIC_RETRY_BUDGET: u32 = 10;
/// Base backoff between microphone retries; multiplied by the attempt number.
const MIC_RETRY_BACKOFF_MS: u64 = 1_000;

/// Truncates to `TOOL_RESULT_MAX_CHARS` on a char boundary, so the JSON stays valid.
fn truncate_tool_result(mut content: String) -> String {
    if let Some((byte_idx, _)) = content.char_indices().nth(TOOL_RESULT_MAX_CHARS) {
        content.truncate(byte_idx);
    }
    content
}

// ── Speaking first ───────────────────────────────────────────────────────────

/// Local time of day as minutes past midnight, `[0, 1440)`; `new` is the only constructor.
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

    /// The time of day at `at` in the IANA zone `zone`, or in this machine's zone when `zone`
    /// names none.
    pub fn in_zone(zone: &str, at: chrono::DateTime<chrono::Utc>) -> Self {
        use chrono::Timelike;
        let (hour, minute) = match zone.trim().parse::<chrono_tz::Tz>() {
            Ok(tz) => {
                let local = at.with_timezone(&tz);
                (local.hour(), local.minute())
            }
            Err(_) => {
                let local = at.with_timezone(&chrono::Local);
                (local.hour(), local.minute())
            }
        };
        Self((hour * 60 + minute) as u16)
    }
}

/// Why the pond stayed quiet; every variant is a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechRefusal {
    /// The settings read failed, so nothing is known about consent. Silence.
    SettingsUnreadable,
    /// `unprompted_speech_enabled` is off. The default, and the common case.
    NotEnabled,
    /// Inside the quiet-hours window, which is absolute.
    QuietHours,
    /// Unparseable quiet-hours bounds, read as always quiet: a broken row, not a choice.
    QuietHoursUnreadable,
    /// This notification's category is not one the household enabled.
    CategoryNotEnabled,
    /// A turn is in flight; never speak mid-conversation.
    MidConversation,
    /// The utterance is addressed to `Guest`.
    GuestSession,
    /// Addressed to `Household`, i.e. the room: the broadcast this gate exists to prevent.
    NotAddressedToAMember,
    /// The member it is for is not present; anyone who hears it is the wrong person.
    MemberNotPresent,
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
    Spoken,
    Refused(SpeechRefusal),
}

/// Something the pond might say unasked; no field is optional, so the gate defaults nothing.
pub struct UnpromptedUtterance<'a> {
    /// Who this is for. Only [`ProfileScope::Owner`] can be spoken to.
    pub audience: &'a ProfileScope,
    /// `Notification.category` -- `alert` / `info` / `action_required`.
    pub category: &'a str,
    /// What would be said.
    pub text: &'a str,
    /// Members believed present, per presence events; empty is an empty room, so a refusal.
    pub present_members: &'a [String],
    /// The local wall clock, from the same clock the rules engine evaluates its windows against.
    pub now: LocalTimeOfDay,
    /// Whether a turn is currently being served.
    pub turn_in_flight: bool,
}

/// Whether `now` is inside the `[start, end)` quiet window, which may wrap midnight.
/// `None` if a bound isn't `HH:MM`. Equal bounds (compared parsed: `9:00` = `09:00`) are all day.
fn quiet_hours_cover(start: &str, end: &str, now: LocalTimeOfDay) -> Option<bool> {
    let parse = |s: &str| {
        let t = chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()?;
        LocalTimeOfDay::new(chrono::Timelike::hour(&t), chrono::Timelike::minute(&t))
    };
    let (start, end) = (parse(start)?, parse(end)?);
    Some(if start == end {
        true
    } else if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    })
}

/// Whether `category` is in the comma-separated `enabled` list; any bad entry means less speech.
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

/// Decide whether the pond may say this, unasked; quiet hours are checked before all else.
/// `None` settings (a failed read) refuse rather than fall back to a default that may change.
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
        // Equal bounds: quiet all day (disabling speech is `unprompted_speech_enabled`'s job).
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

    // 6. Present, i.e. spoke recently: evidence the member is here, not a claim about the room.
    if !utterance.present_members.iter().any(|p| p == member) {
        return Refused(SpeechRefusal::MemberNotPresent);
    }

    if utterance.text.trim().is_empty() {
        return Refused(SpeechRefusal::NothingToSay);
    }

    UnpromptedSpeech::Spoken
}

/// Guard for the working tone: stopping on drop means no early return leaves it playing.
struct WorkingTone {
    output: Arc<dyn VoiceOutput>,
    stopped: bool,
}

impl WorkingTone {
    /// Always returns a guard, even when disabled: stopping a tone that never started is a no-op.
    fn start(output: Arc<dyn VoiceOutput>, enabled: bool) -> Self {
        if enabled {
            output.start_thinking_tone();
        }
        Self {
            output,
            stopped: false,
        }
    }

    /// Stop the tone; idempotent, since the first speakable sentence has several paths.
    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.output.stop_thinking_tone();
        }
    }
}

/// Ends the turn when dropped, so every way out of `stream_response_inner`, an error included, ends
/// what `begin_utterance` began: other audio paused for the turn must not stay paused after it.
struct TurnEnds(Arc<dyn VoiceOutput>);

impl Drop for TurnEnds {
    fn drop(&mut self) {
        self.0.end_utterance();
    }
}

impl Drop for WorkingTone {
    fn drop(&mut self) {
        self.stop();
    }
}

// TTS text handling is in `pond-voice` so the desktop shell (separate workspace) shares it.
use pond_voice::text::split_speech_units;

/// Re-exported to keep pond-core's public API path.
pub use pond_voice::text::filter_thinking;

/// No-LLM fallback title for `ensure_session_title`; empty means no usable words.
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
        let truncated: String = title.chars().take(MAX_CHARS).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Runs the Wait → Listen → Thinking → Speak loop and owns turn persistence.
/// `provider` is only for titles. Keep `Clone` cheap: the speculative path clones the service.
#[derive(Clone)]
pub struct ChatService {
    agent: Arc<dyn Agent>,
    provider: Option<Arc<dyn LlmProvider>>,
    voice_input: Arc<dyn VoiceInput>,
    voice_output: Arc<dyn VoiceOutput>,
    speech_energy: Arc<dyn SpeechEnergy>,
    /// Defaults to `InstantActivation` (keyboard/stdin mode).
    wake_word_detector: Arc<dyn StreamingWakeWordDetector>,
    session_id: String,
    session_storage: Arc<dyn SessionStorage>,
    /// Sent on every completion; defaults to `SYSTEM_PROMPT`.
    system_prompt: String,
    /// Optional Answer Reviewer — adversarial post-inference quality gate.
    answer_reviewer: Option<Arc<dyn crate::models::ports::answer_reviewer::AnswerReviewer>>,
    /// When set, `persist_assistant_turn` records Agent, Inference and per-tool events.
    event_log: Option<Arc<dyn EventLog>>,
    /// `emit_event` also forwards here, e.g. to the `--json-events` NDJSON writer.
    event_sink: Option<WorkflowEventSink>,
    /// Human banners on stdout; `false` in `--json-events` mode, where stdout is NDJSON only.
    stdout_diagnostics: bool,
    /// When set, confirmed voice turns record a `TurnMetrics` row, as the REST path does.
    telemetry: Option<Arc<dyn crate::security::ports::telemetry::TelemetryPort>>,
    /// Model identifier for telemetry rows (the CLI knows `--model`).
    model_name: Option<String>,
    /// Whose turns these are. See [`with_profile_scope`](Self::with_profile_scope).
    profile_scope: ProfileScope,
    /// Mirrors `settings.voice_thinking_tone_enabled`; defaults true, like that setting.
    thinking_tone: bool,
    /// Whether reasoning text may be stored at all; off unless `with_thinking` turns it on.
    persist_thinking: bool,
    /// This turn's reasoning, drained by `persist_assistant_turn`, which mints the id to key it.
    thinking_blocks: Arc<std::sync::Mutex<Vec<String>>>,
    /// The desktop shell's stop and interrupt commands; `None` outside `--json-events`.
    control: Option<VoiceControl>,
    /// The last state announced was Speak, which a reused speculative reply may already be in.
    speaking: Arc<std::sync::atomic::AtomicBool>,
}

/// Result of one non-persisting agent stream, with the stats from its `Done` event.
#[derive(Debug)]
struct TurnOutcome {
    text: String,
    usage: Option<crate::models::ports::provider::UsageStats>,
    stats: Option<crate::shared::domain::turn_stats::TurnStats>,
    total_latency_ms: u64,
    /// Set when the user spoke over the reply, which ended it unfinished.
    barged_in: Option<BargedIn>,
}

/// The user talked over the reply.
#[derive(Debug)]
struct BargedIn {
    /// When they started, pre-roll included; the follow-up capture begins here.
    onset: std::time::Instant,
    /// Keeps the mic open until the follow-up capture claims it, so nothing said is lost between.
    hold: MicHold,
}

struct BargeInWatch {
    handle: tokio::task::JoinHandle<()>,
    /// Here rather than in the task, so a reply heard out lets go of the mic as it ends; the
    /// task takes it only to hand it on with a barge-in.
    hold: Arc<std::sync::Mutex<Option<MicHold>>>,
}

impl Drop for BargeInWatch {
    fn drop(&mut self) {
        self.handle.abort();
        let hold = self.hold.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(hold);
    }
}

type SpeculativeReply = (String, tokio::task::JoinHandle<Result<TurnOutcome>>);

/// Aborts and silences a speculative reply nobody took, e.g. when a stop drops the listen.
struct SpeculativeJob {
    job: Option<SpeculativeReply>,
    voice_output: Arc<dyn VoiceOutput>,
}

impl Drop for SpeculativeJob {
    fn drop(&mut self) {
        if let Some((_, handle)) = self.job.take() {
            handle.abort();
            self.voice_output.stop_speaking();
            self.voice_output.stop_thinking_tone();
        }
    }
}

/// Workflow-event observer, e.g. the `--json-events` NDJSON writer the desktop shell parses.
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
            control: None,
            speaking: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Play (or suppress) the working tone; pass the household's setting, not a literal.
    pub fn with_thinking_tone(mut self, enabled: bool) -> Self {
        self.thinking_tone = enabled;
        self
    }

    /// Allow this turn's reasoning text to be persisted; pass the user's setting, not a literal.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.persist_thinking = enabled;
        self
    }

    /// Offer one reasoning passage; the privacy gate is here, so handlers call it unconditionally.
    /// `&self` because SSE handlers hold the service across awaits inside `async_stream!`.
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

    /// Attach the activity log; recording is best-effort and never fatal.
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

    /// Scope this session's turns are written under; must match `AgentRequest.profile_scope`.
    pub fn with_profile_scope(mut self, scope: ProfileScope) -> Self {
        self.profile_scope = scope;
        self
    }

    /// Speak unasked, or (usually) decline; the only unprompted speech path, so add any here.
    /// A playback failure is logged but still reports `Spoken`: it is not a gate refusal.
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

    /// Attach an LLM provider, used only for session title generation.
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

    /// Listen for the user talking over the reply, holding the mic open until then.
    fn watch_for_barge_in(
        &self,
        barged: tokio::sync::oneshot::Sender<BargedIn>,
    ) -> Option<BargeInWatch> {
        use pond_voice::barge;

        if self.speech_energy.is_inert() {
            return None;
        }

        let energy = self.speech_energy.clone();
        let voice_output = self.voice_output.clone();
        let hold = Arc::new(std::sync::Mutex::new(Some(energy.hold_mic())));
        let task_hold = hold.clone();

        let handle = tokio::spawn(async move {
            let mut gate = barge::BargeIn::while_speaking();
            let poll = std::time::Duration::from_millis(barge::POLL_MS);
            let pre_roll = std::time::Duration::from_millis(barge::ONSET_PRE_ROLL_MS);
            let mut warned_inert = false;

            loop {
                tokio::time::sleep(poll).await;

                if voice_output.is_playing() == Some(false) {
                    // Between sentences (a tool running): loud talk then is not over the reply.
                    gate.reset();
                    continue;
                }
                let Some(rms) = energy.recent_rms(barge::WINDOW_MS) else {
                    gate.reset();
                    if !warned_inert {
                        tracing::debug!("barge-in inert: the microphone is not open for this turn");
                        warned_inert = true;
                    }
                    continue;
                };

                if let Some(began) = gate.on_window(rms, std::time::Instant::now()) {
                    tracing::debug!(rms, "barge-in: the user spoke over the reply");
                    let onset = began.checked_sub(pre_roll).unwrap_or(began);
                    let hold = task_hold
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                        .unwrap_or_else(MicHold::none);
                    // Before silencing, so the turn ends before it can start another sentence.
                    let _ = barged.send(BargedIn { onset, hold });
                    voice_output.stop_speaking();
                    return;
                }
            }
        });

        Some(BargeInWatch { handle, hold })
    }

    /// Set the wake-word detector; defaults to `InstantActivation` (no wait).
    pub fn with_wake_word_detector(mut self, detector: Arc<dyn StreamingWakeWordDetector>) -> Self {
        self.wake_word_detector = detector;
        self
    }

    /// Let the desktop shell stop the session and interrupt turns (`run_loop` only).
    pub fn with_control(mut self, control: VoiceControl) -> Self {
        self.control = Some(control);
        self
    }

    fn stop_requested(&self) -> Option<StopReason> {
        self.control.as_ref().and_then(VoiceControl::stop_requested)
    }

    /// Never resolves without a control channel.
    async fn stop_signal(&self) -> StopReason {
        match &self.control {
            Some(control) => control.stopped().await,
            None => std::future::pending().await,
        }
    }

    /// The first interrupt after this call; never resolves without a control channel.
    fn interrupt_signal(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let next = self.control.as_ref().map(VoiceControl::next_interrupt);
        async move {
            match next {
                Some(next) => next.await,
                None => std::future::pending().await,
            }
        }
    }

    /// `fut`'s output, or the stop reason if a stop comes first. An earlier stop wins unpolled.
    async fn unless_stopped<T>(
        &self,
        fut: impl std::future::Future<Output = T>,
    ) -> std::result::Result<T, StopReason> {
        tokio::select! {
            biased;
            reason = self.stop_signal() => Err(reason),
            out = fut => Ok(out),
        }
    }

    /// Silence and cancel a turn in flight; nothing of it has been persisted.
    fn abandon_turn(&self, handle: &tokio::task::JoinHandle<Result<TurnOutcome>>) {
        self.voice_output.stop_speaking();
        self.voice_output.stop_thinking_tone();
        // Stops Goose's inference within a token (drop-based).
        handle.abort();
    }

    /// Override the system prompt; `prompts::build_system_prompt()` builds one from `Settings`.
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }

    /// Attach a workflow-event sink; it runs synchronously in the loop, so keep it cheap.
    pub fn with_event_sink(mut self, sink: WorkflowEventSink) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Whether `run_loop` prints banners to stdout; `false` for `--json-events` (NDJSON only).
    pub fn with_stdout_diagnostics(mut self, enabled: bool) -> Self {
        self.stdout_diagnostics = enabled;
        self
    }

    /// Single-shot chat; Goose keeps its own history, so `SessionStorage` only feeds the REST API.
    pub async fn chat_once(&self, message: String) -> Result<String> {
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: resolve_voice_role(&message),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
            // From the builder: this serves both REST /chat and the voice loop.
            profile_scope: self.profile_scope.clone(),
            // Voice has no speaker identification, so no member's preferences apply.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
            suggestion_context: None,
            deep_research_task: None,
            interview: None,
        };
        let response_text = self.agent.chat(request).await?.text;

        let assistant_msg = ChatMessage::assistant(response_text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        self.maybe_generate_title(&message, &response_text).await;

        Ok(response_text)
    }

    /// Best-effort title after the first exchange, using the idle re-titler's prompt and rules.
    /// Stored as `model` provenance so the idle pass may correct this early guess later.
    async fn maybe_generate_title(&self, user_text: &str, assistant_text: &str) {
        use crate::shared::services::session_title::{normalise_title, TITLE_SYSTEM_PROMPT};
        let provider = match &self.provider {
            Some(p) => p,
            None => return,
        };

        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

        // First exchange only (2 messages; fetch 3 to tell). The newest id is how far the
        // title reaches, which the idle pass needs to tell if the chat outgrew it.
        let through_message_id = match self
            .session_storage
            .get_messages_paginated(&self.session_id, 3, 0)
            .await
        {
            Ok(msgs) if msgs.len() == 2 => msgs[1].id.clone(),
            _ => return,
        };

        let context = format!("User: {}\nAssistant: {}", user_text, assistant_text);
        let messages = vec![ChatMessage::user(&context)];

        match provider.complete(TITLE_SYSTEM_PROMPT, messages).await {
            Ok(response) => {
                // Shared normaliser, so this path can't emit titles the re-titler would reject.
                let Some(title) = normalise_title(&response.content) else {
                    tracing::debug!(
                        session_id = %self.session_id,
                        "title generation returned nothing usable — leaving it to the fallback"
                    );
                    return;
                };

                // Not `update_title`: that marks a human rename, which the idle pass won't touch.
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

    /// Persist the user side of a turn; call before streaming so it survives a stream error.
    pub async fn persist_user_message(&self, message: &str) -> Result<()> {
        self.persist_user_message_with_images(message, Vec::new())
            .await
            .map(|_| ())
    }

    /// Persist the user side of a turn with its images, returning the row id.
    /// The row commits before inference, so the caller needs the id to retract an aborted turn.
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

    /// Persist the assistant side of a turn, after the agent stream drains.
    /// `tool_results` is one JSON string per call, in order; `usage` is `(prompt, completion)`.
    /// A row carrying `keep` is stored as that stub, never its content (`kept_tool_row`).
    pub async fn persist_assistant_turn(
        &self,
        tool_results: Vec<String>,
        assistant_text: &str,
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
    ) -> Result<()> {
        // Entries carry `tool`, `tool_call_id`, `arguments`; an unparseable one still persists.
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

        // The calls this turn made, linked to the tool rows below by id.
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
            let content = match v {
                Some(row) => kept_tool_row(row).unwrap_or(content),
                None => content,
            };
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

        // After the assistant row commits: `session_thinking` has a foreign key onto it.
        // Best-effort; a failure here must not cost the committed turn.
        self.persist_thinking_blocks(&assistant_id).await;

        if let Some((prompt, completion)) = usage {
            if prompt > 0 || completion > 0 {
                let _ = self
                    .session_storage
                    .increment_usage(&self.session_id, prompt, completion, model_name)
                    .await;
            }
        }

        // Handlers attach no provider, so this no-LLM fallback is what titles most sessions.
        self.ensure_session_title().await;

        self.record_turn_activity(&tool_names, usage, model_name)
            .await;

        Ok(())
    }

    /// Drain the turn's reasoning into storage, keyed to its assistant row.
    /// Always drains, even on failure: a long-lived service must not carry it into the next turn.
    async fn persist_thinking_blocks(&self, assistant_message_id: &str) {
        let blocks: Vec<String> = match self.thinking_blocks.lock() {
            Ok(mut buf) => std::mem::take(&mut *buf),
            Err(poisoned) => {
                // Still clear a poisoned buffer: stale text leaking into the next turn is worse.
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

    /// Append the turn's Agent / Inference / Tool events; best-effort, never fails the turn.
    /// Metadata only (content lives in `session_messages`), so the events stay `Internal`.
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

    /// Best-effort: title an untitled session from its first user message, without an LLM.
    async fn ensure_session_title(&self) {
        // A missing session counts as untitled; the update is a no-op for it.
        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

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

        // Not `update_title`: that marks a human rename, which the idle re-titler won't touch.
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

    /// Stream, speak by sentence, and persist a confirmed turn; don't `speak()` the result again.
    /// Speculative transcripts must use the non-persisting `stream_response_inner` instead.
    pub async fn chat_stream_once(&self, message: String) -> Result<String> {
        let fired_at = std::time::Instant::now();
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
        // Cut off by the user: not a completed reply, so not persisted as one.
        if outcome.barged_in.is_some() {
            return Ok(outcome.text);
        }

        self.persist_assistant_response(&outcome.text, outcome.usage.as_ref())
            .await?;
        self.maybe_generate_title(&message, &outcome.text).await;
        self.record_turn_outcome(&outcome).await;
        Ok(outcome.text)
    }

    /// Stream and speak a response with no persistence, so a speculative run can be discarded.
    /// `fired_at` is when inference was kicked off, for TTFT telemetry only.
    async fn stream_response_inner(
        &self,
        message: String,
        fired_at: std::time::Instant,
    ) -> Result<TurnOutcome> {
        // Only the voice loop reaches this, hence `voice_mode: true` (TTS-friendly prompt).
        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: true,
            canvas_mode: false,
            // From the builder; voice leaves it at `Household`.
            profile_scope: self.profile_scope.clone(),
            // Voice has no speaker identification, so no member's preferences apply.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
            suggestion_context: None,
            deep_research_task: None,
            interview: None,
        };

        // Clear interrupts once per turn, not per sentence, so a barge-in stops the whole reply.
        self.voice_output.begin_utterance();
        let _turn = TurnEnds(self.voice_output.clone());

        // ── Working tone ──────────────────────────────────────────────────
        // Plays from before inference until the first speakable sentence.
        let mut tone = WorkingTone::start(self.voice_output.clone(), self.thinking_tone);

        let mut stream = self.agent.chat_stream(request).await?;
        let mut turn_usage: Option<crate::models::ports::provider::UsageStats> = None;
        let mut turn_stats: Option<crate::shared::domain::turn_stats::TurnStats> = None;
        let mut full_text = String::new();
        let mut sentence_buf = String::new();
        let mut spoken_first = false;
        let mut barge_in: Option<BargeInWatch> = None;
        let (barge_tx, mut barge_rx) = tokio::sync::oneshot::channel::<BargedIn>();
        let mut barge_tx = Some(barge_tx);
        let mut thought_filter = crate::models::services::thought_filter::ThoughtFilter::new();

        enum AudioItem {
            Wav(Vec<u8>),
            Fallback(String),
        }

        let (speech_tx, mut speech_rx) = tokio::sync::mpsc::channel::<String>(16);
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<AudioItem>(16);

        let voice_for_synth = self.voice_output.clone();
        let synth_handle = tokio::spawn(async move {
            while let Some(text) = speech_rx.recv().await {
                let spoken = text.trim().to_string();
                if spoken.is_empty() {
                    continue;
                }
                match voice_for_synth.synthesize(&spoken).await {
                    Ok(Some(wav)) if !wav.is_empty() => {
                        if audio_tx.send(AudioItem::Wav(wav)).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        if audio_tx.send(AudioItem::Fallback(spoken)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let voice_for_playback = self.voice_output.clone();
        let playback_handle = tokio::spawn(async move {
            while let Some(item) = audio_rx.recv().await {
                match item {
                    AudioItem::Wav(wav) => {
                        if let Err(e) = voice_for_playback.play_audio(wav).await {
                            tracing::warn!("TTS playback failed: {}", e);
                        }
                    }
                    AudioItem::Fallback(spoken) => {
                        if let Err(e) = voice_for_playback.speak(&spoken).await {
                            tracing::warn!("TTS failed: {}", e);
                        }
                    }
                }
            }
        });

        struct TaskGuard(Option<tokio::task::JoinHandle<()>>);
        impl Drop for TaskGuard {
            fn drop(&mut self) {
                if let Some(h) = self.0.take() {
                    h.abort();
                }
            }
        }
        let mut synth_guard = TaskGuard(Some(synth_handle));
        let mut playback_guard = TaskGuard(Some(playback_handle));

        let speaking = async {
            while let Some(event_result) = stream.next().await {
                match event_result? {
                AgentStreamEvent::ToolCall { id, tool, .. } => {
                    // Shown, not spoken: the working tone covers tool use, and keeps playing.
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
                        // User-perceived latency; the engine's own TTFT comes in `Done`.
                        tracing::debug!(
                            "[Q2-26 TTFT] {}ms from-fire",
                            fired_at.elapsed().as_millis()
                        );
                        self.emit_event(WorkflowEvent::StateChanged {
                            state: WorkflowState::Speak,
                        });
                        spoken_first = true;
                    }
                    // Emitted before buffering so an unfinished sentence still reaches the UI.
                    self.emit_event(WorkflowEvent::Token {
                        content: content.clone(),
                    });
                    full_text.push_str(&content);
                    sentence_buf.push_str(&content);

                    let (units, remainder) = split_speech_units(&sentence_buf, true);
                    sentence_buf = remainder;
                    for unit in units {
                        tone.stop();
                        if let Some(tx) = barge_tx.take() {
                            barge_in = self.watch_for_barge_in(tx);
                        }
                        if speech_tx.send(unit).await.is_err() {
                            break;
                        }
                    }
                }
                AgentStreamEvent::Done { usage, stats, .. } => {
                    turn_usage = usage;
                    turn_stats = stats;
                    // The filter's held-back tail goes into `full_text` too, or history drops it.
                    let tail = thought_filter.flush();
                    if !tail.is_empty() {
                        // UI transcripts are built from `Token`s only; `Speak` must come first.
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
                    let remainder = sentence_buf.trim().to_string();
                    if !remainder.is_empty() {
                        tone.stop();
                        if let Some(tx) = barge_tx.take() {
                            barge_in = self.watch_for_barge_in(tx);
                        }
                        let _ = speech_tx.send(remainder).await;
                    }
                    sentence_buf.clear();
                    break;
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow::anyhow!("Agent stream error: {}", content));
                }
                AgentStreamEvent::ToolResult {
                    id, tool, content, ..
                } => {
                    // Not spoken; truncated so a huge payload cannot bloat one NDJSON line.
                    let content = truncate_tool_result(content);
                    self.emit_event(WorkflowEvent::ToolResult { tool, id, content });
                }
                AgentStreamEvent::Status { .. }
                | AgentStreamEvent::Thinking { .. }
                | AgentStreamEvent::ReviewStatus { .. }
                | AgentStreamEvent::ReviewRevision { .. }
                // The cap message itself arrives (and is spoken) as Text; this marker is for UIs.
                | AgentStreamEvent::TurnLimitReached { .. }
                // Never added to `full_text`: subagent activity stays out of the parent's history.
                | AgentStreamEvent::SubagentProgress { .. } => {
                    // Not spoken during streaming — informational only
                }
            }
            }

            // If the stream ended without Done, its filter tail must reach `full_text` here too.
            let tail = thought_filter.flush();
            if !tail.is_empty() {
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
            let remainder = sentence_buf.trim().to_string();
            if !remainder.is_empty() {
                tone.stop();
                if let Some(tx) = barge_tx.take() {
                    barge_in = self.watch_for_barge_in(tx);
                }
                let _ = speech_tx.send(remainder).await;
            }
            sentence_buf.clear();

            drop(speech_tx);
            if let Some(h) = synth_guard.0.take() {
                let _ = h.await;
            }
            if let Some(h) = playback_guard.0.take() {
                let _ = h.await;
            }

            // A turn that produced no speakable text still has to stop the tone.
            tone.stop();

            // Explicit drop: stop the barge-in watch now that TTS is done, after the tone.
            drop(barge_in.take());
            Ok::<(), anyhow::Error>(())
        };

        let barged_in = tokio::select! {
            biased;
            Ok(barged) = &mut barge_rx => Some(barged),
            finished = speaking => {
                finished?;
                None
            }
        };
        if barged_in.is_some() {
            self.voice_output.stop_speaking();
        }

        Ok(TurnOutcome {
            text: full_text,
            usage: turn_usage,
            stats: turn_stats,
            total_latency_ms: fired_at.elapsed().as_millis() as u64,
            barged_in,
        })
    }

    /// Record a turn's usage, metrics and summary line; failures are logged, never fatal.
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

    /// One console line per turn; suppressed in `--json-events` mode (NDJSON-only stdout).
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
        // Prefilled tokens, not prompt size: only the former matches the prefill time.
        match (stats.prefill_ms, stats.prefill_tok_per_sec) {
            (Some(prefill), Some(rate)) => parts.push(format!(
                "prefill {} of {} tok in {:.1}s ({:.0} tok/s)",
                stats.prefilled_tokens,
                stats.prompt_tokens,
                prefill as f32 / 1000.0,
                rate
            )),
            // No rate: nothing prefilled, or no measurable time. Only the former is "all cached".
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
        // Separate from decode: this is GIAP's count of the thinking channel, not the engine's.
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

    /// Persist one assistant message; separate so a speculative turn persists only once confirmed.
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
                // `persist_assistant_turn`'s tuple can't carry this: its rows stay NULL, not zero.
                .with_reasoning_tokens(usage.and_then(|u| u.reasoning_tokens));
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;
        // Must drain: the voice loop reuses one service, so leftovers would reach the next turn.
        self.persist_thinking_blocks(&assistant_id).await;
        Ok(())
    }

    /// Persist a confirmed voice turn exactly once, keyed to the confirmed transcript.
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
        // LLM title when a provider exists; else the derived fallback, which no-ops once titled.
        self.maybe_generate_title(confirmed_message, response_text)
            .await;
        self.ensure_session_title().await;
        Ok(())
    }

    /// Listen while speculatively streaming a non-persisting reply to the provisional transcript.
    /// The caller must persist the job's result only if its `spec_transcript` is the final one.
    async fn listen_with_speculative_chat(
        &self,
    ) -> Result<(Option<String>, Option<SpeculativeReply>)> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SpeculativeSignal>();
        let callback: Box<dyn Fn(SpeculativeSignal) + Send + Sync> = Box::new(move |signal| {
            let _ = tx.send(signal);
        });

        let listen_fut = self.voice_input.listen_with_speculative(callback);
        tokio::pin!(listen_fut);

        // Keeps the fired-on transcript so `run_loop` can match it to the final one.
        let mut speculative = SpeculativeJob {
            job: None,
            voice_output: self.voice_output.clone(),
        };

        loop {
            tokio::select! {
                result = &mut listen_fut => {
                    let transcript = result?;
                    return Ok((transcript, speculative.job.take()));
                }
                Some(signal) = rx.recv() => {
                    match signal {
                        SpeculativeSignal::Ready(text)
                            if !text.is_empty() && !is_dismissal_or_exit_phrase(&text) =>
                        {
                            // Non-persisting: a wrong provisional transcript leaves no turn.
                            let svc = self.clone();
                            let spec_text = text.clone();
                            let fired_at = std::time::Instant::now();
                            let handle = tokio::spawn(async move {
                                svc.stream_response_inner(spec_text, fired_at).await
                            });
                            speculative.job = Some((text, handle));
                        }
                        // Empty / dismissal / exit — let run_loop handle it normally.
                        SpeculativeSignal::Ready(_) => {}
                        SpeculativeSignal::Invalidated => {
                            // Speech resumed: abort the job and silence anything it started.
                            if let Some((_, handle)) = speculative.job.take() {
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

    /// Wait for the wake word, retrying a missing mic up to [`MIC_RETRY_BUDGET`] times.
    /// Each failure is printed: silently not listening looks like hearing nothing.
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

    /// Run the interactive Wait → Listen → Thinking → Speak loop.
    pub async fn run_loop(&self) -> Result<()> {
        // Wake word for the first turn, and again after any turn where nothing was heard.
        let mut first_turn = true;

        // Text captured by a wake-word interrupt; the next iteration skips listening for it.
        let mut pending_input: Option<String> = None;

        // Human-facing stdout; suppressed in `--json-events` mode, where stdout is NDJSON only.
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
        // Leaves the loop; `run_chat` then releases the microphone and the process exits 0.
        macro_rules! end_session {
            ($reason:expr) => {{
                let reason: StopReason = $reason;
                self.emit_event(WorkflowEvent::Exit {
                    reason: reason.as_exit_reason().to_string(),
                });
                diag!("\n  session stopped");
                break;
            }};
        }

        loop {
            // A stop that came in during a persist or a farewell is honoured once that work is done.
            if let Some(reason) = self.stop_requested() {
                end_session!(reason);
            }

            // Subscribed before listening: a speculative reply can start speaking, and be
            // interrupted, while the listen is still running.
            let interrupted_while_listening = self.interrupt_signal();

            // `speculative`: a (provisional transcript, job) already streaming a reply.
            let (input, speculative) = if let Some(text) = pending_input.take() {
                // Interrupt text: skip listening, but still emit Listen for the UI.
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

                // A mic that fails to open must not end the session; devices come and go.
                let activation = match self
                    .unless_stopped(self.wait_for_activation_resiliently())
                    .await
                {
                    Err(reason) => end_session!(reason),
                    Ok(Ok(a)) => a,
                    Ok(Err(e)) => {
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

                match self
                    .unless_stopped(self.listen_with_speculative_chat())
                    .await
                {
                    Err(reason) => end_session!(reason),
                    Ok(heard) => heard?,
                }
            } else {
                // ── Conversational turn — listen without wake word ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                diag!("\n  listening");

                match self
                    .unless_stopped(self.listen_with_speculative_chat())
                    .await
                {
                    Err(reason) => end_session!(reason),
                    Ok(heard) => heard?,
                }
            };

            // Every early exit (no speech / dismissal / exit) must abort a live speculative job.
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

            // NDJSON `transcript` before the LLM starts; `UserInput` is kept for the tracing hook.
            self.emit_event(WorkflowEvent::Transcript {
                text: input.clone(),
            });
            self.emit_event(WorkflowEvent::UserInput(input.clone()));

            // ── Phantom-turn gate ───────────────────────────────────────────
            // Reuse the speculative job only if its transcript matches: one persisted turn each.
            let reusable_speculative = match speculative {
                Some((spec_transcript, handle)) if spec_transcript == input => Some(handle),
                Some((_, handle)) => {
                    // Mismatch: abort and silence the job; nothing was persisted.
                    handle.abort();
                    self.voice_output.stop_speaking();
                    self.voice_output.stop_thinking_tone();
                    None
                }
                None => None,
            };

            // Only a reused reply was audible during the listen; otherwise an Interrupt then was idle.
            let interrupted = if reusable_speculative.is_some() {
                interrupted_while_listening
            } else {
                self.interrupt_signal()
            };

            // ── Thinking → Speak (streaming), with wake-word interrupt ──
            // Not over a reused reply already speaking: the shell would hide its Interrupt button.
            let already_speaking = reusable_speculative.is_some()
                && self.speaking.load(std::sync::atomic::Ordering::SeqCst);
            if !already_speaking {
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Thinking,
                });
            }

            let input_for_task = input.clone();
            let chat_handle = reusable_speculative.unwrap_or_else(|| {
                let svc = self.clone();
                let msg = input_for_task;
                let fired_at = std::time::Instant::now();
                tokio::spawn(async move { svc.stream_response_inner(msg, fired_at).await })
            });

            // ── InstantActivation race guard ─────────────────────────────────
            // `InstantActivation` would win the race and abort every turn, so it never races.
            enum Raced {
                Turn(std::result::Result<Result<TurnOutcome>, tokio::task::JoinError>),
                Stop(StopReason),
                Interrupted,
                Wake(Result<WakeWordActivation>),
            }
            tokio::pin!(chat_handle);
            // Scoped so the wake detector is dropped, and lets go of the mic, before any
            // follow-up capture below.
            let raced = {
                let wake_fut = async {
                    if self.wake_word_detector.supports_interruption() {
                        self.wake_word_detector
                            .wait_for_activation_with_audio()
                            .await
                    } else {
                        std::future::pending().await
                    }
                };
                tokio::pin!(wake_fut);
                tokio::pin!(interrupted);

                // Persistence happens only after the turn completes. Biased so a turn that
                // already finished is persisted rather than abandoned by a stop or interrupt in
                // the same tick.
                tokio::select! {
                    biased;
                    chat_result = &mut chat_handle => Raced::Turn(chat_result),
                    reason = self.stop_signal() => Raced::Stop(reason),
                    () = &mut interrupted => Raced::Interrupted,
                    wake_result = &mut wake_fut => Raced::Wake(wake_result),
                }
            };

            match raced {
                // Ended like a wake-word interrupt: nothing persisted, straight back to Listen.
                Raced::Turn(Ok(Ok(TurnOutcome {
                    barged_in: Some(barged),
                    ..
                }))) => {
                    diag!("\n  interrupted");
                    self.voice_output.stop_thinking_tone();
                    self.emit_event(WorkflowEvent::StateChanged {
                        state: WorkflowState::Listen,
                    });
                    diag_inline!("  {}", self.voice_input.prompt());

                    // The user is mid-sentence: capture from when they started, not from now.
                    self.voice_input.prime_with_onset(barged.onset);
                    let heard = self.unless_stopped(self.voice_input.listen()).await;
                    // Only now: the capture has its own claim, so releasing this can't close it.
                    drop(barged.hold);

                    match heard {
                        Err(reason) => end_session!(reason),
                        Ok(Ok(Some(new_text))) if !new_text.is_empty() => {
                            pending_input = Some(new_text);
                        }
                        _ => {
                            diag!("  nothing heard");
                            first_turn = true;
                        }
                    }
                }
                Raced::Turn(chat_result) => {
                    if !self.finalize_confirmed_turn(chat_result, &input).await {
                        first_turn = true;
                    }
                }
                Raced::Stop(reason) => {
                    self.abandon_turn(&chat_handle);
                    end_session!(reason);
                }
                Raced::Interrupted => {
                    diag!("\n  interrupted");
                    self.abandon_turn(&chat_handle);
                    // Back to the wake word: an interrupt asks for quiet, not a new recording.
                    first_turn = true;
                }
                Raced::Wake(wake_result) => {
                    diag!("\n  interrupted");
                    self.abandon_turn(&chat_handle);

                    // Stash the new speech for the next iteration, which keeps it interruptible.
                    match wake_result {
                        Ok(activation) => {
                            self.emit_event(WorkflowEvent::StateChanged {
                                state: WorkflowState::Listen,
                            });
                            diag_inline!("  {}", self.voice_input.prompt());

                            if let Some(wav) = activation.captured_audio {
                                self.voice_input.prime_with_captured(wav);
                            }

                            match self.unless_stopped(self.voice_input.listen()).await {
                                Err(reason) => end_session!(reason),
                                Ok(Ok(Some(new_text))) if !new_text.is_empty() => {
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

    /// Persist a finished turn once, then emit its events; `false` means reset to wake word.
    /// A persist failure keeps the conversation (the reply was heard) but skips `TurnComplete`.
    async fn finalize_confirmed_turn(
        &self,
        chat_result: std::result::Result<Result<TurnOutcome>, tokio::task::JoinError>,
        input: &str,
    ) -> bool {
        match chat_result {
            Ok(Ok(outcome)) => {
                let response_text = outcome.text.clone();
                if let Err(e) = self
                    .persist_confirmed_turn(input, &response_text, outcome.usage.as_ref())
                    .await
                {
                    // E.g. SQLITE_BUSY: serve and the child both write the WAL.
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

    /// Trace a workflow event, then forward it to the optional sink.
    fn emit_event(&self, event: WorkflowEvent) {
        match &event {
            WorkflowEvent::StateChanged { state } => {
                tracing::debug!("Workflow state: {}", state);
                self.speaking.store(
                    *state == WorkflowState::Speak,
                    std::sync::atomic::Ordering::SeqCst,
                );
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
                // Prefixed ids, as streamed; events record the bare names.
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

    /// The `agent_chat_stream` path reports no token usage.
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

    #[tokio::test]
    async fn a_transient_tool_result_is_stored_as_its_stub() {
        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("sess-t".to_string()).await.unwrap();
        let service = ChatService::new(
            Arc::new(MockAgent::new()),
            "sess-t".to_string(),
            storage.clone(),
        );
        let stub = "Web search results are not kept after the turn. Sources: kws.go.ke.";
        let searched = serde_json::json!({
            "tool_call_id": "call-1",
            "tool": "giap-knowledge__search_web",
            "content": "Web results for: when does the park close\nNairobi National Park closes at 6:00 PM.",
            "arguments": "{\"question\":\"when does the park close\"}",
            "keep": stub,
        })
        .to_string();
        service
            .persist_assistant_turn(
                vec![searched, tool_result("giap-weather__get_current_weather")],
                "It closes at 6 pm.",
                None,
                None,
            )
            .await
            .unwrap();

        let messages = storage.get_messages("sess-t").await.unwrap();
        let rows: Vec<serde_json::Value> = messages
            .iter()
            .filter(|m| m.message.role == crate::models::domain::message::Role::Tool)
            .map(|m| serde_json::from_str(&m.message.content).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["content"], stub);
        assert_eq!(rows[0]["transient"], true);
        assert!(rows[0].get("keep").is_none(), "{}", rows[0]);
        assert_eq!(rows[0]["tool_call_id"], "call-1");
        assert_eq!(
            rows[0]["arguments"],
            "{\"question\":\"when does the park close\"}"
        );
        let stored: String = messages.iter().map(|m| m.message.content.clone()).collect();
        assert!(!stored.contains("6:00 PM"), "{stored}");
        assert_eq!(
            rows[1]["content"], "ok",
            "a result with no stub is kept whole"
        );
        assert!(rows[1].get("transient").is_none());
    }

    #[test]
    fn a_row_without_a_usable_stub_is_kept_as_it_was() {
        for row in [
            serde_json::json!({"content": "x"}),
            serde_json::json!({"content": "x", "keep": "  "}),
            serde_json::json!({"content": "x", "keep": 3}),
        ] {
            assert_eq!(kept_tool_row(&row), None, "{row}");
        }
    }

    // ── truncate_tool_result (NDJSON 2000-char cap) ──────────────────────

    #[test]
    fn truncate_tool_result_leaves_sub_cap_payload_unchanged() {
        let s = "small result".to_string();
        assert_eq!(truncate_tool_result(s), "small result");
    }

    #[test]
    fn truncate_tool_result_returns_exactly_cap_chars_unchanged() {
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
        // "é" is 2 bytes: a naive byte cut could split one.
        let s = "é".repeat(TOOL_RESULT_MAX_CHARS + 10);
        let out = truncate_tool_result(s);
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
        // Still valid UTF-8 (would panic on a mid-codepoint truncate).
        assert!(out.chars().all(|c| c == 'é'));
    }

    // ── derive_title_from_text / ensure_session_title ────────────────────

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

    // ── listen_with_speculative_chat ─────────────────────────────────────

    /// Fires scripted signals, pausing so `select!` can react, then returns `final_transcript`.
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

    // ── Phantom-turn persistence invariant (data integrity) ──────────────

    #[tokio::test]
    async fn confirmed_turn_persists_exactly_once_user_and_assistant() {
        // The ground truth the speculative path must match.
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

    /// Counts the turns begun and ended.
    #[derive(Default)]
    struct CountingTurns {
        begun: std::sync::atomic::AtomicUsize,
        ended: std::sync::atomic::AtomicUsize,
    }

    impl CountingTurns {
        fn counts(&self) -> (usize, usize) {
            use std::sync::atomic::Ordering::SeqCst;
            (self.begun.load(SeqCst), self.ended.load(SeqCst))
        }
    }

    #[async_trait::async_trait]
    impl VoiceOutput for CountingTurns {
        async fn speak(&self, _text: &str) -> Result<()> {
            Ok(())
        }
        fn begin_utterance(&self) {
            self.begun.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn end_utterance(&self) {
            self.ended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// An engine that goes away half a sentence into its reply.
    struct EngineGoesAway;

    #[async_trait::async_trait]
    impl crate::models::ports::agent::Agent for EngineGoesAway {
        async fn chat(
            &self,
            _request: crate::models::ports::agent::AgentRequest,
        ) -> Result<crate::models::ports::agent::AgentResponse> {
            anyhow::bail!("the engine went away")
        }
        async fn chat_stream(
            &self,
            _request: crate::models::ports::agent::AgentRequest,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<crate::models::ports::agent::AgentStreamEvent>,
            >,
        > {
            use futures::StreamExt;
            let stream = async_stream::stream! {
                yield Ok(crate::models::ports::agent::AgentStreamEvent::Text {
                    content: "Half a sen".to_string(),
                });
                yield Err(anyhow::anyhow!("the engine went away"));
            };
            Ok(stream.boxed())
        }
    }

    /// Other audio is paused for a turn (`QuietVoiceOutput`) until the turn ends, so a turn that
    /// fails partway must still end, or the music never comes back.
    #[tokio::test]
    async fn every_turn_begun_is_ended_even_one_that_fails() {
        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("turns".to_string()).await.unwrap();
        let out = Arc::new(CountingTurns::default());

        let service = ChatService::new(Arc::new(MockAgent::new()), "turns".into(), storage.clone())
            .with_voice_output(out.clone());
        service
            .stream_response_inner("hello".to_string(), std::time::Instant::now())
            .await
            .unwrap();
        assert_eq!(out.counts(), (1, 1));

        let failing = ChatService::new(Arc::new(EngineGoesAway), "turns".into(), storage)
            .with_voice_output(out.clone());
        let outcome = failing
            .stream_response_inner("hello".to_string(), std::time::Instant::now())
            .await;
        assert!(outcome.is_err());
        assert_eq!(out.counts(), (2, 2), "a failed turn was never ended");
    }

    #[tokio::test]
    async fn persist_confirmed_turn_writes_confirmed_transcript_only() {
        // Simulates the run_loop gate after a mismatched provisional transcript.
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

        service.chat_once("Message 1".to_string()).await.unwrap();

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

        service.chat_once("Hello".to_string()).await.unwrap();
        let first_title = storage
            .get_session(&session_id)
            .await
            .unwrap()
            .title
            .clone();

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

    /// A `user`-marked title would be frozen: the idle pass never touches what a person named.
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
    // pond-server is check-only in CI, so this `run_loop` coverage lives here.

    /// Plays scripted utterances, then `None` (EOF) so `run_loop` exits cleanly.
    struct ScriptedListenInput {
        script: std::sync::Mutex<std::collections::VecDeque<Option<String>>>,
    }

    impl ScriptedListenInput {
        fn new(lines: impl IntoIterator<Item = &'static str>) -> Self {
            let mut deque: std::collections::VecDeque<Option<String>> =
                lines.into_iter().map(|s| Some(s.to_string())).collect();
            deque.push_back(None);
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

    /// Start-then-stop is not "off": it is an audible click.
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

    /// Asserts counts, not "no panic": the failure this guards is a silent no-op.
    #[test]
    fn an_enabled_tone_starts_once_and_stops_once() {
        let out = Arc::new(CountingTone::default());
        {
            let mut tone = WorkingTone::start(out.clone(), true);
            assert_eq!(out.counts(), (1, 0), "tone did not start when enabled");
            // stop() runs more than once in practice (several paths to the first sentence).
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

    /// Covers the `?`-on-stream-error path, which never calls `stop()`.
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
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.to_ndjson().is_some())
                .cloned()
                .collect()
        }
    }

    /// Storage whose `add_message` always fails (a simulated SQLITE_BUSY); the rest delegates.
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

        // A prefix, not equality: the loop keeps cycling Wait/Listen until the script hits EOF.
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

        let transcripts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Transcript { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(transcripts, vec!["tell me a joke"], "one transcript event");

        let token_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::Token { .. }))
            .count();
        assert!(
            token_count >= 1,
            "tokens must be streamed; got {token_count}"
        );

        let turn_complete_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 1,
            "exactly one turn_complete on completion"
        );

        // `Ready` comes from the CLI after model load, not from run_loop.
        match events.last() {
            Some(WorkflowEvent::Exit { reason }) => assert_eq!(reason, "stdin_eof"),
            other => panic!("last event must be exit(stdin_eof); got {other:?}"),
        }
    }

    #[tokio::test]
    async fn thought_filter_tail_is_emitted_as_token_and_matches_persisted_text() {
        // MockAgent echoes this; the partial marker "<end_of_tu" is withheld until flush().
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

        let streamed: String = events
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

        assert_eq!(
            streamed, assistant.message.content,
            "the streamed Token events must reconstruct the full persisted reply, \
             including the ThoughtFilter tail flushed at stream end"
        );
        assert!(
            streamed.ends_with("<end_of_tu"),
            "the withheld tail must reach the Token stream; got {streamed:?}"
        );
    }

    #[tokio::test]
    async fn no_tail_is_withheld_when_the_reply_ends_on_ordinary_text() {
        // Mirror of the test above: a tail that cannot begin a marker must never be held.
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
        assert!(
            !tokens.is_empty(),
            "an ordinary reply must produce at least one Token before flush"
        );
        // MockAgent sends the reply as one chunk, so without holdback the first Token is all of it.
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
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(FailingAddStorage::new());
        let session_id = "persist-fail-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let svc = ChatService::new(agent, session_id.clone(), storage)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        let stayed_conversational = svc
            .finalize_confirmed_turn(
                Ok(Ok(TurnOutcome {
                    text: "the answer is 42".to_string(),
                    usage: None,
                    stats: None,
                    total_latency_ms: 0,
                    barged_in: None,
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

    /// An interruptible detector that activates at once, so every race is an interrupt.
    struct AlwaysInterruptDetector;

    #[async_trait::async_trait]
    impl StreamingWakeWordDetector for AlwaysInterruptDetector {
        async fn wait_for_activation_with_audio(
            &self,
        ) -> Result<crate::models::ports::wake_word::WakeWordActivation> {
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
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "interrupt-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let input = Arc::new(ScriptedListenInput::new(["a long question"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_wake_word_detector(Arc::new(AlwaysInterruptDetector))
            .with_event_sink(collector.sink());

        // MockAgent sleeps 300ms before streaming, so the instant wake always wins.
        svc.run_loop().await.unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "an interrupted turn must persist nothing; found {} messages",
            msgs.len()
        );

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

    // ── Barge-in: speaking over the reply ends the turn ──────────────────

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// What the fake mic was asked to do.
    #[derive(Default)]
    struct MicLog {
        holds: AtomicUsize,
        releases: AtomicUsize,
        readings: AtomicUsize,
    }

    impl MicLog {
        fn held(&self) -> bool {
            self.holds.load(Ordering::SeqCst) > self.releases.load(Ordering::SeqCst)
        }
    }

    /// Reads nothing until held open, like a closed mic; then hears the user talk over the
    /// first reply, and quiet after that.
    struct TalkOverFirstReply(Arc<MicLog>);

    impl SpeechEnergy for TalkOverFirstReply {
        fn recent_rms(&self, _window_ms: u64) -> Option<f32> {
            if !self.0.held() {
                return None;
            }
            let n = self.0.readings.fetch_add(1, Ordering::SeqCst);
            Some(if n < pond_voice::barge::CONSECUTIVE_WINDOWS as usize {
                0.5
            } else {
                0.0
            })
        }

        fn hold_mic(&self) -> MicHold {
            self.0.holds.fetch_add(1, Ordering::SeqCst);
            let log = self.0.clone();
            MicHold::new(move || {
                log.releases.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    /// Sets its flag when dropped, i.e. when the agent's stream is dropped.
    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// The first reply runs on for seconds, so only an ended turn stops it; then it echoes.
    struct LongFirstReply {
        calls: AtomicUsize,
        first_stream_dropped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::models::ports::agent::Agent for LongFirstReply {
        async fn chat(
            &self,
            _request: crate::models::ports::agent::AgentRequest,
        ) -> Result<crate::models::ports::agent::AgentResponse> {
            anyhow::bail!("voice uses chat_stream")
        }

        async fn chat_stream(
            &self,
            request: crate::models::ports::agent::AgentRequest,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<crate::models::ports::agent::AgentStreamEvent>,
            >,
        > {
            use crate::models::ports::agent::AgentStreamEvent as E;
            use futures::StreamExt;

            let session_id = request.session_id.clone();
            let model_role = request.model_role.clone();
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let dropped = SetOnDrop(self.first_stream_dropped.clone());
                let stream = async_stream::stream! {
                    let _dropped = dropped;
                    yield Ok(E::Text { content: "Monday will be sunny. ".to_string() });
                    tokio::time::sleep(Duration::from_millis(1_500)).await;
                    yield Ok(E::Text { content: "Tuesday brings rain. ".to_string() });
                    yield Ok(E::Done { session_id, model_role, usage: None, stats: None });
                };
                return Ok(stream.boxed());
            }
            let reply = format!("Echo: {}", request.message);
            let stream = async_stream::stream! {
                yield Ok(E::Text { content: reply });
                yield Ok(E::Done { session_id, model_role, usage: None, stats: None });
            };
            Ok(stream.boxed())
        }
    }

    /// One `listen()`: the onset it was primed with, and the state of things when it ran.
    struct Capture {
        onset: Option<Instant>,
        at: Instant,
        mic_held: bool,
        first_reply_dropped: bool,
    }

    /// Scripted speech that records every capture.
    struct RecordingInput {
        script: std::sync::Mutex<std::collections::VecDeque<Option<String>>>,
        onset: std::sync::Mutex<Option<Instant>>,
        captures: std::sync::Mutex<Vec<Capture>>,
        mic: Arc<MicLog>,
        first_reply_dropped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl VoiceInput for RecordingInput {
        async fn listen(&self) -> Result<Option<String>> {
            self.captures.lock().unwrap().push(Capture {
                onset: self.onset.lock().unwrap().take(),
                at: Instant::now(),
                mic_held: self.mic.held(),
                first_reply_dropped: self.first_reply_dropped.load(Ordering::SeqCst),
            });
            Ok(self.script.lock().unwrap().pop_front().flatten())
        }

        fn prime_with_onset(&self, onset: Instant) {
            *self.onset.lock().unwrap() = Some(onset);
        }
    }

    /// Activates on the listed calls (1-based) and otherwise never.
    struct ActivatesOn(AtomicUsize, &'static [usize]);

    #[async_trait::async_trait]
    impl StreamingWakeWordDetector for ActivatesOn {
        async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
            let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            if !self.1.contains(&call) {
                std::future::pending::<()>().await;
            }
            Ok(WakeWordActivation {
                captured_audio: None,
            })
        }
    }

    struct BargeInRun {
        persisted: Vec<SessionMessage>,
        events: Vec<WorkflowEvent>,
        captures: Vec<Capture>,
        spoken: Vec<String>,
        mic: Arc<MicLog>,
    }

    /// Ask a question, talk over the first sentence of the answer, then ask the follow-up.
    async fn talk_over_the_reply(detector: Arc<dyn StreamingWakeWordDetector>) -> BargeInRun {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "barge-in".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let mic = Arc::new(MicLog::default());
        let first_reply_dropped = Arc::new(AtomicBool::new(false));
        let input = Arc::new(RecordingInput {
            script: std::sync::Mutex::new(
                [
                    Some("what is the weather this week"),
                    Some("what about saturday"),
                    None,
                ]
                .into_iter()
                .map(|s| s.map(str::to_string))
                .collect(),
            ),
            onset: std::sync::Mutex::new(None),
            captures: std::sync::Mutex::new(Vec::new()),
            mic: mic.clone(),
            first_reply_dropped: first_reply_dropped.clone(),
        });
        let output = Arc::new(CapturingSpeak::default());
        let collector = EventCollector::default();
        let agent = Arc::new(LongFirstReply {
            calls: AtomicUsize::new(0),
            first_stream_dropped: first_reply_dropped,
        });

        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input.clone())
            .with_voice_output(output.clone())
            .with_speech_energy(Arc::new(TalkOverFirstReply(mic.clone())))
            .with_wake_word_detector(detector)
            .with_event_sink(collector.sink());

        tokio::time::timeout(Duration::from_secs(10), svc.run_loop())
            .await
            .expect("the loop must not hang on a barge-in")
            .unwrap();

        let captures = std::mem::take(&mut *input.captures.lock().unwrap());
        let spoken = output.spoken.lock().unwrap().clone();
        BargeInRun {
            persisted: storage.get_messages(&session_id).await.unwrap(),
            events: collector.ndjson_events(),
            captures,
            spoken,
            mic,
        }
    }

    fn assert_the_barge_in_ended_the_turn(run: &BargeInRun) {
        let persisted: Vec<(Role, &str)> = run
            .persisted
            .iter()
            .map(|m| (m.message.role.clone(), m.message.content.as_str()))
            .collect();
        assert_eq!(
            persisted,
            vec![
                (Role::User, "what about saturday"),
                (Role::Assistant, "Echo: what about saturday"),
            ],
            "the interrupted reply must not be persisted as a completed turn"
        );
        assert!(
            !run.spoken.iter().any(|s| s.contains("Tuesday")),
            "synthesis went on after the barge-in: {:?}",
            run.spoken
        );
        let completed = run
            .events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(completed, 1, "only the follow-up turn completed");

        let follow_up = &run.captures[1];
        assert!(
            follow_up.first_reply_dropped,
            "the interrupted reply was still generating when the follow-up capture began"
        );
        let onset = follow_up
            .onset
            .expect("the follow-up capture must start from the barge-in onset, not from now");
        let debounce_and_pre_roll = Duration::from_millis(
            pond_voice::barge::POLL_MS * (pond_voice::barge::CONSECUTIVE_WINDOWS as u64 - 1)
                + pond_voice::barge::WINDOW_MS
                + pond_voice::barge::ONSET_PRE_ROLL_MS,
        );
        assert!(
            follow_up.at.duration_since(onset) >= debounce_and_pre_roll,
            "the capture must reach back past the debounce to the pre-roll; it reached {:?}",
            follow_up.at.duration_since(onset)
        );
        assert!(
            follow_up.mic_held,
            "the mic was let go before the follow-up capture, losing what was said meanwhile"
        );
        assert!(
            run.captures[0].onset.is_none() && run.captures[2..].iter().all(|c| c.onset.is_none()),
            "only the capture after a barge-in starts in the past"
        );
        assert_eq!(
            run.mic.holds.load(Ordering::SeqCst),
            run.mic.releases.load(Ordering::SeqCst),
            "every mic hold must be released"
        );
    }

    /// With no wake-word detector nothing else listens mid-reply, so the watch must open the mic.
    #[tokio::test]
    async fn a_barge_in_ends_the_turn_unpersisted_and_captures_from_its_onset() {
        let run = talk_over_the_reply(Arc::new(InstantActivation)).await;
        assert_the_barge_in_ended_the_turn(&run);
    }

    #[tokio::test]
    async fn a_barge_in_ends_the_turn_the_same_way_while_a_wake_word_detector_races_it() {
        // 1: the first wake word. 2, 3: the races during each reply, which must not fire.
        // 4: back to Wait once the script runs out, so the loop reaches end of input.
        let run = talk_over_the_reply(Arc::new(ActivatesOn(AtomicUsize::new(0), &[1, 4]))).await;
        assert_the_barge_in_ended_the_turn(&run);
    }

    #[tokio::test]
    async fn a_reply_heard_out_releases_the_mic_and_reports_no_barge_in() {
        struct Quiet(Arc<MicLog>);
        impl SpeechEnergy for Quiet {
            fn recent_rms(&self, _window_ms: u64) -> Option<f32> {
                Some(0.0)
            }
            fn hold_mic(&self) -> MicHold {
                self.0.holds.fetch_add(1, Ordering::SeqCst);
                let log = self.0.clone();
                MicHold::new(move || {
                    log.releases.fetch_add(1, Ordering::SeqCst);
                })
            }
        }

        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("heard-out".into()).await.unwrap();
        let mic = Arc::new(MicLog::default());
        let svc = ChatService::new(Arc::new(MockAgent::new()), "heard-out".into(), storage)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_speech_energy(Arc::new(Quiet(mic.clone())));

        let outcome = svc
            .stream_response_inner("One. Two.".to_string(), Instant::now())
            .await
            .unwrap();
        assert!(outcome.barged_in.is_none());
        assert_eq!(outcome.text, "Echo: One. Two.");
        assert_eq!(
            mic.holds.load(Ordering::SeqCst),
            1,
            "the reply must hold the mic"
        );
        assert_eq!(
            mic.releases.load(Ordering::SeqCst),
            1,
            "a finished reply must let go of the mic"
        );
    }

    /// A tool running between sentences plays nothing, so talk then is not over the reply.
    #[tokio::test]
    async fn talk_while_nothing_plays_between_sentences_is_not_a_barge_in() {
        struct Loud(Arc<MicLog>);
        impl SpeechEnergy for Loud {
            fn recent_rms(&self, _window_ms: u64) -> Option<f32> {
                Some(0.5)
            }
            fn hold_mic(&self) -> MicHold {
                self.0.holds.fetch_add(1, Ordering::SeqCst);
                let log = self.0.clone();
                MicHold::new(move || {
                    log.releases.fetch_add(1, Ordering::SeqCst);
                })
            }
        }
        #[derive(Default)]
        struct NothingPlaying(CapturingSpeak);
        #[async_trait::async_trait]
        impl VoiceOutput for NothingPlaying {
            async fn speak(&self, text: &str) -> Result<()> {
                self.0.speak(text).await
            }
            fn is_playing(&self) -> Option<bool> {
                Some(false)
            }
        }

        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("between".into()).await.unwrap();
        let mic = Arc::new(MicLog::default());
        let agent = Arc::new(LongFirstReply {
            calls: AtomicUsize::new(0),
            first_stream_dropped: Arc::new(AtomicBool::new(false)),
        });
        let svc = ChatService::new(agent, "between".into(), storage)
            .with_voice_output(Arc::new(NothingPlaying::default()))
            .with_speech_energy(Arc::new(Loud(mic.clone())));

        let outcome = svc
            .stream_response_inner("what is the weather this week".to_string(), Instant::now())
            .await
            .unwrap();
        assert!(
            outcome.barged_in.is_none(),
            "talk over silence ended the turn"
        );
        assert!(outcome.text.contains("Tuesday"), "got {:?}", outcome.text);
        assert_eq!(
            mic.holds.load(Ordering::SeqCst),
            mic.releases.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn pipelined_tts_streams_synthesis_and_playback_concurrently() {
        #[derive(Default)]
        struct MockPipelinedVoice {
            actions: std::sync::Mutex<Vec<String>>,
        }

        #[async_trait::async_trait]
        impl VoiceOutput for MockPipelinedVoice {
            async fn speak(&self, text: &str) -> Result<()> {
                self.actions.lock().unwrap().push(format!("speak:{text}"));
                Ok(())
            }
            async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
                self.actions.lock().unwrap().push(format!("synth:{text}"));
                Ok(Some(text.as_bytes().to_vec()))
            }
            async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
                let text = String::from_utf8_lossy(&audio).to_string();
                self.actions.lock().unwrap().push(format!("play:{text}"));
                Ok(())
            }
        }

        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("stream-tts".into()).await.unwrap();
        let voice = Arc::new(MockPipelinedVoice::default());
        let svc = ChatService::new(Arc::new(MockAgent::new()), "stream-tts".into(), storage)
            .with_voice_output(voice.clone());

        let outcome = svc
            .stream_response_inner(
                "Hello world. How are you today?".to_string(),
                Instant::now(),
            )
            .await
            .unwrap();
        assert!(outcome.barged_in.is_none());
        let actions = voice.actions.lock().unwrap().clone();
        assert!(
            actions.iter().any(|a| a.starts_with("synth:")),
            "expected synthesis actions, got {actions:?}"
        );
        assert!(
            actions.iter().any(|a| a.starts_with("play:")),
            "expected playback actions, got {actions:?}"
        );
    }

    // ── Shell control: stdin `interrupt` and `stop` ───────────────────────

    /// Speaks one sentence, then never finishes: a reply long enough to interrupt.
    struct EndlessReplyAgent;

    #[async_trait::async_trait]
    impl Agent for EndlessReplyAgent {
        async fn chat(
            &self,
            _request: AgentRequest,
        ) -> Result<crate::shared::domain::agent::AgentResponse> {
            unreachable!("the voice loop streams")
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
            let stream = async_stream::stream! {
                yield Ok(AgentStreamEvent::Text {
                    content: "Once upon a time, a goose sat in a pond.".to_string(),
                });
                std::future::pending::<()>().await;
            };
            Ok(stream.boxed())
        }
    }

    /// Counts `stop_speaking`; with `stop_at_turn_end`, stops the session as a turn finishes.
    #[derive(Default)]
    struct StopCountingSpeak {
        stops: std::sync::atomic::AtomicUsize,
        stop_at_turn_end: Option<VoiceControl>,
    }

    impl StopCountingSpeak {
        fn stops(&self) -> usize {
            self.stops.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl VoiceOutput for StopCountingSpeak {
        async fn speak(&self, _text: &str) -> Result<()> {
            Ok(())
        }
        fn stop_speaking(&self) {
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        // The last thing `stream_response_inner` does, with no await before the task returns.
        fn end_utterance(&self) {
            if let Some(control) = &self.stop_at_turn_end {
                control.stop(StopReason::Requested);
            }
        }
    }

    /// Interruptible; hears the wake word at once the first time, then never again.
    #[derive(Default)]
    struct WakesOnce {
        woken: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl StreamingWakeWordDetector for WakesOnce {
        async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
            if self.woken.swap(true, std::sync::atomic::Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(WakeWordActivation {
                captured_audio: None,
            })
        }
        fn supports_interruption(&self) -> bool {
            true
        }
    }

    struct NeverHears;

    #[async_trait::async_trait]
    impl VoiceInput for NeverHears {
        async fn listen(&self) -> Result<Option<String>> {
            std::future::pending().await
        }
    }

    /// Records every event, then hands it to `steer` to drive the loop from its own events.
    fn steering_sink(
        collector: &EventCollector,
        steer: impl Fn(&WorkflowEvent) + Send + Sync + 'static,
    ) -> WorkflowEventSink {
        let record = collector.sink();
        Arc::new(move |event: &WorkflowEvent| {
            record(event);
            steer(event);
        })
    }

    fn is_state(event: &WorkflowEvent, wanted: WorkflowState) -> bool {
        matches!(event, WorkflowEvent::StateChanged { state } if *state == wanted)
    }

    fn states_of(events: &[WorkflowEvent]) -> Vec<WorkflowState> {
        events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::StateChanged { state } => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn exit_reasons(events: &[WorkflowEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Exit { reason } => Some(reason.as_str()),
                _ => None,
            })
            .collect()
    }

    fn turn_completes(events: &[WorkflowEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count()
    }

    async fn run_bounded(svc: &ChatService) -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(5), svc.run_loop())
            .await
            .expect("run_loop must end instead of hanging")
    }

    #[tokio::test]
    async fn an_interrupt_ends_the_turn_persisting_nothing_and_returns_to_the_wake_word() {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-interrupt".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        let collector = EventCollector::default();
        let output = Arc::new(StopCountingSpeak::default());
        let svc = ChatService::new(
            Arc::new(EndlessReplyAgent),
            session_id.clone(),
            storage.clone(),
        )
        .with_voice_input(Arc::new(ScriptedListenInput::new(["tell me a long story"])))
        .with_voice_output(output.clone())
        .with_control(control.clone())
        .with_event_sink(steering_sink(&collector, move |event| {
            if is_state(event, WorkflowState::Speak) {
                control.apply(crate::shared::services::voice_control::ControlCommand::Interrupt);
            }
        }));

        // InstantActivation: the turn is not raced against a wake word, only against the shell.
        run_bounded(&svc).await.unwrap();

        assert!(
            storage.get_messages(&session_id).await.unwrap().is_empty(),
            "an interrupted turn must persist nothing"
        );
        let events = collector.events.lock().unwrap().clone();
        assert_eq!(
            turn_completes(&events),
            0,
            "no turn_complete for an interrupted turn"
        );
        assert!(output.stops() >= 1, "the interrupt must stop speech");

        let states = states_of(&events);
        let speak = states
            .iter()
            .position(|s| *s == WorkflowState::Speak)
            .expect("the reply started speaking");
        assert_eq!(
            states.get(speak + 1),
            Some(&WorkflowState::Wait),
            "after an interrupt the loop waits for the wake word; got {states:?}"
        );
        assert_eq!(exit_reasons(&events), vec!["stdin_eof"]);
    }

    #[tokio::test]
    async fn an_interrupt_wins_while_the_wake_word_race_is_armed_and_a_stop_ends_the_wait() {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-interrupt-race".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        let collector = EventCollector::default();
        let output = Arc::new(StopCountingSpeak::default());
        let waits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let svc = ChatService::new(
            Arc::new(EndlessReplyAgent),
            session_id.clone(),
            storage.clone(),
        )
        .with_voice_input(Arc::new(ScriptedListenInput::new(["tell me a long story"])))
        .with_voice_output(output.clone())
        .with_wake_word_detector(Arc::new(WakesOnce::default()))
        .with_control(control.clone())
        .with_event_sink(steering_sink(&collector, move |event| {
            if is_state(event, WorkflowState::Speak) {
                control.interrupt();
            }
            // The second Wait is the one after the interrupt; the detector never wakes again.
            if is_state(event, WorkflowState::Wait)
                && waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
            {
                control.stop(StopReason::Requested);
            }
        }));

        run_bounded(&svc).await.unwrap();

        assert!(storage.get_messages(&session_id).await.unwrap().is_empty());
        let events = collector.events.lock().unwrap().clone();
        assert_eq!(turn_completes(&events), 0);
        assert!(output.stops() >= 1, "the interrupt must stop speech");
        assert_eq!(exit_reasons(&events), vec!["stopped"]);
        assert_eq!(
            states_of(&events).last(),
            Some(&WorkflowState::Wait),
            "the stop must end the wake-word wait without listening"
        );
    }

    #[tokio::test]
    async fn a_stop_mid_turn_abandons_it_and_ends_the_loop() {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-stop-mid-turn".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        let collector = EventCollector::default();
        let output = Arc::new(StopCountingSpeak::default());
        let svc = ChatService::new(
            Arc::new(EndlessReplyAgent),
            session_id.clone(),
            storage.clone(),
        )
        .with_voice_input(Arc::new(ScriptedListenInput::new([
            "tell me a long story",
            "more",
        ])))
        .with_voice_output(output.clone())
        .with_control(control.clone())
        .with_event_sink(steering_sink(&collector, move |event| {
            if is_state(event, WorkflowState::Speak) {
                control.apply(crate::shared::services::voice_control::ControlCommand::Stop);
            }
        }));

        run_bounded(&svc).await.unwrap();

        assert!(
            storage.get_messages(&session_id).await.unwrap().is_empty(),
            "an abandoned turn must persist nothing"
        );
        let events = collector.events.lock().unwrap().clone();
        assert_eq!(turn_completes(&events), 0);
        assert!(output.stops() >= 1, "the stop must silence the reply");
        assert_eq!(exit_reasons(&events), vec!["stopped"]);
        assert!(
            matches!(events.last(), Some(WorkflowEvent::Exit { .. })),
            "nothing may follow the exit; got {:?}",
            events.last()
        );
    }

    #[tokio::test]
    async fn a_stop_while_listening_ends_the_loop() {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-stop-listening".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        let collector = EventCollector::default();
        let svc = ChatService::new(Arc::new(MockAgent::new()), session_id, storage)
            .with_voice_input(Arc::new(NeverHears))
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_control(control.clone())
            .with_event_sink(steering_sink(&collector, move |event| {
                if is_state(event, WorkflowState::Listen) {
                    control.stop(StopReason::StdinEof);
                }
            }));

        run_bounded(&svc).await.unwrap();

        let events = collector.events.lock().unwrap().clone();
        assert_eq!(exit_reasons(&events), vec!["stdin_eof"]);
        assert_eq!(turn_completes(&events), 0);
    }

    #[tokio::test]
    async fn a_stop_requested_before_the_loop_opens_nothing() {
        struct MustNotWait;

        #[async_trait::async_trait]
        impl StreamingWakeWordDetector for MustNotWait {
            async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
                panic!("a stopped session must not open the microphone")
            }
        }

        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-stop-early".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        control.stop(StopReason::Requested);
        let collector = EventCollector::default();
        let svc = ChatService::new(Arc::new(MockAgent::new()), session_id, storage)
            .with_wake_word_detector(Arc::new(MustNotWait))
            .with_control(control)
            .with_event_sink(collector.sink());

        run_bounded(&svc).await.unwrap();

        let events = collector.events.lock().unwrap().clone();
        assert!(
            matches!(events.as_slice(), [WorkflowEvent::Exit { reason }] if reason == "stopped"),
            "only the exit may be emitted; got {events:?}"
        );
    }

    #[tokio::test]
    async fn an_interrupt_sent_while_idle_does_not_cut_the_next_turn() {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-idle-interrupt".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        control.interrupt();
        let collector = EventCollector::default();
        let svc = ChatService::new(
            Arc::new(MockAgent::new()),
            session_id.clone(),
            storage.clone(),
        )
        .with_voice_input(Arc::new(ScriptedListenInput::new(["hello there"])))
        .with_voice_output(Arc::new(CapturingSpeak::default()))
        .with_control(control)
        .with_event_sink(collector.sink());

        run_bounded(&svc).await.unwrap();

        assert_eq!(storage.get_messages(&session_id).await.unwrap().len(), 2);
        assert_eq!(turn_completes(&collector.events.lock().unwrap()), 1);
    }

    #[tokio::test]
    async fn a_turn_that_already_finished_is_persisted_before_a_stop_is_honoured() {
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "shell-stop-after-turn".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let control = VoiceControl::new();
        let collector = EventCollector::default();
        let output = Arc::new(StopCountingSpeak {
            stop_at_turn_end: Some(control.clone()),
            ..StopCountingSpeak::default()
        });
        let svc = ChatService::new(
            Arc::new(MockAgent::new()),
            session_id.clone(),
            storage.clone(),
        )
        .with_voice_input(Arc::new(ScriptedListenInput::new([
            "hello there",
            "unheard",
        ])))
        .with_voice_output(output.clone())
        .with_control(control)
        .with_event_sink(collector.sink());

        run_bounded(&svc).await.unwrap();

        assert_eq!(
            storage.get_messages(&session_id).await.unwrap().len(),
            2,
            "a reply that was fully spoken must be persisted, not abandoned"
        );
        let events = collector.events.lock().unwrap().clone();
        assert_eq!(turn_completes(&events), 1);
        assert_eq!(output.stops(), 0, "nothing was cut off");
        assert_eq!(exit_reasons(&events), vec!["stopped"]);
        let transcripts = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::Transcript { .. }))
            .count();
        assert_eq!(
            transcripts, 1,
            "the stop must come before the next turn is heard"
        );
    }

    // ── Speaking first ────────────────────────────────────────────────────

    use crate::user_data::domain::settings::Settings;

    #[test]
    fn the_time_of_day_is_read_in_the_zone_named() {
        let at: chrono::DateTime<chrono::Utc> = "2026-10-14T20:30:00Z".parse().unwrap();
        assert_eq!(
            LocalTimeOfDay::in_zone("Africa/Nairobi", at),
            at_time(23, 30)
        );
        assert_eq!(LocalTimeOfDay::in_zone(" UTC ", at), at_time(20, 30));
        let here = at.with_timezone(&chrono::Local);
        assert_eq!(
            LocalTimeOfDay::in_zone("not a zone", at),
            at_time(
                chrono::Timelike::hour(&here),
                chrono::Timelike::minute(&here)
            )
        );
    }

    fn at_time(hour: u32, minute: u32) -> LocalTimeOfDay {
        LocalTimeOfDay::new(hour, minute).unwrap()
    }

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
        /// The case that should speak; each test below changes one thing.
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

    /// The vacuity control for every refusal test in this section.
    #[test]
    fn an_enabled_present_member_outside_quiet_hours_is_spoken_to() {
        assert_eq!(
            decide(Some(&speech_on()), &Utt::speakable()),
            UnpromptedSpeech::Spoken
        );
    }

    /// Asserts the refusal reason, not mere silence: quiet hours must be checked first.
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

                            // Vacuity control: some of these must speak by day.
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

    /// The default is silent today too, but it may change; an unreadable store is never consent.
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

    /// Speaking to `Household` is addressed to nobody and heard by everybody.
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

    /// The shipped list is `alert` alone.
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

    /// Deliberately the opposite of `in_time_window`'s `false`: here false would mean speaking.
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

    /// `%H` accepts unpadded hours; compared as stored text, these would read as never quiet.
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

    /// Vacuity control for the test above: unpadded, distinct bounds still form a window.
    #[test]
    fn an_unpadded_bound_is_still_read_as_the_time_it_names() {
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
        // Vacuity control: the edges are accepted.
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

    /// Drives the real `ChatService`, so it fails if `speak_unprompted` speaks before deciding.
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

        // Vacuity control: the allowed case does reach the speaker.
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
