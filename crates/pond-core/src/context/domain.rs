//! Personal context streaming: the domain.
//! [`ContextItem::from_parts`] is the only constructor and redacts via a [`Redactor`].

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::security::domain::event::{EventCategory, PrivacySensitivity};
use crate::security::domain::redaction::{RedactionKind, RedactionLevel};
use crate::security::ports::redactor::Redactor;

/// `Secrets`, not `Full`: items are read back to the model, which must still see names.
/// Must equal `RedactingMemoryRepository::LEVEL`, or one chokepoint's redaction buys nothing.
pub const INGEST_REDACTION_LEVEL: RedactionLevel = RedactionLevel::Secrets;

// ── Source kind ─────────────────────────────────────────────────────────────

/// Where a context source's data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// A sensor already registered with this pond.
    Sensor,
    /// A camera already attached to this pond.
    Camera,
    /// A voice transcript this pond produced.
    Voice,
    /// The mobile companion (GOTG) pushing calendar, location or contacts.
    Mobile,
    /// An e-mail account.
    Mail,
    /// A calendar account.
    Calendar,
    /// A file or document store.
    Files,
    /// A chat account (Slack, Telegram, a user-run bridge).
    Chat,
}

/// Whether ingest accepts items from a kind, and if not, what has to land first.
/// [`IngestPipeline`](crate::context::ingest::IngestPipeline) refuses every non-`Landed` kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAvailability {
    /// Ingest works today.
    Landed,
    /// Waiting on `POST /api/v1/context/ingest`; a paired client pushes these, so no egress gate.
    AwaitingIngestRoute,
    /// Waiting on a read-only connector; otherwise `upsert_source` mints never-filled sources.
    AwaitingReadConnector,
}

impl SourceAvailability {
    /// Why a caller was refused, in a sentence naming the thing that is missing.
    pub fn refusal(&self) -> &'static str {
        match self {
            Self::Landed => "",
            Self::AwaitingIngestRoute => {
                "this source kind is pushed to the pond by a paired client, and the ingest \
                 route it would arrive on (PAI-8 P3) does not exist yet"
            }
            Self::AwaitingReadConnector => {
                "this source kind needs a connector that signs in to the account and reads \
                 it, and no connector for this protocol exists yet"
            }
        }
    }
}

impl SourceKind {
    /// Every variant. Update with the enum: `parse` only recognises kinds listed here.
    pub const ALL: [SourceKind; 8] = [
        SourceKind::Sensor,
        SourceKind::Camera,
        SourceKind::Voice,
        SourceKind::Mobile,
        SourceKind::Mail,
        SourceKind::Calendar,
        SourceKind::Files,
        SourceKind::Chat,
    ];

    /// Stable name, written to SQLite: part of the schema, not a display detail.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sensor => "sensor",
            Self::Camera => "camera",
            Self::Voice => "voice",
            Self::Mobile => "mobile",
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Files => "files",
            Self::Chat => "chat",
        }
    }

    /// Parse a stored kind; the storage adapter reads `None` as "no item", which narrows.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// What must land before ingest accepts this kind.
    pub fn availability(&self) -> SourceAvailability {
        match self {
            // Data the pond already holds. Ingesting it adds no egress.
            Self::Sensor | Self::Camera | Self::Voice => SourceAvailability::Landed,
            Self::Mobile => SourceAvailability::AwaitingIngestRoute,
            // Adapter, credentials and sync all exist; without them a `Landed` source stays empty.
            Self::Calendar | Self::Mail => SourceAvailability::Landed,
            Self::Files | Self::Chat => SourceAvailability::AwaitingReadConnector,
        }
    }

    /// Whether connecting this kind means signing in; the connect route and sync sweep share it.
    pub fn needs_credentials(&self) -> bool {
        match self {
            Self::Sensor | Self::Camera | Self::Voice | Self::Mobile => false,
            Self::Mail | Self::Calendar | Self::Files | Self::Chat => true,
        }
    }

    /// Sensitivity floor for this kind's items; redactor findings only raise it. Never `Public`.
    pub fn min_sensitivity(&self) -> PrivacySensitivity {
        match self {
            // "The hall sensor saw motion at 03:12" is about a person.
            Self::Sensor => PrivacySensitivity::Internal,
            Self::Camera | Self::Voice | Self::Mobile => PrivacySensitivity::Sensitive,
            Self::Mail | Self::Calendar | Self::Files | Self::Chat => PrivacySensitivity::Sensitive,
        }
    }

    /// Which [`EventCategory`]'s retention setting (the user's existing map) governs this kind.
    pub fn retention_category(&self) -> EventCategory {
        match self {
            Self::Sensor => EventCategory::Sensor,
            Self::Camera => EventCategory::Camera,
            // A transcript is something the agent produced from a turn.
            Self::Voice => EventCategory::Agent,
            Self::Mobile => EventCategory::Device,
            // Everything a connector fetched crossed the network to get here.
            Self::Mail | Self::Calendar | Self::Files | Self::Chat => EventCategory::Network,
        }
    }
}

