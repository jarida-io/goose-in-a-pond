//! The proactive reviewer — PAI-7 P4 — and what its feedback teaches — P7.
//!
//! Section 3.3: a `proactive-reviewer` role that runs in idle time over what
//! has happened and emits **proposals, never actions**. This module is the
//! decision half of that: when a review may start, what the child is allowed to
//! be, how its answer becomes proposals, and which proposals the member has
//! already said no to.
//!
//! # Nothing here performs I/O, and since 2026-08-11 all of it runs
//!
//! Pure functions over plain values, in the same shape as
//! [`consolidation_schedule`](super::consolidation_schedule) and for the same
//! reason: the three rules that broke consolidation were untestable while they
//! lived inside a loop. The loop that calls this is
//! `pond-server`'s `run_proactive_reviewer`, and **it exists** — this paragraph
//! said it did not for one phase, which was true and had to stop being true for
//! the workstream to mean anything.
//!
//! That the loop is reached is asserted rather than assumed:
//! `pond-infra/tests/proactive_reviewer_is_wired.rs` fails if the `tokio::spawn`
//! goes away. It has to be, because `ci.yml` runs `cargo check -p pond-server`
//! and never `cargo test -p pond-server`, and because a `pub` item in a library
//! crate never earns a `dead_code` warning — the blind spot PAI-1 P5 shipped
//! inert behind for a whole phase.
//!
//! # The reviewer cannot write its own proposal, and that shapes everything
//!
//! PAI-6's `groups_denied_to_subagents` withholds the actuating groups from every
//! child, because nothing in a subagent's turn can resolve it as an actor with
//! the household's authority. So the reviewer **cannot act**, and no amount of
//! role configuration would let it: the groups are subtracted after the role's
//! request, not from it.
//!
//! This paragraph used to argue the point through `giap-draft`, the staging
//! group — a proposal was a `drafts` row and the child could not write one.
//! That group is gone; the property is unchanged and now rests directly on the
//! actuating groups, which is what it always actually rested on.
//!
//! That is not a limitation to work around — it is the safety property. The
//! subagent produces *words*; the loop that spawned it — which holds the
//! `ProposalRepository`, runs with the household's authority, and is not a
//! model — turns those words into rows through [`interpret_answer`], which
//! validates every one of them. A model cannot name the member a proposal is
//! addressed to (it never sees a `ProposalAudience`), cannot set an expiry,
//! cannot exceed the daily cap, and cannot propose anything but a prompt.
//!
//! # Invariants this module is answerable for
//!
//! - **1, GIAP proposes and the user disposes.** The only [`TaskKind`] an
//!   impulse can become is [`TaskKind::AgentPrompt`] — see
//!   [`impulse_action`]. There is no path from model output to a webhook or a
//!   sensor rule, and the child holds no actuating group.
//! - **2, every proposal carries a rationale.** `Proposal::from_parts` refuses
//!   a blank one; an impulse without one never becomes a proposal.
//! - **3, proactive work never runs mid-turn and is cancelled by activity.**
//!   [`should_review`] delegates the timing to `consolidation_schedule`;
//!   [`cancelled_by_activity`] delegates the interruption to the same module's
//!   `saw_activity_since_start`. Neither rule is re-implemented here.
//! - **4 and 5, addressed to one member, never a broadcast and never a guest.**
//!   [`plan_review`] takes a [`ProposalAudience`], which cannot hold `Guest` or
//!   `Household`. A review for an unidentified speaker is not refused at run
//!   time; it is unaskable.
//! - **7, a proposal expires.** [`PROPOSAL_TTL`], applied by this module rather
//!   than by the model.

use crate::shared::domain::orchestration::{
    AgentRole, DelegationAuthority, DelegationRefused, RoleError, TaskRequest, TaskRun, TaskSpec,
    ROLE_YAML_KEY,
};
use crate::shared::domain::session_activity::SessionOrigin;
use crate::shared::ports::event_bus::BusEvent;
use crate::user_data::domain::proposal::{
    BusEventRef, Proposal, ProposalAudience, ProposalDecision, ProposalError, ProposalShape,
    TriggerIdentity,
};
use crate::user_data::domain::schedule::TaskKind;
use crate::user_data::domain::session::Session;
use crate::user_data::services::consolidation_schedule::{
    saw_activity_since_start, should_run, GateDecision, GateInputs, SkipReason,
};
use crate::user_data::services::member_attribution;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

// ── The role ────────────────────────────────────────────────────────────────

/// The role name a review runs under.
pub const PROACTIVE_REVIEWER_ROLE: &str = "proactive-reviewer";

/// The shipped recipe for that role.
///
/// It is a `const` rather than a row seeded into `agent_recipes`, and the
/// difference matters: a role read from the database is one a user can edit,
/// and the thing they would edit first is `tool_groups`. This one is read
/// through the same door as a stored recipe — [`AgentRole::from_recipe_yaml`],
/// which is `deny_unknown_fields` inside the `giap_role` block — so it is
/// validated by the same code, but it cannot be widened at run time.
///
/// **The instructions carry the schema because the child has no other way to
/// learn it.** They also carry the date, via the brief, because
/// `groups_denied_to_subagents` withholds `giap-system` and with it
/// `get_current_time`: a subagent has no clock at all.
pub const REVIEWER_ROLE_RECIPE: &str = r#"
version: 1.0.0
title: Proactive reviewer
description: Looks over what has happened lately and says what might be worth mentioning.
giap_role:
  tool_groups:
    - giap-memory
    - giap-device
    - giap-sensors
    - giap-weather
  personal_data: inherit
  max_turns: 4
  context_fraction: 0.35
  instructions: |
    You are reviewing what has happened in this household recently and deciding
    whether anything is worth raising with the person you are working for.

    You do not act. You cannot turn anything on, write anything down, schedule
    anything or send anything. Everything you suggest is shown to the person,
    who decides. Suggesting something they did not want costs more than saying
    nothing, so when in doubt, say nothing.

    You have no clock and no calendar. The current time is stated in the brief;
    do not guess at it and do not use any other time.

    Answer with a JSON array and nothing else. An empty array is a complete and
    common answer. Each element:

      {
        "trigger_kind": "camera" | "sensor" | "device" | "time" | "presence",
        "source_id": "the device, camera or sensor it came from, or omit",
        "signal": "the reading or event type, or omit",
        "rationale": "why this matters to this person, in one sentence",
        "suggestion": "what you would ask them about, as one instruction",
        "confidence": 0.0 to 1.0
      }

    No other keys are allowed; an element carrying one is discarded whole.
    Never repeat a suggestion the brief says was already declined.
"#;

/// Build the shipped reviewer role.
///
/// Fallible rather than a `lazy_static` unwrap: the recipe is text, the parser
/// is `deny_unknown_fields`, and a typo in it should surface as a refusal at
/// the call site rather than a panic inside a background loop.
pub fn proactive_reviewer_role() -> Result<AgentRole, RoleError> {
    AgentRole::from_recipe_yaml(PROACTIVE_REVIEWER_ROLE, REVIEWER_ROLE_RECIPE)?.ok_or(
        RoleError::Yaml {
            role: PROACTIVE_REVIEWER_ROLE.to_string(),
            message: format!("the shipped recipe has no `{ROLE_YAML_KEY}` block"),
        },
    )
}

/// The ceiling on what a review may reach, before the role has asked for
/// anything.
///
/// This is the `tool_groups` of the synthetic root authority in
/// [`plan_review`], and it is the honest answer to a question the design does
/// not otherwise have one for: a delegation's ceiling is normally "what the
/// parent turn actually got", and a background review has no parent turn.
///
/// Every entry **reads**. None of them decides, actuates, schedules, writes a
/// file or sends anything, and that is not a coincidence to be preserved by
/// memory — `no_group_the_reviewer_may_reach_can_change_anything` fails on a
/// name that is not on the read-only list next to it. Widening this constant is
/// the single easiest way to turn a proposer into an actor.
pub const PROACTIVE_ROOT_GROUPS: &[&str] = &[
    "giap-memory",
    "giap-device",
    "giap-sensors",
    "giap-weather",
    "giap-knowledge",
];

// ── When it runs ────────────────────────────────────────────────────────────

/// The session id prefix a review run is opened under.
///
/// It starts with the scheduler's `sched-` **on purpose**, because that is what
/// `SessionOrigin::of` classifies as pond-authored. PAI-7 P1's sharpest defect
/// was a background job whose session row made the pond believe somebody was
/// home; a reviewer that minted `proactive-...` would reproduce it exactly —
/// the session activity observer would publish `Started`, presence would follow,
/// and the reviewer would then be reasoning about a person its own run invented.
///
/// `session_origin_covers_every_minted_session` cannot catch that for us: it
/// classifies files that call `create_session`, and this module calls nothing.
/// [`a_review_never_looks_like_somebody_being_at_the_pond`] is the guard.
pub const REVIEW_SESSION_PREFIX: &str = "sched-proactive-review-";

/// The parent session id for one review run.
pub fn review_session_id(run_id: &str) -> String {
    format!("{REVIEW_SESSION_PREFIX}{}", run_id.trim())
}

/// At most this many proposals may be made for one member in a day.
pub const MAX_PROPOSALS_PER_DAY: usize = 6;

/// At most this many may come out of a single review.
///
/// Lower than the daily cap so one talkative run cannot spend the whole day's
/// budget in one breath, which is what makes the difference between an
/// assistant and a notification feed.
pub const MAX_PROPOSALS_PER_RUN: usize = 3;

/// How long a proposal from a review stays live.
///
/// Half of `MAX_PROPOSAL_TTL`. A proactive suggestion is about something
/// happening now; the ceiling is a ceiling, not a target.
pub const PROPOSAL_TTL: Duration = Duration::hours(12);

/// Below this confidence an impulse is not worth a member's attention.
///
/// The wire field defaults to `0.0`, so an impulse that omits it is refused
/// here rather than admitted at full confidence. That direction is the whole
/// point: a defaulted field must narrow.
pub const MIN_PROPOSAL_CONFIDENCE: f32 = 0.5;

/// Everything [`should_review`] needs, in one value.
///
/// Fields are private and [`for_tick`](ReviewInputs::for_tick) is the only
/// constructor, so a caller in another crate cannot assemble a partly-filled
/// one — the shape `PresenceInputs` was repaired into for the same reason, one
/// phase earlier in this workstream.
#[derive(Debug, Clone, Copy)]
pub struct ReviewInputs {
    schedule: GateInputs,
    orchestrator_enabled: bool,
    proposals_today: usize,
    run_in_flight: bool,
}

impl ReviewInputs {
    /// Assemble one tick's inputs.
    ///
    /// `orchestrator_enabled` is separate from `schedule.enabled` rather than
    /// folded into it, because they are different facts and the reviewer needs
    /// both: `ext_orchestrator_enabled` ships **off**, and with it off there is
    /// no `delegate` machinery to run a child at all. Passing it explicitly is
    /// what makes the reviewer inherit PAI-6's off-by-default posture
    /// structurally instead of by comment.
    pub fn for_tick(
        schedule: GateInputs,
        orchestrator_enabled: bool,
        proposals_today: usize,
        run_in_flight: bool,
    ) -> Self {
        Self {
            schedule,
            orchestrator_enabled,
            proposals_today,
            run_in_flight,
        }
    }
}

