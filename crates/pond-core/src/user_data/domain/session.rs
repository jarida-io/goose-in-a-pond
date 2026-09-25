use crate::models::domain::message::ChatMessage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    pub message: ChatMessage,
    pub created_at: DateTime<Utc>,
    /// Real prompt tokens for the turn this message completed (assistant rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    /// Real completion-token count for this assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    /// Hidden reasoning-token count (the text is not stored); `None` = not counted, not zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Training feedback from the like/dislike UI; `Some(false)` excludes the row from training.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liked: Option<bool>,
}

impl SessionMessage {
    pub fn new(id: String, session_id: String, message: ChatMessage) -> Self {
        Self {
            id,
            session_id,
            message,
            created_at: Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        }
    }

    /// Attach the turn's real token counts (assistant rows).
    pub fn with_token_counts(mut self, prompt: Option<u32>, completion: Option<u32>) -> Self {
        self.prompt_tokens = prompt;
        self.completion_tokens = completion;
        self
    }

    /// Attach the GIAP-derived (not provider-reported) reasoning-token count.
    pub fn with_reasoning_tokens(mut self, reasoning: Option<u32>) -> Self {
        self.reasoning_tokens = reasoning;
        self
    }
}

/// Image attachment metadata; the bytes live outside `pond_system.db`, which every turn reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAttachment {
    pub id: String,
    pub message_id: String,
    pub session_id: String,
    /// Position within its message, 0-based. Preserves "the first picture".
    pub ordinal: u32,
    pub mime_type: String,
    /// Decoded size on disk. Lets a client render a size without a fetch.
    pub byte_size: u64,
    pub created_at: DateTime<Utc>,
}

/// Evidence behind an attribution (variants strongest first); the id alone authorises nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationSource {
    /// A bearer token from a device paired to this member. Cryptographic.
    PairedDevice,
    /// The member said so, or picked themselves in the UI. Deliberate.
    Explicit,
    /// Above-threshold face match, anti-spoof passed; the only source with a confidence.
    Face,
    /// Nobody has been identified. The default, and never an error.
    Unknown,
}

impl IdentificationSource {
    /// Stored form; must match the values in migration `0037_session_identification.sql`.
    pub fn as_str(&self) -> &'static str {
        match self {
            IdentificationSource::PairedDevice => "paired_device",
            IdentificationSource::Explicit => "explicit",
            IdentificationSource::Face => "face",
            IdentificationSource::Unknown => "unknown",
        }
    }

    /// An unrecognised value is [`Unknown`](Self::Unknown), the weakest claim, not an error.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "paired_device" => IdentificationSource::PairedDevice,
            "explicit" => IdentificationSource::Explicit,
            "face" => IdentificationSource::Face,
            _ => IdentificationSource::Unknown,
        }
    }

    /// Stored form and rank of every source, for adapters that compare ranks in SQL.
    pub const ALL_RANKED: &'static [(&'static str, u8)] = &[
        ("paired_device", 0),
        ("explicit", 1),
        ("face", 2),
        ("unknown", 3),
    ];

    /// Strength, lower is stronger. Only meaningful in comparison.
    pub fn rank(&self) -> u8 {
        match self {
            IdentificationSource::PairedDevice => 0,
            IdentificationSource::Explicit => 1,
            IdentificationSource::Face => 2,
            IdentificationSource::Unknown => 3,
        }
    }
}

/// A session's attribution: who, and on what evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionIdentity {
    /// `None` means unattributed. It does NOT mean "the primary member".
    pub profile_id: Option<String>,
    pub source: IdentificationSource,
    /// Set only for [`Face`](IdentificationSource::Face).
    pub confidence: Option<f32>,
}

impl SessionIdentity {
    /// The unattributed identity. What every existing session reads as.
    pub fn unknown() -> Self {
        Self {
            profile_id: None,
            source: IdentificationSource::Unknown,
            confidence: None,
        }
    }