// ── Item kind ───────────────────────────────────────────────────────────────

/// What sort of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Message,
    Event,
    Document,
    Location,
    Task,
}

impl ItemKind {
    pub const ALL: [ItemKind; 5] = [
        ItemKind::Message,
        ItemKind::Event,
        ItemKind::Document,
        ItemKind::Location,
        ItemKind::Task,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Event => "event",
            Self::Document => "document",
            Self::Location => "location",
            Self::Task => "task",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }
}

// ── Source status ───────────────────────────────────────────────────────────

/// How a source is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    Connected,
    NeedsReauth,
    Error,
    /// Reported in `network_mode = offline`; not `Error`, since that was the operator's choice.
    Paused,
}

impl SourceStatus {
    pub const ALL: [SourceStatus; 4] = [
        SourceStatus::Connected,
        SourceStatus::NeedsReauth,
        SourceStatus::Error,
        SourceStatus::Paused,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::NeedsReauth => "needs_reauth",
            Self::Error => "error",
            Self::Paused => "paused",
        }
    }

    /// Parse a stored status; anything unrecognised is [`Error`](Self::Error), so syncing stops.
    pub fn parse(raw: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == raw)
            .unwrap_or(Self::Error)
    }
}

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    #[error("a context {0} needs an id")]
    BlankId(&'static str),
    #[error(
        "a context {0} must belong to a household member; a blank profile id is the `None` \
         this type exists to make unrepresentable"
    )]
    BlankOwner(&'static str),
    #[error("a context item must name the source it came from")]
    BlankSourceId,
    #[error(
        "a context item needs an external id, which is what makes re-syncing it idempotent \
         rather than duplicating it"
    )]
    BlankExternalId,
    #[error("a context item with neither a title nor a body carries nothing")]
    Empty,
    #[error("a context source needs a provider name")]
    BlankProvider,
}

// ── ContextSource ───────────────────────────────────────────────────────────

/// An account, sensor or device that produces context for one household member.
/// No token: credentials live in the encrypted secret store; this holds at most a `secret_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSource {
    id: String,
    kind: SourceKind,
    provider: String,
    profile_id: String,
    scopes: Vec<String>,
    cursor: Option<String>,
    last_sync: Option<DateTime<Utc>>,
    status: SourceStatus,
    secret_ref: Option<String>,
    created_at: DateTime<Utc>,
}

/// Secret-store key for a source's sign-in; the connect route and sync sweep must both use it.
pub fn secret_key_for(source_id: &str) -> String {
    format!("caldav:{source_id}")
}

