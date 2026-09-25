//! Drafts: destructive actions the LLM stages for the user to approve or reject.

use crate::user_data::domain::session::IdentificationSource;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A staged action awaiting user confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    /// The ENGINE session id from MCP `_meta` `agent-session-id`, never a model-supplied value.
    pub session_id: String,
    /// Action type tag (e.g. "shell_command", "file_write", "schedule_create").
    pub kind: String,
    /// Human-readable one-line description of what the action will do.
    pub summary: String,
    /// JSON string with all parameters needed to execute the action.
    pub payload: String,
    /// The member whose turn staged this; `None` means unresolved (not "shared", as on memories).
    /// An unowned draft narrows: see `is_draft_decision_permitted`.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// How that owner was established. Never dropped.
    #[serde(default)]
    pub identification_source: Option<IdentificationSource>,
    pub status: DraftStatus,
    pub created_at: DateTime<Utc>,
    /// When this stops being approvable; `None` (every `save_draft` row) means never.
    /// On `Draft`, not just `Proposal`, so `giap-draft`'s single decision path honours it.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Draft {
    /// Dead from the instant it expires, as in `Proposal::is_live_at` and the repositories' SQL.
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expiry| now < expiry)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    /// Awaiting user review.
    Pending,
    /// User approved — ready for execution.
    Approved,
    /// User rejected — will not be executed.
    Rejected,
    /// Timed out without user action.
    Expired,
}

impl std::fmt::Display for DraftStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Approved => write!(f, "approved"),
            Self::Rejected => write!(f, "rejected"),
            Self::Expired => write!(f, "expired"),
        }
    }
}

impl std::str::FromStr for DraftStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            other => Err(format!("unknown draft status: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_status_roundtrips() {
        for status in [
            DraftStatus::Pending,
            DraftStatus::Approved,
            DraftStatus::Rejected,
            DraftStatus::Expired,
        ] {
            let s = status.to_string();
            let parsed: DraftStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn draft_serializes_to_json() {
        let draft = Draft {
            id: "draft_abc123".to_string(),
            session_id: "sess_1".to_string(),
            profile_id: Some("liz".to_string()),
            identification_source: Some(IdentificationSource::Explicit),
            kind: "shell_command".to_string(),
            summary: "List files in /tmp".to_string(),
            payload: r#"{"command":"ls /tmp"}"#.to_string(),
            status: DraftStatus::Pending,
            created_at: chrono::Utc::now(),
            expires_at: None,
        };
        let json = serde_json::to_string(&draft).unwrap();
        assert!(json.contains("shell_command"));
        assert!(json.contains("pending"));

        let back: Draft = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "draft_abc123");
        assert_eq!(back.status, DraftStatus::Pending);
        assert_eq!(back.expires_at, None);
    }

    fn expiring(expires_at: Option<chrono::DateTime<chrono::Utc>>) -> Draft {
        Draft {
            id: "d1".to_string(),
            session_id: "sess_1".to_string(),
            profile_id: None,
            identification_source: None,
            kind: "proposal".to_string(),
            summary: "remind me about the parcel".to_string(),
            payload: "{}".to_string(),
            status: DraftStatus::Pending,
            created_at: chrono::Utc::now(),
            expires_at,
        }
    }

    #[test]
    fn a_draft_without_an_expiry_is_always_live() {
        let d = expiring(None);
        assert!(d.is_live_at(chrono::Utc::now()));
        assert!(d.is_live_at(chrono::DateTime::from_timestamp(4_000_000_000, 0).unwrap()));
    }

    #[test]
    fn a_draft_is_dead_at_its_expiry_not_after_it() {
        let expiry = chrono::DateTime::from_timestamp(1_785_000_000, 0).unwrap();
        let d = expiring(Some(expiry));
        assert!(d.is_live_at(expiry - chrono::Duration::seconds(1)));
        assert!(
            !d.is_live_at(expiry),
            "exclusive at the boundary, matching the SQL filter"
        );
        assert!(!d.is_live_at(expiry + chrono::Duration::seconds(1)));
    }
}
