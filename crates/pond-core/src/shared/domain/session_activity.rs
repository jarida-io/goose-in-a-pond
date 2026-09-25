//! Session lifecycle and presence events: tells the proactive phase whether the user is around.
//!
//! Pond-authored sessions are filtered out via [`SessionOrigin`]: they look like a person's, and
//! false presence is worse than none. Idle reuses consolidation's threshold so the two agree.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::{IdentificationSource, Session, SessionIdentity};
use crate::user_data::services::consolidation_schedule as sched;
use crate::user_data::services::identity_resolution;

/// Where the user is in an interaction, as far as the pond can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// A conversation began.
    Started,
    /// The pond has been quiet for the idle threshold, having been active.
    Idle,
    /// Activity reappeared after an [`Idle`](SessionPhase::Idle).
    Resumed,
}

impl SessionPhase {
    /// Short, stable label for structured logs and the event log.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Started => "started",
            SessionPhase::Idle => "idle",
            SessionPhase::Resumed => "resumed",
        }
    }
}

/// Id prefixes of sessions the pond mints for its own work (the scheduler's `sched-…`).
/// Deny-list: human ids take any form. Audited by `session_origin_covers_every_minted_session`.
pub const POND_AUTHORED_SESSION_PREFIXES: &[&str] = &["sched-"];

/// Who caused a session row to exist, inferred from the id: `sessions` has no origin column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// Somebody opened this conversation.
    Human,
    /// The pond opened it for itself, to run background work.
    Machine,
}

impl SessionOrigin {
    pub fn of(session_id: &str) -> Self {
        if POND_AUTHORED_SESSION_PREFIXES
            .iter()
            .any(|prefix| session_id.starts_with(prefix))
        {
            SessionOrigin::Machine
        } else {
            SessionOrigin::Human
        }
    }

    pub fn is_human(self) -> bool {
        matches!(self, SessionOrigin::Human)
    }
}

/// A transition in the user's interaction with the pond.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionLifecycle {
    pub phase: SessionPhase,
    /// Set for `Started`; `None` for `Idle`/`Resumed`, which track the pond-wide activity clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub at: DateTime<Utc>,
    /// How long the pond had been quiet immediately before this transition.
    pub idle_secs: u64,
}

/// A conversation the pond knows about, projected onto what the observer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStart<'a> {
    pub id: &'a str,
    pub created_at: DateTime<Utc>,
    /// [`ActivityObserver`] announces only human starts: a second gate behind [`human_activity`].
    pub origin: SessionOrigin,
}

impl<'a> SessionStart<'a> {
    pub fn of(session: &'a Session) -> Self {
        Self {
            id: &session.id,
            created_at: session.created_at,
            origin: SessionOrigin::of(&session.id),
        }
    }
}

/// The session store minus the pond's own sessions, in **both** arrivals and the activity clock:
/// an unfiltered clock would let a 3am cron fire open the never-at-startup gate.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanActivity<'a> {
    /// Conversations a person opened, in store order.
    pub starts: Vec<SessionStart<'a>>,
    /// Newest `updated_at` among them, so activity persisted by another process (voice) counts.
    pub newest_activity: Option<DateTime<Utc>>,
}

/// Project the session store onto the conversations a person had.
pub fn human_activity(sessions: &[Session]) -> HumanActivity<'_> {
    let mut starts = Vec::new();
    let mut newest_activity: Option<DateTime<Utc>> = None;
    for session in sessions {
        if !SessionOrigin::of(&session.id).is_human() {
            continue;
        }
        newest_activity = newest_activity.max(Some(session.updated_at));
        starts.push(SessionStart::of(session));
    }
    HumanActivity {
        starts,
        newest_activity,
    }
}

/// Everything one observation needs from outside.
#[derive(Debug, Clone, Copy)]
pub struct ActivityInputs {
    /// False until real activity is seen; before that the clock's boot value would read as idle.
    pub saw_activity_since_start: bool,
    /// How long since the most recent activity from any source.
    pub idle_for: Duration,
    /// How long the pond must be quiet before the user counts as away.
    pub idle_threshold: Duration,
    pub now: DateTime<Utc>,
}

/// One reading of the clocks the polling loop owns; every decision on them is in [`poll_inputs`].
#[derive(Debug, Clone, Copy)]
pub struct PollClock {
    /// Captured before any request could be served: the never-at-startup guard's baseline.
    pub started_at: Instant,
    /// The same moment in UTC, for comparing against database timestamps.
    pub started_at_utc: DateTime<Utc>,
    /// The shared `last_user_activity` clock, bumped by every HTTP route.
    pub in_process_at: Instant,
    pub now: DateTime<Utc>,
    /// Quiet time before the user counts as away (`INACTIVITY_THRESHOLD_SECS`).
    pub idle_threshold: Duration,
}

/// What one poll means, before transitions; public so the never-at-startup gate is testable.
pub fn poll_inputs(sessions: &[Session], clock: PollClock) -> ActivityInputs {
    let db_activity = human_activity(sessions).newest_activity;
    ActivityInputs {
        saw_activity_since_start: sched::saw_activity_since_start(
            clock.started_at,
            clock.in_process_at,
            clock.started_at_utc,
            db_activity,
        ),
        idle_for: sched::combined_idle_for(clock.in_process_at, db_activity, clock.now),
        idle_threshold: clock.idle_threshold,
        now: clock.now,
    }
}

/// What the observer last saw; the baseline for transitions, never published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observed {
    Active,
    Idle,
}

/// Turns the pond's session list and activity clock into [`SessionLifecycle`] transitions.
#[derive(Debug, Clone)]
pub struct ActivityObserver {
    phase: Option<Observed>,
    /// Ids already seen (`None` = no baseline yet). Not a time watermark: SQLite times are whole
    /// seconds and unparseable ones read as now. Never pruned, since forgetting re-announces.
    known: Option<HashSet<String>>,
}

impl ActivityObserver {
    /// Take existing sessions as the baseline so a restart announces nothing; a failed read is
    /// not an empty pond, so it defers the baseline to the first successful poll.
    pub fn seeded_from<E>(sessions: Result<Vec<Session>, E>) -> Self {
        match sessions {
            Ok(sessions) => Self {
                phase: None,
                known: Some(sessions.into_iter().map(|s| s.id).collect()),
            },
            Err(_) => Self::awaiting_baseline(),
        }
    }

    /// Start with no baseline: the first poll records what it finds and announces none of it.
    pub fn awaiting_baseline() -> Self {
        Self {
            phase: None,
            known: None,
        }
    }

    /// Fold in one poll, returning any `Started` events then at most one activity-clock edge.
    pub fn poll(&mut self, sessions: &[Session], clock: PollClock) -> Vec<SessionLifecycle> {
        let inputs = poll_inputs(sessions, clock);
        self.observe(&human_activity(sessions).starts, inputs)
    }

