//! When a household's agents run on their own, and what the runner does around a run. Pure: the
//! `pond-server` loop subscribes to the bus and keeps the clock, the long-task runner does the
//! writes, and every rule here is tested without either.
//!
//! An activation queues a long task of one step, so the existing runner runs it on the lane at
//! the household's pace; a hand-off appends one step to the same task.

use crate::shared::domain::long_task::{
    validate_appended_step, validate_plan, Artifact, ArtifactError, ArtifactKind, ArtifactSummary,
    LongTask, LongTaskBudget, LongTaskError, LongTaskParts, LongTaskState, LongTaskStateRefused,
    OutcomeKind, PlanError, PlanStep, StepDraft, StepOutcome, StepState, ValidatedPlan,
    DEFAULT_LONG_TASK_STEPS, MAX_STEP_KEY_CHARS,
};
use crate::shared::domain::orchestration::AgentRole;
use crate::shared::domain::role_block::{
    Activation, AfterHook, BeforeHook, Cooldown, CronSchedule, EventTrigger, Subject, Threshold,
};
use crate::shared::domain::session_activity::{PresenceTransition, ProfilePresence};
use crate::shared::ports::event_bus::BusEvent;
use crate::shared::services::long_task_runner::RESULT_TITLE;
use crate::user_data::domain::device::DeviceStateValue;
use crate::user_data::domain::profile::ProfileScope;
use chrono::{DateTime, Timelike, Utc};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use thiserror::Error;

/// Hand-offs one task may take; past it a chain is refused, which bounds a run to 1 + 4 steps.
pub const MAX_HAND_OFFS_PER_TASK: usize = 4;

/// Marks the steps the runner added for a hand-off, so the cap and the chain can be read back.
/// A planner's key that happens to start so only makes the cap stricter.
pub const HAND_OFF_KEY_PREFIX: &str = "hand-to-";

/// Marks a task an activation queued: durable without a column, so a restart still knows it.
pub const ACTIVATION_TASK_ID_PREFIX: &str = "agent-";

/// Marks a task an agent's `start_task` hook started; such a task may not start another.
pub const FOLLOW_UP_TASK_ID_PREFIX: &str = "follow-up-";

/// An activation's run is stale after this; a guess at how late a scheduled brief is still
/// worth having, never measured.
pub const ACTIVATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

/// Items of a list a before-hook shows; the block rides in every step's window.
pub const MAX_BEFORE_HOOK_ITEMS: usize = 8;

/// Agents a step's brief offers to hand on to.
pub const MAX_OFFERED_HAND_OFFS: usize = 6;

pub const BEFORE_HOOKS_HEADING: &str = "Gathered before you started:";

/// Why an activation's run is told it started; kept short, it sits in the step's instructions.
pub const WHY_HEADING: &str = "Why this run started:";

// ── Which activation fires ──────────────────────────────────────────────────

/// One activation of one agent. The fingerprint changes when the activation is edited, so its
/// cooldown and clock start afresh.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActivationKey {
    pub agent: String,
    pub index: usize,
    fingerprint: u64,
}

impl ActivationKey {
    pub fn of(agent: &str, index: usize, activation: &Activation) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{activation:?}").hash(&mut hasher);
        Self {
            agent: agent.to_string(),
            index,
            fingerprint: hasher.finish(),
        }
    }
}

/// An active recipe's role and whose it is.
#[derive(Debug, Clone)]
pub struct ActivationAgent {
    pub role: AgentRole,
    pub owner_profile_id: Option<String>,
}

/// What a bus event was, as far as one trigger cares.
#[derive(Debug, Clone, PartialEq)]
pub struct EventMatch {
    /// The member it is about; only presence names one.
    pub member: Option<String>,
    /// The camera, sensor or device it came from, so a threshold's edge is tracked per source.
    pub source: Option<String>,
    /// For a threshold, whether this reading is past it.
    pub past_threshold: Option<bool>,
    /// A clause for the brief, without the member's name.
    pub detail: String,
}