/// Why a tick did not start a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSkip {
    /// `ext_orchestrator_enabled` is off, so nothing could run a child.
    OrchestratorDisabled,
    /// The shared idle/startup/interval gate said no.
    Schedule(SkipReason),
    /// A review is already running.
    RunInFlight,
    /// This member has had their day's proposals.
    DailyCapReached { made: usize, cap: usize },
}

impl ReviewSkip {
    /// Short, stable label for structured logs, in the same shape as
    /// [`SkipReason::as_str`].
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewSkip::OrchestratorDisabled => "orchestrator_disabled",
            ReviewSkip::Schedule(reason) => reason.as_str(),
            ReviewSkip::RunInFlight => "run_in_flight",
            ReviewSkip::DailyCapReached { .. } => "daily_cap_reached",
        }
    }
}

/// The verdict for one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Run,
    Skip(ReviewSkip),
}

impl ReviewDecision {
    pub fn is_run(self) -> bool {
        matches!(self, ReviewDecision::Run)
    }
}

/// May a review start now?
///
/// The timing rules are **not** restated here. `should_run` already encodes *at
/// most once per interval, only after real user activity in this process
/// lifetime, never at startup*, and it was written to fix precisely the failure
/// a second copy would reintroduce: a background loop firing fifteen minutes
/// after every boot on a machine nobody has touched. What this adds is the
/// three refusals that are the reviewer's own.
pub fn should_review(inputs: &ReviewInputs) -> ReviewDecision {
    if !inputs.orchestrator_enabled {
        return ReviewDecision::Skip(ReviewSkip::OrchestratorDisabled);
    }
    if let GateDecision::Skip(reason) = should_run(inputs.schedule) {
        return ReviewDecision::Skip(ReviewSkip::Schedule(reason));
    }
    match reviewer_refusal(inputs.run_in_flight, inputs.proposals_today) {
        Some(skip) => ReviewDecision::Skip(skip),
        None => ReviewDecision::Run,
    }
}

/// The refusals that are about whether a review is WORTH running, with no
/// timing in them at all.
///
/// Split out because the reviewer runs on the shared inference lane, and the
/// lane owns the timing half: `InferenceLane::acquire` applies the enable
/// toggle, the never-at-startup guard, the quiet threshold and the interval
/// floor, for this job and six others, and decides which of them gets the one
/// slot. What it cannot know is that this particular job has nothing worth
/// doing — and a job must not take the only inference slot in order to discover
/// that.
///
/// Neither refusal here is waivable, and that is the point of their being
/// separate from the timing ones. A person pressing "Run now" is asking the
/// pond to skip its politeness, not to interrupt a household past the one limit
/// it has on being interrupted — `MAX_PROPOSALS_PER_DAY` is that limit, and a
/// button that could be pressed past it would turn a cap into a suggestion.
///
/// `orchestrator_enabled` is deliberately NOT here: it is the pond's own enable
/// toggle for this job, so it belongs in the `enabled` argument the lane
/// already takes, where it is counted and reported as `disabled` like every
/// other job's switch.
pub fn reviewer_refusal(run_in_flight: bool, proposals_today: usize) -> Option<ReviewSkip> {
    if run_in_flight {
        return Some(ReviewSkip::RunInFlight);
    }
    if proposals_today >= MAX_PROPOSALS_PER_DAY {
        return Some(ReviewSkip::DailyCapReached {
            made: proposals_today,
            cap: MAX_PROPOSALS_PER_DAY,
        });
    }
    None
}

/// Invariant 3's second half: must a review already in flight be cancelled?
///
/// Delegates to `consolidation_schedule::saw_activity_since_start` with the
/// **run's** start rather than the process's. That is the same question in a
/// different frame — "has anybody touched the pond since this moment?" — and
/// the function already answers it correctly for both sources, including the
/// voice child, which is a separate OS process whose only visible trace is a
/// session row. A second implementation here would be one more place to forget
/// the out-of-process half.
///
/// On the Orin this is correctness rather than politeness: the review is
/// holding the only GPU the household's next turn needs.
pub fn cancelled_by_activity(
    run_started_at: Instant,
    in_process_activity: Instant,
    run_started_at_utc: DateTime<Utc>,
    db_activity: Option<DateTime<Utc>>,
) -> bool {
    saw_activity_since_start(
        run_started_at,
        in_process_activity,
        run_started_at_utc,
        db_activity,
    )
}

// ── What the child is allowed to be ─────────────────────────────────────────

/// The root authority one review run stands on.
///
/// **Separate from [`plan_review`] because the loop needs the same value, and
/// two constructions of it would be two ceilings.** `GooseOrchestrator::spawn`
/// refuses any spec whose `parent_session_id` has no live turn in the
/// `TurnAuthorityRegistry` — which is right, and which a background reviewer
/// fails by construction, because a review has no user turn behind it. So the
/// loop publishes *this* authority for the duration of the run and the review
/// becomes its own parent turn.
///
/// That is not a workaround for the check; it is what the check was asking for.
/// The registry entry is keyed to a cancellation token, and cancelling that
/// token is what invariant 3 needs — a review interrupted by the user coming
/// back must take its child down with it, and PAI-6 invariant 5 already makes
/// that cascade work for anything the registry knows about.
///
/// `a_review_publishes_the_same_authority_the_orchestrator_will_look_for` pins
/// the pairing, because the failure mode if they drift is not a wrong scope —
/// it is every review being refused, silently, forever.
pub fn review_authority(audience: &ProposalAudience, session_id: &str) -> DelegationAuthority {
    DelegationAuthority::root(
        session_id,
        audience.scope(),
        PROACTIVE_ROOT_GROUPS
            .iter()
            .map(|g| (*g).to_string())
            .collect(),
    )
}

/// Authorise one review run.
///
/// Takes a [`ProposalAudience`] and not a `ProfileScope`, which is how
/// invariants 4 and 5 stop being run-time checks: there is no value of that
/// type meaning "the household" or "an unidentified speaker", so a review for
/// one cannot be requested. The audience's scope becomes the root authority's,
/// the role's `personal_data: inherit` keeps it, and PAI-6's clamp keeps it
/// from widening.
///
/// The ceiling is [`PROACTIVE_ROOT_GROUPS`] rather than a turn's real allow-set
/// because there is no turn. `delegate` then intersects the role's request with
/// it and subtracts `groups_denied_to_subagents`, so the child ends up with
/// less than both lists, never more.
///
/// `background: false`: PAI-6 P8 refuses a background child outright on any
/// provider that runs on this device, which is the default deployment. The
/// review is already off the user's turn — it *is* the background — so asking
/// for the flag would buy nothing and fail on a Jetson.
pub fn plan_review(
    role: &AgentRole,
    audience: &ProposalAudience,
    session_id: &str,
    brief: &str,
    inputs: serde_json::Value,
) -> Result<TaskSpec, DelegationRefused> {
    review_authority(audience, session_id).delegate(
        role,
        TaskRequest {
            role: role.name().to_string(),
            instructions: brief.to_string(),
            inputs,
            background: false,
        },
    )
}

// ── Who a review is for, and what it may be told ────────────────────────────

/// How far back a review may look for the member it addresses.
///
/// **It must be longer than the idle threshold that starts the review, and
/// that is not a tuning choice.** A review runs after
/// `INACTIVITY_THRESHOLD_SECS` of quiet, so by the time it starts, every
/// conversation is by definition at least that stale — and
/// [`attribution_candidates`], which the presence observer uses to decide who
/// is *here*, is bounded by exactly that same threshold. Reusing it to decide
/// who a review is *for* would return the empty list on every tick, forever,
/// and the reviewer would look like a feature nobody had enabled rather than
/// like one whose window was a quarter of an hour too short.
///
/// [`the_audience_window_outlives_the_idle_that_starts_a_review`] is the guard.
/// Six hours is a judgement — long enough to survive an afternoon out, short
/// enough that a suggestion is not addressed to whoever last used the pond
/// yesterday.
///
/// [`attribution_candidates`]: crate::shared::domain::session_activity::attribution_candidates
/// [`the_audience_window_outlives_the_idle_that_starts_a_review`]: #
pub const AUDIENCE_WINDOW: Duration = Duration::hours(6);

/// At most this many events go into one brief.
///
/// A quarter of an hour of a chatty temperature sensor is thousands of
/// readings, and the child's window on the target hardware is 4 096 tokens
/// **including** its role instructions. An unbounded brief does not degrade
/// gracefully here — it evicts the schema the answer has to match.
///
/// The cap applies after [`brief_events`] has collapsed repeats, so it bites on
/// twenty-four *distinct* things having happened, which is a different and much
/// rarer event than a sensor reporting twenty-four times.
pub const MAX_BRIEF_EVENTS: usize = 24;

/// Project one bus event onto the reference a proposal can carry, or refuse it.
///
/// The match is exhaustive with no wildcard arm **on purpose**: a seventh
/// `BusEvent` variant must not silently join the reviewer's diet, and the
/// compiler is a better guard than a test for that particular mistake.
///
/// Two families are refused, and both refusals are load-bearing:
///
/// - **`Time`.** The hourly tick is a heartbeat, not a household fact. The
///   brief already states the time, from the clock rather than from an event,
///   so admitting these would spend the cap on "it is now 3am" and teach the
///   model that the passage of time is something to have opinions about.
/// - **`Session`.** A session lifecycle event is the pond noticing *itself*.
///   Worse, `Idle` is the very transition that lets a review start, so feeding
///   it back would hand the child its own trigger as evidence and invite a
///   proposal about the user having stopped talking — which they had, to go to
///   bed.
pub fn reviewable(event: &BusEvent) -> Option<BusEventRef> {
    // `BusEventRef::new` is fallible only for a blank `kind`, and every kind
    // below is a literal. `.ok()` rather than an `expect` so a future arm that
    // computes one cannot panic inside a background loop.
    match event {
        BusEvent::Sensor(r) => BusEventRef::new(
            "sensor",
            Some(r.device_id.clone()),
            Some(r.sensor_type.clone()),
            r.recorded_at,
        )
        .ok(),
        BusEvent::Camera(c) => BusEventRef::new(
            "camera",
            Some(c.camera_id.clone()),
            Some(c.event_type.clone()),
            c.created_at,
        )
        .ok(),
        BusEvent::Device(d) => BusEventRef::new(
            "device",
            Some(d.device_id.to_string()),
            Some(d.key.clone()),
            d.changed_at,
        )
        .ok(),
        BusEvent::Presence(p) => BusEventRef::new(
            "presence",
            Some(p.profile_id.clone()),
            Some(p.transition.as_str().to_string()),
            p.at,
        )
        .ok(),
        BusEvent::Time(_) | BusEvent::Session(_) => None,
    }
}

