use crate::user_data::domain::profile::ProfileScope;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Mutex;

/// One step of the boot-time prefix warm-up (see `Agent::prewarm`).
/// States, not a percentage: the engine reports no progress inside a model load or prefill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WarmupPhase {
    /// The warm-up generation is running: model load + prompt-prefix prefill.
    Warming,
    /// The prefix is resident in the engine's KV cache; turn 1 will reuse it.
    Ready,
    /// Nothing to warm on this backend (mock, HTTP providers), or warm-up is disabled.
    Skipped { reason: String },
    /// The warm-up generation failed; the first real turn just pays the full prefill.
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub message: String,
    pub session_id: String,
    pub model_role: String, // "chat" | "think" | "task"
    /// Optional image attachments for multimodal models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::models::domain::message::ImageAttachment>,
    /// From voice mode: the agent disables thinking and keeps replies short and unformatted.
    #[serde(default)]
    pub voice_mode: bool,
    /// From Canvas mode: prefer tool calls over prose so results render as visual cards.
    #[serde(default)]
    pub canvas_mode: bool,
    /// Whose data this turn may reach; resolved once at the edge, never recomputed downstream.
    /// No `Default` on `AgentRequest`, so every construction site must choose. The serde
    /// default exists only for payloads serialized before this field.
    #[serde(default = "ProfileScope::household")]
    pub profile_scope: ProfileScope,
    /// The speaking member's own preferences, resolved at the edge alongside the scope.
    /// `None` means no personal context; never substitute the primary member's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_context: Option<crate::prompts::ProfileContext>,
    /// Tool-group prefixes (e.g. `"giap-weather"`) a recipe's `extensions:` limits this turn to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_group_allowlist: Option<Vec<String>>,
    /// This turn is the boot-time prefix warm-up, so the completeness check is not armed.
    /// Safe: the check only appends a later message, never altering the warmed first request.
    #[serde(default)]
    pub warmup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub text: String,
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    Status {
        content: String,
    },
    /// Model reasoning (chain-of-thought); only emitted when `show_thinking` is enabled.
    Thinking {
        content: String,
    },
    ToolCall {
        id: String,
        tool: String,
        input: Option<serde_json::Value>,
    },
    ToolResult {
        id: String,
        tool: String,
        content: String,
    },
    Text {
        content: String,
    },
    /// Adversarial review status — emitted during post-inference answer review.
    ReviewStatus {
        content: String,
    },
    /// Revised answer from the adversarial reviewer; replaces the text streamed so far.
    ReviewRevision {
        content: String,
        score: u8,
        rounds: u32,
    },
    /// The turn budget ran out before the task finished; lets a client offer a continuation.
    TurnLimitReached {
        max_turns: u32,
    },
    /// A delegation running under this turn changed state.
    /// `detail` holds only a tool name or GIAP's failure reason (never child prose or arguments)
    /// and must never be folded into the turn's text or persisted tool results.
    SubagentProgress {
        /// The `TaskRun` id; group frames by this, not role (a turn may delegate a role twice).
        task_id: String,
        /// The role that was delegated to, for the label on the tree.
        role: String,
        status: SubagentStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Done {
        session_id: String,
        model_role: String,
        /// Token usage for this response (estimated if real counts unavailable).
        usage: Option<crate::models::ports::provider::UsageStats>,
        /// Per-turn inference stats (TTFT, tok/s, context use), when the engine reports any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<super::turn_stats::TurnStats>,
    },
    Error {
        content: String,
    },
}

/// Where a delegation has got to, as a client sees it.
/// Unlike `TaskStatus` (the run lifecycle) it can say "calling a tool", the usual state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    /// Authorised, waiting for the device (e.g. behind a sibling delegation from the same turn).
    Queued,
    /// Holding the device and replying.
    Running,
    /// Calling a tool. `detail` is the tool's name and never its arguments.
    Tool,
    /// Finished; the answer arrives as the `delegate` tool result, never in a progress frame.
    Completed,
    /// Stopped, by the parent or by the parent's own turn ending.
    Cancelled,
    /// Ran out of turns. Whatever came back is a budget message, not an answer.
    TurnBudgetExhausted,
    /// Did not produce an answer. `detail` is GIAP's own reason.
    Failed,
}