/// Whether `event` is one `trigger` listens for, for an agent `owner` owns. Time-of-day triggers
/// never match the bus.
pub fn match_event(
    trigger: &EventTrigger,
    event: &BusEvent,
    owner: Option<&str>,
) -> Option<EventMatch> {
    match (trigger, event) {
        (EventTrigger::PresenceArrive { member }, BusEvent::Presence(p))
            if matches!(p.transition, PresenceTransition::Arrived)
                && member.admits(&p.profile_id, owner) =>
        {
            Some(presence_match(p, "arrived home"))
        }
        (EventTrigger::PresenceLeave { member }, BusEvent::Presence(p))
            if matches!(p.transition, PresenceTransition::Departed)
                && member.admits(&p.profile_id, owner) =>
        {
            Some(presence_match(p, "left home"))
        }
        (EventTrigger::CameraPerson { camera }, BusEvent::Camera(c))
            if c.event_type.eq_ignore_ascii_case("person")
                && camera.admits(&c.camera_id, owner) =>
        {
            Some(source_match(
                &c.camera_id,
                format!("camera {} saw a person", c.camera_id),
            ))
        }
        (EventTrigger::CameraMotion { camera }, BusEvent::Camera(c))
            if c.event_type.eq_ignore_ascii_case("motion")
                && camera.admits(&c.camera_id, owner) =>
        {
            Some(source_match(
                &c.camera_id,
                format!("camera {} saw motion", c.camera_id),
            ))
        }
        (
            EventTrigger::Sensor {
                sensor_type,
                device,
                threshold,
            },
            BusEvent::Sensor(r),
        ) if r.sensor_type.eq_ignore_ascii_case(sensor_type)
            && device.admits(&r.device_id, owner) =>
        {
            let (word, limit) = match threshold {
                Threshold::Above(x) => ("above", x),
                Threshold::Below(x) => ("below", x),
            };
            Some(EventMatch {
                member: None,
                source: Some(r.device_id.clone()),
                past_threshold: Some(threshold.passes(r.value)),
                detail: format!(
                    "{} on {} read {} {}, {word} {limit}",
                    r.sensor_type, r.device_id, r.value, r.unit
                ),
            })
        }
        (EventTrigger::DeviceChange { key, device }, BusEvent::Device(d))
            if d.key.eq_ignore_ascii_case(key) && device.admits(d.device_id.as_str(), owner) =>
        {
            let value = match &d.value {
                DeviceStateValue::Bool(b) => b.to_string(),
                DeviceStateValue::Int(i) => i.to_string(),
                DeviceStateValue::Float(f) => f.to_string(),
                DeviceStateValue::Text(t) => t.clone(),
            };
            Some(source_match(
                d.device_id.as_str(),
                format!("{} on {} changed to {value}", d.key, d.device_id.as_str()),
            ))
        }
        _ => None,
    }
}

fn presence_match(p: &ProfilePresence, detail: &str) -> EventMatch {
    EventMatch {
        member: Some(p.profile_id.clone()),
        source: None,
        past_threshold: None,
        detail: detail.to_string(),
    }
}

fn source_match(source: &str, detail: String) -> EventMatch {
    EventMatch {
        member: None,
        source: Some(source.to_string()),
        past_threshold: None,
        detail,
    }
}

/// What the book says about one candidate run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Fire,
    CoolingDown,
    /// A reading still past its threshold, or back short of it: only the crossing runs.
    NoEdge,
    /// A clock activation whose time has not come.
    NotDue,
    /// The first sight since start or since an edit: it sets the baseline and runs nothing,
    /// so a restart never fires a time already past or a threshold already crossed.
    Baseline,
}

/// Cooldowns, threshold levels, clocks and the task each activation last queued. In memory: a
/// restart forgets them and begins again from a baseline.
#[derive(Debug, Default)]
pub struct ActivationBook {
    last_fired: HashMap<ActivationKey, DateTime<Utc>>,
    levels: HashMap<(ActivationKey, String), bool>,
    next_due: HashMap<ActivationKey, DateTime<Utc>>,
    tasks: HashMap<ActivationKey, String>,
}

impl ActivationBook {
    pub fn admit_event(
        &mut self,
        key: &ActivationKey,
        matched: &EventMatch,
        cooldown: Cooldown,
        now: DateTime<Utc>,
    ) -> Admission {
        if let Some(level) = matched.past_threshold {
            let source = matched.source.clone().unwrap_or_default();
            match self.levels.insert((key.clone(), source), level) {
                None => return Admission::Baseline,
                Some(true) => return Admission::NoEdge,
                Some(false) if !level => return Admission::NoEdge,
                Some(false) => {}
            }
        }
        self.cool(key, cooldown, now)
    }

    /// Whether a schedule or a daily time has come round. Missed occurrences collapse into one
    /// run, never a backlog.
    pub fn admit_clock(
        &mut self,
        key: &ActivationKey,
        cron: &CronSchedule,
        cooldown: Cooldown,
        timezone: &str,
        now: DateTime<Utc>,
    ) -> Admission {
        match self.next_due.get(key).copied() {
            None => {
                if let Some(next) = cron.next_after(now, timezone) {
                    self.next_due.insert(key.clone(), next);
                }
                Admission::Baseline
            }
            Some(due) if now < due => Admission::NotDue,
            Some(_) => {
                match cron.next_after(now, timezone) {
                    Some(next) => self.next_due.insert(key.clone(), next),
                    None => self.next_due.remove(key),
                };
                self.cool(key, cooldown, now)
            }
        }
    }

    fn cool(&mut self, key: &ActivationKey, cooldown: Cooldown, now: DateTime<Utc>) -> Admission {
        let window = chrono::Duration::from_std(cooldown.duration())
            .unwrap_or_else(|_| chrono::Duration::zero());
        if let Some(&last) = self.last_fired.get(key) {
            if now < last + window {
                return Admission::CoolingDown;
            }
        }
        self.last_fired.insert(key.clone(), now);
        Admission::Fire
    }