/// The member one review is addressed to, or nobody.
///
/// Invariant 4 says a proposal is addressed to a profile and never broadcast,
/// and this is where that gets decided. The answer is the most recently active
/// conversation that (a) a person held — [`SessionOrigin::is_human`], so the
/// pond's own `sched-` rows can never nominate an audience, which matters
/// doubly here because a review's own session id starts with `sched-` — and
/// (b) carries an attribution, inside [`AUDIENCE_WINDOW`].
///
/// **`None` is a first-class answer and the loop must treat it as "no review".**
/// On a pond with no profiles, or one where nobody has been identified in six
/// hours, there is no member to address, and the alternative to skipping is a
/// suggestion sent to the household — which is the broadcast this workstream
/// exists to avoid. [`ProposalAudience`] cannot express one, so a caller that
/// ignored this would have nothing to pass.
///
/// # The sole-member fallthrough, and why it is not a hole in invariant 4
///
/// `members` is the household roster, and when it holds exactly one person an
/// unattributed conversation is addressed to them. Nothing is broadcast: the
/// fallthrough RESOLVES a member and returns an `Owner` audience, so no
/// downstream caller ever sees "the household". With two members it answers
/// `None` exactly as before, because picking one would be attribution by row
/// order, which is evidence of nothing — the rule PAI-1 P3 refused.
///
/// This is the same call `881da889` made for `context_source_owner` and the one
/// `proposal_caller` now makes on the read side: a one-member pond has exactly
/// one possible answer to "whose is this?", and requiring proof of it means
/// requiring a paired, attributed device that most desktop ponds do not have.
///
/// It was measured, not assumed. On a real pond: 961 sessions, **0** carrying a
/// `profile_id`, because `resolve_turn_scope` only writes one on the
/// `DeviceRung::Member` arm and the desktop's own device row is never
/// attributed. So this function returned `None` on every tick of every process
/// the reviewer has ever run — which made PAI-7's entire model-backed producer
/// unreachable, and left the Home column showing only the template-tier
/// suggestions, which is what a household actually notices.
///
/// [`SessionOrigin::is_human`]: crate::shared::domain::session_activity::SessionOrigin::is_human
pub fn audience_for_review(
    sessions: &[Session],
    now: DateTime<Utc>,
    members: &[String],
) -> Option<ProposalAudience> {
    let cutoff = now - AUDIENCE_WINDOW;
    let attributed = sessions
        .iter()
        .filter(|s| SessionOrigin::of(&s.id).is_human())
        .filter(|s| s.updated_at > cutoff)
        .filter_map(|s| s.profile_id.as_deref().map(|p| (s.updated_at, p)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, profile_id)| profile_id.to_string());

    // An attribution the pond actually made always wins. The roster is only
    // consulted when there is none, so this can never override a face match or
    // a paired device with a guess.
    let profile_id = match attributed {
        Some(id) => id,
        None => {
            // Still requires SOMEBODY to have been here inside the window. A
            // pond nobody has talked to in six hours has nothing to review, and
            // the roster does not change that.
            let anyone_here = sessions
                .iter()
                .filter(|s| SessionOrigin::of(&s.id).is_human())
                .any(|s| s.updated_at > cutoff);
            if !anyone_here {
                return None;
            }
            member_attribution::sole_member(members)?
        }
    };

    ProposalAudience::for_member(&profile_id).ok()
}

/// The events a review addressed to `audience` may actually be shown.
///
/// Three things happen here, in this order, and the first is the only one that
/// is about safety:
///
/// 1. **A presence event naming somebody else is dropped.** The child's scope
///    is `Owner(audience)` and PAI-6's clamp holds it there, but the brief is
///    prose handed straight to the model — it goes around the scope, not
///    through it. "Ada arrived at 18:04" in a review addressed to Liz is a
///    disclosure the tool layer would have refused. Device, sensor and camera
///    events are household facts and stay; a `presence` row is the one family
///    whose `source_id` is a person.
/// 2. **Repeats collapse to the newest.** [`TriggerIdentity`] is the same
///    projection the feedback ledger suppresses on, so "the same thing, again"
///    means here exactly what it means when the member says no to it.
/// 3. **Newest first, then [`MAX_BRIEF_EVENTS`].** If the cap has to bite, it
///    should drop the oldest, not whatever the bus happened to deliver last.
pub fn brief_events(audience: &ProposalAudience, observed: &[BusEventRef]) -> Vec<BusEventRef> {
    let mut newest: BTreeMap<TriggerIdentity, BusEventRef> = BTreeMap::new();
    for event in observed {
        if event.kind() == "presence" && event.source_id() != Some(audience.profile_id()) {
            continue;
        }
        let identity = TriggerIdentity::of(event);
        match newest.get(&identity) {
            Some(kept) if kept.observed_at() >= event.observed_at() => {}
            _ => {
                newest.insert(identity, event.clone());
            }
        }
    }
    let mut events: Vec<BusEventRef> = newest.into_values().collect();
    events.sort_by_key(|e| std::cmp::Reverse(e.observed_at()));
    events.truncate(MAX_BRIEF_EVENTS);
    events
}

/// The brief a review is given.
///
/// It states the time because the child cannot ask for it — `giap-system`, and
/// with it `get_current_time`, is withheld from every subagent — and it states
/// how many proposals are left today so the model is not asked to produce three
/// and then have two silently dropped.
///
/// Rejections are listed in words as well as being enforced in
/// [`interpret_answer`]. Both halves are wanted: the words save a turn of the
/// child's four-turn budget, and the enforcement is what holds when the model
/// ignores them, which it will.
pub fn review_brief(now: DateTime<Utc>, recent: &[BusEventRef], ledger: &FeedbackLedger) -> String {
    let mut brief = format!(
        "The current time is {} (UTC). Review what has happened and answer with the JSON array.\n",
        now.to_rfc3339()
    );
    if recent.is_empty() {
        brief.push_str("\nNothing has been observed since the last review.\n");
    } else {
        brief.push_str("\nRecent events:\n");
        for event in recent {
            brief.push_str(&format!(
                "- {} at {}\n",
                TriggerIdentity::of(event).describe(),
                event.observed_at().to_rfc3339()
            ));
        }
    }
    let declined = ledger.declined_descriptions();
    if !declined.is_empty() {
        brief.push_str("\nAlready declined, do not suggest again:\n");
        for line in declined {
            brief.push_str(&format!("- {line}\n"));
        }
    }
    brief
}

// ── What the child said, and what becomes of it ─────────────────────────────

/// One suggestion, as the model is asked to write it.
///
/// Note what is **absent**. There is no audience, no expiry, no profile, no
/// task kind and no id. Every one of those is decided by [`interpret_answer`]
/// and [`build_proposal`] from values the model never sees — the audience comes
/// from the caller, `now` from the caller's clock, and the action from
/// [`impulse_action`], which can only ever return `TaskKind::AgentPrompt`.
///
/// # Why this is NOT `deny_unknown_fields`
///
/// It was, copied from `TaskRequest` on the reasoning that a field this struct
/// does not have is either a typo or an attempt to name something the model may
/// not name. The second half of that is false here, and it cost the whole
/// feature.
///
/// Measured on an Orin 2026-08-12: the review loop fired, resolved its
/// audience, spawned a child that answered in 32 s — and **every** impulse was
/// refused, because a 2B model wrote a `type` field alongside the ones asked
/// for. Zero proposals, one DEBUG line each, a GPU spent per interval for
/// nothing, and on a household pond nobody would ever see the reason. PAI-7 was
/// recorded as COMPLETE while yielding nothing, twice.
///
/// The safety property never depended on the attribute. Nothing in this struct
/// is a capability: an unknown key cannot widen an audience, choose a task
/// kind, set an expiry or name a profile, because none of those are read from
/// here. What actually holds the line is unchanged and is worth stating,
/// because a later reader will be tempted to put the attribute back:
///
/// * `rationale`, `suggestion` and `trigger_kind` have **no** `#[serde(default)]`,
///   so a misspelt key is still a hard refusal — the typo case is covered by
///   requiredness, not by strictness.
/// * `confidence` defaults to `0.0`, below [`MIN_PROPOSAL_CONFIDENCE`], so an
///   impulse that misspells it is refused rather than admitted at full trust.
/// * [`impulse_action`] is the only route to a `TaskKind`, and it returns one
///   variant.
///
/// So the attribute bought strictness against a field that could do nothing,
/// and charged for it with every suggestion the pond would ever have made.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReviewerImpulse {
    /// The bus event family this is about.
    pub trigger_kind: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub signal: Option<String>,
    /// Invariant 2. Refused when blank, by `Proposal::from_parts`.
    pub rationale: String,
    /// What to ask the member about. Becomes a prompt and nothing else.
    pub suggestion: String,
    /// Defaults to `0.0` — below [`MIN_PROPOSAL_CONFIDENCE`] — so an impulse
    /// that omits it is refused rather than admitted.
    #[serde(default)]
    pub confidence: f32,
}

/// The one action an impulse may become.
///
/// A free function with a return type rather than an inline expression, so the
/// claim "a proposal from a review is always a prompt" has somewhere to be
/// tested. `TaskKind::Webhook` would be a network egress chosen by a model
/// (PAI-2's concern, not a preference), and `TaskKind::SensorTrigger` would be
/// a standing rule that keeps firing long after the member forgot approving it.
/// Neither is reachable from here.
pub fn impulse_action(suggestion: &str) -> TaskKind {
    TaskKind::AgentPrompt {
        prompt: suggestion.trim().to_string(),
    }
}

/// Why one impulse did not become a proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum ImpulseRefused {
    /// The run did not produce an answer the parent may read: cancelled, out of
    /// turns, or failed.
    NoAnswer,
    /// The answer contained no JSON array at all.
    NotAJsonArray,
    /// One element did not match the schema.
    Unreadable { index: usize, message: String },
    /// Below [`MIN_PROPOSAL_CONFIDENCE`].
    LowConfidence { index: usize, confidence: f32 },
    /// The domain refused it — a blank rationale, an unusable trigger.
    Invalid { index: usize, error: ProposalError },
    /// The member has already said no to this — PAI-7 P7.
    Suppressed {
        index: usize,
        suppression: Suppression,
    },
    /// Already proposed in this same answer.
    RepeatedInThisRun { index: usize },
    /// The run cap or the day's remaining allowance is spent.
    OverCap { index: usize, allowance: usize },
}

/// What one review produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewYield {
    /// Validated, addressed, expiring proposals, ready for
    /// `ProposalRepository::save`.
    pub proposals: Vec<Proposal>,
    /// Everything that did not make it, and why. Kept rather than logged and
    /// dropped: a reviewer that silently produces nothing is indistinguishable
    /// from a reviewer nobody wired up.
    pub refusals: Vec<ImpulseRefused>,
}

impl ReviewYield {
    fn refused(refusal: ImpulseRefused) -> Self {
        Self {
            proposals: Vec::new(),
            refusals: vec![refusal],
        }
    }
}