    /// The transition rules. Private so nobody bypasses `poll`'s filter and startup gate.
    fn observe(
        &mut self,
        sessions: &[SessionStart<'_>],
        inputs: ActivityInputs,
    ) -> Vec<SessionLifecycle> {
        let mut out = Vec::new();

        // ── New conversations ────────────────────────────────────────────
        // Not gated on `saw_activity_since_start`: a person's session row is itself presence.
        // Machine rows are filtered again here, a second gate after `human_activity`.
        let taking_baseline = self.known.is_none();
        let known = self.known.get_or_insert_with(HashSet::new);
        let mut fresh: Vec<&SessionStart<'_>> = sessions
            .iter()
            .filter(|s| s.origin.is_human() && !known.contains(s.id))
            .collect();
        fresh.sort_by_key(|s| s.created_at);
        for session in &fresh {
            known.insert(session.id.to_string());
        }
        if !taking_baseline {
            for session in fresh {
                out.push(SessionLifecycle {
                    phase: SessionPhase::Started,
                    session_id: Some(session.id.to_string()),
                    at: inputs.now,
                    idle_secs: inputs.idle_for.as_secs(),
                });
            }
        }
        if !out.is_empty() {
            // Mark active so the clock below doesn't also report this arrival as `Resumed`.
            self.phase = Some(Observed::Active);
        }

        // ── The activity clock ───────────────────────────────────────────
        if !inputs.saw_activity_since_start {
            return out;
        }
        let quiet = inputs.idle_for >= inputs.idle_threshold;
        match (self.phase, quiet) {
            // First observation: baseline only; an event here would fire on every restart.
            (None, _) => {
                self.phase = Some(if quiet {
                    Observed::Idle
                } else {
                    Observed::Active
                });
            }
            (Some(Observed::Active), true) => {
                self.phase = Some(Observed::Idle);
                out.push(SessionLifecycle {
                    phase: SessionPhase::Idle,
                    session_id: None,
                    at: inputs.now,
                    idle_secs: inputs.idle_for.as_secs(),
                });
            }
            (Some(Observed::Idle), false) => {
                self.phase = Some(Observed::Active);
                out.push(SessionLifecycle {
                    phase: SessionPhase::Resumed,
                    session_id: None,
                    at: inputs.now,
                    idle_secs: inputs.idle_for.as_secs(),
                });
            }
            _ => {}
        }
        out
    }
}

// ── Presence ─────────────────────────────────────────────────────────────

/// Which way a household member's presence changed: edges only, never the re-derived level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceTransition {
    /// The pond gained fresh evidence naming this member, having had none.
    Arrived,
    /// The evidence went stale or was released: inferred, as nothing observes anyone leaving.
    Departed,
}

impl PresenceTransition {
    /// Short, stable label for structured logs and the event log.
    pub fn as_str(self) -> &'static str {
        match self {
            PresenceTransition::Arrived => "arrived",
            PresenceTransition::Departed => "departed",
        }
    }
}

/// A household member arrived or left, as far as the pond can tell.
/// Only a resolved [`ProfileScope::Owner`] qualifies: no anonymous, `Household` or `Guest` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfilePresence {
    pub profile_id: String,
    pub transition: PresenceTransition,
    /// Which identification rung the belief rests on, so a proposer can weigh it.
    pub source: IdentificationSource,
    /// Set only for [`IdentificationSource::Face`]; the match threshold lives at identification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// For `Arrived` the conversation that named them; for `Departed` the one that went quiet.
    pub session_id: String,
    pub at: DateTime<Utc>,
}

/// One conversation, as the presence observer is allowed to see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresenceEvidence<'a> {
    pub session_id: &'a str,
    pub origin: SessionOrigin,
    /// When somebody last spoke here (`sessions.updated_at`), not when attribution was written.
    pub last_activity: DateTime<Utc>,
    /// Who the session row says is speaking (`SessionStorage::get_session_identity`).
    pub identity: &'a SessionIdentity,
}

/// Presence's activity clock: when somebody last spoke. Binding an identity doesn't bump it,
/// so a face tagged on a long-quiet session produces no presence.
fn last_spoken_at(session: &Session) -> DateTime<Utc> {
    session.updated_at
}

/// Whether evidence this old still counts as somebody being here.
/// Evidence exactly window-old is stale: [`SessionPhase::Idle`] fires at that same instant.
fn is_fresh(last_activity: DateTime<Utc>, presence_window: Duration, now: DateTime<Utc>) -> bool {
    let window = i64::try_from(presence_window.as_secs()).unwrap_or(i64::MAX);
    now.signed_duration_since(last_activity).num_seconds() < window
}

impl<'a> PresenceEvidence<'a> {
    pub fn of(session: &'a Session, identity: &'a SessionIdentity) -> Self {
        Self {
            session_id: &session.id,
            origin: SessionOrigin::of(&session.id),
            last_activity: last_spoken_at(session),
            identity,
        }
    }
}

/// Staleness at which a member stops counting as here; shares `Idle`'s threshold so they agree.
pub const PRESENCE_WINDOW: Duration = Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

/// Sessions worth a `get_session_identity` read; skips rows the observer would discard anyway.
/// Read-avoidance, not a gate: [`present_members`] still checks origin and freshness itself.
pub fn attribution_candidates(sessions: &[Session], now: DateTime<Utc>) -> Vec<&Session> {
    sessions
        .iter()
        .filter(|session| SessionOrigin::of(&session.id).is_human())
        .filter(|session| session.profile_id.is_some())
        .filter(|session| is_fresh(last_spoken_at(session), PRESENCE_WINDOW, now))
        .collect()
}

/// Whether the pond has more than one household member; a failed read answers `true` so an
/// unidentified speaker narrows to `Guest`, not the whole household.
pub fn household_has_multiple_members<T, E>(profiles: &Result<Vec<T>, E>) -> bool {
    match profiles {
        Ok(members) => members.len() > 1,
        Err(_) => true,
    }
}

/// Everything one presence observation needs; fields are private so no caller picks the window.
pub struct PresenceInputs<'a> {
    sessions: &'a [PresenceEvidence<'a>],
    household_has_multiple_members: bool,
    /// Always [`PRESENCE_WINDOW`]; a field only so this module's tests can name their own window.
    presence_window: Duration,
    now: DateTime<Utc>,
}

impl<'a> PresenceInputs<'a> {
    /// Inputs for one publisher poll; the window is supplied here so the loop has none to name.
    pub fn for_poll(
        sessions: &'a [PresenceEvidence<'a>],
        household_has_multiple_members: bool,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            sessions,
            household_has_multiple_members,
            presence_window: PRESENCE_WINDOW,
            now,
        }
    }
}

