use crate::models::domain::message::ChatMessage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single message within a session, including metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    pub message: ChatMessage,
    pub created_at: DateTime<Utc>,
    /// Real prompt-token count for the turn this message completed
    /// (assistant rows only; None for user/tool rows and old rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    /// Real completion-token count for this assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    /// Tokens the turn spent on reasoning the user never saw (assistant rows).
    ///
    /// The COUNT only. The reasoning text is not persisted here and is not
    /// replayed into context — PAI-5 P6 owns that decision and it has not
    /// landed. `None` means nobody counted (every row written before this
    /// column existed, and every row written by a path that does not carry
    /// reasoning through); it is not the same as `Some(0)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Training-feedback signal from the chat UI's like/dislike controls.
    /// `None` = no vote, `Some(true)` = liked (keep as training data),
    /// `Some(false)` = disliked (excluded from training data).
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

    /// Attach the turn's GIAP-derived reasoning-token count (assistant rows).
    ///
    /// Separate from `with_token_counts` on purpose: those two come from the
    /// provider and this one does not, and a single setter would invite a
    /// caller to pass all three from the same source.
    pub fn with_reasoning_tokens(mut self, reasoning: Option<u32>) -> Self {
        self.reasoning_tokens = reasoning;
        self
    }
}

/// Metadata for one persisted image attachment (phase F2).
///
/// The bytes themselves live outside the database — `pond_system.db` is read on
/// every turn and every session listing, and a megabyte-per-row BLOB would bloat
/// its page cache for data that is only ever fetched whole and rarely. The
/// storage adapter owns the file layout; nothing above it should parse
/// `file_path`, which exists for operators debugging a session by hand.
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

/// How a session came to be attributed to a household member.
///
/// The profile id alone is not enough to authorise anything. "This is Liz
/// because her paired phone signed the request" and "this is Liz because a
/// camera frame matched her face at 0.62" are different claims, and a policy
/// that cannot distinguish them will either refuse the phone or trust the
/// camera. Recording the source is what keeps that decision available later.
///
/// The order of the variants is the strength order, strongest first. That is
/// load-bearing -- see [`rank`](Self::rank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentificationSource {
    /// A bearer token from a device paired to this member. Cryptographic.
    PairedDevice,
    /// The member said so, or picked themselves in the UI. Deliberate.
    Explicit,
    /// A face match above the per-profile threshold, anti-spoof passed.
    /// Probabilistic, and the only source that carries a confidence.
    Face,
    /// Nobody has been identified. The default, and never an error.
    Unknown,
}

impl IdentificationSource {
    /// The stored representation. Matches the values named in migration
    /// `0037_session_identification.sql`.
    pub fn as_str(&self) -> &'static str {
        match self {
            IdentificationSource::PairedDevice => "paired_device",
            IdentificationSource::Explicit => "explicit",
            IdentificationSource::Face => "face",
            IdentificationSource::Unknown => "unknown",
        }
    }

    /// Read back a stored value.
    ///
    /// An unrecognised string is [`Unknown`](Self::Unknown), not an error. A
    /// row written by a newer version, or corrupted, must degrade to the
    /// weakest claim rather than fail a session read -- but it must never
    /// degrade to a *strong* one, which is why there is no fallible variant to
    /// get this wrong in the other direction.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "paired_device" => IdentificationSource::PairedDevice,
            "explicit" => IdentificationSource::Explicit,
            "face" => IdentificationSource::Face,
            _ => IdentificationSource::Unknown,
        }
    }

    /// Every source paired with its rank, strongest first.
    ///
    /// Exists so an adapter can push the comparison into a query without
    /// re-deciding the ordering. The ranking is policy and stays here; the
    /// adapter transports numbers.
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

    /// Whether this identification should replace `existing`.
    ///
    /// The case this exists for: a household member's paired phone opens a
    /// session, then the camera in the room sees whoever walked past. Without
    /// this check the face match silently overwrites a cryptographic binding
    /// with a probabilistic one, and every later decision is made on the weaker
    /// evidence. A weaker source may not take over a session it did not bind.
    ///
    /// Equal strength does supersede -- a fresh face match replacing an older
    /// one is a re-identification, which is the whole point of the endpoint.
    pub fn supersedes(&self, existing: &SessionIdentity) -> bool {
        self.source.rank() <= existing.source.rank()
    }
}

/// How far batch memory extraction has read into one conversation.
///
/// Three columns rather than one because the three questions are different:
/// *where did the walk stop*, *when did it last look*, and *how many times has
/// it failed to make sense of what came back since it last moved*. Collapsing
/// any pair of them loses a decision the engine has to make -- ordering the
/// backlog needs the stamp, giving up needs the count, and resuming needs the
/// id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionCursor {
    /// The newest message the walk has covered. `None` means this conversation
    /// has never been examined, which is a different state from "examined and
    /// found nothing" -- the latter has a stamp.
    pub through_message_id: Option<String>,
    /// When the walk last looked at this conversation, successfully or not.
    pub extracted_at: Option<DateTime<Utc>>,
    /// Consecutive unparseable replies against the CURRENT watermark. Reset to
    /// zero every time the watermark moves.
    pub attempts: u32,
}

impl ExtractionCursor {
    /// A conversation nobody has read yet. What every existing session reads as.
    pub fn unstarted() -> Self {
        Self {
            through_message_id: None,
            extracted_at: None,
            attempts: 0,
        }
    }
}

/// Represents a conversation session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    /// The household member this session is attributed to, if any.
    ///
    /// The column has existed since migration `0003_profiles.sql`; this field
    /// is what finally reads it. `None` is the overwhelmingly common value and
    /// means unattributed -- read [`SessionIdentity`] for the evidence behind a
    /// `Some`, because the id on its own does not say how much to trust it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// Cumulative prompt tokens across all messages in this session.
    #[serde(default)]
    pub total_prompt_tokens: u32,
    /// Cumulative completion tokens across all messages in this session.
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

    /// A row written by a newer version, or one corrupted in place, must not
    /// be readable as a strong claim. Degrading to Unknown is the only safe
    /// direction, so this pins it.
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

    /// The scenario this method exists for: a paired phone binds the session,
    /// then the room camera sees somebody walk past. If the face match wins,
    /// every later authorisation decision is made on the weaker evidence --
    /// and, here, about the wrong person.
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

    /// Re-identification is the endpoint's normal case -- a second face match
    /// in the same session must be allowed to correct the first.
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

    /// `ALL_RANKED` is transported into SQL, so it has to agree with `rank()`
    /// exactly. If they drift, a conditional write enforces one ordering while
    /// every in-memory check enforces another -- and the disagreement would
    /// only ever surface as an occasional, unreproducible downgrade.
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