/// Turn a finished run's answer into proposals.
///
/// **This is the writer the subagent cannot be.** See the module docs: a child
/// holds no actuating group, so the words come from the model and every fact
/// about the resulting row comes from here — the audience, the id, the expiry,
/// the action kind and the cap.
///
/// `run.result_for_parent()`, not `run.result`: PAI-6 records that Goose
/// returns `Ok(partial_text)` for a cancelled child and the literal max-turns
/// message for an exhausted one, so reading the field directly would turn an
/// interrupted review into proposals. Invariant 3 says a review is cancelled by
/// activity; cancelling it and then acting on what it had managed to say is not
/// a cancellation.
pub fn interpret_answer(
    run: &TaskRun,
    audience: &ProposalAudience,
    now: DateTime<Utc>,
    ledger: &FeedbackLedger,
    proposals_today: usize,
) -> ReviewYield {
    let Some(answer) = run.result_for_parent() else {
        return ReviewYield::refused(ImpulseRefused::NoAnswer);
    };
    let Some(array) = first_json_array(answer) else {
        return ReviewYield::refused(ImpulseRefused::NotAJsonArray);
    };
    let Ok(elements) = serde_json::from_str::<Vec<serde_json::Value>>(array) else {
        return ReviewYield::refused(ImpulseRefused::NotAJsonArray);
    };

    // The narrower of the two caps, and the day's remainder can be zero.
    let allowance =
        MAX_PROPOSALS_PER_RUN.min(MAX_PROPOSALS_PER_DAY.saturating_sub(proposals_today));

    let mut proposals: Vec<Proposal> = Vec::new();
    let mut refusals: Vec<ImpulseRefused> = Vec::new();
    let mut seen: BTreeSet<ProposalShape> = BTreeSet::new();

    for (index, element) in elements.into_iter().enumerate() {
        if proposals.len() >= allowance {
            refusals.push(ImpulseRefused::OverCap { index, allowance });
            continue;
        }
        // Per element, not per array: one malformed suggestion must not cost
        // the other two. A 3B model gets one of three elements wrong far more
        // often than it gets all three wrong.
        let impulse = match serde_json::from_value::<ReviewerImpulse>(element) {
            Ok(impulse) => impulse,
            Err(e) => {
                refusals.push(ImpulseRefused::Unreadable {
                    index,
                    message: e.to_string(),
                });
                continue;
            }
        };
        if !(impulse.confidence.is_finite() && impulse.confidence >= MIN_PROPOSAL_CONFIDENCE) {
            refusals.push(ImpulseRefused::LowConfidence {
                index,
                confidence: impulse.confidence,
            });
            continue;
        }
        let proposal = match build_proposal(&impulse, audience, now) {
            Ok(proposal) => proposal,
            Err(error) => {
                refusals.push(ImpulseRefused::Invalid { index, error });
                continue;
            }
        };
        let shape = ProposalShape::of(&proposal);
        if let Some(suppression) = ledger.suppression_of(&shape) {
            refusals.push(ImpulseRefused::Suppressed { index, suppression });
            continue;
        }
        if !seen.insert(shape) {
            refusals.push(ImpulseRefused::RepeatedInThisRun { index });
            continue;
        }
        proposals.push(proposal);
    }

    ReviewYield {
        proposals,
        refusals,
    }
}

fn build_proposal(
    impulse: &ReviewerImpulse,
    audience: &ProposalAudience,
    now: DateTime<Utc>,
) -> Result<Proposal, ProposalError> {
    // `now`, never a timestamp from the model. A subagent has no clock, so any
    // time it quotes is copied out of its own prompt or invented, and an
    // observed_at in the future would outlive the proposal it belongs to.
    let trigger = BusEventRef::new(
        impulse.trigger_kind.as_str(),
        impulse.source_id.clone(),
        impulse.signal.clone(),
        now,
    )?;
    Proposal::expiring_after(
        uuid::Uuid::new_v4().to_string(),
        trigger,
        impulse.rationale.as_str(),
        impulse_action(&impulse.suggestion),
        audience.clone(),
        impulse.confidence,
        now,
        PROPOSAL_TTL,
    )
}

/// The first balanced `[...]` in the text, or `None`.
///
/// Small models wrap JSON in prose and fences however they feel, and an answer
/// that is 95% correct should not be discarded whole. String-aware, because a
/// suggestion containing a bracket is ordinary English ("check the meter [it
/// reads high]") and counting brackets naively would cut the array in half.
fn first_json_array(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(from) = start {
                        return text.get(from..=index);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ── P7: what the member already said ────────────────────────────────────────

/// How long a rejection keeps suppressing.
///
/// Not forever. A household changes, and a rejection from a year ago is a fact
/// about a life somebody was living then. Thirty days is long enough that a
/// suppression is felt as "it stopped bringing that up" rather than as a
/// missing feature.
pub const SUPPRESSION_WINDOW: Duration = Duration::days(30);

/// How many *different* rejected suggestions about one trigger silence the
/// trigger itself.
///
/// Distinct shapes, not repeats: the same shape rejected three times means the
/// suppression below was not applied, which is a bug rather than a preference.
/// Three different suggestions about the garage door, all declined, is the
/// member saying something about the garage door.
pub const REJECTIONS_THAT_SILENCE_A_TRIGGER: usize = 3;

/// Why a proposal was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppression {
    /// This exact suggestion, about this exact trigger, was rejected.
    AlreadyRejected,
    /// Enough different suggestions about this trigger were rejected that the
    /// trigger is no longer worth raising.
    TriggerSilenced { rejections: usize },
}

/// What the member's past decisions mean for the next review — PAI-7 P7.
///
/// **This is deliberately not a learning system**, and section 6 defers the one
/// that would be: inferring new rules from behaviour is a much larger claim
/// than this earns. What it is: a rejected proposal is a fact about a
/// preference, and the useful version of that fact is that the same suggestion
/// stops coming back.
///
/// It answers on the shape rather than on the words, so a model rephrasing a
/// declined suggestion does not get a second hearing — and it errs toward
/// silence in both of its rules, because proactivity's failure direction is to
/// say less.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedbackLedger {
    rejected: BTreeSet<ProposalShape>,
    silenced: BTreeMap<TriggerIdentity, usize>,
}

impl FeedbackLedger {
    /// A ledger that suppresses nothing — a pond where nobody has decided
    /// anything yet, which is every pond today.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Fold the member's decisions into what the next review may not say.
    ///
    /// Only rejections count and only recent ones: see
    /// [`ProposalDecision::silences_a_repeat`] for why an expiry is not a no,
    /// and [`SUPPRESSION_WINDOW`] for why a rejection is not forever.
    pub fn from_decisions(decisions: &[ProposalDecision], now: DateTime<Utc>) -> Self {
        let mut rejected: BTreeSet<ProposalShape> = BTreeSet::new();
        for decision in decisions {
            if !decision.silences_a_repeat() {
                continue;
            }
            if now - decision.decided_at() > SUPPRESSION_WINDOW {
                continue;
            }
            rejected.insert(decision.shape().clone());
        }
        let mut silenced: BTreeMap<TriggerIdentity, usize> = BTreeMap::new();
        for shape in &rejected {
            *silenced.entry(shape.trigger().clone()).or_insert(0) += 1;
        }
        silenced.retain(|_, count| *count >= REJECTIONS_THAT_SILENCE_A_TRIGGER);
        Self { rejected, silenced }
    }

    /// Why this proposal may not be made, or `None`.
    pub fn suppression_of(&self, shape: &ProposalShape) -> Option<Suppression> {
        if self.rejected.contains(shape) {
            return Some(Suppression::AlreadyRejected);
        }
        self.silenced
            .get(shape.trigger())
            .map(|rejections| Suppression::TriggerSilenced {
                rejections: *rejections,
            })
    }

    /// The declined suggestions, in words, for [`review_brief`].
    pub fn declined_descriptions(&self) -> Vec<String> {
        self.rejected
            .iter()
            .map(|shape| format!("{}: {}", shape.trigger().describe(), shape.action()))
            .collect()
    }

    /// How many distinct suggestions are being suppressed outright.
    pub fn rejected_count(&self) -> usize {
        self.rejected.len()
    }

    /// The triggers that have gone quiet altogether.
    pub fn silenced_triggers(&self) -> impl Iterator<Item = &TriggerIdentity> {
        self.silenced.keys()
    }
}

