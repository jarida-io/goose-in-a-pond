use serde::{Deserialize, Serialize};

/// A user-defined skill injected into the agent's system prompt.
///
/// Skills are Markdown-formatted instruction blocks (e.g. "How to control
/// the lights", "Morning briefing format"). Each active skill is injected
/// as `extend_system_prompt("skill:{name}", content)` on every turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSkill {
    /// UUID primary key.
    pub id: String,
    /// Human-readable name, must be unique (e.g. "light_control", "morning_briefing").
    pub name: String,
    /// Markdown instruction content injected into the system prompt.
    pub content: String,
    /// Whether this skill is currently active.
    pub active: bool,
    /// ISO datetime when this skill was created.
    pub created_at: String,
}

impl UserSkill {
    /// Maximum allowed content length (bytes). Skills are injected into
    /// the system prompt on every turn — a single oversized skill can
    /// exhaust the model's context window.
    pub const MAX_CONTENT_LEN: usize = 5000;

    /// Validate the skill content length.
    pub fn validate(&self) -> Result<(), String> {
        if self.content.len() > Self::MAX_CONTENT_LEN {
            return Err(format!(
                "Skill content exceeds {} bytes (got {})",
                Self::MAX_CONTENT_LEN, self.content.len()
            ));
        }
        Ok(())
    }
}
