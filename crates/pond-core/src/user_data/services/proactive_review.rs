//! Pure decision half of the proactive reviewer: when a review runs, what its child may reach,
//! how its answer becomes proposals, and which ones the member already declined.
//!
//! The child holds no actuating group, so it only produces words; [`interpret_answer`] validates
//! them into proposals whose audience, expiry, cap and kind the model cannot choose.

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
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

// ── The role ────────────────────────────────────────────────────────────────

/// The role name a review runs under.
pub const PROACTIVE_REVIEWER_ROLE: &str = "proactive-reviewer";

/// The shipped recipe: a `const`, not an `agent_recipes` row, so nobody can widen its
/// `tool_groups` at run time. The date comes via the brief: subagents lack `get_current_time`.
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

/// Build the shipped reviewer role; fallible so a recipe typo is a refusal, not a loop panic.
pub fn proactive_reviewer_role() -> Result<AgentRole, RoleError> {
    AgentRole::from_recipe_yaml(PROACTIVE_REVIEWER_ROLE, REVIEWER_ROLE_RECIPE)?.ok_or(
        RoleError::Yaml {
            role: PROACTIVE_REVIEWER_ROLE.to_string(),
            message: format!("the shipped recipe has no `{ROLE_YAML_KEY}` block"),
        },
    )
}

/// Tool-group ceiling for a review, which has no parent turn to inherit one from.
/// Read-only groups only: widening this is the easiest way to turn a proposer into an actor.
pub const PROACTIVE_ROOT_GROUPS: &[&str] = &[
    "giap-memory",
    "giap-device",
    "giap-sensors",
    "giap-weather",
    "giap-knowledge",
];

// ── When it runs ────────────────────────────────────────────────────────────

/// Starts with `sched-` on purpose, so `SessionOrigin::of` treats review sessions as the pond's
/// own and they never read as somebody being home.
pub const REVIEW_SESSION_PREFIX: &str = "sched-proactive-review-";

/// The parent session id for one review run.
pub fn review_session_id(run_id: &str) -> String {
    format!("{REVIEW_SESSION_PREFIX}{}", run_id.trim())
}

/// At most this many proposals may be made for one member in a day.
pub const MAX_PROPOSALS_PER_DAY: usize = 6;

/// Max proposals from one review; below the daily cap so one run can't spend the day's budget.
pub const MAX_PROPOSALS_PER_RUN: usize = 3;

/// How long a review's proposal stays live: half of `MAX_PROPOSAL_TTL`, since it's about now.
pub const PROPOSAL_TTL: Duration = Duration::hours(12);

/// Minimum impulse confidence; the wire field defaults to `0.0`, so omitting it is refused.
pub const MIN_PROPOSAL_CONFIDENCE: f32 = 0.5;

/// Everything [`should_review`] needs; private fields so no caller builds a partial one.
#[derive(Debug, Clone, Copy)]
pub struct ReviewInputs {
    schedule: GateInputs,
    orchestrator_enabled: bool,
    proposals_today: usize,
    run_in_flight: bool,
}

