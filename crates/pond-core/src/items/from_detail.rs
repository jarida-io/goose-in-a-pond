//! The header each detail table's row gets. A NULL owner means something different in each
//! table, so each has its own mapping: shared, unknown, household, or impossible.

use crate::context::domain::{ItemKind as ContextItemKind, SourceKind};
use crate::security::domain::event::PrivacySensitivity;
use crate::security::domain::redaction::RedactionLevel;
use crate::security::ports::redactor::Redactor;

use super::{source, timestamp, Kind, Modality, ProducedBy, Scope, State};

/// Header text is redacted at the level memory writes and ingest already use, so text that
/// arrived redacted comes out unchanged (redaction is idempotent).
pub const HEADER_REDACTION_LEVEL: RedactionLevel = RedactionLevel::Secrets;

/// A header's fields as its detail row determines them; the store adds id and timestamps.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderFields {
    pub kind: Kind,
    pub source_id: String,
    pub external_id: String,
    pub scope: Scope,
    pub sensitivity: PrivacySensitivity,
    pub modality: Modality,
    /// `None` when the row's date is unreadable: the store keeps the header's, or uses now.
    pub observed_at: Option<String>,
    pub valid_until: Option<String>,
    pub title: String,
    pub body: String,
    pub context: String,
    pub state: State,
    pub produced_by: ProducedBy,
    pub confidence: Option<f32>,
}

fn redact(redactor: &dyn Redactor, text: Option<&str>) -> String {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => redactor.redact(text, HEADER_REDACTION_LEVEL).text,
        None => String::new(),
    }
}

pub struct MemoryRow<'a> {
    pub id: &'a str,
    pub profile_id: Option<&'a str>,
    pub segment: Option<&'a str>,
    pub lifecycle: Option<&'a str>,
    pub content: &'a str,
    pub source: &'a str,
    pub created_at: &'a str,
}

/// A memory's segment decides fact or trait: a preference or a routine is a standing trait
/// of a member, everything else a fact. Nothing is an entity until an agent writes one.
pub fn memory_kind(segment: Option<&str>) -> Kind {
    match segment {
        Some("preference" | "routine") => Kind::Trait,
        _ => Kind::Fact,
    }
}

/// `memory_fragments.profile_id` NULL means shared with the household.
pub fn memory_scope(profile_id: Option<&str>) -> Scope {
    match profile_id.map(str::trim).filter(|p| !p.is_empty()) {
        Some(owner) => Scope::private(owner).expect("a non-blank owner"),
        None => Scope::household(None),
    }
}

/// Recall reads only `active` (or NULL) rows, so anything else stays out of recall here too.
pub fn memory_state(lifecycle: Option<&str>) -> State {
    match lifecycle {
        None | Some("active") => State::Active,
        Some("merged") => State::Superseded,
        Some(_) => State::Archived,
    }
}

pub fn memory_header(row: &MemoryRow<'_>, redactor: &dyn Redactor) -> HeaderFields {
    let writer = match row.source.trim() {
        "" => "memory",
        source => source,
    };
    HeaderFields {
        kind: memory_kind(row.segment),
        source_id: source::MEMORY.to_string(),
        external_id: row.id.to_string(),
        scope: memory_scope(row.profile_id),
        sensitivity: PrivacySensitivity::Sensitive,
        modality: Modality::Text,
        observed_at: timestamp::normalise(row.created_at),
        valid_until: None,
        title: String::new(),
        body: redact(redactor, Some(row.content)),
        context: String::new(),
        state: memory_state(row.lifecycle),
        produced_by: ProducedBy::pond(writer),
        confidence: None,
    }
}

pub struct SessionRow<'a> {
    pub id: &'a str,
    pub profile_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub rolling_summary: Option<&'a str>,
    pub created_at: &'a str,
}

/// `sessions.profile_id` NULL means nobody knows who was speaking. No audience is honest for
/// that, so the conversation gets no header until it is attributed; recall never surfaced it.
pub fn conversation_scope(profile_id: Option<&str>) -> Option<Scope> {
    Scope::private(profile_id?).ok()
}

