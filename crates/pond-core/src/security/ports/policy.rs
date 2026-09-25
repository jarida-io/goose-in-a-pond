//! Driven port: authorization and audit at the privacy/security boundary.
//! A hook, not a gate: handlers call [`SecurityPolicy::allow`] before touching a user-data scope
//! and [`SecurityPolicy::audit`] at the crossing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::user_data::domain::profile::ProfileScope;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Coarse-grained user-data scope identifiers used by [`SecurityPolicy::allow`].
pub mod scopes {
    /// Stored conversational memory fragments.
    pub const MEMORY: &str = "memory";
    /// Assistant settings (identity, LLM, voice, retention).
    pub const SETTINGS: &str = "settings";
    /// Secret material (API keys, OAuth tokens).
    pub const SECRETS: &str = "secrets";
    /// Household member profiles.
    pub const PROFILE: &str = "profile";
    /// Conversation sessions and their history.
    pub const SESSION: &str = "session";
    /// Scheduled / cron tasks.
    pub const SCHEDULE: &str = "schedule";
    /// IoT sensor readings.
    pub const SENSOR: &str = "sensor";
    /// Camera frames and events.
    pub const CAMERA: &str = "camera";
    /// Side-effecting actions staged for confirmation.
    pub const DRAFT: &str = "draft";
}

/// Origin of a request crossing the privacy/security boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalKind {
    /// A request from the loopback interface (`127.0.0.1`), trusted on a single-device deployment.
    Loopback,
    /// A remote client authenticated by session token, carrying the token's `client_id`.
    Token(String),
    /// An in-process caller (scheduler, memory extractor, …) with no external origin.
    Internal,
}

/// The authenticated (or implicitly trusted) caller behind a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind: PrincipalKind,
    /// Remote socket address, when the request came over the network.
    pub remote_addr: Option<String>,
    /// The household member this caller has been **proved** to be; `None` for every caller today.
    /// Never fill it from an asserted value; filling it from `devices.profile_id` would let a
    /// paired phone assert its own member via [`is_identity_assertion_proven`].
    pub proven_profile_id: Option<String>,
    /// The device the pond issued this caller's token to, from `Handshake::caller_for_token` only.
    /// Read it via `ProvenDevice::from_principal`. `None` for loopback (dev bypass included) and
    /// internal callers: neither presented a token.
    pub device_id: Option<String>,
}

impl Principal {
    /// A loopback caller (no remote address).
    pub fn loopback() -> Self {
        Self {
            kind: PrincipalKind::Loopback,
            remote_addr: None,
            proven_profile_id: None,
            device_id: None,
        }
    }

    /// An in-process caller with no external origin.
    pub fn internal() -> Self {
        Self {
            kind: PrincipalKind::Internal,
            remote_addr: None,
            proven_profile_id: None,
            device_id: None,
        }
    }

    /// A remote caller authenticated as `client_id` via a session token.
    pub fn token(client_id: impl Into<String>) -> Self {
        Self {
            kind: PrincipalKind::Token(client_id.into()),
            remote_addr: None,
            proven_profile_id: None,
            device_id: None,
        }
    }

    pub fn with_remote_addr(mut self, addr: impl Into<String>) -> Self {
        self.remote_addr = Some(addr.into());
        self
    }

    /// Attach the token's device; the argument must come from `Handshake::caller_for_token` only.
    pub fn with_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }
}

/// How hard the policy bites. Parsed from `settings.security_policy_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    /// No evaluation, no audit entries.
    Off,
    /// Evaluate and record every decision; block none.
    Audit,
    /// Denials bite.
    Enforce,
}

impl PolicyMode {
    /// Parse, defaulting to [`PolicyMode::Audit`]: a typo must not disable auditing or enforce.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "enforce" => Self::Enforce,
            _ => Self::Audit,
        }
    }

    /// Whether a denial actually blocks the caller in this mode.
    pub fn denies_bite(&self) -> bool {
        matches!(self, Self::Enforce)
    }

    /// The wire form; round-trips through [`PolicyMode::parse`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Audit => "audit",
            Self::Enforce => "enforce",
        }
    }
}