impl ReviewInputs {
    /// Keeps `orchestrator_enabled` apart from `schedule.enabled`: without it no child can run.
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
    /// Short, stable label for structured logs, like [`SkipReason::as_str`].
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

/// May a review start now? Timing is delegated to `should_run`; don't restate it here.
pub fn should_review(inputs: &ReviewInputs) -> ReviewDecision {
    if !inputs.orchestrator_enabled {
        return ReviewDecision::Skip(ReviewSkip::OrchestratorDisabled);
    }
    if let GateDecision::Skip(reason) = should_run(inputs.schedule) {
        return ReviewDecision::Skip(ReviewSkip::Schedule(reason));
    }
    if inputs.run_in_flight {
        return ReviewDecision::Skip(ReviewSkip::RunInFlight);
    }
    if inputs.proposals_today >= MAX_PROPOSALS_PER_DAY {
        return ReviewDecision::Skip(ReviewSkip::DailyCapReached {
            made: inputs.proposals_today,
            cap: MAX_PROPOSALS_PER_DAY,
        });
    }
    ReviewDecision::Run
}

/// Must an in-flight review be cancelled? Reuses `saw_activity_since_start` from the run's
/// start, which also sees the out-of-process voice child; on an Orin the review holds the GPU.
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

/// The root authority for one review, shared with [`plan_review`]. The loop registers it as the
/// run's parent turn: `GooseOrchestrator::spawn` refuses a spec with no live turn behind it.
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

/// Authorise one review run; a [`ProposalAudience`] can't be the household or a guest.
/// `background: false`: on-device providers refuse background children.
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

/// How far back a review looks for its member: past the idle threshold that starts a review
/// (or nobody is ever addressed), but not back to yesterday's user.
pub const AUDIENCE_WINDOW: Duration = Duration::hours(6);

/// Max distinct events (after [`brief_events`] collapses repeats) in one brief: the child's
/// 4096-token window includes its role instructions, and overflow evicts the answer schema.
pub const MAX_BRIEF_EVENTS: usize = 24;

/// Project a bus event onto a proposal's reference, or refuse it; no wildcard arm on purpose.
/// Refused: `Time` (the brief states the time) and `Session` (`Idle` is the review's own trigger).
pub fn reviewable(event: &BusEvent) -> Option<BusEventRef> {
    // `.ok()` not `expect`: a future computed kind must not panic inside a background loop.
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

/// The member of the most recent attributed human session in [`AUDIENCE_WINDOW`], or `None`,
/// which means no review: the alternative is a household broadcast.
pub fn audience_for_review(sessions: &[Session], now: DateTime<Utc>) -> Option<ProposalAudience> {
    let cutoff = now - AUDIENCE_WINDOW;
    sessions
        .iter()
        .filter(|s| SessionOrigin::of(&s.id).is_human())
        .filter(|s| s.updated_at > cutoff)
        .filter_map(|s| s.profile_id.as_deref().map(|p| (s.updated_at, p)))
        .max_by_key(|(at, _)| *at)
        .and_then(|(_, profile_id)| ProposalAudience::for_member(profile_id).ok())
}

/// The events a review for `audience` may see. Presence naming anyone else is dropped: the
/// brief is prose straight to the model, so the tool-layer scope can't protect it.
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

/// The brief a review is given. States the time since subagents lack `get_current_time`; lists
/// declines to save a turn, though [`interpret_answer`] enforces them regardless.
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

/// One suggestion as the model writes it; nothing here is a capability. Deliberately not
/// `deny_unknown_fields`: small models add stray keys, and required fields already catch typos.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReviewerImpulse {
    /// The bus event family this is about.
    pub trigger_kind: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub signal: Option<String>,
    /// Required; `Proposal::from_parts` refuses a blank one.
    pub rationale: String,
    /// What to ask the member about. Becomes a prompt and nothing else.
    pub suggestion: String,
    /// Defaults below [`MIN_PROPOSAL_CONFIDENCE`], so omitting it is a refusal.
    #[serde(default)]
    pub confidence: f32,
}

/// The one action an impulse may become: always a prompt, never a model-chosen webhook or rule.
pub fn impulse_action(suggestion: &str) -> TaskKind {
    TaskKind::AgentPrompt {
        prompt: suggestion.trim().to_string(),
    }
}

/// Why one impulse did not become a proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum ImpulseRefused {
    /// No answer the parent may read: cancelled, out of turns, or failed.
    NoAnswer,
    /// The answer contained no JSON array at all.
    NotAJsonArray,
    /// One element did not match the schema.
    Unreadable { index: usize, message: String },
    /// Below [`MIN_PROPOSAL_CONFIDENCE`].
    LowConfidence { index: usize, confidence: f32 },
    /// The domain refused it — a blank rationale, an unusable trigger.
    Invalid { index: usize, error: ProposalError },
    /// The member has already said no to this.
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
    /// Validated, addressed, expiring proposals, ready for `ProposalRepository::save`.
    pub proposals: Vec<Proposal>,
    /// Everything that did not make it, and why; kept so a silent reviewer can be diagnosed.
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

/// Turn a finished run's answer into proposals; every fact but the words comes from here.
/// Reads `result_for_parent()`: Goose returns partial text for a cancelled or exhausted child.
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
        // Per element, so one malformed suggestion doesn't cost the others.
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
    // `now`, never the model's timestamp: a subagent has no clock.
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

/// The first balanced `[...]` in the text, ignoring brackets inside JSON strings.
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

/// How long a rejection keeps suppressing: long enough to feel heard, not forever.
pub const SUPPRESSION_WINDOW: Duration = Duration::days(30);

/// How many *distinct* rejected suggestions about one trigger silence the trigger itself.
pub const REJECTIONS_THAT_SILENCE_A_TRIGGER: usize = 3;

/// Why a proposal was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppression {
    /// This exact suggestion, about this exact trigger, was rejected.
    AlreadyRejected,
    /// Enough distinct suggestions about this trigger were rejected to stop raising it.
    TriggerSilenced { rejections: usize },
}

