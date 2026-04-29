use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub message: String,
    pub session_id: String,
    pub model_role: String, // "chat" | "think" | "task"
    /// Optional image attachments for multimodal models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::domain::message::ImageAttachment>,
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
    /// Internal reasoning / chain-of-thought from models that support thinking
    /// (Gemma 4, Qwen3, DeepSeek-R1). Only emitted when `show_thinking` is enabled.
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
    /// Revised answer from the adversarial reviewer.
    /// The frontend should replace the previously streamed text with this content.
    ReviewRevision {
        content: String,
        score: u8,
        rounds: u32,
    },
    Done {
        session_id: String,
        model_role: String,
    },
    Error {
        content: String,
    },
}

/// The four states of the workflow loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Events emitted during state transitions (for UI / logging hooks).
#[derive(Debug, Clone)]
pub enum WorkflowEvent {
    StateChanged(WorkflowState),
    UserInput(String),
    AgentOutput(String),
    Exit,
}