/// The conversation's body is its redacted rolling summary; the raw one stays on the session,
/// which is what a conversation replays.
pub fn conversation_header(row: &SessionRow<'_>, redactor: &dyn Redactor) -> Option<HeaderFields> {
    Some(HeaderFields {
        kind: Kind::Conversation,
        source_id: source::SESSION.to_string(),
        external_id: row.id.to_string(),
        scope: conversation_scope(row.profile_id)?,
        sensitivity: PrivacySensitivity::Sensitive,
        modality: Modality::Text,
        observed_at: timestamp::normalise(row.created_at),
        valid_until: None,
        title: redact(redactor, row.title),
        body: redact(redactor, row.rolling_summary),
        context: String::new(),
        state: State::Active,
        produced_by: ProducedBy::pond("chat"),
        confidence: None,
    })
}

pub struct ContextRow<'a> {
    pub source_id: &'a str,
    pub external_id: &'a str,
    pub profile_id: &'a str,
    pub source_kind: &'a str,
    pub item_kind: &'a str,
    pub occurred_at: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub sensitivity: PrivacySensitivity,
}

/// `None` for a source no ingest path accepts yet; a test fails when one lands without a kind.
pub fn context_kind(source: SourceKind, item: ContextItemKind) -> Option<Kind> {
    match source {
        SourceKind::Mail => Some(Kind::Mail),
        SourceKind::Calendar => Some(Kind::Event),
        SourceKind::Sensor | SourceKind::Camera => Some(Kind::HomeEvent),
        SourceKind::Mobile => match item {
            ContextItemKind::Message => Some(Kind::Notification),
            ContextItemKind::Event => Some(Kind::Event),
            _ => None,
        },
        SourceKind::Voice | SourceKind::Files | SourceKind::Chat => None,
    }
}

/// `context_items.profile_id` is NOT NULL: an item is private to its source's owner.
pub fn context_header(row: &ContextRow<'_>, redactor: &dyn Redactor) -> Option<HeaderFields> {
    let kind = context_kind(
        SourceKind::parse(row.source_kind)?,
        ContextItemKind::parse(row.item_kind)?,
    )?;
    Some(HeaderFields {
        kind,
        source_id: row.source_id.to_string(),
        external_id: row.external_id.to_string(),
        scope: Scope::private(row.profile_id).ok()?,
        sensitivity: row.sensitivity,
        modality: Modality::Text,
        observed_at: timestamp::normalise(row.occurred_at),
        valid_until: None,
        title: redact(redactor, Some(row.title)),
        body: redact(redactor, Some(row.body)),
        context: String::new(),
        state: State::Active,
        produced_by: ProducedBy::Ingest,
        confidence: None,
    })
}

pub struct SuggestionRow<'a> {
    pub id: &'a str,
    pub profile_id: Option<&'a str>,
    pub prompt: &'a str,
    pub reason: &'a str,
    pub state: &'a str,
    pub created_at: &'a str,
}

/// `suggestion_queue.profile_id` NULL means the household: its memory was unattributed.
pub fn suggestion_scope(profile_id: Option<&str>) -> Scope {
    match profile_id.map(str::trim).filter(|p| !p.is_empty()) {
        Some(owner) => Scope::private(owner).expect("a non-blank owner"),
        None => Scope::household(None),
    }
}

pub fn suggestion_state(state: &str) -> State {
    match state {
        "taken" => State::Accepted,
        "dismissed" => State::Dismissed,
        _ => State::Active,
    }
}