/// Groups no reviewer may reach, written out independently of
/// [`PROACTIVE_ROOT_GROUPS`] and of `groups_denied_to_subagents`.
///
/// Iterating either of those would shrink with them: PAI-6 recorded that
/// deleting an entry from `groups_denied_to_subagents` left the whole suite
/// green, because every guard looped the source of truth. This is the other
/// direction — a list of names with the reason each one would turn a proposer
/// into an actor.
#[cfg(test)]
const GROUPS_A_PROPOSER_MAY_NOT_HOLD: [(&str, &str); 5] = [
    (
        "giap-device-control",
        "actuates the house, which is the one thing a proposal exists to ask permission for",
    ),
    (
        "giap-schedule",
        "leaves standing work behind that runs with the household's authority after the review \
         has ended",
    ),
    (
        "giap-system",
        "send_notification: a proposer that can notify has skipped the member entirely (the \
         file and shell tools this also covered were removed on 2026-09-10)",
    ),
    (
        "giap-toolkit",
        "widens its own allow-set, so any other entry on this list becomes reachable",
    ),
    (
        "giap-orchestrator",
        "spawns further agents, which is unbounded background work nobody asked for",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::domain::tool_group::{find_group, groups_denied_to_subagents};
    use crate::shared::domain::session_activity::SessionOrigin;
    use crate::user_data::domain::profile::{ProfileScope, EXEMPLAR_OWNER_ID};
    use crate::user_data::services::consolidation_schedule::{
        interval_floor_from_hours, INACTIVITY_THRESHOLD_SECS,
    };
    use std::time::Duration as StdDuration;

    fn audience() -> ProposalAudience {
        ProposalAudience::for_member(EXEMPLAR_OWNER_ID).unwrap()
    }

    fn role() -> AgentRole {
        proactive_reviewer_role().expect("the shipped reviewer recipe must parse")
    }

    fn idle_schedule() -> GateInputs {
        GateInputs {
            enabled: true,
            saw_activity_since_start: true,
            idle_for: StdDuration::from_secs(INACTIVITY_THRESHOLD_SECS + 1),
            idle_threshold: StdDuration::from_secs(INACTIVITY_THRESHOLD_SECS),
            since_last_run: None,
            interval_floor: interval_floor_from_hours(6),
        }
    }

    fn inputs() -> ReviewInputs {
        ReviewInputs::for_tick(idle_schedule(), true, 0, false)
    }

    fn answer(body: &str) -> TaskRun {
        TaskRun {
            id: "task-1".into(),
            role: PROACTIVE_REVIEWER_ROLE.into(),
            parent_session_id: review_session_id("run-1"),
            status: crate::shared::domain::orchestration::TaskStatus::Completed,
            result: Some(body.to_string()),
            error: None,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
        }
    }

    fn one_impulse(source: &str, suggestion: &str, confidence: f32) -> String {
        format!(
            r#"[{{"trigger_kind":"camera","source_id":"{source}","signal":"person",
                  "rationale":"the delivery window closes at six",
                  "suggestion":"{suggestion}","confidence":{confidence}}}]"#
        )
    }

    // ── The role ───────────────────────────────────────────────────────────

    #[test]
    fn the_shipped_recipe_parses_through_the_same_door_as_a_stored_one() {
        let role = role();
        assert_eq!(role.name(), PROACTIVE_REVIEWER_ROLE);
        assert!(!role.instructions().trim().is_empty());
        assert!(role.max_turns() <= 6, "a review is not an errand");
        assert!(role.requested_model().is_none(), "no model is named");
    }

    /// The claim that matters about the ceiling, and it is a claim about the
    /// MECHANISM rather than about the constant: a role asking for the groups
    /// that would let a proposer act gets none of them, whether they were
    /// withheld by the root grant or by `groups_denied_to_subagents`.
    #[test]
    fn a_reviewer_asking_to_act_is_given_nothing_it_asked_for() {
        let greedy = AgentRole::new(
            PROACTIVE_REVIEWER_ROLE,
            "act on everything",
            GROUPS_A_PROPOSER_MAY_NOT_HOLD
                .iter()
                .map(|(name, _)| (*name).to_string())
                .chain(std::iter::once("giap-memory".to_string()))
                .collect(),
            crate::shared::domain::orchestration::RolePersonalData::Inherit,
            4,
            0.35,
        )
        .unwrap();
        let spec = plan_review(
            &greedy,
            &audience(),
            &review_session_id("run-1"),
            "review",
            serde_json::json!({}),
        )
        .expect("a review must be authorisable");

        for (group, reason) in GROUPS_A_PROPOSER_MAY_NOT_HOLD {
            assert!(
                !spec.tool_groups().contains(group),
                "a review was given `{group}`, which {reason}"
            );
            assert!(
                !spec.grants_tool(&format!("{group}__anything")),
                "a review may call a tool in `{group}`, which {reason}"
            );
        }
        // Vacuity control: the narrowing is not "deny everything". A group the
        // role asked for that IS on the read-only ceiling survives, or the
        // sweep above would pass against a reviewer with no tools at all --
        // which is a reviewer that cannot see anything to propose about.
        assert!(
            spec.tool_groups().contains("giap-memory"),
            "the narrowing removed everything, so the sweep above proves nothing"
        );
    }

    #[test]
    fn no_group_the_reviewer_may_reach_can_change_anything() {
        for group in PROACTIVE_ROOT_GROUPS {
            assert!(
                find_group(group).is_some(),
                "`{group}` is not a group GIAP has: a typo here narrows silently, and the \
                 reviewer would run with less than the design says"
            );
            assert!(
                !GROUPS_A_PROPOSER_MAY_NOT_HOLD
                    .iter()
                    .any(|(denied, _)| denied == group),
                "`{group}` is on the reviewer's ceiling and on the list of groups that turn a \
                 proposer into an actor"
            );
            assert!(
                !groups_denied_to_subagents().contains(group),
                "`{group}` is on the reviewer's ceiling and is withheld from every subagent: \
                 the ceiling would be describing a grant that cannot exist"
            );
        }
    }

    // ── Where it runs ──────────────────────────────────────────────────────

    /// PAI-7 P1's sharpest defect, in the shape this phase could reproduce it:
    /// a background run whose session id reads as a person's conversation makes
    /// the pond publish presence for nobody, and the reviewer then reasons
    /// about a member its own run invented.
    #[test]
    fn a_review_never_looks_like_somebody_being_at_the_pond() {
        for run_id in ["run-1", "", "  ", "42"] {
            let id = review_session_id(run_id);
            assert_eq!(
                SessionOrigin::of(&id),
                SessionOrigin::Machine,
                "a review session `{id}` reads as a person's conversation"
            );
            assert!(!SessionOrigin::of(&id).is_human());
        }
        // Vacuity control: the classifier is not answering Machine for
        // everything. A real conversation id must still read as a person, or
        // the assertions above hold for a reason that has nothing to do with
        // the prefix.
        assert_eq!(
            SessionOrigin::of("f47ac10b-58cc-4372-a567-0e02b2c3d479"),
            SessionOrigin::Human
        );
    }

    // ── When it runs ───────────────────────────────────────────────────────

    #[test]
    fn an_idle_pond_after_real_activity_may_review() {
        assert_eq!(should_review(&inputs()), ReviewDecision::Run);
    }

    /// The reviewer runs on the lane now, and calls this half directly.
    ///
    /// The lane owns the timing -- enable toggle, never-at-startup, quiet
    /// threshold, interval floor -- for this job and six others, because it is
    /// what decides which of them gets the one slot. What is left here is the
    /// question the lane cannot answer: is a review worth running at all?
    #[test]
    fn the_reviewers_own_refusals_are_the_cap_and_a_run_in_flight() {
        assert_eq!(reviewer_refusal(false, 0), None);
        assert_eq!(reviewer_refusal(true, 0), Some(ReviewSkip::RunInFlight));
        assert_eq!(
            reviewer_refusal(false, MAX_PROPOSALS_PER_DAY),
            Some(ReviewSkip::DailyCapReached {
                made: MAX_PROPOSALS_PER_DAY,
                cap: MAX_PROPOSALS_PER_DAY,
            })
        );
        assert_eq!(
            reviewer_refusal(false, MAX_PROPOSALS_PER_DAY - 1),
            None,
            "the cap is a ceiling, not a fence one short of it"
        );
    }

    /// Splitting the gate must not have made a second copy of the timing rules.
    ///
    /// The whole reason `should_run` is shared is that a second copy is what
    /// reintroduces the failure it was written to fix -- a background loop
    /// firing fifteen minutes after every boot on a machine nobody has touched.
    /// This half takes no clock, no duration and no activity reading, and its
    /// signature is what enforces that: there is nothing here to get wrong.
    #[test]
    fn the_reviewers_own_half_of_the_gate_has_no_timing_in_it() {
        // Two calls that differ in nothing but the caller's imagination about
        // when they happened. A timing rule hiding in here would have to read
        // something, and there is nothing to read.
        assert_eq!(reviewer_refusal(false, 3), reviewer_refusal(false, 3));
        assert_eq!(
            should_review(&ReviewInputs::for_tick(
                GateInputs {
                    enabled: true,
                    saw_activity_since_start: true,
                    idle_for: std::time::Duration::from_secs(9_999),
                    idle_threshold: std::time::Duration::ZERO,
                    since_last_run: None,
                    interval_floor: std::time::Duration::ZERO,
                },
                true,
                MAX_PROPOSALS_PER_DAY,
                false,
            )),
            ReviewDecision::Skip(ReviewSkip::DailyCapReached {
                made: MAX_PROPOSALS_PER_DAY,
                cap: MAX_PROPOSALS_PER_DAY,
            }),
            "a perfect schedule does not buy a way past the cap"
        );
    }

    /// The reviewer is off on a stock install, and that is structural rather
    /// than documented: with `ext_orchestrator_enabled` off there is no
    /// machinery to run a child at all.
    #[test]
    fn the_orchestrator_being_off_outranks_every_other_reason_to_run() {
        let off = ReviewInputs::for_tick(idle_schedule(), false, 0, false);
        assert_eq!(
            should_review(&off),
            ReviewDecision::Skip(ReviewSkip::OrchestratorDisabled)
        );
    }

    /// Invariant 3's first half is not re-implemented here, so the test that
    /// matters is that the shared gate's refusals are the reviewer's refusals.
    /// Quantified over the three failures that broke consolidation.
    #[test]
    fn every_refusal_the_shared_gate_makes_is_a_refusal_here() {
        let cases = [
            (
                GateInputs {
                    enabled: false,
                    ..idle_schedule()
                },
                SkipReason::Disabled,
            ),
            (
                GateInputs {
                    saw_activity_since_start: false,
                    idle_for: StdDuration::from_secs(86_400),
                    ..idle_schedule()
                },
                SkipReason::NoActivitySinceStart,
            ),
            (
                GateInputs {
                    idle_for: StdDuration::from_secs(60),
                    ..idle_schedule()
                },
                SkipReason::StillActive,
            ),
            (
                GateInputs {
                    since_last_run: Some(StdDuration::from_secs(60)),
                    ..idle_schedule()
                },
                SkipReason::IntervalFloor,
            ),
        ];
        for (schedule, expected) in cases {
            let decision = should_review(&ReviewInputs::for_tick(schedule, true, 0, false));
            assert_eq!(
                decision,
                ReviewDecision::Skip(ReviewSkip::Schedule(expected)),
                "the reviewer ran while the shared gate said {}",
                expected.as_str()
            );
        }
    }

    #[test]
    fn a_member_who_has_had_their_day_gets_no_more_reviews() {
        let spent = ReviewInputs::for_tick(idle_schedule(), true, MAX_PROPOSALS_PER_DAY, false);
        assert_eq!(
            should_review(&spent),
            ReviewDecision::Skip(ReviewSkip::DailyCapReached {
                made: MAX_PROPOSALS_PER_DAY,
                cap: MAX_PROPOSALS_PER_DAY
            })
        );
        // One under the cap still runs, so the comparison is a cap and not an
        // off-by-one that silences the last proposal of the day.
        let nearly =
            ReviewInputs::for_tick(idle_schedule(), true, MAX_PROPOSALS_PER_DAY - 1, false);
        assert_eq!(should_review(&nearly), ReviewDecision::Run);
    }

    #[test]
    fn two_reviews_never_run_at_once() {
        let running = ReviewInputs::for_tick(idle_schedule(), true, 0, true);
        assert_eq!(
            should_review(&running),
            ReviewDecision::Skip(ReviewSkip::RunInFlight)
        );
    }

    /// Invariant 3's second half. The out-of-process case is the one a second
    /// implementation would miss: the voice child is a separate OS process and
    /// its only visible trace is a session row.
    #[test]
    fn any_activity_after_a_review_starts_cancels_it() {
        let started = Instant::now();
        let started_utc = Utc::now();

        assert!(
            !cancelled_by_activity(started, started, started_utc, None),
            "a review with nobody around must not cancel itself"
        );
        assert!(
            cancelled_by_activity(
                started,
                started + StdDuration::from_millis(1),
                started_utc,
                None
            ),
            "an HTTP request during a review must cancel it"
        );
        assert!(
            cancelled_by_activity(
                started,
                started,
                started_utc,
                Some(started_utc + Duration::seconds(1))
            ),
            "a voice turn in another process during a review must cancel it"
        );
        assert!(
            !cancelled_by_activity(
                started,
                started,
                started_utc,
                Some(started_utc - Duration::days(3))
            ),
            "history from before the review is not somebody arriving"
        );
    }

    // ── What the child is allowed to be ────────────────────────────────────

    #[test]
    fn a_review_runs_as_the_one_member_it_is_for() {
        let spec = plan_review(
            &role(),
            &audience(),
            &review_session_id("run-1"),
            "review",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            spec.profile_scope(),
            &ProfileScope::Owner(EXEMPLAR_OWNER_ID.to_string())
        );
        assert!(
            !matches!(
                spec.profile_scope(),
                ProfileScope::Household | ProfileScope::Guest
            ),
            "a review must never run as the household or as an unidentified speaker"
        );
        assert!(
            !spec.background(),
            "a background child is refused on-device"
        );
        assert_eq!(spec.role(), PROACTIVE_REVIEWER_ROLE);
    }

    #[test]
    fn a_review_with_nothing_to_say_is_refused_before_it_starts() {
        assert!(matches!(
            plan_review(
                &role(),
                &audience(),
                &review_session_id("run-1"),
                "   ",
                serde_json::json!({})
            ),
            Err(DelegationRefused::EmptyInstructions { .. })
        ));
    }

    // ── The answer ─────────────────────────────────────────────────────────

    #[test]
    fn a_well_formed_impulse_becomes_one_addressed_expiring_proposal() {
        let now = Utc::now();
        let out = interpret_answer(
            &answer(&one_impulse("front-door", "ask about the delivery", 0.8)),
            &audience(),
            now,
            &FeedbackLedger::empty(),
            0,
        );
        assert!(
            out.refusals.is_empty(),
            "unexpected refusals: {:?}",
            out.refusals
        );
        assert_eq!(out.proposals.len(), 1);
        let p = &out.proposals[0];
        assert_eq!(p.audience().profile_id(), EXEMPLAR_OWNER_ID);
        assert_eq!(p.rationale(), "the delivery window closes at six");
        assert_eq!(p.expires_at(), now + PROPOSAL_TTL);
        assert!(p.is_live_at(now));
        assert!(matches!(p.proposed_action(), TaskKind::AgentPrompt { .. }));
    }

    /// Invariant 1 at the type level: there is no route from model output to
    /// anything that acts. A webhook would be a network egress a model chose,
    /// and a sensor rule would keep firing long after the member forgot
    /// approving it.
    #[test]
    fn a_proposal_from_a_review_is_always_a_prompt_and_never_a_webhook() {
        for suggestion in [
            "POST https://example.invalid/hook",
            "create a rule that opens the garage",
        ] {
            match impulse_action(suggestion) {
                TaskKind::AgentPrompt { prompt } => assert_eq!(prompt, suggestion),
                other => panic!("a review produced {other:?}"),
            }
        }
    }

    /// The recorded failure this guards is PAI-6's: Goose returns
    /// `Ok(partial_text)` for a cancelled child, so a reader that took
    /// `run.result` would turn an interrupted review into proposals — and
    /// invariant 3 says a review is cancelled by activity.
    #[test]
    fn a_run_that_was_cancelled_or_ran_out_of_turns_yields_nothing() {
        use crate::shared::domain::orchestration::TaskStatus;
        for status in [
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
            TaskStatus::Running,
            TaskStatus::Queued,
        ] {
            let mut run = answer(&one_impulse("front-door", "ask about the delivery", 0.9));
            run.status = status;
            let out = interpret_answer(&run, &audience(), Utc::now(), &FeedbackLedger::empty(), 0);
            assert!(
                out.proposals.is_empty(),
                "{status:?} produced proposals from text that is not an answer"
            );
            assert_eq!(out.refusals, vec![ImpulseRefused::NoAnswer]);
        }
        // Vacuity control: the same body under Completed does produce one, so
        // the sweep above is about the status and not about the fixture.
        let out = interpret_answer(
            &answer(&one_impulse("front-door", "ask about the delivery", 0.9)),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert_eq!(out.proposals.len(), 1);
    }

    /// A field the model may not name is IGNORED, and naming it changes
    /// nothing — which is a stronger claim than refusing the whole impulse, and
    /// the one that survives contact with a 2B model.
    ///
    /// This test used to assert the refusal, under `deny_unknown_fields`. On an
    /// Orin that made the entire feature yield zero: the model wrote a `type`
    /// key next to the ones it was asked for and every suggestion the pond
    /// would have made was discarded, with the reason at DEBUG. Strictness
    /// against a field that can do nothing is not a safety property, it is a
    /// tax — so the test now pins what actually matters. `audience` is the
    /// sharpest case available: it is the one thing a model naming it would
    /// most want to control, and PAI-1's boundary depends on it.
    #[test]
    fn naming_the_audience_does_not_let_a_model_choose_one() {
        let body = r#"[{"trigger_kind":"camera","rationale":"why","suggestion":"ask",
                        "confidence":0.9,"audience":"somebody-else",
                        "profile_id":"somebody-else","type":"reminder",
                        "expires_at":"2099-01-01T00:00:00Z"}]"#;
        let now = Utc::now();
        let out = interpret_answer(&answer(body), &audience(), now, &FeedbackLedger::empty(), 0);

        assert!(
            out.refusals.is_empty(),
            "an impulse was refused for naming fields that cannot reach a decision. \
             That is the defect that made PAI-7 yield nothing on a real device: {:?}",
            out.refusals
        );
        assert_eq!(out.proposals.len(), 1);
        let p = &out.proposals[0];

        // Every one of the four keys above was ignored, and the values below
        // came from the caller.
        assert_eq!(
            p.audience().profile_id(),
            EXEMPLAR_OWNER_ID,
            "the model named an audience and got it. The audience must come from \
             the caller -- this is PAI-1's boundary, and a proposal addressed by a \
             subagent would route a household member's suggestion to somebody else."
        );
        assert_eq!(
            p.expires_at(),
            now + PROPOSAL_TTL,
            "the model named an expiry and got it; the TTL is the caller's."
        );
        assert!(
            matches!(p.proposed_action(), TaskKind::AgentPrompt { .. }),
            "the model named a `type` and got something other than a prompt."
        );
    }

    /// The typo case, which is what `deny_unknown_fields` was actually being
    /// relied on for — and which requiredness covers on its own.
    ///
    /// Worth its own test because removing the attribute makes it tempting to
    /// believe nothing is enforced any more. A misspelt `suggestion` is still a
    /// hard refusal, because the field has no `#[serde(default)]`; if a later
    /// change ever adds one "for robustness", this fails and says so.
    #[test]
    fn a_misspelt_required_field_is_still_refused_without_the_strict_attribute() {
        let body = r#"[{"trigger_kind":"camera","rationale":"why",
                        "suggestion_text":"ask","confidence":0.9}]"#;
        let out = interpret_answer(
            &answer(body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert!(
            out.proposals.is_empty(),
            "an impulse with no `suggestion` became a proposal. Requiredness is the \
             only thing refusing typos now that the impulse tolerates extra keys."
        );
        assert!(matches!(
            out.refusals.as_slice(),
            [ImpulseRefused::Unreadable { index: 0, .. }]
        ));
    }

    /// A defaulted field must narrow. `confidence` is absent far more often
    /// than it is wrong, and a serde default of `0.0` with a floor above it is
    /// what makes the absence a refusal instead of a full-confidence proposal.
    #[test]
    fn an_impulse_with_no_confidence_is_refused_rather_than_believed() {
        let body = r#"[{"trigger_kind":"camera","rationale":"why","suggestion":"ask about it"}]"#;
        let out = interpret_answer(
            &answer(body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert!(out.proposals.is_empty());
        assert!(matches!(
            out.refusals.as_slice(),
            [ImpulseRefused::LowConfidence {
                index: 0,
                confidence
            }] if *confidence == 0.0
        ));
    }

    #[test]
    fn an_impulse_with_no_rationale_never_becomes_a_proposal() {
        let body =
            r#"[{"trigger_kind":"camera","rationale":"  ","suggestion":"ask","confidence":0.9}]"#;
        let out = interpret_answer(
            &answer(body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert!(out.proposals.is_empty());
        assert!(matches!(
            out.refusals.as_slice(),
            [ImpulseRefused::Invalid {
                index: 0,
                error: ProposalError::MissingRationale { .. }
            }]
        ));
    }

    #[test]
    fn one_bad_element_costs_only_itself() {
        let body = format!(
            r#"Here is what I found:
            [{{"trigger_kind":"camera","rationale":"why","suggestion":"nope"}},
             {{"trigger_kind":"sensor","source_id":"freezer","signal":"temp",
               "rationale":"the freezer is warming up","suggestion":"check the freezer",
               "confidence":0.9}}]
            Hope that helps."#
        );
        let out = interpret_answer(
            &answer(&body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert_eq!(out.proposals.len(), 1, "the good element must survive");
        assert_eq!(out.proposals[0].summary(), "check the freezer");
        assert_eq!(out.refusals.len(), 1);
    }

    #[test]
    fn an_answer_with_no_array_at_all_yields_nothing() {
        for body in ["I could not find anything worth mentioning.", "", "{}"] {
            let out = interpret_answer(
                &answer(body),
                &audience(),
                Utc::now(),
                &FeedbackLedger::empty(),
                0,
            );
            assert_eq!(out.refusals, vec![ImpulseRefused::NotAJsonArray]);
        }
        // An empty array is a complete answer and not a failure to parse.
        let out = interpret_answer(
            &answer("[]"),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert!(out.proposals.is_empty());
        assert!(out.refusals.is_empty(), "silence is a valid review");
    }

    /// A balanced pair inside a string proves nothing about the string
    /// tracking — the depth goes up and comes back down, so a scanner that
    /// cannot see strings gets the same answer. Verified by mutation: deleting
    /// the `in_string` branch left the balanced case green. The cases that
    /// discriminate are the UNBALANCED ones, which is also the shape a model
    /// actually produces: `:]` closes the array early, and a lone `[` means it
    /// never closes at all.
    #[test]
    fn a_bracket_inside_a_suggestion_does_not_cut_the_array_in_half() {
        let cases = [
            ("balanced", "check the meter [it reads high]"),
            (
                "a lone closing bracket",
                "check the meter, it reads high :]",
            ),
            ("a lone opening bracket", "check the meter [it reads high"),
        ];
        for (name, suggestion) in cases {
            let body = format!(
                r#"[{{"trigger_kind":"sensor","source_id":"meter","signal":"kwh",
                       "rationale":"usage is up again","suggestion":"{suggestion}",
                       "confidence":0.7}}]"#
            );
            let out = interpret_answer(
                &answer(&body),
                &audience(),
                Utc::now(),
                &FeedbackLedger::empty(),
                0,
            );
            assert_eq!(
                out.proposals.len(),
                1,
                "{name} in a suggestion lost the whole answer: {:?}",
                out.refusals
            );
            assert_eq!(out.proposals[0].summary(), suggestion);
        }
    }

    #[test]
    fn a_run_may_not_spend_more_than_its_share_of_the_day() {
        let many: Vec<String> = (0..6)
            .map(|i| {
                format!(
                    r#"{{"trigger_kind":"camera","source_id":"cam-{i}","signal":"person",
                         "rationale":"something happened","suggestion":"look at camera {i}",
                         "confidence":0.9}}"#
                )
            })
            .collect();
        let body = format!("[{}]", many.join(","));

        let fresh = interpret_answer(
            &answer(&body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert_eq!(fresh.proposals.len(), MAX_PROPOSALS_PER_RUN);

        // With the day nearly spent, the run cap is not the binding one.
        let nearly_spent = interpret_answer(
            &answer(&body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            MAX_PROPOSALS_PER_DAY - 1,
        );
        assert_eq!(nearly_spent.proposals.len(), 1);

        let spent = interpret_answer(
            &answer(&body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            MAX_PROPOSALS_PER_DAY,
        );
        assert!(
            spent.proposals.is_empty(),
            "the daily cap must hold even when a review was allowed to start"
        );
    }

    #[test]
    fn a_model_that_says_the_same_thing_twice_is_heard_once() {
        let one = r#"{"trigger_kind":"camera","source_id":"front-door","signal":"person",
                      "rationale":"the delivery window closes at six",
                      "suggestion":"ask about the delivery","confidence":0.8}"#;
        let body = format!("[{one},{one}]");
        let out = interpret_answer(
            &answer(&body),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert_eq!(out.proposals.len(), 1);
        assert_eq!(
            out.refusals,
            vec![ImpulseRefused::RepeatedInThisRun { index: 1 }]
        );
    }

    // ── P7: the feedback loop ──────────────────────────────────────────────

    fn decision_about(
        source: &str,
        suggestion: &str,
        status: crate::user_data::domain::draft::DraftStatus,
        decided_at: DateTime<Utc>,
    ) -> ProposalDecision {
        let proposal = Proposal::expiring_after(
            "prop-x",
            BusEventRef::new(
                "camera",
                Some(source.to_string()),
                Some("person".to_string()),
                decided_at,
            )
            .unwrap(),
            "why",
            impulse_action(suggestion),
            audience(),
            0.8,
            decided_at,
            Duration::hours(1),
        )
        .unwrap();
        ProposalDecision::recorded(ProposalShape::of(&proposal), status, decided_at).unwrap()
    }

    /// The whole of P7's useful claim: the reviewer stops proposing the thing
    /// that was rejected. Driven through `interpret_answer`, not through the
    /// ledger's own method, because a suppression that holds in the ledger and
    /// is never consulted by the interpreter is the failure this programme has
    /// recorded most often.
    #[test]
    fn a_rejected_suggestion_does_not_come_back() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        let ledger = FeedbackLedger::from_decisions(
            &[decision_about(
                "front-door",
                "ask about the delivery",
                DraftStatus::Rejected,
                now - Duration::days(1),
            )],
            now,
        );
        let out = interpret_answer(
            &answer(&one_impulse("front-door", "ask about the delivery", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert!(out.proposals.is_empty());
        assert_eq!(
            out.refusals,
            vec![ImpulseRefused::Suppressed {
                index: 0,
                suppression: Suppression::AlreadyRejected
            }]
        );
        // Vacuity control: the same ledger does not silence everything. A
        // different suggestion about the same door still gets through, or this
        // is a mute button rather than a feedback loop.
        let other = interpret_answer(
            &answer(&one_impulse("front-door", "close the garage", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert_eq!(other.proposals.len(), 1, "refusals: {:?}", other.refusals);
    }

    /// Rephrasing is not a second hearing. The shape compares normalised text,
    /// so capitalising and adding a full stop does not get past a rejection.
    #[test]
    fn rephrasing_a_rejected_suggestion_does_not_get_past_it() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        let ledger = FeedbackLedger::from_decisions(
            &[decision_about(
                "front-door",
                "ask about the delivery",
                DraftStatus::Rejected,
                now,
            )],
            now,
        );
        let out = interpret_answer(
            &answer(&one_impulse("Front-Door", "Ask about the delivery.", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert!(out.proposals.is_empty(), "a rewording got a second hearing");
    }

    #[test]
    fn three_different_declines_about_one_trigger_silence_the_trigger() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        let decisions: Vec<ProposalDecision> = (0..REJECTIONS_THAT_SILENCE_A_TRIGGER)
            .map(|i| {
                decision_about(
                    "garage",
                    &format!("suggestion number {i}"),
                    DraftStatus::Rejected,
                    now,
                )
            })
            .collect();
        let ledger = FeedbackLedger::from_decisions(&decisions, now);

        let out = interpret_answer(
            &answer(&one_impulse("garage", "something entirely new", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert!(matches!(
            out.refusals.as_slice(),
            [ImpulseRefused::Suppressed {
                suppression: Suppression::TriggerSilenced { .. },
                ..
            }]
        ));
        // One fewer decline leaves the trigger audible, so the threshold is a
        // threshold and not "any rejection silences the source".
        let ledger = FeedbackLedger::from_decisions(
            &decisions[..REJECTIONS_THAT_SILENCE_A_TRIGGER - 1],
            now,
        );
        let out = interpret_answer(
            &answer(&one_impulse("garage", "something entirely new", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert_eq!(out.proposals.len(), 1, "refusals: {:?}", out.refusals);
    }

    /// The same suggestion rejected three times is a bug in the suppression,
    /// not three opinions about the trigger. Counting repeats would silence a
    /// whole camera on the strength of one preference.
    #[test]
    fn the_same_decline_recorded_three_times_silences_only_itself() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        let decisions: Vec<ProposalDecision> = (0..3)
            .map(|_| decision_about("garage", "the same suggestion", DraftStatus::Rejected, now))
            .collect();
        let ledger = FeedbackLedger::from_decisions(&decisions, now);
        assert_eq!(ledger.rejected_count(), 1);
        assert_eq!(ledger.silenced_triggers().count(), 0);
        let out = interpret_answer(
            &answer(&one_impulse("garage", "something entirely new", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert_eq!(out.proposals.len(), 1, "refusals: {:?}", out.refusals);
    }

    #[test]
    fn an_approval_and_an_expiry_suppress_nothing() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        for status in [DraftStatus::Approved, DraftStatus::Expired] {
            let ledger = FeedbackLedger::from_decisions(
                &[decision_about(
                    "front-door",
                    "ask about the delivery",
                    status.clone(),
                    now,
                )],
                now,
            );
            assert_eq!(
                ledger.rejected_count(),
                0,
                "{status} was treated as a refusal"
            );
            let out = interpret_answer(
                &answer(&one_impulse("front-door", "ask about the delivery", 0.9)),
                &audience(),
                now,
                &ledger,
                0,
            );
            assert_eq!(out.proposals.len(), 1, "{status} silenced a suggestion");
        }
    }

    #[test]
    fn a_rejection_stops_suppressing_once_it_is_old() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        let stale = decision_about(
            "front-door",
            "ask about the delivery",
            DraftStatus::Rejected,
            now - SUPPRESSION_WINDOW - Duration::seconds(1),
        );
        assert_eq!(
            FeedbackLedger::from_decisions(&[stale.clone()], now).rejected_count(),
            0
        );
        // Inside the window it still suppresses, so the comparison is a window
        // and not a switch that discards everything.
        let fresh = decision_about(
            "front-door",
            "ask about the delivery",
            DraftStatus::Rejected,
            now - SUPPRESSION_WINDOW + Duration::seconds(1),
        );
        assert_eq!(
            FeedbackLedger::from_decisions(&[fresh], now).rejected_count(),
            1
        );
    }

    #[test]
    fn the_brief_tells_the_child_the_time_it_cannot_ask_for() {
        use crate::user_data::domain::draft::DraftStatus;
        let now = Utc::now();
        let event = BusEventRef::new(
            "camera",
            Some("front-door".into()),
            Some("person".into()),
            now,
        )
        .unwrap();
        let ledger = FeedbackLedger::from_decisions(
            &[decision_about(
                "front-door",
                "ask about the delivery",
                DraftStatus::Rejected,
                now,
            )],
            now,
        );
        let brief = review_brief(now, std::slice::from_ref(&event), &ledger);
        assert!(
            brief.contains(&now.to_rfc3339()),
            "a subagent has no clock: the brief is the only place the time can come from"
        );
        assert!(brief.contains("camera events from front-door (person)"));
        assert!(
            brief.contains("ask about the delivery"),
            "the brief must say what was already declined"
        );
        // With nothing observed, the brief says so rather than inventing a list.
        assert!(
            review_brief(now, &[], &FeedbackLedger::empty()).contains("Nothing has been observed")
        );
    }

    /// The refusal this phase would otherwise have hit on every single tick.
    ///
    /// `GooseOrchestrator::spawn` starts by asking the registry for the live
    /// turn behind `spec.parent_session_id()` and errors when there is none —
    /// and `parent_turn_token` matches on `authority.session_id()`, not on the
    /// key the entry was published under. A reviewer that published under one
    /// id and planned under another would compile, run, and refuse every
    /// review with "no live turn holds the authority", which reads like a
    /// correctly-working guard rather than like broken wiring.
    #[test]
    fn a_review_publishes_the_same_authority_the_orchestrator_will_look_for() {
        use crate::shared::services::turn_authority::TurnAuthorityRegistry;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let session_id = review_session_id("run-1");
        let audience = audience();

        let spec = plan_review(
            &role(),
            &audience,
            &session_id,
            "review what happened",
            serde_json::json!({}),
        )
        .expect("the shipped role must be delegable under its own root authority");

        let registry = Arc::new(TurnAuthorityRegistry::new());
        let cancel = CancellationToken::new();
        let _lease = registry.publish(
            &session_id,
            review_authority(&audience, &session_id),
            cancel.clone(),
        );

        let found = registry.parent_turn_token(spec.parent_session_id()).expect(
            "the orchestrator looks the parent turn up by the spec's parent_session_id; \
                 without a match it refuses the spawn and no review ever runs",
        );
        // Not merely "a token": cancelling the loop's own token must be what
        // reaches the child, which is invariant 3's interruption path.
        assert!(!found.is_cancelled());
        cancel.cancel();
        assert!(
            found.is_cancelled(),
            "the registry handed back a token that is not the reviewer's, so activity could \
             never cancel a run in flight"
        );

        // The lease revokes on drop, so a review that has ended cannot be
        // delegated from — the same property a user's finished turn has.
        drop(_lease);
        assert!(registry.parent_turn_token(&session_id).is_none());
    }

    // ── Who a review is for, and what it may be told ────────────────────────

    fn session(id: &str, profile_id: Option<&str>, updated_at: DateTime<Utc>) -> Session {
        Session {
            id: id.to_string(),
            title: None,
            profile_id: profile_id.map(str::to_string),
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            model_name: None,
            created_at: updated_at,
            updated_at,
        }
    }

    /// The defect this phase would otherwise have shipped: a reviewer that can
    /// never address anybody, on every pond, forever, looking exactly like a
    /// feature nobody switched on.
    ///
    /// A review starts after `INACTIVITY_THRESHOLD_SECS` of quiet. The presence
    /// observer's `attribution_candidates` is bounded by that *same* constant,
    /// so at the moment a review becomes eligible, its answer is guaranteed
    /// empty. Reusing it here was the obvious move and it is wrong; this pins
    /// the relationship rather than the number, so raising the idle threshold
    /// tomorrow fails here instead of silently switching the reviewer off.
    #[test]
    fn the_audience_window_outlives_the_idle_that_starts_a_review() {
        let idle = i64::try_from(INACTIVITY_THRESHOLD_SECS).unwrap();
        assert!(
            AUDIENCE_WINDOW.num_seconds() > idle,
            "AUDIENCE_WINDOW is {}s and a review cannot start until {}s of quiet — every tick \
             would find nobody to address and the reviewer would never propose anything",
            AUDIENCE_WINDOW.num_seconds(),
            idle
        );
    }

    /// Exhaustiveness is the compiler's job; what this pins is the *disposition*
    /// — which families feed a review and which are refused — and the field
    /// mapping, because a `source_id` and a `signal` that swap places make the
    /// feedback ledger suppress the wrong thing.
    #[test]
    fn two_bus_families_are_refused_and_the_other_four_map_to_their_source_and_signal() {
        use crate::shared::domain::session_activity::{
            PresenceTransition, ProfilePresence, SessionLifecycle, SessionPhase,
        };
        use crate::shared::domain::time_tick::{TimeBoundary, TimeTick};
        use crate::user_data::domain::device::{DeviceId, DeviceStateChanged, DeviceStateValue};
        use crate::user_data::domain::sensor::{CameraEvent, SensorReading};
        use crate::user_data::domain::session::IdentificationSource;

        let now = Utc::now();

        let sensor = reviewable(&BusEvent::Sensor(SensorReading {
            device_id: "hallway".into(),
            sensor_type: "temperature".into(),
            value: 19.0,
            unit: "C".into(),
            recorded_at: now,
        }))
        .expect("a sensor reading is a household fact");
        assert_eq!(
            (sensor.kind(), sensor.source_id(), sensor.signal()),
            ("sensor", Some("hallway"), Some("temperature"))
        );

        let camera = reviewable(&BusEvent::Camera(CameraEvent {
            id: None,
            camera_id: "front-door".into(),
            event_type: "person".into(),
            confidence: Some(0.8),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: now,
        }))
        .expect("a camera event is a household fact");
        assert_eq!(
            (camera.kind(), camera.source_id(), camera.signal()),
            ("camera", Some("front-door"), Some("person"))
        );

        let device = reviewable(&BusEvent::Device(DeviceStateChanged {
            device_id: DeviceId::from("porch-light"),
            key: "on".into(),
            value: DeviceStateValue::Bool(true),
            changed_at: now,
        }))
        .expect("a device state change is a household fact");
        assert_eq!(
            (device.kind(), device.source_id(), device.signal()),
            ("device", Some("porch-light"), Some("on"))
        );

        let presence = reviewable(&BusEvent::Presence(ProfilePresence {
            profile_id: EXEMPLAR_OWNER_ID.into(),
            transition: PresenceTransition::Arrived,
            source: IdentificationSource::Explicit,
            confidence: None,
            session_id: "chat-1".into(),
            at: now,
        }))
        .expect("presence is what makes a suggestion timely");
        assert_eq!(
            (presence.kind(), presence.source_id(), presence.signal()),
            ("presence", Some(EXEMPLAR_OWNER_ID), Some("arrived"))
        );

        assert!(
            reviewable(&BusEvent::Time(TimeTick {
                boundary: TimeBoundary::Hour,
                at: now,
                local_hour: 3,
            }))
            .is_none(),
            "the hourly tick is a heartbeat; the brief already states the time from the clock"
        );
        assert!(
            reviewable(&BusEvent::Session(SessionLifecycle {
                phase: SessionPhase::Idle,
                session_id: Some("chat-1".into()),
                idle_secs: 900,
                at: now,
            }))
            .is_none(),
            "Idle is the transition that STARTS a review — feeding it back hands the child its \
             own trigger as evidence"
        );
    }

    #[test]
    fn a_review_is_addressed_to_whoever_spoke_most_recently() {
        let now = Utc::now();
        let sessions = vec![
            session("chat-old", Some("ada"), now - Duration::hours(4)),
            session(
                "chat-new",
                Some(EXEMPLAR_OWNER_ID),
                now - Duration::hours(1),
            ),
            session("chat-anon", None, now - Duration::minutes(1)),
        ];
        let audience = audience_for_review(&sessions, now, &[]).expect("somebody was here");
        assert_eq!(
            audience.profile_id(),
            EXEMPLAR_OWNER_ID,
            "an unattributed conversation is newer, but it names nobody to address"
        );
    }

    /// The measured blocker, and the fallthrough that clears it.
    ///
    /// On a real pond: 961 sessions, zero carrying a `profile_id`, because the
    /// only writer is `resolve_turn_scope`'s `DeviceRung::Member` arm and the
    /// desktop's own device row is never attributed. So this returned `None` on
    /// every tick the reviewer has ever run, and PAI-7's model-backed producer
    /// was unreachable — which is why Home only ever showed the template-tier
    /// suggestions.
    #[test]
    fn a_one_member_pond_is_addressed_even_when_nothing_is_attributed() {
        let now = Utc::now();
        let roster = vec![EXEMPLAR_OWNER_ID.to_string()];
        let unattributed = session("chat-1", None, now - Duration::minutes(2));

        // The control first: the same pond with no roster still answers None,
        // so what follows is the fallthrough and not some other admission.
        assert!(
            audience_for_review(&[unattributed.clone()], now, &[]).is_none(),
            "with no roster there is nobody to fall through to"
        );

        assert_eq!(
            audience_for_review(&[unattributed], now, &roster)
                .expect("a one-member pond has exactly one possible answer")
                .profile_id(),
            EXEMPLAR_OWNER_ID,
        );
    }

    /// Invariant 4, held. Two members and no attribution is the case where
    /// picking would be attribution by row order — the rule PAI-1 P3 refused —
    /// so the answer stays "no review".
    #[test]
    fn two_members_and_no_attribution_is_still_nobody() {
        let now = Utc::now();
        let roster = vec![EXEMPLAR_OWNER_ID.to_string(), "liz".to_string()];
        let unattributed = session("chat-1", None, now - Duration::minutes(2));

        assert!(
            audience_for_review(&[unattributed], now, &roster).is_none(),
            "with two members, choosing one would be attribution by row order"
        );
    }

    /// The roster never overrides an attribution the pond actually made. A face
    /// match or a paired device outranks a guess, even a guess with only one
    /// candidate.
    #[test]
    fn an_attribution_the_pond_made_outranks_the_roster() {
        let now = Utc::now();
        let roster = vec!["liz".to_string()];
        let theirs = session(
            "chat-1",
            Some(EXEMPLAR_OWNER_ID),
            now - Duration::minutes(1),
        );

        assert_eq!(
            audience_for_review(&[theirs], now, &roster)
                .expect("an attributed conversation names its member")
                .profile_id(),
            EXEMPLAR_OWNER_ID,
            "the roster must not rename somebody the pond identified",
        );
    }

    /// The fallthrough does not wake a pond nobody has touched. `AUDIENCE_WINDOW`
    /// still has to hold, or a one-member pond would be proposed to forever on
    /// the strength of a conversation from last March.
    #[test]
    fn a_silent_pond_is_not_addressed_just_because_it_has_one_member() {
        let now = Utc::now();
        let roster = vec![EXEMPLAR_OWNER_ID.to_string()];
        let stale = session("chat-1", None, now - AUDIENCE_WINDOW - Duration::minutes(1));

        assert!(
            audience_for_review(&[stale], now, &roster).is_none(),
            "nobody has been here inside the window, so there is nothing to review",
        );
    }

    /// A review's own session id starts with `sched-`, so this is not a
    /// hypothetical: without the origin filter, the first review would nominate
    /// itself as the audience for the second.
    #[test]
    fn the_ponds_own_conversations_never_nominate_an_audience() {
        let now = Utc::now();
        let mine = session(
            &review_session_id("run-1"),
            Some(EXEMPLAR_OWNER_ID),
            now - Duration::minutes(1),
        );
        assert!(!SessionOrigin::of(&mine.id).is_human());
        assert!(
            audience_for_review(&[mine], now, &[]).is_none(),
            "a review addressed to the member its own previous run was scoped to is the pond \
             talking to itself"
        );

        // Vacuity control: the same row under a human id DOES address them, so
        // the assertion above is the origin filter and not some other refusal.
        let theirs = session(
            "chat-1",
            Some(EXEMPLAR_OWNER_ID),
            now - Duration::minutes(1),
        );
        assert_eq!(
            audience_for_review(&[theirs], now, &[])
                .expect("a human conversation names its member")
                .profile_id(),
            EXEMPLAR_OWNER_ID
        );
    }

    #[test]
    fn nobody_here_for_six_hours_means_no_review_rather_than_a_broadcast() {
        let now = Utc::now();
        let stale = session(
            "chat-1",
            Some(EXEMPLAR_OWNER_ID),
            now - AUDIENCE_WINDOW - Duration::seconds(1),
        );
        assert!(audience_for_review(&[stale], now, &[]).is_none());
        assert!(audience_for_review(&[], now, &[]).is_none());
    }

    /// The brief goes around the scope, not through it: it is prose handed to
    /// the model, so PAI-6's clamp cannot see it. A presence row is the one
    /// event family whose `source_id` is a person.
    #[test]
    fn a_presence_event_about_another_member_never_reaches_the_brief() {
        let now = Utc::now();
        let mine = BusEventRef::new(
            "presence",
            Some(EXEMPLAR_OWNER_ID.into()),
            Some("arrived".into()),
            now,
        )
        .unwrap();
        let theirs = BusEventRef::new(
            "presence",
            Some("someone-else".into()),
            Some("arrived".into()),
            now,
        )
        .unwrap();
        let door = BusEventRef::new(
            "camera",
            Some("front-door".into()),
            Some("person".into()),
            now,
        )
        .unwrap();

        let shown = brief_events(&audience(), &[mine.clone(), theirs.clone(), door.clone()]);
        assert!(
            !shown.contains(&theirs),
            "'someone-else arrived' in a review addressed to {} is a disclosure the tool layer \
             would have refused",
            EXEMPLAR_OWNER_ID
        );
        // Two vacuity controls, because "the list is empty" would also satisfy
        // the assertion above: the addressed member's own presence survives,
        // and so does a household fact that names no one.
        assert!(
            shown.contains(&mine),
            "the audience's own presence is theirs"
        );
        assert!(shown.contains(&door), "a camera event names no member");
    }

    #[test]
    fn a_chatty_sensor_cannot_crowd_the_brief_out_of_the_childs_window() {
        let now = Utc::now();
        let mut observed = Vec::new();
        // One sensor reporting sixty times is one thing that happened.
        for minute in 0..60 {
            observed.push(
                BusEventRef::new(
                    "sensor",
                    Some("hallway".into()),
                    Some("temperature".into()),
                    now - Duration::minutes(minute),
                )
                .unwrap(),
            );
        }
        let shown = brief_events(&audience(), &observed);
        assert_eq!(shown.len(), 1, "repeats collapse to one line");
        assert_eq!(
            shown[0].observed_at(),
            now,
            "and the line kept is the newest reading, not the first one seen"
        );

        // Distinct things, on the other hand, are capped — oldest dropped.
        let distinct: Vec<BusEventRef> = (0..MAX_BRIEF_EVENTS + 10)
            .map(|n| {
                BusEventRef::new(
                    "sensor",
                    Some(format!("device-{n}")),
                    Some("temperature".into()),
                    now - Duration::minutes(n as i64),
                )
                .unwrap()
            })
            .collect();
        let shown = brief_events(&audience(), &distinct);
        assert_eq!(shown.len(), MAX_BRIEF_EVENTS);
        assert_eq!(
            shown[0].observed_at(),
            now,
            "newest first, so the cap drops the oldest rather than the last delivered"
        );
    }
}
