//! Proactive proposals: staged actions addressed to one member, who approves or rejects them.
//! Policy only; stored as `drafts` rows. Shape enforces rationale, single audience and expiry.

use crate::user_data::domain::draft::DraftStatus;
use crate::user_data::domain::schedule::TaskKind;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Reserved `drafts.kind` tag that tells proposals apart from user-staged drafts.
pub const PROPOSAL_DRAFT_KIND: &str = "proposal";

/// `drafts.origin` of proactive rows (`NULL` = user-staged); migration 0041's trigger repeats it.
pub const PROPOSAL_ORIGIN: &str = "proactive";

/// Sentinel `drafts.session_id` (the column is `NOT NULL`), namespaced so it can never match
/// an engine session and land in its `list_drafts`.
pub const PROPOSAL_SESSION_ID: &str = "giap:proactive";

/// Longest a proposal may stay live. A day fits same-day deadlines; tomorrow can re-propose.
pub const MAX_PROPOSAL_TTL: Duration = Duration::hours(24);

// ── The trigger reference ───────────────────────────────────────────────────

/// The event behind a proposal. `kind` is the bus's serde tag rather than a `BusEvent`, so new
/// bus variants need no edit; `signal` is the sensor type, camera event type or state key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "BusEventRefWire")]
pub struct BusEventRef {
    kind: String,
    source_id: Option<String>,
    signal: Option<String>,
    observed_at: DateTime<Utc>,
}

/// Deserializes through [`BusEventRef::new`] so stored payloads are validated too.
#[derive(Deserialize)]
struct BusEventRefWire {
    kind: String,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    signal: Option<String>,
    observed_at: DateTime<Utc>,
}

impl TryFrom<BusEventRefWire> for BusEventRef {
    type Error = ProposalError;

    fn try_from(w: BusEventRefWire) -> Result<Self, Self::Error> {
        BusEventRef::new(w.kind, w.source_id, w.signal, w.observed_at)
    }
}

impl BusEventRef {
    /// Refuses a blank `kind`: an unattributable trigger can't be explained or learned from.
    pub fn new(
        kind: impl Into<String>,
        source_id: Option<String>,
        signal: Option<String>,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, ProposalError> {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(ProposalError::MissingTriggerKind);
        }
        Ok(Self {
            kind: kind.trim().to_string(),
            source_id: blank_to_none(source_id),
            signal: blank_to_none(signal),
            observed_at,
        })
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }

    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    pub fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }
}

fn blank_to_none(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ── The audience ────────────────────────────────────────────────────────────

/// Own module because privacy is per-module, so only it can fill `ProposalAudience`'s field.
mod audience {
    use super::ProposalError;
    use crate::user_data::domain::profile::ProfileScope;
    use serde::{Deserialize, Serialize};

    /// The one member a proposal is addressed to. Holds a profile id, not a `ProfileScope`, so
    /// `Household` (a broadcast) and `Guest` are unrepresentable rather than refused.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(try_from = "String", into = "String")]
    pub struct ProposalAudience(String);

    impl ProposalAudience {
        pub fn for_member(profile_id: impl Into<String>) -> Result<Self, ProposalError> {
            let id = profile_id.into();
            let id = id.trim();
            if id.is_empty() {
                return Err(ProposalError::UnaddressableAudience {
                    scope: "an empty profile id".to_string(),
                });
            }
            Ok(Self(id.to_string()))
        }

        /// Refuses `Household` and `Guest`: with no member to name, there is no proposal at all.
        pub fn from_scope(scope: &ProfileScope) -> Result<Self, ProposalError> {
            match scope.owner_id() {
                Some(id) => Self::for_member(id),
                None => Err(ProposalError::UnaddressableAudience {
                    scope: match scope {
                        ProfileScope::Household => "the whole household".to_string(),
                        ProfileScope::Guest => "an unidentified speaker".to_string(),
                        // Unreachable, but a refusal is safer than a panic.
                        ProfileScope::Owner(_) => "no one".to_string(),
                    },
                }),
            }
        }

        pub fn profile_id(&self) -> &str {
            &self.0
        }

        /// The implied read scope; always [`ProfileScope::Owner`].
        pub fn scope(&self) -> ProfileScope {
            ProfileScope::Owner(self.0.clone())
        }
    }

    impl TryFrom<String> for ProposalAudience {
        type Error = ProposalError;

        fn try_from(s: String) -> Result<Self, Self::Error> {
            Self::for_member(s)
        }
    }

    impl From<ProposalAudience> for String {
        fn from(a: ProposalAudience) -> String {
            a.0
        }
    }
}

pub use self::audience::ProposalAudience;
use crate::user_data::domain::profile::ProfileScope;

// ── The proposal ────────────────────────────────────────────────────────────

/// A proactive suggestion awaiting one member's approval. Deliberately no `Deserialize`: build
/// and rehydrate only via validating [`from_parts`](Self::from_parts).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    id: String,
    trigger: BusEventRef,
    rationale: String,
    proposed_action: TaskKind,
    audience: ProposalAudience,
    confidence: f32,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// The [`Proposal`] parts the `drafts` table has no column for; nothing is stored twice, and
/// `rationale` stays a column so SQLite can refuse an empty one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalPayload {
    pub trigger: BusEventRef,
    pub proposed_action: TaskKind,
    pub confidence: f32,
}