impl SubagentStatus {
    /// Every variant, so a guard can iterate them rather than list them.
    pub const ALL: [SubagentStatus; 7] = [
        SubagentStatus::Queued,
        SubagentStatus::Running,
        SubagentStatus::Tool,
        SubagentStatus::Completed,
        SubagentStatus::Cancelled,
        SubagentStatus::TurnBudgetExhausted,
        SubagentStatus::Failed,
    ];

    /// The serde wire spelling, for a renderer with no serializer to hand.
    pub fn as_str(self) -> &'static str {
        match self {
            SubagentStatus::Queued => "queued",
            SubagentStatus::Running => "running",
            SubagentStatus::Tool => "tool",
            SubagentStatus::Completed => "completed",
            SubagentStatus::Cancelled => "cancelled",
            SubagentStatus::TurnBudgetExhausted => "turn_budget_exhausted",
            SubagentStatus::Failed => "failed",
        }
    }
}

impl From<crate::shared::domain::orchestration::TaskStatus> for SubagentStatus {
    fn from(status: crate::shared::domain::orchestration::TaskStatus) -> Self {
        use crate::shared::domain::orchestration::TaskStatus;
        match status {
            TaskStatus::Queued => SubagentStatus::Queued,
            TaskStatus::Running => SubagentStatus::Running,
            TaskStatus::Completed => SubagentStatus::Completed,
            TaskStatus::Cancelled => SubagentStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted => SubagentStatus::TurnBudgetExhausted,
            TaskStatus::Failed => SubagentStatus::Failed,
        }
    }
}

/// The four states of the workflow loop, serialized as the NDJSON contract's state strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    /// Idle – ready for the next interaction.
    Wait,
    /// Receiving user input.
    Listen,
    /// Processing the input through the agent.
    Thinking,
    /// Delivering the agent's response.
    Speak,
}

impl fmt::Display for WorkflowState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait => write!(f, "Wait"),
            Self::Listen => write!(f, "Listen"),
            Self::Thinking => write!(f, "Thinking"),
            Self::Speak => write!(f, "Speak"),
        }
    }
}

/// Events emitted during the workflow loop (for UI / logging / NDJSON hooks).
///
/// # NDJSON contract (terminal-voice-in-desktop, Architecture A)
///
/// The variants that carry an external contract payload serialize — via
/// `serde_json::to_string` — to EXACTLY the shapes the desktop shell parses off
/// the child's stdout (one JSON object per line, `snake_case`), tagged with an
/// `"event"` field:
///
/// ```text
/// {"event":"ready","session_id":"<uuid>"}
/// {"event":"state","state":"wait"}            // wait | listen | thinking | speak
/// {"event":"transcript","text":"..."}
/// {"event":"token","content":"..."}
/// {"event":"tool_call","tool":"giap__x","id":"..."}
/// {"event":"tool_result","tool":"giap__x","id":"...","content":"..."}
/// {"event":"turn_complete","session_id":"<uuid>"}
/// {"event":"error","message":"..."}
/// {"event":"exit","reason":"stdin_eof"}       // stdin_eof | dismissed | error
/// {"event":"audio_level","rms":0.42}          // wait/recording mic level, throttled
/// ```
///
/// `UserInput` and `AgentOutput` are legacy internal-only variants retained for
/// the tracing hook and existing call sites; they carry no external contract
/// shape and the NDJSON sink deliberately skips them (the streamed `Token`
/// deltas and `Transcript` already cover that information for the UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// Prefix warm-up progress: `warming`, then exactly one of `ready`/`skipped`/`failed`.
    /// Emitted before `Ready`, while the child is alive but cannot yet listen.
    Warmup { state: String },
    /// Emitted once after models are loaded, before entering the wait loop.
    Ready { session_id: String },
    /// A workflow state transition; serializes as `{"event":"state","state":"wait"}`.
    #[serde(rename = "state")]
    StateChanged { state: WorkflowState },
    /// A confirmed user utterance (post-ASR).
    Transcript { text: String },
    /// An assistant token delta.
    Token { content: String },
    /// An MCP tool invocation is starting.
    ToolCall { tool: String, id: String },
    /// An MCP tool returned. `content` is truncated to 2000 chars by the emitter.
    ToolResult {
        tool: String,
        id: String,
        content: String,
    },
    /// The turn finished and was persisted exactly once.
    TurnComplete { session_id: String },
    /// A recoverable error occurred during the turn.
    Error { message: String },
    /// The loop is exiting cleanly. `reason` is `stdin_eof` | `dismissed` | `error`.
    Exit { reason: String },
    /// Live mic input level during `wait`/`recording`, throttled by the emitter.
    AudioLevel { rms: f32 },
    /// Internal-only user input text; `to_ndjson` skips it.
    UserInput(String),
    /// Internal-only full agent output text; `to_ndjson` skips it.
    AgentOutput(String),
}