/// What the policy decided, kept separate from what the caller was allowed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    /// Whether the caller proceeds. In `audit` this is `true` even for a denial.
    pub allowed: bool,
    /// `Some(reason)` when the rule said no, whatever the mode did about it.
    pub denied_reason: Option<&'static str>,
    /// The mode the decision was taken under, so a log line is self-describing.
    pub mode: PolicyMode,
}

impl PolicyDecision {
    /// The rule permitted this.
    pub fn permit(mode: PolicyMode) -> Self {
        Self {
            allowed: true,
            denied_reason: None,
            mode,
        }
    }

    /// The rule refused. Whether that blocks depends on the mode.
    pub fn refuse(mode: PolicyMode, reason: &'static str) -> Self {
        Self {
            allowed: !mode.denies_bite(),
            denied_reason: Some(reason),
            mode,
        }
    }

    /// True when the rule refused but the mode let it through anyway.
    pub fn would_deny(&self) -> bool {
        self.denied_reason.is_some() && self.allowed
    }

    /// The verdict for an audit entry: `allow`, `deny`, or `would_deny`.
    pub fn verdict(&self) -> &'static str {
        match (self.denied_reason.is_some(), self.allowed) {
            (false, _) => "allow",
            (true, true) => "would_deny",
            (true, false) => "deny",
        }
    }
}

/// Every [`PolicyDecision::verdict`] value, in report order; readers use this, not literals.
pub const VERDICTS: [&str; 3] = ["allow", "would_deny", "deny"];

/// The `action` every [`SecurityPolicy::audit`] entry is recorded under.
pub const AUDIT_ACTION: &str = "security.audit";

/// Attribute keys on an audit event; `VERDICT`, not `OK`, shows what `enforce` would change.
pub mod audit_attrs {
    pub const PRINCIPAL: &str = "principal";
    pub const ACTION: &str = "action";
    pub const SCOPE: &str = "scope";
    pub const OK: &str = "ok";
    pub const VERDICT: &str = "verdict";
    pub const MODE: &str = "mode";
    pub const REASON: &str = "reason";
    pub const REMOTE_ADDR: &str = "remote_addr";
}

/// Process-lifetime tallies of policy decisions, by verdict.
/// Deliberately no reset: unlike the pruned, user-clearable event log, nothing can zero these.
#[derive(Debug)]
pub struct PolicyCounters {
    allow: AtomicU64,
    would_deny: AtomicU64,
    deny: AtomicU64,
    counting_since: OnceLock<DateTime<Utc>>,
}

/// The one process-wide instance.
pub static POLICY_COUNTERS: PolicyCounters = PolicyCounters::new();

impl PolicyCounters {
    const fn new() -> Self {
        Self {
            allow: AtomicU64::new(0),
            would_deny: AtomicU64::new(0),
            deny: AtomicU64::new(0),
            counting_since: OnceLock::new(),
        }
    }