impl Proposal {
    /// Validating constructor, also used for rehydration: no "trust the database" back door.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        id: impl Into<String>,
        trigger: BusEventRef,
        rationale: impl Into<String>,
        proposed_action: TaskKind,
        audience: ProposalAudience,
        confidence: f32,
        created_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, ProposalError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ProposalError::MissingId);
        }
        let rationale = rationale.into();
        if rationale.trim().is_empty() {
            return Err(ProposalError::MissingRationale { id });
        }
        if !(confidence.is_finite() && (0.0..=1.0).contains(&confidence)) {
            return Err(ProposalError::ConfidenceOutOfRange {
                id,
                requested: confidence,
            });
        }
        if expires_at <= created_at {
            return Err(ProposalError::AlreadyExpired { id });
        }
        if expires_at - created_at > MAX_PROPOSAL_TTL {
            return Err(ProposalError::TtlTooLong {
                id,
                hours: MAX_PROPOSAL_TTL.num_hours(),
            });
        }
        Ok(Self {
            id: id.trim().to_string(),
            trigger,
            rationale: rationale.trim().to_string(),
            proposed_action,
            audience,
            confidence,
            created_at,
            expires_at,
        })
    }

    /// Build a proposal that expires `ttl` after it was created.
    #[allow(clippy::too_many_arguments)]
    pub fn expiring_after(
        id: impl Into<String>,
        trigger: BusEventRef,
        rationale: impl Into<String>,
        proposed_action: TaskKind,
        audience: ProposalAudience,
        confidence: f32,
        created_at: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<Self, ProposalError> {
        Self::from_parts(
            id,
            trigger,
            rationale,
            proposed_action,
            audience,
            confidence,
            created_at,
            created_at + ttl,
        )
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn trigger(&self) -> &BusEventRef {
        &self.trigger
    }

    /// Why GIAP thinks this matters; never empty.
    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    pub fn proposed_action(&self) -> &TaskKind {
        &self.proposed_action
    }

    pub fn audience(&self) -> &ProposalAudience {
        &self.audience
    }

    /// The audience's read scope; always [`ProfileScope::Owner`].
    pub fn scope(&self) -> ProfileScope {
        self.audience.scope()
    }

    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// `expires_at` is exclusive: a proposal is dead at the instant it expires.
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }

    /// The parts that go into the `drafts.payload` column.
    pub fn payload(&self) -> ProposalPayload {
        ProposalPayload {
            trigger: self.trigger.clone(),
            proposed_action: self.proposed_action.clone(),
            confidence: self.confidence,
        }
    }

    /// One-line description of the action (never the rationale) for draft listings.
    pub fn summary(&self) -> String {
        match &self.proposed_action {
            TaskKind::AgentPrompt { prompt } => truncate(prompt, 120),
            TaskKind::Webhook { webhook_url } => format!("POST to {}", truncate(webhook_url, 100)),
            TaskKind::SensorTrigger(spec) => format!(
                "watch {} for {}",
                spec.source.device_id.as_deref().unwrap_or("any device"),
                spec.source.signal.as_deref().unwrap_or("any signal")
            ),
        }
    }
}