impl WorkflowEvent {
    /// One NDJSON line (no trailing newline) for the stdout contract; `None` for internal-only.
    pub fn to_ndjson(&self) -> Option<String> {
        match self {
            Self::UserInput(_) | Self::AgentOutput(_) => None,
            other => serde_json::to_string(other).ok(),
        }
    }
}

/// Minimum interval between emitted audio-level readings.
const AUDIO_LEVEL_MIN_INTERVAL_MS: u128 = 100;
/// Minimum RMS change required to emit; cuts idle-silence chatter once the level settles.
const AUDIO_LEVEL_MIN_DELTA: f32 = 0.02;

/// Throttles a per-poll RMS stream to a UI-friendly cadence; shared by whisper and piper.
pub struct ThrottledAudioLevelSink {
    inner: Box<dyn Fn(f32) + Send + Sync>,
    last_emit: Mutex<Option<std::time::Instant>>,
    last_value: Mutex<f32>,
}

impl ThrottledAudioLevelSink {
    pub fn new(inner: Box<dyn Fn(f32) + Send + Sync>) -> Self {
        Self {
            inner,
            last_emit: Mutex::new(None),
            last_value: Mutex::new(0.0),
        }
    }

    /// Feed one poll's RMS reading; emits only if the interval has passed and the value moved.
    pub fn maybe_emit(&self, rms: f32) {
        let now = std::time::Instant::now();
        let mut last_emit = self.last_emit.lock().unwrap();
        let mut last_value = self.last_value.lock().unwrap();

        let due = match *last_emit {
            None => true,
            Some(t) => now.duration_since(t).as_millis() >= AUDIO_LEVEL_MIN_INTERVAL_MS,
        };
        if !due || (rms - *last_value).abs() < AUDIO_LEVEL_MIN_DELTA {
            return;
        }
        *last_emit = Some(now);
        *last_value = rms;
        (self.inner)(rms);
    }
}

// ── Golden NDJSON serializer tests ──────────────────────────────────────────
// Pinned byte-for-byte: the desktop shell parses these lines off child stdout.
#[cfg(test)]
mod ndjson_golden_tests {
    use super::*;

    #[test]
    fn ready_line_matches_contract() {
        let ev = WorkflowEvent::Ready {
            session_id: "abc-123".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"ready","session_id":"abc-123"}"#
        );
    }

    #[test]
    fn state_lines_match_contract_snake_case() {
        let cases = [
            (WorkflowState::Wait, r#"{"event":"state","state":"wait"}"#),
            (
                WorkflowState::Listen,
                r#"{"event":"state","state":"listen"}"#,
            ),
            (
                WorkflowState::Thinking,
                r#"{"event":"state","state":"thinking"}"#,
            ),
            (WorkflowState::Speak, r#"{"event":"state","state":"speak"}"#),
        ];
        for (state, expected) in cases {
            let ev = WorkflowEvent::StateChanged { state };
            assert_eq!(ev.to_ndjson().unwrap(), expected, "state {:?}", state);
        }
    }

    #[test]
    fn transcript_line_matches_contract() {
        let ev = WorkflowEvent::Transcript {
            text: "what's the weather".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"transcript","text":"what's the weather"}"#
        );
    }

    #[test]
    fn token_line_matches_contract() {
        let ev = WorkflowEvent::Token {
            content: "Hello".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"token","content":"Hello"}"#
        );
    }

    #[test]
    fn tool_call_line_matches_contract() {
        let ev = WorkflowEvent::ToolCall {
            tool: "giap__get_weather".to_string(),
            id: "call-1".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"tool_call","tool":"giap__get_weather","id":"call-1"}"#
        );
    }

    #[test]
    fn tool_result_line_matches_contract() {
        let ev = WorkflowEvent::ToolResult {
            tool: "giap__get_weather".to_string(),
            id: "call-1".to_string(),
            content: "sunny".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"tool_result","tool":"giap__get_weather","id":"call-1","content":"sunny"}"#
        );
    }

    #[test]
    fn turn_complete_line_matches_contract() {
        let ev = WorkflowEvent::TurnComplete {
            session_id: "abc-123".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"turn_complete","session_id":"abc-123"}"#
        );
    }

    #[test]
    fn error_line_matches_contract() {
        let ev = WorkflowEvent::Error {
            message: "boom".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"error","message":"boom"}"#
        );
    }

    #[test]
    fn exit_line_matches_contract() {
        for reason in ["stdin_eof", "dismissed", "error"] {
            let ev = WorkflowEvent::Exit {
                reason: reason.to_string(),
            };
            assert_eq!(
                ev.to_ndjson().unwrap(),
                format!(r#"{{"event":"exit","reason":"{}"}}"#, reason)
            );
        }
    }

    #[test]
    fn audio_level_line_matches_contract() {
        let ev = WorkflowEvent::AudioLevel { rms: 0.42 };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"audio_level","rms":0.42}"#
        );
    }

    #[test]
    fn legacy_variants_are_not_serialized_to_ndjson() {
        assert!(WorkflowEvent::UserInput("hi".to_string())
            .to_ndjson()
            .is_none());
        assert!(WorkflowEvent::AgentOutput("out".to_string())
            .to_ndjson()
            .is_none());
    }

    #[test]
    fn ndjson_lines_contain_no_interior_newlines() {
        let ev = WorkflowEvent::Token {
            content: "a\nb".to_string(),
        };
        let line = ev.to_ndjson().unwrap();
        assert!(!line.contains('\n'), "line had a raw newline: {line:?}");
        assert!(line.contains("\\n"));
    }
}