    /// Tally one decision at the decision site, not inside [`SecurityPolicy::audit`]'s impls.
    pub fn record(&self, decision: &PolicyDecision) {
        self.mark_start();
        let counter = match decision.verdict() {
            "allow" => &self.allow,
            "would_deny" => &self.would_deny,
            _ => &self.deny,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Read the tallies, starting the clock if nothing has been recorded yet.
    pub fn snapshot(&self) -> PolicyCounterSnapshot {
        let counting_since = self.mark_start();
        PolicyCounterSnapshot {
            allow: self.allow.load(Ordering::Relaxed),
            would_deny: self.would_deny.load(Ordering::Relaxed),
            deny: self.deny.load(Ordering::Relaxed),
            counting_since,
        }
    }

    fn mark_start(&self) -> DateTime<Utc> {
        *self.counting_since.get_or_init(Utc::now)
    }
}

/// A read of [`POLICY_COUNTERS`] at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyCounterSnapshot {
    pub allow: u64,
    pub would_deny: u64,
    pub deny: u64,
    /// When counting started; anything earlier was another process.
    pub counting_since: DateTime<Utc>,
}

/// May this caller bind a session to a named member (`PUT /sessions/{id}/user`, `Explicit`)?
/// Nothing can prove identity yet, so under `enforce` only loopback may; hence the `audit` default.
pub fn is_identity_assertion_proven(principal: &Principal, asserted_profile_id: &str) -> bool {
    match &principal.proven_profile_id {
        Some(proven) => proven == asserted_profile_id,
        // Loopback is the pond's own console; whoever is at it already has the box.
        None => matches!(principal.kind, PrincipalKind::Loopback),
    }
}

/// Reason string for a refused identity assertion.
pub const REASON_UNPROVEN_IDENTITY: &str =
    "session identity asserted by a principal that has not proved it";

/// May this caller decide a draft? `Err` carries the refusal reason.
/// Guests are refused here as a backstop to the tool-group denylist. An unowned draft falls back
/// to its staging session, since `save_draft` cannot yet resolve a speaker on every pond.
pub fn is_draft_decision_permitted(
    actor: Option<&ProfileScope>,
    actor_session_id: &str,
    draft_owner: Option<&str>,
    draft_session_id: &str,
) -> Result<(), &'static str> {
    let Some(actor) = actor else {
        return Err(REASON_UNRESOLVED_ACTOR);
    };
    if actor_session_id.trim().is_empty() {
        return Err(REASON_UNRESOLVED_ACTOR);
    }
    if matches!(actor, ProfileScope::Guest) {
        return Err(REASON_GUEST_DRAFT_DECISION);
    }
    match draft_owner {
        Some(owner) => match actor {
            ProfileScope::Owner(id) if id == owner => Ok(()),
            ProfileScope::Owner(_) => Err(REASON_FOREIGN_DRAFT),
            // Single-member pond: there is no other member to protect from.
            ProfileScope::Household => Ok(()),
            ProfileScope::Guest => Err(REASON_GUEST_DRAFT_DECISION),
        },
        None => {
            if actor_session_id == draft_session_id {
                Ok(())
            } else {
                Err(REASON_UNOWNED_DRAFT)
            }
        }
    }
}

pub const REASON_UNRESOLVED_ACTOR: &str = "draft decided by a caller that could not be resolved";

pub const REASON_FOREIGN_DRAFT: &str = "draft belongs to a different household member";

pub const REASON_UNOWNED_DRAFT: &str = "unowned draft decided from a session that did not stage it";

pub const REASON_GUEST_DRAFT_DECISION: &str = "draft decided by an unidentified speaker";

/// Driven port: authorization decisions and audit at the privacy boundary.
#[async_trait]
pub trait SecurityPolicy: Send + Sync {
    /// Is this caller allowed to access the named user-data [`scopes`] entry?
    /// `Err` means the policy could not be evaluated (not a denial); callers must fail closed.
    async fn allow(&self, principal: &Principal, scope: &str) -> Result<bool>;

    /// Append an audit entry: who did what to which scope, and what the policy decided.
    /// Must never fail the caller; implementations swallow or log their own errors.
    async fn audit(
        &self,
        principal: &Principal,
        action: &str,
        scope: &str,
        decision: &PolicyDecision,
    );
}

#[cfg(test)]
mod policy_rule_tests {
    use super::*;

    #[test]
    fn mode_parses_and_anything_unrecognised_is_audit() {
        assert_eq!(PolicyMode::parse("off"), PolicyMode::Off);
        assert_eq!(PolicyMode::parse("audit"), PolicyMode::Audit);
        assert_eq!(PolicyMode::parse("enforce"), PolicyMode::Enforce);
        assert_eq!(PolicyMode::parse("  ENFORCE "), PolicyMode::Enforce);
        assert_eq!(PolicyMode::parse("enfroce"), PolicyMode::Audit);
        assert_eq!(PolicyMode::parse(""), PolicyMode::Audit);
    }