/// Char-safe truncation: byte slicing would panic on multi-byte model output.
fn truncate(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{head}...")
}

// ── What a proposal is about, for the feedback loop ─────────────────────────

/// A trigger minus `observed_at`, with model-written text normalised so repeats compare equal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct TriggerIdentity {
    kind: String,
    source_id: Option<String>,
    signal: Option<String>,
}

impl TriggerIdentity {
    pub fn of(trigger: &BusEventRef) -> Self {
        Self {
            kind: comparison_key(trigger.kind()),
            source_id: trigger
                .source_id()
                .map(comparison_key)
                .filter(|s| !s.is_empty()),
            signal: trigger
                .signal()
                .map(comparison_key)
                .filter(|s| !s.is_empty()),
        }
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }

    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    /// Names this trigger in the third person, self-contained, so `memory::fact_defect` keeps it.
    pub fn describe(&self) -> String {
        match (&self.source_id, &self.signal) {
            (Some(source), Some(signal)) => {
                format!("{} events from {source} ({signal})", self.kind)
            }
            (Some(source), None) => format!("{} events from {source}", self.kind),
            (None, Some(signal)) => format!("{} events of type {signal}", self.kind),
            (None, None) => format!("{} events", self.kind),
        }
    }
}

/// Trigger plus action summary, for the feedback loop. The summary's 120-char cut can merge
/// shapes; deliberate, since erring toward saying less is the safe direction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProposalShape {
    trigger: TriggerIdentity,
    action: String,
}

impl ProposalShape {
    /// The only constructor, so recording and checking a rejection share one definition.
    pub fn of(proposal: &Proposal) -> Self {
        Self {
            trigger: TriggerIdentity::of(proposal.trigger()),
            action: comparison_key(&proposal.summary()),
        }
    }

    pub fn trigger(&self) -> &TriggerIdentity {
        &self.trigger
    }

    pub fn action(&self) -> &str {
        &self.action
    }
}