#[cfg(test)]
mod agent_request_scope_tests {
    use super::*;

    fn request(scope: ProfileScope) -> AgentRequest {
        AgentRequest {
            message: "what did I say about the boiler".to_string(),
            session_id: "s1".to_string(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
            profile_scope: scope,
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        }
    }

    #[test]
    fn the_scope_survives_a_serde_round_trip() {
        for scope in [
            ProfileScope::Owner("jerry".into()),
            ProfileScope::Household,
            ProfileScope::Guest,
        ] {
            let json = serde_json::to_string(&request(scope.clone())).unwrap();
            let back: AgentRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(back.profile_scope, scope);
        }
    }

    /// It must land on household access, the behaviour from before the field existed.
    #[test]
    fn a_payload_from_before_the_field_existed_still_deserializes() {
        let legacy = r#"{"message":"hi","session_id":"s1","model_role":"chat"}"#;
        let parsed: AgentRequest = serde_json::from_str(legacy).expect("legacy payload must parse");
        assert_eq!(parsed.profile_scope, ProfileScope::Household);
    }

    /// `main.rs` prints `as_str` and `routes.rs` serializes, for different clients.
    #[test]
    fn the_wire_spelling_is_the_serialized_spelling() {
        assert_eq!(
            SubagentStatus::ALL.len(),
            7,
            "SubagentStatus::ALL no longer lists every variant, so every guard \
             that iterates it now skips one"
        );
        for status in SubagentStatus::ALL {
            let wire = serde_json::to_value(status).expect("status serializes");
            assert_eq!(
                wire.as_str(),
                Some(status.as_str()),
                "{status:?} serializes as {wire} but renders as `{}` -- a client \
                 branching on one of those two spellings can never be true",
                status.as_str()
            );
        }
    }

    #[test]
    fn a_progress_frame_carries_four_fields_and_no_transcript() {
        let event = AgentStreamEvent::SubagentProgress {
            task_id: "t1".into(),
            role: "researcher".into(),
            status: SubagentStatus::Tool,
            detail: Some("giap-weather__get_forecast".into()),
        };
        let json = serde_json::to_value(&event).expect("event serializes");
        let object = json.as_object().expect("an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["detail", "role", "status", "task_id", "type"],
            "the progress frame grew a field. Every field on it is visible to \
             the desktop and to any paired client, so a new one is a decision \
             about what a subagent may say about itself"
        );
        assert_eq!(object["type"], "subagent_progress");

        let quiet = AgentStreamEvent::SubagentProgress {
            task_id: "t1".into(),
            role: "researcher".into(),
            status: SubagentStatus::Queued,
            detail: None,
        };
        let json = serde_json::to_value(&quiet).expect("event serializes");
        assert!(
            json.get("detail").is_none(),
            "a frame with no detail must omit the key rather than send null"
        );
    }
}