/// The question is the title and the pond's reason under it the body.
pub fn suggestion_header(row: &SuggestionRow<'_>, redactor: &dyn Redactor) -> HeaderFields {
    HeaderFields {
        kind: Kind::Suggestion,
        source_id: source::SUGGESTION.to_string(),
        external_id: row.id.to_string(),
        scope: suggestion_scope(row.profile_id),
        sensitivity: PrivacySensitivity::Sensitive,
        modality: Modality::Text,
        observed_at: timestamp::normalise(row.created_at),
        valid_until: None,
        title: redact(redactor, Some(row.prompt)),
        body: redact(redactor, Some(row.reason)),
        context: String::new(),
        state: suggestion_state(row.state),
        produced_by: ProducedBy::pond("suggestion_generation"),
        confidence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::SourceAvailability;
    use crate::items::Audience;
    use crate::security::domain::redaction::Redacted;

    /// Stands in for the rule redactor: masks the word "secret".
    struct MaskSecret;
    impl Redactor for MaskSecret {
        fn redact(&self, text: &str, _level: RedactionLevel) -> Redacted {
            Redacted {
                text: text.replace("secret", "[KEY]"),
                findings: vec![],
            }
        }
    }

    fn memory(
        profile_id: Option<&'static str>,
        lifecycle: Option<&'static str>,
    ) -> MemoryRow<'static> {
        MemoryRow {
            id: "m1",
            profile_id,
            segment: Some("preference"),
            lifecycle,
            content: "likes the secret recipe",
            source: "mcp_tool",
            created_at: "2026-11-05 09:30:15",
        }
    }

    #[test]
    fn an_unattributed_memory_is_the_households_and_an_attributed_one_its_owners() {
        let shared = memory_header(&memory(None, None), &MaskSecret);
        assert_eq!(shared.scope.audience(), Audience::Household);
        assert_eq!(shared.scope.owner(), None);
        let mine = memory_header(&memory(Some("amina"), Some("active")), &MaskSecret);
        assert_eq!(mine.scope.audience(), Audience::Private);
        assert_eq!(mine.scope.owner(), Some("amina"));
        assert_eq!(mine.kind, Kind::Trait);
        assert_eq!(mine.body, "likes the [KEY] recipe");
        assert_eq!(mine.produced_by, ProducedBy::pond("mcp_tool"));
        assert_eq!(
            mine.observed_at.as_deref(),
            Some("2026-11-05T09:30:15.000Z")
        );
        assert_eq!(
            (mine.source_id.as_str(), mine.external_id.as_str()),
            (source::MEMORY, "m1")
        );
    }

    #[test]
    fn only_an_active_memory_is_an_active_item() {
        assert_eq!(memory_state(None), State::Active);
        assert_eq!(memory_state(Some("active")), State::Active);
        assert_eq!(memory_state(Some("archived")), State::Archived);
        assert_eq!(memory_state(Some("merged")), State::Superseded);
        assert_eq!(memory_state(Some("something new")), State::Archived);
    }

    #[test]
    fn a_conversation_with_no_known_speaker_gets_no_header() {
        let row = SessionRow {
            id: "s1",
            profile_id: None,
            title: Some("Tea"),
            rolling_summary: Some("They talked about tea."),
            created_at: "2026-11-05 09:30:15",
        };
        assert!(conversation_header(&row, &MaskSecret).is_none());
        let owned = SessionRow {
            profile_id: Some("amina"),
            rolling_summary: Some("Shared the secret code."),
            ..row
        };
        let header = conversation_header(&owned, &MaskSecret).unwrap();
        assert_eq!(header.scope.owner(), Some("amina"));
        assert_eq!(header.body, "Shared the [KEY] code.");
        assert_eq!(header.title, "Tea");
    }

    #[test]
    fn a_suggestion_follows_its_queue_row() {
        let row = SuggestionRow {
            id: "q1",
            profile_id: None,
            prompt: "Plan the secret party?",
            reason: "You mentioned a party.",
            state: "taken",
            created_at: "2026-11-05 09:30:15",
        };
        let header = suggestion_header(&row, &MaskSecret);
        assert_eq!(header.scope.audience(), Audience::Household);
        assert_eq!(header.state, State::Accepted);
        assert_eq!(header.title, "Plan the [KEY] party?");
        assert_eq!(suggestion_state("dismissed"), State::Dismissed);
        assert_eq!(suggestion_state("queued"), State::Active);
    }

    #[test]
    fn every_source_ingest_accepts_maps_to_a_kind_in_context_items() {
        for source in SourceKind::ALL {
            if source.availability() != SourceAvailability::Landed || source == SourceKind::Voice {
                continue;
            }
            let kinds: Vec<Kind> = ContextItemKind::ALL
                .into_iter()
                .filter_map(|item| context_kind(source, item))
                .collect();
            assert!(
                !kinds.is_empty(),
                "{} items would get no header",
                source.as_str()
            );
            for kind in kinds {
                assert_eq!(kind.detail_table(), "context_items", "{kind}");
            }
        }
    }

    #[test]
    fn a_context_item_is_private_to_its_source_owner() {
        let row = ContextRow {
            source_id: "src",
            external_id: "42",
            profile_id: "amina",
            source_kind: "mail",
            item_kind: "message",
            occurred_at: "2026-11-05T09:30:15+00:00",
            title: "Hi",
            body: "Body",
            sensitivity: PrivacySensitivity::Sensitive,
        };
        let header = context_header(&row, &MaskSecret).unwrap();
        assert_eq!(header.kind, Kind::Mail);
        assert_eq!(header.scope.owner(), Some("amina"));
        assert_eq!(header.scope.audience(), Audience::Private);
        assert_eq!(header.produced_by, ProducedBy::Ingest);
        assert!(context_header(
            &ContextRow {
                source_kind: "files",
                ..row
            },
            &MaskSecret
        )
        .is_none());
    }
}