/// The evidence behind one believed-present member.
#[derive(Debug, Clone, PartialEq)]
struct Believed {
    source: IdentificationSource,
    confidence: Option<f32>,
    session_id: String,
    last_activity: DateTime<Utc>,
}

impl Believed {
    /// Stronger rung wins, then the newer one; the same order as `SessionIdentity::supersedes`.
    fn beats(&self, held: &Believed) -> bool {
        match self.source.rank().cmp(&held.source.rank()) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => self.last_activity > held.last_activity,
        }
    }

    /// Report this belief as an edge in `transition`'s direction.
    fn transition(
        &self,
        profile_id: &str,
        transition: PresenceTransition,
        at: DateTime<Utc>,
    ) -> ProfilePresence {
        ProfilePresence {
            profile_id: profile_id.to_string(),
            transition,
            source: self.source,
            confidence: self.confidence,
            session_id: self.session_id.clone(),
            at,
        }
    }
}

/// Turns attributed conversations into [`ProfilePresence`] edges.
#[derive(Debug, Clone, Default)]
pub struct PresenceObserver {
    /// Who the pond believes is here, and on what; `None` until the first poll sets the baseline,
    /// which publishes nothing so a restart isn't everybody arriving.
    believed: Option<BTreeMap<String, Believed>>,
}

impl PresenceObserver {
    /// Begin observing a pond whose current occupancy is unknown.
    pub fn awaiting_baseline() -> Self {
        Self::default()
    }

    /// Fold one poll in; returns arrivals then departures, each in profile-id order.
    pub fn observe(&mut self, inputs: PresenceInputs<'_>) -> Vec<ProfilePresence> {
        let present = present_members(&inputs);
        let Some(previous) = self.believed.replace(present.clone()) else {
            return Vec::new(); // baseline
        };

        let mut out = Vec::new();
        for (profile_id, evidence) in &present {
            if !previous.contains_key(profile_id) {
                out.push(evidence.transition(profile_id, PresenceTransition::Arrived, inputs.now));
            }
        }
        for (profile_id, evidence) in &previous {
            if !present.contains_key(profile_id) {
                out.push(evidence.transition(profile_id, PresenceTransition::Departed, inputs.now));
            }
        }
        out
    }

    /// Fold in a possibly failed read: `Err` publishes nothing and leaves the belief untouched.
    pub fn observe_read<E>(&mut self, read: Result<PresenceInputs<'_>, E>) -> Vec<ProfilePresence> {
        match read {
            Ok(inputs) => self.observe(inputs),
            Err(_) => Vec::new(),
        }
    }
}