    pub fn note_task(&mut self, key: &ActivationKey, task_id: &str) {
        self.tasks.insert(key.clone(), task_id.to_string());
    }

    /// The task this activation last queued; the caller checks whether it is still open.
    pub fn task_for(&self, key: &ActivationKey) -> Option<&str> {
        self.tasks.get(key).map(String::as_str)
    }

    /// Drops what belongs to activations that no longer exist.
    pub fn retain(&mut self, live: &HashSet<ActivationKey>) {
        self.last_fired.retain(|key, _| live.contains(key));
        self.levels.retain(|(key, _), _| live.contains(key));
        self.next_due.retain(|key, _| live.contains(key));
        self.tasks.retain(|key, _| live.contains(key));
    }

    /// Forgets every clock, for a change of the household's timezone.
    pub fn reset_clocks(&mut self) {
        self.next_due.clear();
    }
}

/// Why a run started, for its brief.
#[derive(Debug, Clone, PartialEq)]
pub enum FiringReason {
    Schedule { cron: String },
    Daily { at: String },
    Event(EventMatch),
}

/// An activation that may run now.
#[derive(Debug, Clone, PartialEq)]
pub struct Firing {
    pub key: ActivationKey,
    pub agent: String,
    pub brief: String,
    pub reason: FiringReason,
}

/// Every activation, of every agent, that `event` fires now; a `between` window is read on
/// `timezone`'s wall clock.
pub fn firings_for_event(
    agents: &[ActivationAgent],
    event: &BusEvent,
    timezone: &str,
    book: &mut ActivationBook,
    now: DateTime<Utc>,
) -> Vec<Firing> {
    let local = now.with_timezone(&zone(timezone));
    let (hour, minute) = (local.hour(), local.minute());
    let mut firings = Vec::new();
    for agent in agents {
        for (index, activation) in agent.role.activations().iter().enumerate() {
            let Activation::Event(e) = activation else {
                continue;
            };
            // Outside its window an event is not seen at all, so its cooldown is not spent.
            if e.window
                .is_some_and(|window| !window.contains(hour, minute))
            {
                continue;
            }
            let Some(matched) = match_event(&e.trigger, event, agent.owner_profile_id.as_deref())
            else {
                continue;
            };
            let key = ActivationKey::of(agent.role.name(), index, activation);
            if book.admit_event(&key, &matched, e.cooldown, now) == Admission::Fire {
                firings.push(Firing {
                    key,
                    agent: agent.role.name().to_string(),
                    brief: e.brief.clone(),
                    reason: FiringReason::Event(matched),
                });
            }
        }
    }
    firings
}

/// Every schedule and daily time that has come round by `now`, on `timezone`'s wall clock.
pub fn firings_for_clock(
    agents: &[ActivationAgent],
    timezone: &str,
    book: &mut ActivationBook,
    now: DateTime<Utc>,
) -> Vec<Firing> {
    let mut firings = Vec::new();
    for agent in agents {
        for (index, activation) in agent.role.activations().iter().enumerate() {
            let (cron, cooldown, brief, reason) = match activation {
                Activation::Schedule(s) => (
                    s.cron.clone(),
                    s.cooldown,
                    &s.brief,
                    FiringReason::Schedule {
                        cron: s.cron.as_str().to_string(),
                    },
                ),
                Activation::Event(e) => match (&e.trigger, e.trigger.clock()) {
                    (EventTrigger::TimeDaily { hour, minute }, Some(cron)) => (
                        cron,
                        e.cooldown,
                        &e.brief,
                        FiringReason::Daily {
                            at: format!("{hour:02}:{minute:02}"),
                        },
                    ),
                    _ => continue,
                },
                _ => continue,
            };
            let key = ActivationKey::of(agent.role.name(), index, activation);
            if book.admit_clock(&key, &cron, cooldown, timezone, now) == Admission::Fire {
                firings.push(Firing {
                    key,
                    agent: agent.role.name().to_string(),
                    brief: brief.clone(),
                    reason,
                });
            }
        }
    }
    firings
}

/// The keys of every schedule and event activation, for [`ActivationBook::retain`].
pub fn live_keys(agents: &[ActivationAgent]) -> HashSet<ActivationKey> {
    agents
        .iter()
        .flat_map(|agent| {
            agent
                .role
                .activations()
                .iter()
                .enumerate()
                .filter(|(_, a)| matches!(a, Activation::Schedule(_) | Activation::Event(_)))
                .map(|(index, a)| ActivationKey::of(agent.role.name(), index, a))
        })
        .collect()
}

// ── What a member reads about it ────────────────────────────────────────────