/// What the member's past decisions forbid the next review; deliberately not a learning system.
/// Matches on shape, not words, so a rephrased decline gets no second hearing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedbackLedger {
    rejected: BTreeSet<ProposalShape>,
    silenced: BTreeMap<TriggerIdentity, usize>,
}

impl FeedbackLedger {
    /// A ledger that suppresses nothing.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Fold recent rejections (not expiries) into what the next review may not say.
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

/// Groups no reviewer may reach, listed independently of [`PROACTIVE_ROOT_GROUPS`] and
/// `groups_denied_to_subagents` so the guard can't shrink along with them.
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
        // Vacuity control: a requested group on the read-only ceiling survives.
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
        // Vacuity control: a real conversation id still reads as a person.
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

    #[test]
    fn the_orchestrator_being_off_outranks_every_other_reason_to_run() {
        let off = ReviewInputs::for_tick(idle_schedule(), false, 0, false);
        assert_eq!(
            should_review(&off),
            ReviewDecision::Skip(ReviewSkip::OrchestratorDisabled)
        );
    }

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
        // One under the cap still runs: no off-by-one.
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

    /// Goose returns `Ok(partial_text)` for a cancelled child, so `run.result` is not an answer.
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
        // Vacuity control: the same body under `Completed` does produce one.
        let out = interpret_answer(
            &answer(&one_impulse("front-door", "ask about the delivery", 0.9)),
            &audience(),
            Utc::now(),
            &FeedbackLedger::empty(),
            0,
        );
        assert_eq!(out.proposals.len(), 1);
    }

    /// Stray keys are ignored rather than refused, and none of them reaches a decision.
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

        // The four keys above were ignored; these values came from the caller.
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

    /// Requiredness refuses typos: this fails if `suggestion` ever gains `#[serde(default)]`.
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

    /// Only the unbalanced cases test string tracking; a balanced pair passes without it.
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

    /// Driven through `interpret_answer`, not the ledger, so an unconsulted ledger fails it.
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
        // Vacuity control: a different suggestion about the same door still gets through.
        let other = interpret_answer(
            &answer(&one_impulse("front-door", "close the garage", 0.9)),
            &audience(),
            now,
            &ledger,
            0,
        );
        assert_eq!(other.proposals.len(), 1, "refusals: {:?}", other.refusals);
    }

    /// The shape compares normalised text, so case and a trailing full stop change nothing.
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
        // One fewer decline leaves the trigger audible.
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
        // Inside the window it still suppresses.
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

    /// `parent_turn_token` matches on `authority.session_id()`, not the publish key, so a mismatch
    /// here would refuse every review while looking like a working guard.
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
        // It must be the loop's own token: cancelling it is how activity reaches the child.
        assert!(!found.is_cancelled());
        cancel.cancel();
        assert!(
            found.is_cancelled(),
            "the registry handed back a token that is not the reviewer's, so activity could \
             never cancel a run in flight"
        );

        // The lease revokes on drop, so an ended review can't be delegated from.
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

    /// Pins the relationship, not the number, so raising the idle threshold fails here.
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

    /// Swapped `source_id`/`signal` would make the feedback ledger suppress the wrong thing.
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
        let audience = audience_for_review(&sessions, now).expect("somebody was here");
        assert_eq!(
            audience.profile_id(),
            EXEMPLAR_OWNER_ID,
            "an unattributed conversation is newer, but it names nobody to address"
        );
    }

    /// A review's own session is `sched-`: unfiltered, one review would address the next.
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
            audience_for_review(&[mine], now).is_none(),
            "a review addressed to the member its own previous run was scoped to is the pond \
             talking to itself"
        );

        // Vacuity control: the same row under a human id does address them.
        let theirs = session(
            "chat-1",
            Some(EXEMPLAR_OWNER_ID),
            now - Duration::minutes(1),
        );
        assert_eq!(
            audience_for_review(&[theirs], now)
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
        assert!(audience_for_review(&[stale], now).is_none());
        assert!(audience_for_review(&[], now).is_none());
    }

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
        // Vacuity controls: the member's own presence and a household fact both survive.
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