/// Who the evidence says is here, right now.
fn present_members(inputs: &PresenceInputs<'_>) -> BTreeMap<String, Believed> {
    let mut present: BTreeMap<String, Believed> = BTreeMap::new();

    for evidence in inputs.sessions {
        // `PUT /sessions/{id}/user` binds any id, cron sessions included: skip machine rows.
        if !evidence.origin.is_human() {
            continue;
        }
        if !is_fresh(evidence.last_activity, inputs.presence_window, inputs.now) {
            continue;
        }

        // The authorisation resolver, not a second opinion: presence must agree with access.
        let resolved = identity_resolution::resolve(&identity_resolution::ResolutionInputs {
            // A timer has no request token; the paired-device rung arrives via the session row.
            paired_device_profile: None,
            session: evidence.identity,
            household_has_multiple_members: inputs.household_has_multiple_members,
        });
        let ProfileScope::Owner(profile_id) = resolved.scope else {
            continue;
        };
        // A blank id (a defaulted field) names nobody.
        if profile_id.trim().is_empty() {
            continue;
        }

        let candidate = Believed {
            source: resolved.source,
            confidence: evidence.identity.confidence,
            session_id: evidence.session_id.to_string(),
            last_activity: evidence.last_activity,
        };
        match present.entry(profile_id) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if candidate.beats(slot.get()) {
                    slot.insert(candidate);
                }
            }
        }
    }
    present
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid timestamp")
    }

    const THRESHOLD: Duration = Duration::from_secs(15 * 60);

    /// Shaped like `AgentScheduleExecutor::run_agent_prompt`'s `sched-{task_id}-{unix_ts}` ids.
    const A_CRON_FIRE: &str = "sched-morning-summary-1700000300";

    fn inputs(idle_secs: u64) -> ActivityInputs {
        ActivityInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(idle_secs),
            idle_threshold: THRESHOLD,
            now: t(0),
        }
    }

    fn phases(events: &[SessionLifecycle]) -> Vec<SessionPhase> {
        events.iter().map(|e| e.phase).collect()
    }

    fn session(id: &str, created_at: DateTime<Utc>, updated_at: DateTime<Utc>) -> Session {
        let mut session = Session::new(id.to_string());
        session.created_at = created_at;
        session.updated_at = updated_at;
        session
    }

    fn start(id: &str, created_at: DateTime<Utc>) -> SessionStart<'_> {
        SessionStart {
            id,
            created_at,
            origin: SessionOrigin::of(id),
        }
    }

    fn observer_over(sessions: Vec<Session>) -> ActivityObserver {
        ActivityObserver::seeded_from::<()>(Ok(sessions))
    }

    fn untouched_clock(now: DateTime<Utc>) -> PollClock {
        let started_at = Instant::now();
        PollClock {
            started_at,
            started_at_utc: t(0),
            in_process_at: started_at,
            now,
            idle_threshold: THRESHOLD,
        }
    }

    // ── Origin ───────────────────────────────────────────────────────────

    #[test]
    fn the_scheduler_s_own_conversations_are_not_a_person() {
        assert_eq!(SessionOrigin::of(A_CRON_FIRE), SessionOrigin::Machine);
        assert!(!SessionOrigin::of(A_CRON_FIRE).is_human());

        // Human-path ids: a dashboard UUID, the CLI's name, the voice child's id.
        for human in [
            "9f0c3f4e-6b1a-4a1e-9a6f-0c1d2e3f4a5b",
            "cli-chat",
            "voice-session",
            "unscheduled",
        ] {
            assert_eq!(
                SessionOrigin::of(human),
                SessionOrigin::Human,
                "{human} is a person's conversation and must be readable as presence"
            );
        }
    }

    // ── Projection ───────────────────────────────────────────────────────

    #[test]
    fn a_scheduled_run_reaches_neither_the_arrival_list_nor_the_activity_clock() {
        let rows = vec![session(A_CRON_FIRE, t(300), t(300))];
        let activity = human_activity(&rows);
        assert!(
            activity.starts.is_empty(),
            "the pond announced its own cron fire as a conversation somebody started: {:?}",
            activity.starts
        );
        assert_eq!(
            activity.newest_activity, None,
            "a cron fire bumped the activity clock, so the pond believes the user is here"
        );

        // Vacuity control: the same row with a person's id is kept.
        let human = vec![session("sess-human", t(300), t(300))];
        let activity = human_activity(&human);
        assert_eq!(activity.starts.len(), 1);
        assert_eq!(activity.newest_activity, Some(t(300)));
    }

    #[test]
    fn a_mixed_store_keeps_only_the_person_s_rows() {
        let rows = vec![
            session("sess-human", t(100), t(100)),
            session(A_CRON_FIRE, t(300), t(300)),
        ];
        let activity = human_activity(&rows);
        assert_eq!(
            activity.starts.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec!["sess-human"]
        );
        assert_eq!(
            activity.newest_activity,
            Some(t(100)),
            "the newest row is the pond's own, and taking it would report activity nobody \
             performed"
        );
    }

    // ── The never-at-startup gate ────────────────────────────────────────

    #[test]
    fn a_scheduled_run_does_not_open_the_never_at_startup_gate() {
        let clock = untouched_clock(t(600));
        let machine = vec![session(A_CRON_FIRE, t(300), t(300))];
        assert!(
            !poll_inputs(&machine, clock).saw_activity_since_start,
            "a cron fire opened the never-at-startup gate: the pond has seen nobody, and the \
             next quiet period will be published as the user going away"
        );

        // Vacuity control: the same row with a person's id opens it.
        let human = vec![session("sess-human", t(300), t(300))];
        assert!(
            poll_inputs(&human, clock).saw_activity_since_start,
            "a real conversation persisted by the voice child must still count as activity"
        );
    }

    #[test]
    fn an_http_route_still_opens_the_gate_with_no_sessions_at_all() {
        let started_at = Instant::now();
        let clock = PollClock {
            started_at,
            started_at_utc: t(0),
            in_process_at: started_at + Duration::from_millis(1),
            now: t(600),
            idle_threshold: THRESHOLD,
        };
        assert!(poll_inputs(&[], clock).saw_activity_since_start);
    }

    // ── Arrivals ─────────────────────────────────────────────────────────

    #[test]
    fn a_new_conversation_is_announced_with_its_id() {
        let mut obs = observer_over(vec![]);
        let clock = untouched_clock(t(10));
        let events = obs.poll(&[session("sess-new", t(5), t(5))], clock);
        assert_eq!(phases(&events), vec![SessionPhase::Started]);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-new"));
    }

    #[test]
    fn a_cron_fire_is_never_announced_as_somebody_arriving() {
        let mut obs = observer_over(vec![]);
        let clock = untouched_clock(t(400));
        let events = obs.poll(&[session(A_CRON_FIRE, t(300), t(300))], clock);
        assert!(
            events.is_empty(),
            "a scheduled task published {events:?}; P4 reads a Started as a person arriving"
        );

        let mut control = observer_over(vec![]);
        assert_eq!(
            phases(&control.poll(&[session("sess-human", t(300), t(300))], clock)),
            vec![SessionPhase::Started],
            "the same poll with a person's id must still announce the arrival"
        );
    }

    #[test]
    fn the_observer_itself_refuses_a_machine_session() {
        let mut obs = observer_over(vec![]);
        let announced = obs.observe(&[start(A_CRON_FIRE, t(300))], inputs(0));
        assert!(
            announced.is_empty(),
            "a machine-origin start reached the transition rules and was announced: {announced:?}"
        );

        // Vacuity control: a person's row does announce.
        assert_eq!(
            phases(&obs.observe(&[start("sess-human", t(300))], inputs(0))),
            vec![SessionPhase::Started]
        );
    }

    #[test]
    fn a_restart_does_not_announce_the_existing_history() {
        let existing = vec![
            session("old-1", t(-300), t(-300)),
            session("old-2", t(-200), t(-200)),
        ];
        let mut obs = observer_over(existing.clone());
        let announced = obs.poll(&existing, untouched_clock(t(0)));
        assert!(
            announced.is_empty(),
            "a restart announced {} conversation(s) that already existed as newly started: {:?}",
            announced.len(),
            announced
                .iter()
                .map(|e| e.session_id.as_deref().unwrap_or("-"))
                .collect::<Vec<_>>()
        );

        // Vacuity control: without the seed, the same list is announced.
        let mut unseeded = observer_over(vec![]);
        assert_eq!(unseeded.poll(&existing, untouched_clock(t(0))).len(), 2);
    }

    #[test]
    fn a_failed_read_takes_its_baseline_from_the_first_poll_that_works() {
        let existing = vec![
            session("old-1", t(-300), t(-300)),
            session("old-2", t(-200), t(-200)),
        ];
        let mut obs = ActivityObserver::seeded_from::<&str>(Err("db is busy"));
        assert!(
            obs.poll(&existing, untouched_clock(t(0))).is_empty(),
            "a failed startup read replayed the pond's history as arrivals"
        );

        // The baseline was taken: a later conversation is still announced.
        let mut later = existing.clone();
        later.push(session("sess-new", t(60), t(60)));
        assert_eq!(
            phases(&obs.poll(&later, untouched_clock(t(60)))),
            vec![SessionPhase::Started]
        );
    }

    /// SQLite's `datetime('now')` has no fractional part, so same-second rows share a timestamp.
    #[test]
    fn two_conversations_in_the_same_second_are_both_announced() {
        let mut obs = observer_over(vec![session("first", t(0), t(0))]);
        let same_second = vec![
            session("first", t(0), t(0)),
            session("second", t(0), t(0)),
            session("third", t(0), t(0)),
        ];
        let announced: Vec<String> = obs
            .poll(&same_second, untouched_clock(t(1)))
            .iter()
            .filter_map(|e| e.session_id.clone())
            .collect();
        assert_eq!(
            announced,
            vec!["second", "third"],
            "a conversation that began in the same second as the last one it knew about was \
             never announced"
        );
    }

    /// `parse_dt` turns an unparseable `created_at` into `Utc::now()`, so it moves every poll.
    #[test]
    fn a_conversation_whose_timestamp_keeps_moving_is_announced_once() {
        let mut obs = observer_over(vec![]);
        let mut announcements = 0;
        for poll in 0..5 {
            let drifting = vec![session("sess-unparseable", t(poll * 60), t(poll * 60))];
            announcements += obs.poll(&drifting, untouched_clock(t(poll * 60))).len();
        }
        assert_eq!(
            announcements, 1,
            "one conversation was announced {announcements} times because its timestamp moved"
        );
    }

    #[test]
    fn each_conversation_is_announced_once_and_oldest_first() {
        let mut obs = observer_over(vec![]);
        let batch = vec![
            session("second", t(20), t(20)),
            session("first", t(10), t(10)),
        ];
        let events = obs.poll(&batch, untouched_clock(t(30)));
        assert_eq!(
            events
                .iter()
                .map(|e| e.session_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["first", "second"],
            "announced in the order the conversations began"
        );
        assert!(
            obs.poll(&batch, untouched_clock(t(30))).is_empty(),
            "both conversations are already accounted for"
        );
    }

    // ── The activity clock ───────────────────────────────────────────────

    /// The polls must cross the idle threshold over realistic cron rows, or this passes with the
    /// `saw_activity_since_start` gate deleted.
    #[test]
    fn an_untouched_pond_publishes_nothing() {
        let mut obs = observer_over(vec![]);
        let mut crossed_the_threshold = false;
        for poll in 0..10 {
            let idle_for = Duration::from_secs(10 * 60 * poll);
            crossed_the_threshold |= idle_for >= THRESHOLD;
            // One row per cron fire, none of them a person.
            let cron_fires: Vec<Session> = (0..=poll)
                .map(|n| {
                    let id = format!("sched-morning-summary-{n}");
                    session(&id, t(n as i64 * 600), t(n as i64 * 600))
                })
                .collect();
            // `idle_for` is stepped by hand: an `Instant` can't be moved into the past.
            let gate = poll_inputs(&cron_fires, untouched_clock(t(poll as i64 * 600)))
                .saw_activity_since_start;
            let events = obs.observe(
                &human_activity(&cron_fires).starts,
                ActivityInputs {
                    saw_activity_since_start: gate,
                    idle_for,
                    ..inputs(0)
                },
            );
            assert!(
                events.is_empty(),
                "an untouched pond published {events:?} at poll {poll}, {}s idle",
                idle_for.as_secs()
            );
        }
        assert!(
            crossed_the_threshold,
            "this test proves nothing unless its polls reach {}s of idle",
            THRESHOLD.as_secs()
        );
    }

    /// Vacuity control for the test above; the first poll is past the threshold on purpose, so an
    /// ungated observer would baseline `Idle` and then announce a false `Resumed`.
    #[test]
    fn the_same_observer_does_publish_once_activity_is_real() {
        let mut obs = observer_over(vec![]);
        assert!(
            obs.observe(
                &[],
                ActivityInputs {
                    saw_activity_since_start: false,
                    idle_for: Duration::from_secs(30 * 60),
                    ..inputs(0)
                }
            )
            .is_empty(),
            "an untouched pond is not an idle user, however long it has sat"
        );
        assert!(
            obs.observe(&[], inputs(0)).is_empty(),
            "the first observation after real activity is a baseline, not a Resumed"
        );
        assert_eq!(
            phases(&obs.observe(&[], inputs(16 * 60))),
            vec![SessionPhase::Idle],
            "and once there is a baseline to depart from, going quiet is published"
        );
    }

    #[test]
    fn the_first_observation_is_a_baseline_not_an_event() {
        let mut obs = observer_over(vec![]);
        assert!(obs.observe(&[], inputs(0)).is_empty());

        let mut already_quiet = observer_over(vec![]);
        assert!(
            already_quiet.observe(&[], inputs(60 * 60)).is_empty(),
            "a first poll that is already past the threshold is still a baseline"
        );
    }

    #[test]
    fn going_quiet_publishes_idle_exactly_once() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        assert_eq!(
            phases(&obs.observe(&[], inputs(15 * 60))),
            vec![SessionPhase::Idle],
            "idle fires the moment the threshold is reached"
        );
        assert!(
            obs.observe(&[], inputs(60 * 60)).is_empty(),
            "staying quiet is not a new transition"
        );
    }

    #[test]
    fn coming_back_publishes_resumed() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        obs.observe(&[], inputs(20 * 60));
        assert_eq!(
            phases(&obs.observe(&[], inputs(5))),
            vec![SessionPhase::Resumed]
        );
        assert!(
            obs.observe(&[], inputs(5)).is_empty(),
            "staying active is not a new transition"
        );
    }

    #[test]
    fn idle_secs_reports_the_quiet_before_the_transition() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        let idle = obs.observe(&[], inputs(17 * 60)).remove(0);
        assert_eq!(idle.idle_secs, 17 * 60);
        assert_eq!(
            idle.session_id, None,
            "the activity clock is not per-session"
        );
    }

    #[test]
    fn a_new_conversation_while_idle_says_started_not_resumed() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        assert_eq!(
            phases(&obs.observe(&[], inputs(30 * 60))),
            vec![SessionPhase::Idle]
        );

        let events = obs.observe(&[start("after-the-gap", t(60))], inputs(0));
        assert_eq!(phases(&events), vec![SessionPhase::Started]);
    }

    #[test]
    fn phase_labels_are_stable() {
        assert_eq!(SessionPhase::Started.as_str(), "started");
        assert_eq!(SessionPhase::Idle.as_str(), "idle");
        assert_eq!(SessionPhase::Resumed.as_str(), "resumed");
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;

    const WINDOW: Duration = PRESENCE_WINDOW;

    /// A real scheduler id, which `PUT /sessions/{id}/user` will still bind to a member.
    const A_CRON_FIRE: &str = "sched-morning-summary-1700000300";

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid timestamp")
    }

    /// Begun and last spoken at the same moment; use [`long_running`] when the two clocks matter.
    fn session(id: &str, updated_at: DateTime<Utc>) -> Session {
        long_running(id, updated_at, updated_at)
    }

    fn long_running(id: &str, created_at: DateTime<Utc>, updated_at: DateTime<Utc>) -> Session {
        let mut session = Session::new(id.to_string());
        session.created_at = created_at;
        session.updated_at = updated_at;
        session.profile_id = None; // the observer reads the identity, not this
        session
    }

    /// A row bound to a member: `Some(profile_id)` is what makes an identity read worth issuing.
    fn attributed(id: &str, updated_at: DateTime<Utc>) -> Session {
        let mut session = session(id, updated_at);
        session.profile_id = Some("jerry".to_string());
        session
    }

    fn identity(source: IdentificationSource, who: Option<&str>) -> SessionIdentity {
        SessionIdentity {
            profile_id: who.map(str::to_string),
            source,
            confidence: match source {
                IdentificationSource::Face => Some(0.71),
                _ => None,
            },
        }
    }

    fn poll(
        observer: &mut PresenceObserver,
        sessions: &[PresenceEvidence<'_>],
        household_has_multiple_members: bool,
        now: DateTime<Utc>,
    ) -> Vec<ProfilePresence> {
        observer.observe(PresenceInputs {
            sessions,
            household_has_multiple_members,
            presence_window: WINDOW,
            now,
        })
    }

    /// An observer that has already taken its baseline over `sessions`.
    fn seeded(
        sessions: &[PresenceEvidence<'_>],
        household_has_multiple_members: bool,
        now: DateTime<Utc>,
    ) -> PresenceObserver {
        let mut observer = PresenceObserver::awaiting_baseline();
        let published = poll(&mut observer, sessions, household_has_multiple_members, now);
        assert!(
            published.is_empty(),
            "the first observation is a baseline and must publish nothing, not {published:?}"
        );
        observer
    }

    fn named(events: &[ProfilePresence]) -> Vec<(&str, PresenceTransition)> {
        events
            .iter()
            .map(|e| (e.profile_id.as_str(), e.transition))
            .collect()
    }

    // ── Arrival ──────────────────────────────────────────────────────────

    #[test]
    fn a_member_who_starts_talking_arrives_once_and_the_event_names_them() {
        let mut observer = seeded(&[], true, t(0));

        let row = session("sess-jerry", t(60));
        let who = identity(IdentificationSource::Face, Some("jerry"));
        let evidence = [PresenceEvidence::of(&row, &who)];

        let events = poll(&mut observer, &evidence, true, t(70));
        assert_eq!(named(&events), vec![("jerry", PresenceTransition::Arrived)]);
        assert_eq!(events[0].source, IdentificationSource::Face);
        assert_eq!(
            events[0].confidence,
            Some(0.71),
            "a face match's confidence must survive -- it is how a proposer weighs the claim"
        );
        assert_eq!(events[0].session_id, "sess-jerry");
        assert_eq!(events[0].at, t(70));

        assert!(
            poll(&mut observer, &evidence, true, t(80)).is_empty(),
            "still being here is not a new arrival"
        );
    }

    // ── The column presence is keyed on ──────────────────────────────────

    /// No mirror fixture (`updated_at` before `created_at`): production can't produce that row.
    #[test]
    fn presence_is_keyed_on_when_somebody_last_spoke_not_on_when_the_conversation_began() {
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));

        // Opened three hours ago, spoken in ten seconds ago.
        let live = long_running("sess-long", t(0) - chrono::Duration::hours(3), t(0));
        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&live, &jerry)],
                true,
                t(10)
            )),
            vec![("jerry", PresenceTransition::Arrived)],
            "a conversation opened three hours ago and spoken in ten seconds ago is somebody in \
             the room; keying presence on created_at instead of updated_at loses every \
             conversation older than the window and reports the member as gone"
        );

        // Vacuity control: the same conversation, silent for three hours, publishes nothing.
        let quiet = long_running(
            "sess-long-quiet",
            t(0) - chrono::Duration::hours(3),
            t(0) - chrono::Duration::hours(3),
        );
        let mut control = seeded(&[], true, t(0));
        assert!(
            poll(
                &mut control,
                &[PresenceEvidence::of(&quiet, &jerry)],
                true,
                t(10)
            )
            .is_empty(),
            "a conversation nobody has spoken in for three hours was published as presence"
        );
    }

    // ── The two invariants that decide who may be named ──────────────────

    #[test]
    fn an_unidentified_speaker_in_a_one_member_pond_is_not_presence() {
        let mut observer = seeded(&[], false, t(0));

        let row = session("sess-anon", t(60));
        let nobody = SessionIdentity::unknown();
        let events = poll(
            &mut observer,
            &[PresenceEvidence::of(&row, &nobody)],
            false,
            t(70),
        );
        assert!(
            events.is_empty(),
            "an unidentified speaker was published as {events:?}; the resolver answered \
             Household, which is a scope and not a person"
        );

        // Vacuity control: the same pond and poll with an identified speaker.
        let mut control = seeded(&[], false, t(0));
        let who = identity(IdentificationSource::Explicit, Some("jerry"));
        assert_eq!(
            named(&poll(
                &mut control,
                &[PresenceEvidence::of(&row, &who)],
                false,
                t(70)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );
    }

    /// An unidentified speaker in a shared pond resolves to `Guest`.
    #[test]
    fn a_guest_is_not_presence() {
        let mut observer = seeded(&[], true, t(0));
        let row = session("sess-visitor", t(60));
        let nobody = SessionIdentity::unknown();
        let events = poll(
            &mut observer,
            &[PresenceEvidence::of(&row, &nobody)],
            true,
            t(70),
        );
        assert!(
            events.is_empty(),
            "a guest session published {events:?}; invariant 5 says a Guest generates none"
        );
    }

    #[test]
    fn a_profile_id_with_no_source_is_not_presence() {
        let mut observer = seeded(&[], true, t(0));
        let row = session("sess-odd", t(60));
        let unsourced = identity(IdentificationSource::Unknown, Some("jerry"));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &unsourced)],
                true,
                t(70)
            )
            .is_empty(),
            "a profile id with no source was trusted as a person being home"
        );
    }

    #[test]
    fn a_blank_profile_id_names_nobody() {
        let mut observer = seeded(&[], true, t(0));
        let row = session("sess-blank", t(60));
        let blank = identity(IdentificationSource::Explicit, Some("   "));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &blank)],
                true,
                t(70)
            )
            .is_empty(),
            "a blank profile id was published as a member arriving"
        );
    }

    // ── What produces an identification that is not a person arriving ────

    #[test]
    fn a_scheduled_run_attributed_to_a_member_is_not_that_member_being_home() {
        let mut observer = seeded(&[], true, t(0));
        let cron = session(A_CRON_FIRE, t(60));
        let who = identity(IdentificationSource::Explicit, Some("jerry"));
        let events = poll(
            &mut observer,
            &[PresenceEvidence::of(&cron, &who)],
            true,
            t(70),
        );
        assert!(
            events.is_empty(),
            "the pond's own conversation was published as {events:?}; a proposer reads an \
             Arrived as a household member walking in"
        );

        // Vacuity control: the same identity on a person's session id does arrive.
        let mut control = seeded(&[], true, t(0));
        let human = session("sess-human", t(60));
        assert_eq!(
            named(&poll(
                &mut control,
                &[PresenceEvidence::of(&human, &who)],
                true,
                t(70)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );
    }

    #[test]
    fn a_face_bound_to_a_conversation_that_went_quiet_hours_ago_is_not_a_person_in_the_room() {
        let mut observer = seeded(&[], true, t(0));
        let stale = session("sess-yesterday", t(60) - chrono::Duration::hours(3));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&stale, &liz)],
                true,
                t(70)
            )
            .is_empty(),
            "a face bound to a three-hour-old conversation was published as somebody arriving"
        );

        // Vacuity control: the same face on a live conversation does arrive.
        let mut control = seeded(&[], true, t(0));
        let live = session("sess-now", t(60));
        assert_eq!(
            named(&poll(
                &mut control,
                &[PresenceEvidence::of(&live, &liz)],
                true,
                t(70)
            )),
            vec![("liz", PresenceTransition::Arrived)]
        );
    }

    #[test]
    fn the_freshness_window_is_closed_at_its_far_end() {
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let window = i64::try_from(WINDOW.as_secs()).unwrap();

        let just_inside = session("sess-inside", t(0));
        let mut a = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut a,
                &[PresenceEvidence::of(&just_inside, &jerry)],
                true,
                t(window - 1)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        let at_the_edge = session("sess-edge", t(0));
        let mut b = seeded(&[], true, t(0));
        assert!(
            poll(
                &mut b,
                &[PresenceEvidence::of(&at_the_edge, &jerry)],
                true,
                t(window)
            )
            .is_empty(),
            "evidence exactly as old as the window still counted as somebody being here; the \
             pond publishes Idle at the same instant, so the two would disagree"
        );
    }

    // ── Departure ────────────────────────────────────────────────────────

    #[test]
    fn evidence_going_stale_publishes_one_departure() {
        let row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let evidence = [PresenceEvidence::of(&row, &jerry)];

        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(&mut observer, &evidence, true, t(10))),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        let gone = poll(&mut observer, &evidence, true, t(20 * 60));
        assert_eq!(named(&gone), vec![("jerry", PresenceTransition::Departed)]);
        assert_eq!(
            gone[0].session_id, "sess-jerry",
            "a departure names the conversation that went quiet"
        );
        assert_eq!(
            gone[0].source,
            IdentificationSource::Explicit,
            "and the evidence the belief had rested on"
        );

        assert!(
            poll(&mut observer, &evidence, true, t(60 * 60)).is_empty(),
            "staying away is not a second departure"
        );
    }

    /// A release (`DELETE /sessions/{id}/user`) is a departure: the pond can no longer name them.
    #[test]
    fn releasing_a_binding_ends_the_belief() {
        let row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &jerry)],
                true,
                t(10)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        let released = SessionIdentity::unknown();
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &released)],
                true,
                t(20)
            )),
            vec![("jerry", PresenceTransition::Departed)],
            "the binding was released and the pond went on believing he was here"
        );
    }

    // ── Re-identification is not an arrival ──────────────────────────────

    #[test]
    fn re_identifying_somebody_already_here_publishes_nothing() {
        let first = session("sess-one", t(0));
        let face = identity(IdentificationSource::Face, Some("jerry"));
        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&first, &face)],
                true,
                t(10)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        // The same session, re-identified more strongly.
        let explicit = identity(IdentificationSource::Explicit, Some("jerry"));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&first, &explicit)],
                true,
                t(20)
            )
            .is_empty(),
            "an upgrade from a face match to an explicit binding is the same person, still here"
        );

        // A second conversation opened by the same person.
        let second = session("sess-two", t(30));
        assert!(
            poll(
                &mut observer,
                &[
                    PresenceEvidence::of(&first, &explicit),
                    PresenceEvidence::of(&second, &explicit),
                ],
                true,
                t(40)
            )
            .is_empty(),
            "opening a second conversation is not arriving twice"
        );
    }

    #[test]
    fn the_strongest_evidence_wins_when_two_conversations_name_one_member() {
        let weak_row = session("sess-face", t(30));
        let strong_row = session("sess-explicit", t(0));
        let weak = identity(IdentificationSource::Face, Some("jerry"));
        let strong = identity(IdentificationSource::Explicit, Some("jerry"));

        for order in [
            [
                PresenceEvidence::of(&weak_row, &weak),
                PresenceEvidence::of(&strong_row, &strong),
            ],
            [
                PresenceEvidence::of(&strong_row, &strong),
                PresenceEvidence::of(&weak_row, &weak),
            ],
        ] {
            let mut observer = seeded(&[], true, t(0));
            let events = poll(&mut observer, &order, true, t(40));
            assert_eq!(named(&events), vec![("jerry", PresenceTransition::Arrived)]);
            assert_eq!(
                events[0].source,
                IdentificationSource::Explicit,
                "the weaker rung won on ordering; the newer face row is the one that would"
            );
            assert_eq!(events[0].session_id, "sess-explicit");
        }
    }

    /// Must match `SessionIdentity::supersedes`, where an equal-rung write replaces the old one.
    #[test]
    fn on_an_equal_rung_the_conversation_spoken_in_most_recently_wins() {
        let older = session("sess-older", t(0));
        let newer = session("sess-newer", t(30));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));

        for order in [
            [
                PresenceEvidence::of(&older, &jerry),
                PresenceEvidence::of(&newer, &jerry),
            ],
            [
                PresenceEvidence::of(&newer, &jerry),
                PresenceEvidence::of(&older, &jerry),
            ],
        ] {
            let mut observer = seeded(&[], true, t(0));
            let events = poll(&mut observer, &order, true, t(40));
            assert_eq!(named(&events), vec![("jerry", PresenceTransition::Arrived)]);
            assert_eq!(
                events[0].session_id, "sess-newer",
                "two conversations at the same rung named one member and the stale one won; \
                 `SessionIdentity::supersedes` breaks that tie the other way, so the event and \
                 the session row would disagree about which claim is current"
            );
        }
    }

    // ── A restart is not everybody arriving ──────────────────────────────

    #[test]
    fn a_restart_does_not_announce_the_household_as_arriving() {
        let jerry_row = session("sess-jerry", t(0));
        let liz_row = session("sess-liz", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        let live = [
            PresenceEvidence::of(&jerry_row, &jerry),
            PresenceEvidence::of(&liz_row, &liz),
        ];

        let mut observer = PresenceObserver::awaiting_baseline();
        let first = poll(&mut observer, &live, true, t(10));
        assert!(
            first.is_empty(),
            "a restart announced {first:?}; the pond restarts on every deploy and nobody \
             crossed a threshold"
        );

        // The baseline was taken: the departure that follows is still published.
        assert_eq!(
            named(&poll(&mut observer, &live, true, t(30 * 60))),
            vec![
                ("jerry", PresenceTransition::Departed),
                ("liz", PresenceTransition::Departed),
            ]
        );
    }

    /// Vacuity control for the restart test above.
    #[test]
    fn the_same_rows_do_arrive_once_a_baseline_exists() {
        let jerry_row = session("sess-jerry", t(0));
        let liz_row = session("sess-liz", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let liz = identity(IdentificationSource::Face, Some("liz"));

        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[
                    PresenceEvidence::of(&jerry_row, &jerry),
                    PresenceEvidence::of(&liz_row, &liz),
                ],
                true,
                t(10)
            )),
            vec![
                ("jerry", PresenceTransition::Arrived),
                ("liz", PresenceTransition::Arrived),
            ],
            "each member is addressed by name, arrivals in profile-id order"
        );
    }

    #[test]
    fn one_member_replacing_another_publishes_both_edges_arrival_first() {
        let jerry_row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let mut observer = seeded(&[], true, t(0));
        poll(
            &mut observer,
            &[PresenceEvidence::of(&jerry_row, &jerry)],
            true,
            t(10),
        );

        let liz_row = session("sess-liz", t(20 * 60));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[
                    PresenceEvidence::of(&jerry_row, &jerry),
                    PresenceEvidence::of(&liz_row, &liz),
                ],
                true,
                t(20 * 60 + 10)
            )),
            vec![
                ("liz", PresenceTransition::Arrived),
                ("jerry", PresenceTransition::Departed),
            ]
        );
    }

    // ── PresenceInputs ───────────────────────────────────────────────────

    #[test]
    fn the_freshness_window_is_the_one_that_decides_idle() {
        assert_eq!(
            PresenceInputs::for_poll(&[], true, t(0)).presence_window,
            Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS),
            "presence ages evidence out on a different clock from the one that publishes Idle, \
             so the two will disagree about whether somebody is still at the pond"
        );
    }

    #[test]
    fn skipping_a_read_never_changes_who_is_published() {
        let now = t(0);
        let window = i64::try_from(WINDOW.as_secs()).unwrap();

        let rows = vec![
            attributed("sess-live", t(-10)),
            attributed("sess-stale", t(-window - 10)),
            attributed(A_CRON_FIRE, t(-10)),
            session("sess-unbound", t(-10)),
        ];
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        let nobody = SessionIdentity::unknown();
        let identity_of = |id: &str| -> &SessionIdentity {
            match id {
                "sess-unbound" => &nobody,
                "sess-stale" => &liz,
                _ => &jerry,
            }
        };

        let candidates = attribution_candidates(&rows, now);
        assert_eq!(
            candidates
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sess-live"],
            "the read-avoidance kept the wrong rows: the pond's own cron fire, a conversation \
             silent past the window, and a row with no attribution to read are each a query \
             whose answer the observer discards"
        );

        let every_row: Vec<PresenceEvidence<'_>> = rows
            .iter()
            .map(|session| PresenceEvidence::of(session, identity_of(&session.id)))
            .collect();
        let read_only_the_candidates: Vec<PresenceEvidence<'_>> = candidates
            .iter()
            .map(|session| PresenceEvidence::of(session, identity_of(&session.id)))
            .collect();

        let mut expensive = seeded(&[], true, t(-60));
        let mut cheap = seeded(&[], true, t(-60));
        let from_every_row = expensive.observe(PresenceInputs::for_poll(&every_row, true, now));
        let from_the_candidates = cheap.observe(PresenceInputs::for_poll(
            &read_only_the_candidates,
            true,
            now,
        ));

        assert_eq!(
            named(&from_every_row),
            vec![("jerry", PresenceTransition::Arrived)],
            "vacuity control: reading every row must still publish somebody, or the comparison \
             below is between two empty lists"
        );
        assert_eq!(
            from_every_row, from_the_candidates,
            "skipping the identity read changed who the pond believes is here; it is an \
             optimisation and not a gate, so the two must agree exactly"
        );
    }

    #[test]
    fn a_household_count_that_cannot_be_read_answers_more_than_one() {
        let unreadable: Result<Vec<u8>, ()> = Err(());
        assert!(
            household_has_multiple_members(&unreadable),
            "a profile list that could not be read was answered with `one member`, which is the \
             value that turns an unidentified speaker into the whole household"
        );

        // Vacuity control: a successful read answers honestly.
        assert!(!household_has_multiple_members::<u8, ()>(&Ok(vec![])));
        assert!(!household_has_multiple_members::<u8, ()>(&Ok(vec![1])));
        assert!(household_has_multiple_members::<u8, ()>(&Ok(vec![1, 2])));
    }

    /// Tripwire: the count only matters for `Household`/`Guest`, which presence never publishes.
    #[test]
    fn the_household_count_cannot_change_a_published_presence_event() {
        let named_row = session("sess-jerry", t(0));
        let anon_row = session("sess-anon", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let nobody = SessionIdentity::unknown();
        let evidence = [
            PresenceEvidence::of(&named_row, &jerry),
            PresenceEvidence::of(&anon_row, &nobody),
        ];

        let mut shared = seeded(&[], true, t(0));
        let mut alone = seeded(&[], false, t(0));
        let with_guests = poll(&mut shared, &evidence, true, t(10));
        let one_member = poll(&mut alone, &evidence, false, t(10));

        assert_eq!(
            named(&with_guests),
            vec![("jerry", PresenceTransition::Arrived)],
            "vacuity control: an attributed row must still publish, or this compares two empty \
             lists and would pass against an observer that has stopped speaking"
        );
        assert_eq!(
            with_guests, one_member,
            "the household count now changes a presence event, which it could not before; \
             whatever depends on it needs `household_has_multiple_members`'s failure direction \
             to be load-bearing rather than merely correct"
        );
    }

    #[test]
    fn a_failed_read_publishes_nothing_and_leaves_the_belief_standing() {
        let row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let evidence = [PresenceEvidence::of(&row, &jerry)];

        // Direction one: the outage must not empty the house.
        let mut still_here = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(&mut still_here, &evidence, true, t(10))),
            vec![("jerry", PresenceTransition::Arrived)]
        );
        assert!(
            still_here.observe_read::<()>(Err(())).is_empty(),
            "an unreadable store published a transition of its own"
        );
        assert!(
            poll(&mut still_here, &evidence, true, t(20)).is_empty(),
            "the failed read was folded in as an empty house, so a member who never left \
             departed and then arrived again when the store came back"
        );

        // Direction two: nor may it forget, or the following departure is swallowed.
        let mut departs = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(&mut departs, &evidence, true, t(10))),
            vec![("jerry", PresenceTransition::Arrived)]
        );
        assert!(departs.observe_read::<()>(Err(())).is_empty());
        assert_eq!(
            named(&poll(&mut departs, &evidence, true, t(20 * 60))),
            vec![("jerry", PresenceTransition::Departed)],
            "the failed read dropped the belief it was holding, so the departure that followed \
             was never published"
        );

        // Vacuity control: an `Ok` read is still just `observe`.
        let mut ok = seeded(&[], true, t(0));
        assert_eq!(
            named(&ok.observe_read::<()>(Ok(PresenceInputs::for_poll(&evidence, true, t(10))))),
            vec![("jerry", PresenceTransition::Arrived)]
        );
    }

    #[test]
    fn transition_labels_are_stable() {
        assert_eq!(PresenceTransition::Arrived.as_str(), "arrived");
        assert_eq!(PresenceTransition::Departed.as_str(), "departed");
    }
}