/// When an agent runs, in one line for its card: "Every day at 06:30; when you come home".
pub fn describe_activations(activations: &[Activation]) -> String {
    let text = activations
        .iter()
        .map(describe_activation)
        .collect::<Vec<_>>()
        .join("; ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

fn describe_activation(activation: &Activation) -> String {
    match activation {
        Activation::Delegated => "when your assistant hands it work".to_string(),
        Activation::TaskStep => "as a step of a long task".to_string(),
        Activation::Schedule(s) => describe_cron(&s.cron),
        Activation::Event(e) => {
            let mut text = describe_trigger(&e.trigger);
            if let Some(window) = e.window {
                let ((fh, fm), (th, tm)) = window.bounds();
                text.push_str(&format!(" between {fh:02}:{fm:02} and {th:02}:{tm:02}"));
            }
            text
        }
        Activation::Agent { from } => format!("when {} hands it work", from.join(" or ")),
    }
}

/// The common shapes in words; anything else is shown as the cron itself.
fn describe_cron(cron: &CronSchedule) -> String {
    let fields: Vec<&str> = cron.as_str().split(' ').collect();
    if let [second, minute, hour, "*", "*", weekday] = fields.as_slice() {
        if let (Ok(0), Ok(minute), Ok(hour)) = (
            second.parse::<u32>(),
            minute.parse::<u32>(),
            hour.parse::<u32>(),
        ) {
            let days = match weekday.to_ascii_uppercase().as_str() {
                "*" => "every day".to_string(),
                "MON-FRI" | "1-5" => "on weekdays".to_string(),
                "SAT,SUN" | "SUN,SAT" | "SAT-SUN" | "0,6" | "6,0" => "at weekends".to_string(),
                other => format!("on {other}"),
            };
            return format!("{days} at {hour:02}:{minute:02}");
        }
    }
    format!("on the schedule {}", cron.as_str())
}

fn describe_trigger(trigger: &EventTrigger) -> String {
    let member = |subject: &Subject, verb: &str| match subject {
        Subject::Any => format!("when anyone {verb}s home"),
        Subject::Owner => format!("when you {verb} home"),
        Subject::Named(_) => format!("when a chosen member {verb}s home"),
    };
    let source = |subject: &Subject, kind: &str| match subject {
        Subject::Named(id) => format!("{kind} {id}"),
        Subject::Any | Subject::Owner => format!("any {kind}"),
    };
    match trigger {
        EventTrigger::PresenceArrive { member: who } => member(who, "come"),
        EventTrigger::PresenceLeave { member: who } => member(who, "leave"),
        EventTrigger::CameraPerson { camera } => {
            format!("when {} sees a person", source(camera, "camera"))
        }
        EventTrigger::CameraMotion { camera } => {
            format!("when {} sees movement", source(camera, "camera"))
        }
        EventTrigger::Sensor {
            sensor_type,
            device,
            threshold,
        } => {
            let what = sensor_type.replace('_', " ");
            let (verb, limit) = match threshold {
                Threshold::Above(x) => ("rises above", x),
                Threshold::Below(x) => ("drops below", x),
            };
            match device {
                Subject::Named(id) => format!("when the {what} on {id} {verb} {limit}"),
                Subject::Any | Subject::Owner => format!("when the {what} {verb} {limit}"),
            }
        }
        EventTrigger::DeviceChange { key, device } => {
            format!("when the {key} of {} changes", source(device, "device"))
        }
        EventTrigger::TimeDaily { hour, minute } => format!("every day at {hour:02}:{minute:02}"),
    }
}

// ── Whose run it is ─────────────────────────────────────────────────────────

/// The member an event names, else the recipe's owner.
pub fn activation_owner(event_member: Option<&str>, recipe_owner: Option<&str>) -> Option<String> {
    event_member
        .or(recipe_owner)
        .map(str::trim)
        .filter(|owner| !owner.is_empty())
        .map(str::to_string)
}

/// The scope a run works at, or `None` when there is nobody to run it for. With no owner, a
/// household of one is that one person; a larger one would be everyone's data, which is refused,
/// as an unidentified speaker's turn is.
pub fn activation_scope(owner: Option<&str>, household_members: usize) -> Option<ProfileScope> {
    match owner {
        Some(owner) => Some(ProfileScope::Owner(owner.to_string())),
        None if household_members <= 1 => Some(ProfileScope::Household),
        None => None,
    }
}

/// The clause after [`WHY_HEADING`].
pub fn firing_reason_line(reason: &FiringReason, member_name: Option<&str>) -> String {
    match reason {
        FiringReason::Schedule { cron } => format!("its schedule came round ({cron})"),
        FiringReason::Daily { at } => format!("it is {at}, its time each day"),
        FiringReason::Event(matched) => match &matched.member {
            Some(_) => format!(
                "{} {}",
                member_name.unwrap_or("a household member"),
                matched.detail
            ),
            None => matched.detail.clone(),
        },
    }
}

/// The step's instructions: the activation's brief, then why it started.
pub fn activation_instructions(brief: &str, reason_line: &str) -> String {
    format!("{}\n\n{WHY_HEADING} {reason_line}.", brief.trim())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActivationRefused {
    #[error(transparent)]
    Task(#[from] LongTaskError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    State(#[from] LongTaskStateRefused),
}

/// The task an activation queues, and its one-step plan. The task comes back Paused: store it,
/// store the plan, then move it to Running, so the runner never meets it without its step and
/// plans it itself.
pub fn activation_task(
    role: &AgentRole,
    instructions: &str,
    scope: ProfileScope,
    owner: Option<String>,
    at: DateTime<Utc>,
) -> Result<(LongTask, ValidatedPlan), ActivationRefused> {
    let deadline = chrono::Duration::from_std(ACTIVATION_DEADLINE)
        .ok()
        .map(|window| at + window);
    let budget = LongTaskBudget::new(1 + MAX_HAND_OFFS_PER_TASK as u32, deadline)?;
    let goal = format!(
        "{}: {}",
        role.name(),
        first_line_capped(instructions, GOAL_CHARS)
    );
    let started = LongTask::start(scope, owner, &goal, None, budget, at)?;
    let mut task = with_id_prefix(started, ACTIVATION_TASK_ID_PREFIX)?;
    task.transition_to(LongTaskState::Paused, at)?;
    let plan = validate_plan(
        &task,
        vec![StepDraft {
            key: step_slug(role.name(), MAX_STEP_KEY_CHARS),
            role: role.name().to_string(),
            instructions: instructions.to_string(),
            ..StepDraft::default()
        }],
        &[],
    )?;
    Ok((task, plan))
}

/// Longest goal an activation or a follow-up writes; it titles the owner's notification.
const GOAL_CHARS: usize = 120;

fn with_id_prefix(task: LongTask, prefix: &str) -> Result<LongTask, LongTaskError> {
    LongTask::from_parts(LongTaskParts {
        id: format!("{prefix}{}", task.id()),
        owner_profile_id: task.owner_profile_id().map(str::to_string),
        scope: task.scope().clone(),
        goal: task.goal().to_string(),
        origin_session_id: task.origin_session_id().map(str::to_string),
        state: task.state(),
        max_steps: task.budget().max_steps(),
        deadline: task.budget().deadline(),
        kind: task.kind(),
        answer: task.answer(),
        created_at: task.created_at(),
        updated_at: task.updated_at(),
        finished_at: task.finished_at(),
    })
}

pub fn is_activation_task(task: &LongTask) -> bool {
    task.id().starts_with(ACTIVATION_TASK_ID_PREFIX)
}

pub fn is_follow_up_task(task: &LongTask) -> bool {
    task.id().starts_with(FOLLOW_UP_TASK_ID_PREFIX)
}

fn first_line_capped(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", line[..at].trim_end()),
        None => line.to_string(),
    }
}

/// A valid step key from an agent's name: `a-z`, `0-9` and `-`, at most `max` characters.
fn step_slug(name: &str, max: usize) -> String {
    let mut slug = String::with_capacity(name.len());
    for c in name.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(max).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug.to_string()
    }
}

// ── Around a step ───────────────────────────────────────────────────────────

/// May `role` run `step` of `task`? A planner's step needs a role that takes task steps; an
/// activation's step and a hand-off's were accepted when they were written.
pub fn step_role_allowed(task: &LongTask, step: &PlanStep, role: &AgentRole) -> Result<(), String> {
    if step.role() != role.name() {
        return Err(format!(
            "step {} is for {}, not {}",
            step.key(),
            step.role(),
            role.name()
        ));
    }
    if is_activation_task(task)
        || step.key().as_str().starts_with(HAND_OFF_KEY_PREFIX)
        || role.accepts_task_steps()
    {
        return Ok(());
    }
    Err(format!(
        "the {} agent does not take steps of a long task; give this step to another role",
        role.name()
    ))
}

/// The roles a planner may be offered: those that take task steps.
pub fn plannable_roles(offered: &[AgentRole]) -> Vec<AgentRole> {
    offered
        .iter()
        .filter(|role| role.accepts_task_steps())
        .cloned()
        .collect()
}

/// Whether the runner's own end-of-task notice reaches the owner. An activation speaks only
/// through its `notify_owner` hook, or every scheduled run would buzz a phone; when one gives up,
/// the owner hears only if that hook says they want to hear from it.
pub fn runner_notifies_at_end(
    task: &LongTask,
    steps: &[PlanStep],
    roles: &[AgentRole],
    finished: bool,
) -> bool {
    if !is_activation_task(task) {
        return true;
    }
    if finished {
        return false;
    }
    let Some(first) = steps.iter().min_by_key(|step| step.ordinal()) else {
        return false;
    };
    roles
        .iter()
        .find(|role| role.name() == first.role())
        .is_some_and(|role| role.hooks().after_has(&AfterHook::NotifyOwner))
}

/// Why a planner never runs for an activation's task: it is one agent's run, not a plan.
pub const ACTIVATION_NOT_PLANNED: &str = "its agent's run did not succeed";

/// What a step's brief says the agent may ask for in its hand-back; empty when nothing.
pub fn hand_back_offer(role: &AgentRole, roles: &[AgentRole]) -> String {
    let targets: Vec<String> = roles
        .iter()
        .filter(|target| target.name() != role.name() && target.accepts_hand_off_from(role.name()))
        .take(MAX_OFFERED_HAND_OFFS)
        .map(|target| {
            format!(
                "{} ({})",
                target.name(),
                first_line_capped(target.instructions(), GOAL_CHARS)
            )
        })
        .collect();
    let mut out = String::new();
    if !targets.is_empty() {
        out.push_str(&format!(
            "\nWhen you finish, you may hand this on to: {}. To do so, add `hand_to` (the \
             agent's name) and `brief` (what it should do next) to step_report.\n",
            targets.join("; ")
        ));
    }
    if role.hooks().after_has(&AfterHook::StartTask) {
        out.push_str(
            "If what you found needs a long piece of work of its own, add `start_task` (its \
             goal, in one sentence) to step_report.\n",
        );
    }
    out
}

/// What the before hooks gathered; the gatherer in `pond-server` fills what it could reach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeforeHookContext {
    /// The household's local date, such as "Wednesday 28 October 2026".
    pub date: Option<String>,
    /// Today's scheduled items, such as "07:30 Morning brief", soonest first.
    pub schedule: Vec<String>,
    /// One line; `None` when the weather could not be read.
    pub weather: Option<String>,
    pub presence: Vec<PresenceLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceLine {
    pub name: String,
    pub home: bool,
    /// Local time of the last arrival or departure, already formatted.
    pub since: String,
}

/// The block before hooks add to a step's brief, in a fixed format; empty with no hooks. A run
/// that sees no personal data is told what was withheld rather than shown nothing, so it does
/// not guess.
pub fn render_before_hooks(
    hooks: &[BeforeHook],
    context: &BeforeHookContext,
    scope: &ProfileScope,
) -> String {
    if hooks.is_empty() {
        return String::new();
    }
    let private = scope.excludes_everything();
    let mut out = format!("\n{BEFORE_HOOKS_HEADING}\n");
    for hook in hooks {
        let line = match hook {
            BeforeHook::Today => {
                let date = context.date.as_deref().unwrap_or("unknown");
                if private {
                    format!(
                        "Today: {date}. Its schedule is withheld: this run sees no personal data."
                    )
                } else if context.schedule.is_empty() {
                    format!("Today: {date}. Nothing is scheduled.")
                } else {
                    format!(
                        "Today: {date}. Scheduled: {}.",
                        capped_list(&context.schedule)
                    )
                }
            }
            BeforeHook::Weather => match &context.weather {
                Some(weather) => format!("Weather: {weather}."),
                None => "Weather: could not be read just now.".to_string(),
            },
            BeforeHook::Presence => {
                if private {
                    "Who is home: withheld, since this run sees no personal data.".to_string()
                } else if context.presence.is_empty() {
                    "Who is home: nobody has been seen arriving or leaving since the pond started."
                        .to_string()
                } else {
                    let names = |home: bool| -> Vec<String> {
                        context
                            .presence
                            .iter()
                            .filter(|p| p.home == home)
                            .map(|p| format!("{} (since {})", p.name, p.since))
                            .collect()
                    };
                    let (home, away) = (names(true), names(false));
                    let mut parts = Vec::new();
                    if !home.is_empty() {
                        parts.push(format!("home: {}", capped_list(&home)));
                    }
                    if !away.is_empty() {
                        parts.push(format!("away: {}", capped_list(&away)));
                    }
                    format!("Who is home, as last seen: {}.", parts.join("; "))
                }
            }
        };
        out.push_str("- ");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn zone(timezone: &str) -> chrono_tz::Tz {
    timezone.parse().unwrap_or(chrono_tz::UTC)
}

/// The household's date, such as "Wednesday 28 October 2026", on IANA `timezone` (UTC if unknown).
pub fn local_date_label(now: DateTime<Utc>, timezone: &str) -> String {
    now.with_timezone(&zone(timezone))
        .format("%A %-d %B %Y")
        .to_string()
}

/// `HH:MM` on the household's clock, with the weekday when it is not today.
pub fn local_clock_label(at: DateTime<Utc>, now: DateTime<Utc>, timezone: &str) -> String {
    let tz = zone(timezone);
    let local = at.with_timezone(&tz);
    if same_local_day(at, now, timezone) {
        local.format("%H:%M").to_string()
    } else {
        local.format("%H:%M on %A").to_string()
    }
}

pub fn same_local_day(a: DateTime<Utc>, b: DateTime<Utc>, timezone: &str) -> bool {
    let tz = zone(timezone);
    a.with_timezone(&tz).date_naive() == b.with_timezone(&tz).date_naive()
}

fn capped_list(items: &[String]) -> String {
    let shown: Vec<&str> = items
        .iter()
        .take(MAX_BEFORE_HOOK_ITEMS)
        .map(String::as_str)
        .collect();
    let mut text = shown.join("; ");
    if items.len() > MAX_BEFORE_HOOK_ITEMS {
        text.push_str(&format!(
            " and {} more",
            items.len() - MAX_BEFORE_HOOK_ITEMS
        ));
    }
    text
}

/// Who the pond last saw arrive or leave, per member; fed from the bus.
#[derive(Debug, Default)]
pub struct PresenceBoard {
    latest: BTreeMap<String, (bool, DateTime<Utc>)>,
}

impl PresenceBoard {
    pub fn record(&mut self, presence: &ProfilePresence) {
        let home = matches!(presence.transition, PresenceTransition::Arrived);
        let newer = self
            .latest
            .get(&presence.profile_id)
            .is_none_or(|(_, at)| presence.at >= *at);
        if newer {
            self.latest
                .insert(presence.profile_id.clone(), (home, presence.at));
        }
    }

    /// `(profile_id, home, since)`, most recent first.
    pub fn snapshot(&self) -> Vec<(String, bool, DateTime<Utc>)> {
        let mut out: Vec<(String, bool, DateTime<Utc>)> = self
            .latest
            .iter()
            .map(|(id, (home, at))| (id.clone(), *home, *at))
            .collect();
        out.sort_by(|a, b| b.2.cmp(&a.2));
        out
    }
}

// ── After a step ────────────────────────────────────────────────────────────

/// What the runner does once a step has completed and passed its check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterAction {
    /// Tell the task's owner, on their own devices only.
    NotifyOwner(String),
    /// Keep the hand-back as the task's result: [`hook_result_artifact`].
    SaveArtifact,
    /// Start a long task: [`follow_up_task`].
    StartTask(String),
    /// Run this agent next: [`plan_hand_off`].
    HandTo { agent: String, brief: String },
}

/// The actions `role`'s after hooks and its hand-back's asks call for, in the hooks' order with
/// any hand-off last. Nothing for a step that did not complete; a hand-off the hand-back asks
/// for wins over the one the hooks name, and there is at most one.
pub fn after_hook_actions(
    task: &LongTask,
    role: &AgentRole,
    step: &PlanStep,
    outcome: &StepOutcome,
) -> Vec<AfterAction> {
    if step.role() != role.name()
        || step.state() != StepState::Completed
        || outcome.outcome() != OutcomeKind::Completed
    {
        return Vec::new();
    }
    let summary = outcome.summary().trim();
    let mut actions = Vec::new();
    for hook in &role.hooks().after {
        match hook {
            AfterHook::NotifyOwner if task.owner_profile_id().is_some() => {
                actions.push(AfterAction::NotifyOwner(if summary.is_empty() {
                    "It's done.".to_string()
                } else {
                    summary.to_string()
                }));
            }
            AfterHook::SaveArtifact if !summary.is_empty() => {
                actions.push(AfterAction::SaveArtifact)
            }
            AfterHook::StartTask if !is_follow_up_task(task) => {
                if let Some(goal) = &outcome.asks().start_task {
                    actions.push(AfterAction::StartTask(goal.clone()));
                }
            }
            AfterHook::NotifyOwner
            | AfterHook::SaveArtifact
            | AfterHook::StartTask
            | AfterHook::HandTo(_) => {}
        }
    }
    let asked = outcome.asks().hand_to.as_ref().map(|ask| {
        (
            ask.agent.clone(),
            ask.brief.clone().unwrap_or_else(|| summary.to_string()),
        )
    });
    let hooked = role
        .hooks()
        .hand_to()
        .map(|agent| (agent.to_string(), summary.to_string()));
    if let Some((agent, brief)) = asked.or(hooked) {
        actions.push(AfterAction::HandTo { agent, brief });
    }
    actions
}

/// The hand-back kept as the task's result, so `task_status` reports it.
pub fn hook_result_artifact(
    task: &LongTask,
    step: &PlanStep,
    outcome: &StepOutcome,
    at: DateTime<Utc>,
) -> Result<Artifact, ArtifactError> {
    Artifact::new(
        task.id(),
        Some(step.key().clone()),
        ArtifactKind::Findings,
        RESULT_TITLE,
        &first_line_capped(outcome.summary(), GOAL_CHARS),
        outcome.summary().to_string(),
        at,
    )
}

/// The long task a `start_task` hook starts: the finished task's scope and owner, no
/// conversation to report to, and marked so it cannot start another in turn.
pub fn follow_up_task(
    task: &LongTask,
    goal: &str,
    at: DateTime<Utc>,
) -> Result<LongTask, LongTaskError> {
    let budget = LongTaskBudget::new(DEFAULT_LONG_TASK_STEPS, None)?;
    let started = LongTask::start(
        task.scope().clone(),
        task.owner_profile_id().map(str::to_string),
        goal,
        None,
        budget,
        at,
    )?;
    with_id_prefix(started, FOLLOW_UP_TASK_ID_PREFIX)
}

/// Why a hand-off was not added. Each says what happened in a line the owner can read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HandOffRefused {
    #[error("there is no agent named `{0}` to hand on to")]
    NoSuchAgent(String),
    #[error("`{to}` does not take work from `{from}`: its activations list no `agent: [{from}]`")]
    NotAccepted { from: String, to: String },
    #[error("handing on to `{}` would go round in a circle: {}", .chain.last().map(String::as_str).unwrap_or(""), .chain.join(" -> "))]
    Cycle { chain: Vec<String> },
    #[error("this task has already been handed on {cap} times, the most it may be")]
    CapReached { cap: usize },
    #[error("the hand-off says nothing about what to do next")]
    EmptyBrief,
    #[error(transparent)]
    Plan(#[from] PlanError),
}

/// The plan with one step added for `agent`, waiting on `source` and given its artifacts, if the
/// target accepts work from the source and the chain is under its cap with no cycle. The runner
/// stores it with `replace_open_steps`; agents never message each other.
pub fn plan_hand_off(
    task: &LongTask,
    steps: &[PlanStep],
    source: &PlanStep,
    agent: &str,
    brief: &str,
    roles: &[AgentRole],
    artifacts: &[ArtifactSummary],
) -> Result<ValidatedPlan, HandOffRefused> {
    let agent = agent.trim();
    let Some(target) = roles.iter().find(|role| role.name() == agent) else {
        return Err(HandOffRefused::NoSuchAgent(agent.to_string()));
    };
    let mut chain: Vec<String> = hand_off_chain(steps, source)
        .into_iter()
        .map(str::to_string)
        .collect();
    if chain.iter().any(|seen| seen == target.name()) {
        chain.reverse();
        chain.push(target.name().to_string());
        return Err(HandOffRefused::Cycle { chain });
    }
    if !target.accepts_hand_off_from(source.role()) {
        return Err(HandOffRefused::NotAccepted {
            from: source.role().to_string(),
            to: target.name().to_string(),
        });
    }
    let taken = steps
        .iter()
        .filter(|step| step.key().as_str().starts_with(HAND_OFF_KEY_PREFIX))
        .count();
    if taken >= MAX_HAND_OFFS_PER_TASK {
        return Err(HandOffRefused::CapReached {
            cap: MAX_HAND_OFFS_PER_TASK,
        });
    }
    let brief = brief.trim();
    if brief.is_empty() {
        return Err(HandOffRefused::EmptyBrief);
    }
    let instructions = match source.summary().map(str::trim).filter(|s| !s.is_empty()) {
        Some(summary) => format!(
            "{brief}\n\nHanded on by the {} agent, whose step reported: {summary}",
            source.role()
        ),
        None => format!("{brief}\n\nHanded on by the {} agent.", source.role()),
    };
    let inputs: Vec<String> = artifacts
        .iter()
        .filter(|a| a.step_key.as_ref() == Some(source.key()))
        .map(|a| a.id.clone())
        .collect();
    let draft = StepDraft {
        key: hand_off_key(target.name(), steps),
        role: target.name().to_string(),
        instructions,
        inputs,
        depends_on: vec![source.key().to_string()],
        acceptance: None,
        new_agent: None,
    };
    validate_appended_step(task, draft, steps).map_err(HandOffRefused::from)
}

/// The agents that worked this chain, from `source` back to the step that began it.
fn hand_off_chain<'a>(steps: &'a [PlanStep], source: &'a PlanStep) -> Vec<&'a str> {
    let mut chain = vec![source.role()];
    let mut current = source;
    for _ in 0..steps.len() {
        if !current.key().as_str().starts_with(HAND_OFF_KEY_PREFIX) {
            break;
        }
        let Some(previous) = current
            .depends_on()
            .first()
            .and_then(|dep| steps.iter().find(|step| step.key() == dep))
        else {
            break;
        };
        chain.push(previous.role());
        current = previous;
    }
    chain
}

/// Room left for a `-NN` suffix when a target is handed work twice in one task.
const SUFFIX_ROOM: usize = 3;

fn hand_off_key(target: &str, steps: &[PlanStep]) -> String {
    let room = MAX_STEP_KEY_CHARS - HAND_OFF_KEY_PREFIX.len() - SUFFIX_ROOM;
    let base = format!("{HAND_OFF_KEY_PREFIX}{}", step_slug(target, room));
    let taken = |key: &str| steps.iter().any(|step| step.key().as_str() == key);
    if !taken(&base) {
        return base;
    }
    let mut n: usize = 2;
    loop {
        let key = format!("{base}-{n}");
        if !taken(&key) {
            return key;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests;
