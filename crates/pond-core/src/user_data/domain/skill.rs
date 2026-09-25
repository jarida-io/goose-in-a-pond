use serde::{Deserialize, Serialize};

/// A user skill; only `description` is always in the prompt, `content` loads via `load_skill`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSkill {
    /// UUID primary key.
    pub id: String,
    /// Unique and free-form (not a slug): `load_skill` looks it up by exact string.
    pub name: String,
    /// What the skill is for and when it applies; always visible to the model, so keep it small.
    pub description: String,
    /// Frontend icon key ("bell"); unvalidated, since the client falls back to a default.
    pub icon: String,
    /// Markdown instruction content, loaded into context on demand.
    pub content: String,
    pub active: bool,
    /// ISO datetime when this skill was created.
    pub created_at: String,
}

/// Maximum allowed name length (bytes).
pub const MAX_NAME_LEN: usize = 100;

/// Names must be non-blank, ≤ `MAX_NAME_LEN` bytes and single-line (shown inline to the model).
pub fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Skill name must not be empty".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "Invalid skill name \"{}\". Names must be at most {} characters.",
            name, MAX_NAME_LEN
        ));
    }
    if name.contains('\n') || name.contains('\r') {
        return Err(format!(
            "Invalid skill name \"{}\". Names must be a single line.",
            name
        ));
    }
    Ok(())
}

impl UserSkill {
    /// Max content bytes, so one loaded skill can't blow the turn's context budget.
    pub const MAX_CONTENT_LEN: usize = 5000;

    /// Max description bytes; small because every active skill's description is in every prompt.
    pub const MAX_DESCRIPTION_LEN: usize = 280;

    pub fn validate(&self) -> Result<(), String> {
        validate_skill_name(&self.name)?;
        if self.description.len() > Self::MAX_DESCRIPTION_LEN {
            return Err(format!(
                "Skill description exceeds {} bytes (got {})",
                Self::MAX_DESCRIPTION_LEN,
                self.description.len()
            ));
        }
        if self.content.len() > Self::MAX_CONTENT_LEN {
            return Err(format!(
                "Skill content exceeds {} bytes (got {})",
                Self::MAX_CONTENT_LEN,
                self.content.len()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_readable_title_is_a_valid_name() {
        assert!(validate_skill_name("Task Reminder").is_ok());
        assert!(validate_skill_name("Morning Briefing Assistant").is_ok());
        // Hyphenated slugs remain valid too — nothing forces either style.
        assert!(validate_skill_name("task-reminder").is_ok());
    }

    #[test]
    fn an_empty_or_whitespace_only_name_is_rejected() {
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("   ").is_err());
    }

    #[test]
    fn a_multiline_name_is_rejected() {
        assert!(validate_skill_name("Task\nReminder").is_err());
    }

    #[test]
    fn an_overlong_name_is_rejected() {
        let name = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_skill_name(&name).is_err());
    }
}
