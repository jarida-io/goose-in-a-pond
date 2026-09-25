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
use crate::user_data::ports::memory_extractor::MemoryExtractor;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::ports::session_storage::SessionStorage;
use crate::user_data::services::memory_extraction::MemoryExtractionService;
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

// ── PAI-7 P6: speaking first ─────────────────────────────────────────────────

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

impl Drop for WorkingTone {
    fn drop(&mut self) {
        self.stop();
    }
}

// TTS text handling is in `pond-voice` so the desktop shell (separate workspace) shares it.
use pond_voice::text::{split_sentences, strip_markdown_for_speech};

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
    /// With all three memory fields set, `persist_assistant_turn_with_extraction` runs extraction.
    memory_extractor: Option<Arc<dyn MemoryExtractor>>,
    memory_extraction_service: Option<Arc<MemoryExtractionService>>,
    memory_repo: Option<Arc<dyn MemoryRepository>>,
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
}

/// Result of one non-persisting agent stream, with the stats from its `Done` event.
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
            memory_extractor: None,
            memory_extraction_service: None,
            memory_repo: None,
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

    /// Attach memory extraction, which `persist_assistant_turn_with_extraction` then spawns.
    pub fn with_memory_extraction(
        mut self,
        extractor: Arc<dyn MemoryExtractor>,
        service: Arc<MemoryExtractionService>,
        repo: Arc<dyn MemoryRepository>,
    ) -> Self {
        self.memory_extractor = Some(extractor);
        self.memory_extraction_service = Some(service);
        self.memory_repo = Some(repo);
        self
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

    fn watch_for_barge_in(&self) -> Option<BargeInWatch> {
        use pond_voice::barge;

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

    /// Set the wake-word detector; defaults to `InstantActivation` (no wait).
    pub fn with_wake_word_detector(mut self, detector: Arc<dyn StreamingWakeWordDetector>) -> Self {
        self.wake_word_detector = detector;
        self
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

    /// `persist_assistant_turn` plus background memory extraction, so handlers can't omit it.
    pub async fn persist_assistant_turn_with_extraction(
        &self,
        tool_results: Vec<String>,
        assistant_text: &str,
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
        user_message: &str,
    ) -> Result<()> {
        self.persist_assistant_turn(tool_results, assistant_text, usage, model_name)
            .await?;

        if let (Some(ext), Some(svc), Some(repo)) = (
            self.memory_extractor.clone(),
            self.memory_extraction_service.clone(),
            self.memory_repo.clone(),
        ) {
            let user_msg = user_message.to_string();
            let asst_resp = assistant_text.to_string();
            let sid = self.session_id.clone();
            let scope = self.profile_scope.clone();
            tokio::spawn(async move {
                svc.run(
                    ext.as_ref(),
                    repo.as_ref(),
                    &user_msg,
                    &asst_resp,
                    Some(&sid),
                    &scope,
                )
                .await;
            });
        }

        Ok(())
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
        };

        // Clear interrupts once per turn, not per sentence, so a barge-in stops the whole reply.
        self.voice_output.begin_utterance();

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
        let mut thought_filter = crate::models::services::thought_filter::ThoughtFilter::new();

        // Pipelined TTS: `pending_audio` (WAV) plays while the next sentence is synthesized.
        let mut pending_audio: Option<Vec<u8>> = None;
        let mut first_sentence_spoken = false;

        macro_rules! speak_pipelined {
            ($self:expr, $text:expr, $pending:expr, $first_spoken:expr) => {{
                let text = $text;
                if !*$first_spoken {
                    // First sentence: plain speak() (clause-split) so audio starts at once.
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

                    let (sentences, remainder) = split_sentences(&sentence_buf);
                    sentence_buf = remainder;
                    for sentence in sentences {
                        let spoken = strip_markdown_for_speech(&sentence);
                        if spoken.is_empty() {
                            continue;
                        }
                        tone.stop();
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

        if let Some(last) = pending_audio.take() {
            if let Err(e) = self.voice_output.play_audio(last).await {
                tracing::warn!("TTS final playback failed: {}", e);
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

        // Explicit drop: stop the barge-in watch now that TTS is done, after the tone.
        drop(barge_in.take());

        Ok(TurnOutcome {
            text: full_text,
            usage: turn_usage,
            stats: turn_stats,
            total_latency_ms: fired_at.elapsed().as_millis() as u64,
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

        // Keeps the fired-on transcript so `run_loop` can match it to the final one.
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
                            // Non-persisting: a wrong provisional transcript leaves no turn.
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
                            // Speech resumed: abort the job and silence anything it started.
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

        loop {
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

            // ── Thinking → Speak (streaming), with wake-word interrupt ──
            self.emit_event(WorkflowEvent::StateChanged {
                state: WorkflowState::Thinking,
            });

            // ── Q2-26 phantom-turn gate ─────────────────────────────────────
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

            let input_for_task = input.clone();
            let chat_handle = reusable_speculative.unwrap_or_else(|| {
                let svc = self.clone();
                let msg = input_for_task;
                let fired_at = std::time::Instant::now();
                tokio::spawn(async move { svc.stream_response_inner(msg, fired_at).await })
            });

            // ── InstantActivation race guard ─────────────────────────────────
            // `InstantActivation` would win the race and abort every turn: await directly.
            if !self.wake_word_detector.supports_interruption() {
                let chat_result = chat_handle.await;
                if !self.finalize_confirmed_turn(chat_result, &input).await {
                    first_turn = true;
                }
                continue;
            }

            // Race the turn against the wake word; persistence happens only after it completes.
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

                    self.voice_output.stop_speaking();
                    self.voice_output.stop_thinking_tone();

                    // Stops Goose's inference within a token (drop-based); nothing was persisted.
                    chat_handle.abort();

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

                            match self.voice_input.listen().await {
                                Ok(Some(new_text)) if !new_text.is_empty() => {
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

    // ── Phantom-turn persistence invariant (Q2-26, data integrity) ───────

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
