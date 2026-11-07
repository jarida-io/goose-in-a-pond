use serde::{Deserialize, Serialize};

/// The role of a participant in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
    /// A tool call's result; `role: "tool"` in OpenAI-compatible APIs.
    Tool,
}

/// An image attached to a chat message; models without vision silently ignore it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Base64-encoded image data.
    pub data: String,
    /// MIME type, e.g. "image/jpeg", "image/png", "image/webp".
    pub mime_type: String,
}

/// A recording for a model that hears it; the pond passes it to the engine and never stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioAttachment {
    /// Base64: WAV, M4A/AAC, MP3, FLAC or Ogg Vorbis as sent; 16 kHz mono WAV once prepared.
    pub data: String,
    /// MIME type, e.g. "audio/wav" or "audio/mp4".
    pub mime_type: String,
    /// The file's name, when it was a file rather than a held-mic note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A PDF whose text the pond reads into the turn; the file itself is never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentAttachment {
    /// Base64-encoded PDF.
    pub data: String,
    /// MIME type: "application/pdf".
    pub mime_type: String,
    /// The file's name, which the model and the saved row see.
    #[serde(default)]
    pub name: String,
}

/// A tool call in an assistant message, replayed via `tool_calls` so the model sees its history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    /// The raw JSON-stringified arguments the model produced.
    pub arguments: String,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    /// Set only on assistant messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRecord>,
    /// The originating `tool_calls[].id`; set only on `Role::Tool` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A tool result; `tool_call_id` must match a `ToolCallRecord` id on the preceding message.
    pub fn tool_result(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// `content` may be empty when the model emitted only tool calls.
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCallRecord>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn with_image(mut self, data: String, mime_type: String) -> Self {
        self.images.push(ImageAttachment { data, mime_type });
        self
    }

    /// Order is preserved: "the first picture" in a follow-up means ordinal 0.
    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}