/// What a [`ContextSource`] is built from, on the connect and storage-read paths.
#[derive(Debug, Clone)]
pub struct SourceParts {
    pub id: String,
    pub kind: SourceKind,
    pub provider: String,
    pub profile_id: String,
    pub scopes: Vec<String>,
    pub cursor: Option<String>,
    pub last_sync: Option<DateTime<Utc>>,
    pub status: SourceStatus,
    pub secret_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ContextSource {
    /// The only constructor. Refuses a blank id, owner or provider.
    pub fn from_parts(parts: SourceParts) -> Result<Self, ContextError> {
        if parts.id.trim().is_empty() {
            return Err(ContextError::BlankId("source"));
        }
        if parts.profile_id.trim().is_empty() {
            return Err(ContextError::BlankOwner("source"));
        }
        if parts.provider.trim().is_empty() {
            return Err(ContextError::BlankProvider);
        }
        Ok(Self {
            id: parts.id,
            kind: parts.kind,
            provider: parts.provider,
            profile_id: parts.profile_id,
            scopes: parts.scopes,
            cursor: parts.cursor,
            last_sync: parts.last_sync,
            status: parts.status,
            secret_ref: parts.secret_ref,
            created_at: parts.created_at,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn kind(&self) -> SourceKind {
        self.kind
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
    pub fn last_sync(&self) -> Option<DateTime<Utc>> {
        self.last_sync
    }
    pub fn status(&self) -> SourceStatus {
        self.status
    }
    pub fn secret_ref(&self) -> Option<&str> {
        self.secret_ref.as_deref()
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Advance the sync position only; changing the owner would misattribute stored items.
    pub fn advance(&mut self, cursor: Option<String>, at: DateTime<Utc>, status: SourceStatus) {
        self.cursor = cursor;
        self.last_sync = Some(at);
        self.status = status;
    }
}

// ── ContextItem ─────────────────────────────────────────────────────────────

/// An item's raw parts; `participants` is redacted too, as bridges put anything there.
#[derive(Debug, Clone)]
pub struct ItemParts {
    pub id: String,
    pub source_id: String,
    pub external_id: String,
    pub profile_id: String,
    pub source_kind: SourceKind,
    pub kind: ItemKind,
    pub occurred_at: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    pub title: String,
    pub body: String,
    pub participants: Vec<String>,
    /// A stored row's classification on read, `None` on ingest; it can only tighten.
    pub stored_sensitivity: Option<PrivacySensitivity>,
    pub embedding: Option<Vec<f32>>,
}

/// One thing a source produced, owned by one household member, redacted.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    id: String,
    source_id: String,
    external_id: String,
    profile_id: String,
    source_kind: SourceKind,
    kind: ItemKind,
    occurred_at: DateTime<Utc>,
    ingested_at: DateTime<Utc>,
    title: String,
    body: String,
    participants: Vec<String>,
    sensitivity: PrivacySensitivity,
    findings: Vec<RedactionKind>,
    embedding: Option<Vec<f32>>,
}

impl ContextItem {
    /// The only constructor; redacts at [`INGEST_REDACTION_LEVEL`].
    /// Sensitivity is derived: the strictest of kind floor, findings and `stored_sensitivity`.
    pub fn from_parts(
        redactor: &dyn Redactor,
        parts: ItemParts,
    ) -> Result<ContextItem, ContextError> {
        if parts.id.trim().is_empty() {
            return Err(ContextError::BlankId("item"));
        }
        if parts.source_id.trim().is_empty() {
            return Err(ContextError::BlankSourceId);
        }
        if parts.external_id.trim().is_empty() {
            return Err(ContextError::BlankExternalId);
        }
        if parts.profile_id.trim().is_empty() {
            return Err(ContextError::BlankOwner("item"));
        }
        if parts.title.trim().is_empty() && parts.body.trim().is_empty() {
            return Err(ContextError::Empty);
        }

        let mut findings: Vec<RedactionKind> = Vec::new();
        let mut clean = |raw: &str| -> String {
            let result = redactor.redact(raw, INGEST_REDACTION_LEVEL);
            for kind in result.findings {
                if !findings.contains(&kind) {
                    findings.push(kind);
                }
            }
            result.text
        };

        let title = clean(&parts.title);
        let body = clean(&parts.body);
        let participants: Vec<String> = parts.participants.iter().map(|p| clean(p)).collect();

        let mut sensitivity = parts.source_kind.min_sensitivity();
        if !findings.is_empty() {
            sensitivity = sensitivity.max(PrivacySensitivity::Sensitive);
        }
        if let Some(stored) = parts.stored_sensitivity {
            sensitivity = sensitivity.max(stored);
        }

        Ok(ContextItem {
            id: parts.id,
            source_id: parts.source_id,
            external_id: parts.external_id,
            profile_id: parts.profile_id,
            source_kind: parts.source_kind,
            kind: parts.kind,
            occurred_at: parts.occurred_at,
            ingested_at: parts.ingested_at,
            title,
            body,
            participants,
            sensitivity,
            findings,
            embedding: parts.embedding,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn source_id(&self) -> &str {
        &self.source_id
    }
    pub fn external_id(&self) -> &str {
        &self.external_id
    }
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
    pub fn source_kind(&self) -> SourceKind {
        self.source_kind
    }
    pub fn kind(&self) -> ItemKind {
        self.kind
    }
    pub fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
    pub fn ingested_at(&self) -> DateTime<Utc> {
        self.ingested_at
    }
    /// The redacted title; the raw one is never stored.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// The redacted body.
    pub fn body(&self) -> &str {
        &self.body
    }
    pub fn participants(&self) -> &[String] {
        &self.participants
    }
    pub fn sensitivity(&self) -> PrivacySensitivity {
        self.sensitivity
    }
    /// What the redactor found (kinds only, never the matched text), replaced or not.
    pub fn findings(&self) -> &[RedactionKind] {
        &self.findings
    }
    pub fn embedding(&self) -> Option<&[f32]> {
        self.embedding.as_deref()
    }

    /// Redacted text to embed; a vector over a secret would be a durable derivative of it.
    pub fn embedding_text(&self) -> String {
        if self.title.trim().is_empty() {
            self.body.clone()
        } else if self.body.trim().is_empty() {
            self.title.clone()
        } else {
            format!("{}\n{}", self.title, self.body)
        }
    }

    /// Attach a vector computed over [`embedding_text`](Self::embedding_text).
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::mocks::mock_redactor::MockRedactor;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    fn redactor() -> MockRedactor {
        MockRedactor::replacing(KEY, RedactionKind::ApiKey)
    }

    fn parts(title: &str, body: &str) -> ItemParts {
        ItemParts {
            id: "i1".into(),
            source_id: "src-1".into(),
            external_id: "ext-1".into(),
            profile_id: "jerry".into(),
            source_kind: SourceKind::Voice,
            kind: ItemKind::Message,
            occurred_at: Utc::now(),
            ingested_at: Utc::now(),
            title: title.into(),
            body: body.into(),
            participants: vec![],
            stored_sensitivity: None,
            embedding: None,
        }
    }

    #[test]
    fn a_credential_is_gone_before_the_value_exists() {
        let item = ContextItem::from_parts(&redactor(), parts("subject", &format!("key {KEY} x")))
            .expect("valid item");
        assert!(
            !item.body().contains(KEY),
            "the constructor built an item whose body still holds the credential, so a \
             `ContextItem` in hand is no longer evidence that redaction ran: {}",
            item.body()
        );
        assert!(item.body().contains("[redacted:api-key]"));
        assert!(item.body().contains(" x"), "prose was mangled");
        assert!(
            !item.embedding_text().contains(KEY),
            "the embedding would be computed over the credential, making the vector a durable \
             derivative of it"
        );
    }

    #[test]
    fn a_participant_is_redacted_too() {
        let mut p = parts("subject", "body");
        p.participants = vec![format!("bot {KEY}")];
        let item = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert!(
            !item.participants()[0].contains(KEY),
            "a participant reached storage unredacted: {}",
            item.participants()[0]
        );
        assert!(item.findings().contains(&RedactionKind::ApiKey));
    }

    #[test]
    fn the_level_is_secrets_and_the_caller_cannot_choose_it() {
        let r = redactor();
        let _ = ContextItem::from_parts(&r, parts("t", "b")).expect("valid item");
        let calls = r.calls();
        assert!(!calls.is_empty(), "the redactor was never consulted");
        for (_, level) in calls {
            assert_eq!(
                level,
                RedactionLevel::Secrets,
                "ingest redacts credentials, not contact details -- see INGEST_REDACTION_LEVEL"
            );
        }
    }

    #[test]
    fn every_text_field_goes_through_the_redactor() {
        let r = redactor();
        let mut p = parts("title text", "body text");
        p.participants = vec!["someone".into()];
        let _ = ContextItem::from_parts(&r, p).expect("valid item");
        let texts: Vec<String> = r.calls().into_iter().map(|(t, _)| t).collect();
        for expected in ["title text", "body text", "someone"] {
            assert!(
                texts.iter().any(|t| t == expected),
                "{expected:?} never reached the redactor; seen: {texts:?}"
            );
        }
    }

    #[test]
    fn a_blank_owner_is_refused_and_so_is_a_blank_external_id() {
        let mut p = parts("t", "b");
        p.profile_id = "   ".into();
        assert_eq!(
            ContextItem::from_parts(&redactor(), p).unwrap_err(),
            ContextError::BlankOwner("item")
        );

        let mut p = parts("t", "b");
        p.external_id = String::new();
        assert_eq!(
            ContextItem::from_parts(&redactor(), p).unwrap_err(),
            ContextError::BlankExternalId
        );
    }

    #[test]
    fn an_item_with_no_content_at_all_is_refused() {
        assert_eq!(
            ContextItem::from_parts(&redactor(), parts("  ", "")).unwrap_err(),
            ContextError::Empty
        );
        // One of the two is enough.
        assert!(ContextItem::from_parts(&redactor(), parts("subject only", "")).is_ok());
    }

    #[test]
    fn sensitivity_is_derived_and_a_finding_raises_it() {
        let mut p = parts("t", "b");
        p.source_kind = SourceKind::Sensor;
        let plain = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(plain.sensitivity(), PrivacySensitivity::Internal);

        let mut p = parts("t", &format!("{KEY} was in here"));
        p.source_kind = SourceKind::Sensor;
        let found = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(
            found.sensitivity(),
            PrivacySensitivity::Sensitive,
            "an item the redactor had to clean is not Internal"
        );
        assert_ne!(
            found.sensitivity(),
            PrivacySensitivity::Secret,
            "Secret means credentials, and the credential is what was removed"
        );
    }

    #[test]
    fn a_stored_classification_can_only_make_it_stricter() {
        let mut p = parts("t", "b");
        p.source_kind = SourceKind::Sensor;
        p.stored_sensitivity = Some(PrivacySensitivity::Public);
        let lowered = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(
            lowered.sensitivity(),
            PrivacySensitivity::Internal,
            "a Public in the row must not beat the source kind's floor"
        );

        let mut p = parts("t", "b");
        p.source_kind = SourceKind::Sensor;
        p.stored_sensitivity = Some(PrivacySensitivity::Secret);
        let raised = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(raised.sensitivity(), PrivacySensitivity::Secret);
    }

    /// Relies on `Redactor::redact` being idempotent, so re-redacting a proper row is free.
    #[test]
    fn the_read_path_repairs_a_row_written_out_of_band() {
        let smuggled = ItemParts {
            stored_sensitivity: Some(PrivacySensitivity::Internal),
            ..parts("t", &format!("someone pasted {KEY} straight into sqlite"))
        };
        let item = ContextItem::from_parts(&redactor(), smuggled).expect("valid item");
        assert!(!item.body().contains(KEY), "{}", item.body());
    }

    #[test]
    fn every_kind_maps_to_a_stable_name_and_back() {
        for kind in SourceKind::ALL {
            assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
        }
        for kind in ItemKind::ALL {
            assert_eq!(ItemKind::parse(kind.as_str()), Some(kind));
        }
        for status in SourceStatus::ALL {
            assert_eq!(SourceStatus::parse(status.as_str()), status);
        }
        assert_eq!(SourceKind::parse("gmail"), None);
        assert_eq!(ItemKind::parse("email"), None);
    }

    #[test]
    fn an_unknown_status_is_an_error_not_connected() {
        assert_eq!(SourceStatus::parse("whatever"), SourceStatus::Error);
        assert_ne!(SourceStatus::parse("whatever"), SourceStatus::Connected);
    }

    /// Pinned because a wrong promotion to `Landed` is silent.
    #[test]
    fn only_kinds_with_a_working_path_are_landed() {
        let landed: Vec<&str> = SourceKind::ALL
            .into_iter()
            .filter(|k| k.availability() == SourceAvailability::Landed)
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            landed,
            vec!["sensor", "camera", "voice", "mail", "calendar"],
            "a kind is Landed only once an adapter, a credential path and a sync exist for it"
        );
        for kind in SourceKind::ALL {
            if kind.availability() != SourceAvailability::Landed {
                assert!(
                    !kind.availability().refusal().is_empty(),
                    "{} is refused with no reason given",
                    kind.as_str()
                );
            }
        }
    }

    #[test]
    fn no_source_kind_floors_at_public() {
        assert!(PrivacySensitivity::Public < PrivacySensitivity::Internal);
        for kind in SourceKind::ALL {
            assert!(
                kind.min_sensitivity() >= PrivacySensitivity::Internal,
                "{} floors at {:?}",
                kind.as_str(),
                kind.min_sensitivity()
            );
        }
    }

    #[test]
    fn a_source_refuses_a_blank_owner_and_keeps_its_kind_across_an_advance() {
        let base = SourceParts {
            id: "src-1".into(),
            kind: SourceKind::Voice,
            provider: "pond".into(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: Utc::now(),
        };

        let blank = SourceParts {
            profile_id: " ".into(),
            ..base.clone()
        };
        assert_eq!(
            ContextSource::from_parts(blank).unwrap_err(),
            ContextError::BlankOwner("source")
        );

        let mut source = ContextSource::from_parts(base).expect("valid source");
        source.advance(Some("cursor-2".into()), Utc::now(), SourceStatus::Paused);
        assert_eq!(source.cursor(), Some("cursor-2"));
        assert_eq!(source.status(), SourceStatus::Paused);
        assert_eq!(source.kind(), SourceKind::Voice);
        assert_eq!(source.profile_id(), "jerry");
    }
}