    /// Whether this replaces `existing`: a weaker source never overrides a stronger one, but equal
    /// strength does, so a fresh face match can re-identify.
    pub fn supersedes(&self, existing: &SessionIdentity) -> bool {
        self.source.rank() <= existing.source.rank()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    /// `None` = unattributed; see [`SessionIdentity`] for how far to trust a `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub total_prompt_tokens: u32,
    #[serde(default)]
    pub total_completion_tokens: u32,
    /// The model most recently used in this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    pub fn new(id: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            title: None,
            profile_id: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            model_name: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod session_identity_tests {
    use super::*;

    #[test]
    fn every_source_round_trips_through_its_stored_form() {
        for source in [
            IdentificationSource::PairedDevice,
            IdentificationSource::Explicit,
            IdentificationSource::Face,
            IdentificationSource::Unknown,
        ] {
            assert_eq!(IdentificationSource::parse(source.as_str()), source);
        }
    }

    #[test]
    fn an_unrecognised_stored_source_degrades_to_unknown() {
        assert_eq!(
            IdentificationSource::parse("voice_print"),
            IdentificationSource::Unknown
        );
        assert_eq!(
            IdentificationSource::parse(""),
            IdentificationSource::Unknown
        );
        assert_eq!(
            IdentificationSource::parse("PAIRED_DEVICE"),
            IdentificationSource::Unknown
        );
    }

    #[test]
    fn sources_rank_strongest_first() {
        assert!(
            IdentificationSource::PairedDevice.rank() < IdentificationSource::Explicit.rank(),
            "a signed token outranks someone typing a name"
        );
        assert!(
            IdentificationSource::Explicit.rank() < IdentificationSource::Face.rank(),
            "a deliberate choice outranks a probabilistic match"
        );
        assert!(
            IdentificationSource::Face.rank() < IdentificationSource::Unknown.rank(),
            "any evidence outranks none"
        );
    }

    fn identity(source: IdentificationSource, who: &str) -> SessionIdentity {
        SessionIdentity {
            profile_id: Some(who.to_string()),
            source,
            confidence: None,
        }
    }

    #[test]
    fn a_face_match_cannot_take_over_a_paired_device_session() {
        let phone = identity(IdentificationSource::PairedDevice, "jerry");
        let passerby = identity(IdentificationSource::Face, "liz");
        assert!(!passerby.supersedes(&phone));
        assert!(phone.supersedes(&passerby));
    }

    #[test]
    fn anything_supersedes_an_unattributed_session() {
        let nobody = SessionIdentity::unknown();
        assert_eq!(nobody.profile_id, None);
        for source in [
            IdentificationSource::PairedDevice,
            IdentificationSource::Explicit,
            IdentificationSource::Face,
        ] {
            assert!(identity(source, "jerry").supersedes(&nobody));
        }
    }

    #[test]
    fn equal_strength_supersedes_so_re_identification_works() {
        let first = identity(IdentificationSource::Face, "jerry");
        let corrected = identity(IdentificationSource::Face, "liz");
        assert!(corrected.supersedes(&first));
    }

    #[test]
    fn a_new_session_is_unattributed() {
        assert_eq!(Session::new("s1".to_string()).profile_id, None);
    }
}

#[cfg(test)]
mod ranked_table_tests {
    use super::*;

    /// SQL uses `ALL_RANKED` and memory uses `rank()`; drift would cause silent downgrades.
    #[test]
    fn the_ranked_table_agrees_with_rank_and_covers_every_source() {
        for source in [
            IdentificationSource::PairedDevice,
            IdentificationSource::Explicit,
            IdentificationSource::Face,
            IdentificationSource::Unknown,
        ] {
            let entry = IdentificationSource::ALL_RANKED
                .iter()
                .find(|(name, _)| *name == source.as_str())
                .unwrap_or_else(|| panic!("ALL_RANKED is missing {}", source.as_str()));
            assert_eq!(
                entry.1,
                source.rank(),
                "ALL_RANKED and rank() disagree about {}",
                source.as_str()
            );
        }
        assert_eq!(
            IdentificationSource::ALL_RANKED.len(),
            4,
            "a new source was added without a rank for the SQL comparison"
        );
    }
}