/// Case-folds, collapses whitespace and strips edge punctuation per word ("front-door" stays).
fn comparison_key(raw: &str) -> String {
    raw.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a member did about a proposal. Uses [`DraftStatus`], since a proposal is a `drafts` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalDecision {
    shape: ProposalShape,
    status: DraftStatus,
    decided_at: DateTime<Utc>,
}

impl ProposalDecision {
    /// Refuses [`DraftStatus::Pending`]: a ledger of decisions must not hold undecided ones.
    pub fn recorded(
        shape: ProposalShape,
        status: DraftStatus,
        decided_at: DateTime<Utc>,
    ) -> Result<Self, ProposalError> {
        if status == DraftStatus::Pending {
            return Err(ProposalError::NotADecision);
        }
        Ok(Self {
            shape,
            status,
            decided_at,
        })
    }

    pub fn shape(&self) -> &ProposalShape {
        &self.shape
    }

    pub fn status(&self) -> &DraftStatus {
        &self.status
    }

    pub fn decided_at(&self) -> DateTime<Utc> {
        self.decided_at
    }

    /// Only a rejection silences a repeat; counting expiries would let an unattended pond go mute.
    pub fn silences_a_repeat(&self) -> bool {
        match self.status {
            DraftStatus::Rejected => true,
            DraftStatus::Approved | DraftStatus::Expired | DraftStatus::Pending => false,
        }
    }

    /// The memory fact this decision teaches; third person and naming the user, to pass the gate.
    pub fn memory_fact(&self) -> Option<String> {
        let verb = match self.status {
            DraftStatus::Approved => "approved",
            DraftStatus::Rejected => "rejected",
            // An expiry is the pond's timeout, not a user preference.
            DraftStatus::Expired | DraftStatus::Pending => return None,
        };
        Some(format!(
            "The user {verb} a proactive suggestion about {}: {}.",
            self.shape.trigger.describe(),
            self.shape.action
        ))
    }
}

/// Why a proposal could not be built.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProposalError {
    #[error("a proposal must have an id")]
    MissingId,
    #[error("proposal `{id}` has no rationale, and a proposal without one may not be shown")]
    MissingRationale { id: String },
    #[error("a proposal's trigger must name an event kind")]
    MissingTriggerKind,
    #[error("proposal `{id}` has a confidence of {requested}; it must be in [0.0, 1.0]")]
    ConfidenceOutOfRange { id: String, requested: f32 },
    #[error("proposal `{id}` expires at or before it was created")]
    AlreadyExpired { id: String },
    #[error("proposal `{id}` would live longer than the {hours}h ceiling")]
    TtlTooLong { id: String, hours: i64 },
    #[error("a proposal cannot be addressed to {scope}: it must name one household member")]
    UnaddressableAudience { scope: String },
    #[error("a pending proposal is not a decision, and a ledger of decisions may not hold one")]
    NotADecision,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::profile::{ProfileScope, EXEMPLAR_OWNER_ID};

    fn trigger() -> BusEventRef {
        BusEventRef::new(
            "camera",
            Some("front-door".into()),
            Some("person".into()),
            Utc::now(),
        )
        .unwrap()
    }

    fn audience() -> ProposalAudience {
        ProposalAudience::for_member(EXEMPLAR_OWNER_ID).unwrap()
    }

    fn action() -> TaskKind {
        TaskKind::AgentPrompt {
            prompt: "tell me about the delivery".into(),
        }
    }

    fn valid(now: DateTime<Utc>) -> Proposal {
        Proposal::expiring_after(
            "prop-1",
            trigger(),
            "the delivery window closes at six",
            action(),
            audience(),
            0.8,
            now,
            Duration::hours(2),
        )
        .unwrap()
    }

    // ── The rationale is mandatory ─────────────────────────────────────────

    #[test]
    fn a_blank_rationale_is_refused_in_every_blank_shape() {
        let now = Utc::now();
        for blank in ["", " ", "\t", "\n  \n"] {
            let err = Proposal::expiring_after(
                "prop-1",
                trigger(),
                blank,
                action(),
                audience(),
                0.5,
                now,
                Duration::hours(1),
            )
            .expect_err(&format!(
                "a rationale of {blank:?} must be refused: invariant 2 says every proposal \
                 carries a reason the user can read, and a blank one is what a defaulted \
                 field produces"
            ));
            assert!(
                matches!(err, ProposalError::MissingRationale { .. }),
                "a rationale of {blank:?} must be refused as MissingRationale, got {err:?}"
            );
        }
    }

    /// Trait absence can't be asserted directly, so this scans the source's derive list.
    #[test]
    fn the_only_way_to_hold_a_proposal_is_the_validating_constructor() {
        fn is_deserializable<T: for<'de> serde::Deserialize<'de>>() -> bool {
            true
        }
        // The wire half; `from_parts` re-checks everything it carries.
        assert!(is_deserializable::<ProposalPayload>());
        // Need Deserialize? Use a `#[serde(try_from = ...)]` wire struct like `BusEventRef`.
        let src = include_str!("proposal.rs");
        assert!(
            !derive_list_of(src, "Proposal").contains("Deserialize"),
            "Proposal must not derive Deserialize: it would bypass from_parts. \
             Derive list was: {}",
            derive_list_of(src, "Proposal")
        );
        // Vacuity control through the same helper, which panics when the key matches nothing.
        assert!(
            derive_list_of(src, "ProposalPayload").contains("Deserialize"),
            "the derive-list search is broken: it cannot see ProposalPayload's \
             own Deserialize. Found: {}",
            derive_list_of(src, "ProposalPayload")
        );
    }

    /// The `#[derive(...)]` list above `pub struct <name> {`; panics if no such declaration.
    fn derive_list_of(src: &str, type_name: &str) -> String {
        let decl = format!("\npub struct {type_name} {{");
        let prefix = src.split(&decl).next().expect("split yields one part");
        assert!(
            prefix.len() < src.len(),
            "no declaration of `{type_name}` in this file: the search key is wrong \
             or the type was renamed"
        );
        prefix
            .rsplit("#[derive(")
            .next()
            .unwrap_or_else(|| panic!("`{type_name}` has no derive list above it"))
            .to_string()
    }

    // ── Addressed to a member, never broadcast ─────────────────────────────

    #[test]
    fn no_scope_but_owner_can_address_a_proposal() {
        for scope in ProfileScope::every_shape() {
            let built = ProposalAudience::from_scope(&scope);
            match &scope {
                ProfileScope::Owner(id) => {
                    assert_eq!(
                        built.expect("an owner must be addressable").profile_id(),
                        id
                    );
                }
                other => {
                    let err = built.expect_err(&format!(
                        "{other:?} must not be able to hold a proposal: invariant 4 forbids the \
                         broadcast and invariant 5 forbids the guest"
                    ));
                    assert!(matches!(err, ProposalError::UnaddressableAudience { .. }));
                }
            }
        }
    }

    #[test]
    fn every_audience_that_exists_names_one_member() {
        for scope in ProfileScope::every_shape() {
            if let Ok(a) = ProposalAudience::from_scope(&scope) {
                assert!(!a.profile_id().is_empty());
                assert_eq!(a.scope(), ProfileScope::Owner(a.profile_id().to_string()));
                assert!(
                    !matches!(a.scope(), ProfileScope::Household | ProfileScope::Guest),
                    "an audience resolved to a broadcast"
                );
            }
        }
    }

    #[test]
    fn an_audience_cannot_be_blank() {
        assert!(matches!(
            ProposalAudience::for_member("   ").unwrap_err(),
            ProposalError::UnaddressableAudience { .. }
        ));
    }

    #[test]
    fn an_audience_round_trips_through_serde_as_a_bare_id() {
        let a = ProposalAudience::for_member("liz").unwrap();
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "\"liz\"");
        let back: ProposalAudience = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        // And the serde door is the same door: a blank id is refused there too.
        assert!(serde_json::from_str::<ProposalAudience>("\"  \"").is_err());
    }

    // ── It expires, and the ceiling is real ────────────────────────────────

    #[test]
    fn a_proposal_is_dead_at_its_expiry_not_after_it() {
        let now = Utc::now();
        let p = valid(now);
        assert!(p.is_live_at(now));
        assert!(p.is_live_at(p.expires_at() - Duration::seconds(1)));
        assert!(
            !p.is_live_at(p.expires_at()),
            "expires_at is exclusive: a proposal is dead at the instant it expires"
        );
        assert!(!p.is_live_at(p.expires_at() + Duration::seconds(1)));
    }

    #[test]
    fn an_expiry_beyond_the_ceiling_is_refused() {
        let now = Utc::now();
        let err = Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            action(),
            audience(),
            0.5,
            now,
            MAX_PROPOSAL_TTL + Duration::seconds(1),
        )
        .unwrap_err();
        assert!(
            matches!(err, ProposalError::TtlTooLong { .. }),
            "a proposal that never expires satisfies the field and fails the invariant, got {err:?}"
        );
        // Exactly the ceiling is allowed.
        assert!(Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            action(),
            audience(),
            0.5,
            now,
            MAX_PROPOSAL_TTL,
        )
        .is_ok());
    }

    #[test]
    fn a_proposal_that_is_born_expired_is_refused() {
        let now = Utc::now();
        for ttl in [Duration::zero(), Duration::seconds(-1)] {
            assert!(matches!(
                Proposal::expiring_after(
                    "prop-1",
                    trigger(),
                    "why",
                    action(),
                    audience(),
                    0.5,
                    now,
                    ttl,
                )
                .unwrap_err(),
                ProposalError::AlreadyExpired { .. }
            ));
        }
    }

    // ── The rest of the constructor ────────────────────────────────────────

    /// A blank id would reach `drafts` as an empty primary key.
    #[test]
    fn a_proposal_without_an_id_is_refused_in_every_blank_shape() {
        let now = Utc::now();
        for blank in ["", " ", "\t", "\n  \n"] {
            let err = Proposal::expiring_after(
                blank,
                trigger(),
                "why",
                action(),
                audience(),
                0.5,
                now,
                Duration::hours(1),
            )
            .expect_err(&format!(
                "an id of {blank:?} must be refused: it becomes the PRIMARY KEY of the \
                 drafts row, and an empty one is what a defaulted field produces"
            ));
            assert!(
                matches!(err, ProposalError::MissingId),
                "an id of {blank:?} must be refused as MissingId, got {err:?}"
            );
        }
        // Vacuity control: the same call with a real id succeeds.
        assert!(Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            action(),
            audience(),
            0.5,
            now,
            Duration::hours(1),
        )
        .is_ok());
    }

    #[test]
    fn confidence_must_be_a_real_number_in_range() {
        let now = Utc::now();
        for bad in [-0.01f32, 1.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                matches!(
                    Proposal::expiring_after(
                        "prop-1",
                        trigger(),
                        "why",
                        action(),
                        audience(),
                        bad,
                        now,
                        Duration::hours(1),
                    )
                    .unwrap_err(),
                    ProposalError::ConfidenceOutOfRange { .. }
                ),
                "confidence {bad} must be refused"
            );
        }
        for ok in [0.0f32, 0.5, 1.0] {
            assert!(Proposal::expiring_after(
                "prop-1",
                trigger(),
                "why",
                action(),
                audience(),
                ok,
                now,
                Duration::hours(1),
            )
            .is_ok());
        }
    }

    #[test]
    fn a_trigger_must_name_an_event_kind() {
        assert!(matches!(
            BusEventRef::new("  ", None, None, Utc::now()).unwrap_err(),
            ProposalError::MissingTriggerKind
        ));
    }

    #[test]
    fn a_trigger_kind_the_bus_does_not_have_yet_is_expressible() {
        for kind in ["sensor", "camera", "device", "time", "presence", "ingest"] {
            let r = BusEventRef::new(kind, None, None, Utc::now()).unwrap();
            assert_eq!(r.kind(), kind);
        }
    }

    #[test]
    fn a_trigger_round_trips_through_its_wire_form() {
        let r = trigger();
        let json = serde_json::to_string(&r).unwrap();
        let back: BusEventRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        // The wire form is the same door: a blank kind is refused there too.
        assert!(serde_json::from_str::<BusEventRef>(
            r#"{"kind":"","observed_at":"2026-08-10T00:00:00Z"}"#
        )
        .is_err());
    }

    #[test]
    fn the_payload_carries_only_what_has_no_column() {
        let p = valid(Utc::now());
        let json = serde_json::to_string(&p.payload()).unwrap();
        assert!(json.contains("camera"));
        assert!(
            !json.contains("delivery window closes"),
            "the rationale must live in its own column, not in the payload: two \
             homes for one field is two things that drift, and the database can \
             only refuse an empty rationale it can see"
        );
        let back: ProposalPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p.payload());
    }

    #[test]
    fn a_summary_describes_the_action_and_never_the_reason() {
        let p = valid(Utc::now());
        assert_eq!(p.summary(), "tell me about the delivery");
        assert!(!p.summary().contains("closes at six"));
    }

    // ── The shape a decision is recorded against ───────────────────────────

    /// Every [`DraftStatus`]; callers' exhaustive `match`es catch a variant missing from here.
    fn every_draft_status() -> [DraftStatus; 4] {
        [
            DraftStatus::Pending,
            DraftStatus::Approved,
            DraftStatus::Rejected,
            DraftStatus::Expired,
        ]
    }

    fn proposal_about(source: &str, action: &str, observed_at: DateTime<Utc>) -> Proposal {
        Proposal::expiring_after(
            "prop-1",
            BusEventRef::new(
                "camera",
                Some(source.into()),
                Some("person".into()),
                observed_at,
            )
            .unwrap(),
            "why this matters",
            TaskKind::AgentPrompt {
                prompt: action.into(),
            },
            audience(),
            0.7,
            observed_at,
            Duration::hours(1),
        )
        .unwrap()
    }

    #[test]
    fn the_same_suggestion_about_the_same_door_shares_a_shape_across_minutes() {
        let noon = Utc::now();
        let a = proposal_about("front-door", "turn on the porch light", noon);
        let b = proposal_about(
            "front-door",
            "turn on the porch light",
            noon + Duration::minutes(37),
        );
        assert_eq!(
            ProposalShape::of(&a),
            ProposalShape::of(&b),
            "a proposal's shape must not carry the clock, or nothing ever matches a rejection"
        );
    }

    #[test]
    fn a_shape_folds_case_and_punctuation_but_never_two_different_devices() {
        let now = Utc::now();
        assert_eq!(
            ProposalShape::of(&proposal_about(
                "Front-Door",
                "Turn on the porch light.",
                now
            )),
            ProposalShape::of(&proposal_about(
                "front-door",
                "turn on the porch light",
                now
            )),
            "both halves of a shape are model output; case and a full stop are not a \
             different suggestion"
        );
        // Vacuity control: an over-wide fold would let one rejection silence everything.
        assert_ne!(
            ProposalShape::of(&proposal_about(
                "front-door",
                "turn on the porch light",
                now
            )),
            ProposalShape::of(&proposal_about("back-door", "turn on the porch light", now)),
            "two different devices must not share a shape"
        );
        assert_ne!(
            ProposalShape::of(&proposal_about(
                "front-door",
                "turn on the porch light",
                now
            )),
            ProposalShape::of(&proposal_about("front-door", "unlock the front door", now)),
            "two different suggestions must not share a shape"
        );
    }

    #[test]
    fn a_pending_proposal_is_not_a_decision() {
        let shape = ProposalShape::of(&valid(Utc::now()));
        assert!(matches!(
            ProposalDecision::recorded(shape, DraftStatus::Pending, Utc::now()).unwrap_err(),
            ProposalError::NotADecision
        ));
    }

    #[test]
    fn only_a_rejection_silences_a_repeat() {
        let shape = ProposalShape::of(&valid(Utc::now()));
        for status in every_draft_status() {
            let expected = match status {
                DraftStatus::Rejected => true,
                DraftStatus::Approved | DraftStatus::Expired | DraftStatus::Pending => false,
            };
            let Ok(decision) =
                ProposalDecision::recorded(shape.clone(), status.clone(), Utc::now())
            else {
                assert_eq!(status, DraftStatus::Pending);
                continue;
            };
            assert_eq!(
                decision.silences_a_repeat(),
                expected,
                "{status} decided the wrong thing about whether to propose this again"
            );
        }
    }

    #[test]
    fn a_decision_that_teaches_writes_a_fact_the_memory_gate_accepts() {
        use crate::user_data::domain::memory::{fact_defect, names_user, normalise_fact_content};

        let shape = ProposalShape::of(&proposal_about(
            "front-door",
            "turn on the porch light",
            Utc::now(),
        ));
        for status in every_draft_status() {
            let expected_fact = match status {
                DraftStatus::Approved | DraftStatus::Rejected => true,
                DraftStatus::Expired | DraftStatus::Pending => false,
            };
            let Ok(decision) =
                ProposalDecision::recorded(shape.clone(), status.clone(), Utc::now())
            else {
                continue;
            };
            match decision.memory_fact() {
                Some(fact) => {
                    assert!(
                        expected_fact,
                        "{status} must teach nothing, but wrote {fact:?}"
                    );
                    let content = normalise_fact_content(&fact);
                    assert_eq!(
                        fact_defect(&content),
                        None,
                        "the feedback fact for {status} is one the memory write gate drops: {content:?}"
                    );
                    assert!(
                        names_user(&content),
                        "the feedback fact for {status} never names the user, so extraction \
                         demotes it out of Preference: {content:?}"
                    );
                    assert!(
                        content.contains("front-door") && content.contains("porch light"),
                        "the feedback fact must quote what was decided about: {content:?}"
                    );
                }
                None => assert!(!expected_fact, "{status} must write a fact, and wrote none"),
            }
        }
    }

    #[test]
    fn a_trigger_with_no_source_still_describes_itself() {
        let bare = BusEventRef::new("time", None, None, Utc::now()).unwrap();
        assert_eq!(TriggerIdentity::of(&bare).describe(), "time events");
        let signal_only = BusEventRef::new("sensor", None, Some("co2".into()), Utc::now()).unwrap();
        assert_eq!(
            TriggerIdentity::of(&signal_only).describe(),
            "sensor events of type co2"
        );
    }

    #[test]
    fn a_summary_of_a_long_multibyte_prompt_does_not_panic() {
        let now = Utc::now();
        let prompt = "\u{00e9}".repeat(400);
        let p = Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            TaskKind::AgentPrompt { prompt },
            audience(),
            0.5,
            now,
            Duration::hours(1),
        )
        .unwrap();
        assert_eq!(p.summary().chars().count(), 120);
        assert!(p.summary().ends_with("..."));
    }
}