    #[test]
    fn a_refusal_in_audit_mode_proceeds_but_still_reports_would_deny() {
        let d = PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY);
        assert!(d.allowed, "audit mode must not block");
        assert!(d.would_deny(), "but the refusal must remain visible");
        assert_eq!(d.verdict(), "would_deny");
        assert_eq!(d.denied_reason, Some(REASON_UNPROVEN_IDENTITY));
    }

    #[test]
    fn the_same_refusal_in_enforce_mode_blocks() {
        let d = PolicyDecision::refuse(PolicyMode::Enforce, REASON_UNPROVEN_IDENTITY);
        assert!(!d.allowed);
        assert!(!d.would_deny(), "it did not merely would-deny, it denied");
        assert_eq!(d.verdict(), "deny");
    }

    #[test]
    fn a_permit_is_a_permit_in_every_mode() {
        for mode in [PolicyMode::Off, PolicyMode::Audit, PolicyMode::Enforce] {
            let d = PolicyDecision::permit(mode);
            assert!(d.allowed);
            assert!(!d.would_deny());
            assert_eq!(d.verdict(), "allow");
        }
    }

    #[test]
    fn permit_and_would_deny_are_indistinguishable_by_allowed_alone() {
        let permit = PolicyDecision::permit(PolicyMode::Audit);
        let refused = PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY);
        assert_eq!(permit.allowed, refused.allowed);
        assert_ne!(permit.verdict(), refused.verdict());
    }

    // ── The identity-assertion rule ────────────────────────────────────────

    #[test]
    fn a_token_principal_cannot_assert_a_member_it_has_not_proved() {
        let phone = Principal::token("some-phone");
        assert!(!is_identity_assertion_proven(&phone, "liz-profile-id"));
    }

    #[test]
    fn a_principal_may_assert_the_member_it_has_been_proved_to_be() {
        let mut phone = Principal::token("liz-phone");
        phone.proven_profile_id = Some("liz-profile-id".to_string());
        assert!(is_identity_assertion_proven(&phone, "liz-profile-id"));
        assert!(!is_identity_assertion_proven(&phone, "jerry-profile-id"));
    }

    #[test]
    fn loopback_may_assert_any_member() {
        assert!(is_identity_assertion_proven(
            &Principal::loopback(),
            "anyone"
        ));
    }

    /// Nothing in-process should bind a session to a member on somebody's behalf.
    #[test]
    fn internal_may_not_assert_a_member() {
        assert!(!is_identity_assertion_proven(&Principal::internal(), "liz"));
    }

    // ── The draft-decision rule ────────────────────────────────────────────

    #[test]
    fn a_member_may_not_decide_another_members_draft() {
        let liz = ProfileScope::Owner("liz".into());
        assert_eq!(
            is_draft_decision_permitted(Some(&liz), "sess-a", Some("jerry"), "sess-a"),
            Err(REASON_FOREIGN_DRAFT),
            "same session is not the boundary: two speakers share a voice session"
        );
        assert_eq!(
            is_draft_decision_permitted(Some(&liz), "sess-b", Some("liz"), "sess-a"),
            Ok(()),
            "her own draft, from her phone, is still hers"
        );
    }

    #[test]
    fn an_unowned_draft_may_only_be_decided_from_the_session_that_staged_it() {
        let anyone = ProfileScope::Owner("liz".into());
        assert_eq!(
            is_draft_decision_permitted(Some(&anyone), "sess-a", None, "sess-a"),
            Ok(())
        );
        assert_eq!(
            is_draft_decision_permitted(Some(&anyone), "sess-b", None, "sess-a"),
            Err(REASON_UNOWNED_DRAFT),
            "the reported hole: any session approving any draft id"
        );
    }

    #[test]
    fn a_guest_and_an_unresolvable_caller_are_both_refused() {
        assert_eq!(
            is_draft_decision_permitted(Some(&ProfileScope::Guest), "sess-a", None, "sess-a"),
            Err(REASON_GUEST_DRAFT_DECISION),
            "a visitor may not run a staged action even in its own session"
        );
        assert_eq!(
            is_draft_decision_permitted(Some(&ProfileScope::Household), "", None, ""),
            Err(REASON_UNRESOLVED_ACTOR),
            "a blank session is not a session; two unresolvable callers must not match"
        );
        assert_eq!(
            is_draft_decision_permitted(None, "sess-a", Some("liz"), "sess-a"),
            Err(REASON_UNRESOLVED_ACTOR)
        );
    }

    /// The second half proves the real resolver can reach the `Household` arm.
    #[test]
    fn household_decides_anything_because_a_household_pond_has_one_member() {
        assert_eq!(
            is_draft_decision_permitted(
                Some(&ProfileScope::Household),
                "sess-a",
                Some("the-only-member"),
                "sess-b"
            ),
            Ok(())
        );
        use crate::user_data::domain::session::SessionIdentity;
        use crate::user_data::services::identity_resolution::{resolve, ResolutionInputs};
        let unknown = SessionIdentity::unknown();
        assert_eq!(
            resolve(&ResolutionInputs {
                paired_device_profile: None,
                session: &unknown,
                household_has_multiple_members: false,
            })
            .scope,
            ProfileScope::Household
        );
    }

    #[test]
    fn no_constructor_grants_a_proved_identity() {
        for p in [
            Principal::loopback(),
            Principal::internal(),
            Principal::token("c1"),
            Principal::token("c2").with_remote_addr("10.0.0.2:5000"),
        ] {
            assert_eq!(
                p.proven_profile_id, None,
                "a constructor handed out a proved identity: {p:?}"
            );
        }
    }

    // ── Process-lifetime counters ──────────────────────────────────────────

    /// Asserts deltas: the counters are process-global and other tests record too.
    #[test]
    fn each_verdict_lands_in_its_own_counter() {
        let before = POLICY_COUNTERS.snapshot();
        POLICY_COUNTERS.record(&PolicyDecision::permit(PolicyMode::Audit));
        POLICY_COUNTERS.record(&PolicyDecision::refuse(
            PolicyMode::Audit,
            REASON_UNPROVEN_IDENTITY,
        ));
        POLICY_COUNTERS.record(&PolicyDecision::refuse(
            PolicyMode::Enforce,
            REASON_UNPROVEN_IDENTITY,
        ));
        let after = POLICY_COUNTERS.snapshot();

        assert_eq!(after.allow - before.allow, 1);
        assert_eq!(
            after.would_deny - before.would_deny,
            1,
            "a refusal that was let through must not be counted as an allow"
        );
        assert_eq!(after.deny - before.deny, 1);
    }

    #[test]
    fn snapshot_starts_the_clock_rather_than_reporting_nothing() {
        let a = POLICY_COUNTERS.snapshot();
        let b = POLICY_COUNTERS.snapshot();
        assert_eq!(
            a.counting_since, b.counting_since,
            "the start instant must not move under repeated reads"
        );
        assert!(a.counting_since <= Utc::now());
    }

    #[test]
    fn every_verdict_a_decision_can_produce_is_a_known_bucket() {
        let produced = [
            PolicyDecision::permit(PolicyMode::Audit).verdict(),
            PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY).verdict(),
            PolicyDecision::refuse(PolicyMode::Enforce, REASON_UNPROVEN_IDENTITY).verdict(),
        ];
        for v in produced {
            assert!(VERDICTS.contains(&v), "unbucketed verdict: {v}");
        }
        for v in VERDICTS {
            assert!(produced.contains(&v), "bucket nothing can produce: {v}");
        }
    }

    #[test]
    fn mode_round_trips_through_its_wire_form() {
        for mode in [PolicyMode::Off, PolicyMode::Audit, PolicyMode::Enforce] {
            assert_eq!(PolicyMode::parse(mode.as_str()), mode);
        }
    }
}
