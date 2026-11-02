//! The ledger of a long-running task: its goal and budget, the plan's steps, and what steps write.
//! A task outgrows any one window, so this is its working memory; a restart re-derives nothing.

use crate::user_data::domain::profile::ProfileScope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use thiserror::Error;

/// Longest step key; keys are repeated in every dependency list and helper prompt.
pub const MAX_STEP_KEY_CHARS: usize = 32;

/// Cap on a step's hand-back summary and failure; the planner reads every step's on each replan.
pub const MAX_STEP_SUMMARY_CHARS: usize = 600;

/// Cap on an artifact's one-line summary, which is all a step sees before paging in a body.
pub const MAX_ARTIFACT_SUMMARY_CHARS: usize = 200;

/// Titles sit beside every summary in a step's prompt, so they are kept short as well.
pub const MAX_ARTIFACT_TITLE_CHARS: usize = 80;

/// Fixed rather than shared with extraction's heuristic: stored `tokens` and paging must agree.
pub const ARTIFACT_CHARS_PER_TOKEN: usize = 4;

/// Largest step budget a task may ask for; a guess at an evening's work on the Orin, never measured.
pub const MAX_LONG_TASK_STEPS: u32 = 32;

/// Step budget for a task whose starter named none; a guess, never measured.
pub const DEFAULT_LONG_TASK_STEPS: u32 = 12;

/// Recorded when a helper says it failed without saying why, so the planner never sees a blank.
const UNEXPLAINED_FAILURE: &str = "the helper reported a failure without saying why";

/// The hand-back shape, quoted in every outcome refusal so a small model can copy it.
const HAND_BACK_EXAMPLE: &str = r#"{"outcome": "completed", "summary": "what this step found, in a few sentences", "artifacts": ["<artifact id>"], "failure": null}"#;

// ── States ──────────────────────────────────────────────────────────────────

/// Where a task is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LongTaskState {
    /// The planner is writing or rewriting the plan.
    Planning,
    /// The runner is working through the plan.
    Running,
    /// A step waits for a member's decision.
    AwaitingApproval,
    /// A member, quiet hours or the device's temperature stopped it; nothing runs.
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl LongTaskState {
    pub const ALL: [LongTaskState; 7] = [
        LongTaskState::Planning,
        LongTaskState::Running,
        LongTaskState::AwaitingApproval,
        LongTaskState::Paused,
        LongTaskState::Completed,
        LongTaskState::Failed,
        LongTaskState::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LongTaskState::Planning => "planning",
            LongTaskState::Running => "running",
            LongTaskState::AwaitingApproval => "awaiting_approval",
            LongTaskState::Paused => "paused",
            LongTaskState::Completed => "completed",
            LongTaskState::Failed => "failed",
            LongTaskState::Cancelled => "cancelled",
        }
    }

    /// Exact stored spelling only; an unknown value is `None`, never a default state.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == raw)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            LongTaskState::Completed | LongTaskState::Failed | LongTaskState::Cancelled
        )
    }

    /// The only moves a task may make. A terminal state has none, and no state moves to itself.
    pub fn can_become(self, next: LongTaskState) -> bool {
        use LongTaskState::*;
        matches!(
            (self, next),
            // The planner may find nothing left to do, so Planning can complete directly.
            (Planning, Running | Paused | Completed | Failed | Cancelled)
                | (
                    Running,
                    Planning | AwaitingApproval | Paused | Completed | Failed | Cancelled
                )
                // Approved resumes the run; declined goes back to the planner.
                | (
                    AwaitingApproval,
                    Planning | Running | Paused | Failed | Cancelled
                )
                // Resuming returns to whichever phase the pause interrupted.
                | (Paused, Planning | Running | AwaitingApproval | Failed | Cancelled)
        )
    }
}

impl fmt::Display for LongTaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a task is for, which decides how its planner and workers are briefed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LongTaskKind {
    #[default]
    General,
    /// A member asked for deep research: search, keep sources, end in a cited report.
    Research,
}

impl LongTaskKind {
    pub const ALL: [LongTaskKind; 2] = [LongTaskKind::General, LongTaskKind::Research];

    pub fn as_str(self) -> &'static str {
        match self {
            LongTaskKind::General => "general",
            LongTaskKind::Research => "research",
        }
    }

    /// Exact stored spelling only; an unknown value is `None`, never a default kind.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }
}

/// How long a task's answer should be. The person chooses it, or the planner reads it from the ask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerShape {
    /// Nobody chose yet: the planner decides from the ask and records it.
    #[default]
    Auto,
    /// The answer itself, in a few sentences, shown whole in the conversation.
    Short,
    /// A report in sections, opened on its own.
    Report,
}

impl AnswerShape {
    pub const ALL: [AnswerShape; 3] = [AnswerShape::Auto, AnswerShape::Short, AnswerShape::Report];

    pub fn as_str(self) -> &'static str {
        match self {
            AnswerShape::Auto => "auto",
            AnswerShape::Short => "short",
            AnswerShape::Report => "report",
        }
    }

    /// Exact stored spelling only; an unknown value is `None`, never a default shape.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shape| shape.as_str() == raw)
    }

    /// What a model or a client sent: the stored spellings, any case, with blank as no choice.
    pub fn read(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase();
        if raw.is_empty() {
            return Some(AnswerShape::Auto);
        }
        Self::parse(&raw)
    }
}

/// Where one step of a plan is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    AwaitingApproval,
}

impl StepState {
    pub const ALL: [StepState; 6] = [
        StepState::Pending,
        StepState::Running,
        StepState::Completed,
        StepState::Failed,
        StepState::Skipped,
        StepState::AwaitingApproval,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            StepState::Pending => "pending",
            StepState::Running => "running",
            StepState::Completed => "completed",
            StepState::Failed => "failed",
            StepState::Skipped => "skipped",
            StepState::AwaitingApproval => "awaiting_approval",
        }
    }

    /// Exact stored spelling only; an unknown value is `None`, never `Pending`.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == raw)
    }

    /// Completed and Running steps are the plan's history; a replan keeps them as they are.
    pub fn survives_replan(self) -> bool {
        matches!(self, StepState::Completed | StepState::Running)
    }

    /// The only moves a step may make. Completed and Skipped have none.
    pub fn can_become(self, next: StepState) -> bool {
        use StepState::*;
        matches!(
            (self, next),
            (Pending, Running | AwaitingApproval | Skipped)
                // Back to Pending when a household turn pre-empts it; it reruns from the ledger.
                | (Running, Completed | Failed | AwaitingApproval | Pending)
                // Running again is the retry.
                | (Failed, Running | Skipped)
                | (AwaitingApproval, Running | Skipped)
        )
    }
}

impl fmt::Display for StepState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A task move [`LongTaskState::can_become`] does not allow.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("task `{task_id}` is {from} and cannot become {to}")]
pub struct LongTaskStateRefused {
    pub task_id: String,
    pub from: LongTaskState,
    pub to: LongTaskState,
}

/// A step move [`StepState::can_become`] or the step's own method does not allow.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("step `{step}` is {from} and cannot become {to}")]
pub struct StepStateRefused {
    pub step: String,
    pub from: StepState,
    pub to: StepState,
}

// ── The task ────────────────────────────────────────────────────────────────

/// How much a task may spend: a step cap and an optional wall-clock cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LongTaskBudget {
    max_steps: u32,
    deadline: Option<DateTime<Utc>>,
}

impl LongTaskBudget {
    /// Refused, not clamped, above [`MAX_LONG_TASK_STEPS`], so the starter learns the real cap.
    pub fn new(max_steps: u32, deadline: Option<DateTime<Utc>>) -> Result<Self, LongTaskError> {
        if max_steps == 0 || max_steps > MAX_LONG_TASK_STEPS {
            return Err(LongTaskError::MaxStepsOutOfRange {
                requested: max_steps,
                cap: MAX_LONG_TASK_STEPS,
            });
        }
        Ok(Self {
            max_steps,
            deadline,
        })
    }

    /// Every step the plan may hold, finished ones included.
    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    pub fn deadline(&self) -> Option<DateTime<Utc>> {
        self.deadline
    }

    pub fn deadline_passed(&self, now: DateTime<Utc>) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

/// A long-running task. Fields are private: [`LongTask::start`] and [`LongTask::from_parts`]
/// check that the scope and the owner agree, and only [`LongTask::transition_to`] moves state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LongTask {
    id: String,
    owner_profile_id: Option<String>,
    /// The scope it runs and writes artifacts at: its starter's, never wider.
    scope: ProfileScope,
    goal: String,
    /// Where the prime reports back; `None` once that conversation is deleted.
    origin_session_id: Option<String>,
    state: LongTaskState,
    budget: LongTaskBudget,
    kind: LongTaskKind,
    answer: AnswerShape,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

/// A stored task's columns, for [`LongTask::from_parts`]; holding one grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongTaskParts {
    pub id: String,
    pub owner_profile_id: Option<String>,
    pub scope: ProfileScope,
    pub goal: String,
    pub origin_session_id: Option<String>,
    pub state: LongTaskState,
    pub max_steps: u32,
    pub deadline: Option<DateTime<Utc>>,
    pub kind: LongTaskKind,
    pub answer: AnswerShape,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl LongTask {
    /// A new task in `Planning`. `scope` is the starting turn's; an `Owner` scope fills a missing
    /// owner and refuses a different one.
    pub fn start(
        scope: ProfileScope,
        owner_profile_id: Option<String>,
        goal: &str,
        origin_session_id: Option<String>,
        budget: LongTaskBudget,
        at: DateTime<Utc>,
    ) -> Result<Self, LongTaskError> {
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(LongTaskError::MissingGoal);
        }
        let owner_profile_id = owner_for(&scope, owner_profile_id)?;
        let origin_session_id = origin_for(origin_session_id)?;
        if budget.deadline.is_some_and(|deadline| deadline <= at) {
            return Err(LongTaskError::DeadlineNotInFuture);
        }
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            owner_profile_id,
            scope,
            goal: goal.to_string(),
            origin_session_id,
            state: LongTaskState::Planning,
            budget,
            kind: LongTaskKind::General,
            answer: AnswerShape::Auto,
            created_at: at,
            updated_at: at,
            finished_at: None,
        })
    }

    pub fn with_kind(mut self, kind: LongTaskKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_answer(mut self, answer: AnswerShape) -> Self {
        self.answer = answer;
        self
    }

    /// The planner's reading of the ask, kept only while nobody has chosen: a member's choice
    /// stands. `false` when it changed nothing.
    pub fn decide_answer(&mut self, answer: AnswerShape) -> bool {
        if self.answer != AnswerShape::Auto || answer == AnswerShape::Auto {
            return false;
        }
        self.answer = answer;
        true
    }

    /// Rebuild a stored task through the same checks; a row that fails them is an error.
    pub fn from_parts(parts: LongTaskParts) -> Result<Self, LongTaskError> {
        let LongTaskParts {
            id,
            owner_profile_id,
            scope,
            goal,
            origin_session_id,
            state,
            max_steps,
            deadline,
            kind,
            answer,
            created_at,
            updated_at,
            finished_at,
        } = parts;
        if id.trim().is_empty() {
            return Err(LongTaskError::MissingId);
        }
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(LongTaskError::MissingGoal);
        }
        if max_steps == 0 {
            return Err(LongTaskError::MaxStepsOutOfRange {
                requested: 0,
                cap: MAX_LONG_TASK_STEPS,
            });
        }
        let owner_profile_id = owner_for(&scope, owner_profile_id)?;
        let origin_session_id = origin_for(origin_session_id)?;
        Ok(Self {
            id,
            owner_profile_id,
            scope,
            goal: goal.to_string(),
            origin_session_id,
            state,
            // A lowered cap narrows stored tasks too; refusing them would hide a member's task.
            budget: LongTaskBudget {
                max_steps: max_steps.min(MAX_LONG_TASK_STEPS),
                deadline,
            },
            kind,
            answer,
            created_at,
            updated_at,
            finished_at,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn owner_profile_id(&self) -> Option<&str> {
        self.owner_profile_id.as_deref()
    }

    pub fn scope(&self) -> &ProfileScope {
        &self.scope
    }

    pub fn goal(&self) -> &str {
        &self.goal
    }

    pub fn origin_session_id(&self) -> Option<&str> {
        self.origin_session_id.as_deref()
    }

    pub fn kind(&self) -> LongTaskKind {
        self.kind
    }

    pub fn answer(&self) -> AnswerShape {
        self.answer
    }

    pub fn state(&self) -> LongTaskState {
        self.state
    }

    pub fn budget(&self) -> &LongTaskBudget {
        &self.budget
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    pub fn finished_at(&self) -> Option<DateTime<Utc>> {
        self.finished_at
    }

    /// Move to `next` if [`LongTaskState::can_become`] allows it; a terminal move stamps
    /// `finished_at`. On refusal nothing changes.
    pub fn transition_to(
        &mut self,
        next: LongTaskState,
        at: DateTime<Utc>,
    ) -> Result<(), LongTaskStateRefused> {
        if !self.state.can_become(next) {
            return Err(LongTaskStateRefused {
                task_id: self.id.clone(),
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        self.updated_at = at;
        if next.is_terminal() {
            self.finished_at = Some(at);
        }
        Ok(())
    }
}

/// The owner a task records for `scope`: an `Owner` scope names it, a `Guest` has none.
fn owner_for(
    scope: &ProfileScope,
    owner_profile_id: Option<String>,
) -> Result<Option<String>, LongTaskError> {
    let owner = owner_profile_id
        .map(|owner| owner.trim().to_string())
        .filter(|owner| !owner.is_empty());
    match scope {
        ProfileScope::Owner(id) if id.trim().is_empty() => Err(LongTaskError::BlankOwner),
        ProfileScope::Owner(id) => match owner {
            None => Ok(Some(id.clone())),
            Some(owner) if owner == *id => Ok(Some(owner)),
            Some(owner) => Err(LongTaskError::OwnerMismatch {
                owner,
                scope_owner: id.clone(),
            }),
        },
        ProfileScope::Household => Ok(owner),
        ProfileScope::Guest => match owner {
            None => Ok(None),
            Some(owner) => Err(LongTaskError::GuestWithOwner { owner }),
        },
    }
}

fn origin_for(origin_session_id: Option<String>) -> Result<Option<String>, LongTaskError> {
    match origin_session_id {
        Some(id) if id.trim().is_empty() => Err(LongTaskError::BlankOriginSession),
        other => Ok(other),
    }
}

/// Why a task could not be started or read back.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LongTaskError {
    #[error("a task needs a goal. Say in one sentence what it should achieve.")]
    MissingGoal,
    #[error(
        "a task may take between 1 and {cap} steps, not {requested}. Ask for {cap} or fewer, \
         or split the goal into two tasks."
    )]
    MaxStepsOutOfRange { requested: u32, cap: u32 },
    #[error("the task's deadline has already passed. Give a time in the future, or none.")]
    DeadlineNotInFuture,
    #[error("a task scoped to one member must name that member")]
    BlankOwner,
    #[error("a task owned by `{owner}` cannot run at `{scope_owner}`'s scope")]
    OwnerMismatch { owner: String, scope_owner: String },
    #[error("a task started by an unidentified speaker cannot be owned by `{owner}`")]
    GuestWithOwner { owner: String },
    #[error("a task's origin conversation id may not be blank; leave it out instead")]
    BlankOriginSession,
    #[error("a stored task has no id")]
    MissingId,
}

// ── The plan ────────────────────────────────────────────────────────────────

/// A step's planner-chosen name: 1 to [`MAX_STEP_KEY_CHARS`] of `a-z`, `0-9` and `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StepKey(String);

impl StepKey {
    /// Surrounding whitespace is forgiven; anything else outside the alphabet is refused.
    pub fn parse(raw: &str) -> Result<Self, PlanError> {
        let key = raw.trim();
        let valid = !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && key.len() <= MAX_STEP_KEY_CHARS;
        if valid {
            Ok(StepKey(key.to_string()))
        } else {
            Err(PlanError::InvalidKey {
                key: key.to_string(),
                suggestion: suggest_key(key),
            })
        }
    }

    /// A plan's key, taken as its suggestion when only the spelling is wrong: `step_1` is `step-1`,
    /// and the plan's `depends_on` is matched the same way.
    pub fn lenient(raw: &str) -> Result<Self, PlanError> {
        StepKey::parse(raw).or_else(|refused| match &refused {
            PlanError::InvalidKey {
                suggestion: Some(near),
                ..
            } => StepKey::parse(near).map_err(|_| refused.clone()),
            _ => Err(refused),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StepKey {
    type Error = PlanError;

    fn try_from(raw: String) -> Result<Self, PlanError> {
        StepKey::parse(&raw)
    }
}

impl From<StepKey> for String {
    fn from(key: StepKey) -> String {
        key.0
    }
}

impl fmt::Display for StepKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A valid key close to what the planner wrote, so the refusal can name one to use.
fn suggest_key(raw: &str) -> Option<String> {
    let mut slug = String::with_capacity(raw.len());
    for c in raw.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(MAX_STEP_KEY_CHARS).collect();
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_string())
}

/// One step as the planner's `plan_write` call supplies it. It carries no authority: the role's
/// tools and scope come from the role and the task, never from here. Fields it does not know are
/// ignored, because refusing a plan over an extra `title` cost the planner a whole turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepDraft {
    #[serde(alias = "id", alias = "step_key")]
    pub key: String,
    /// Empty when the step makes its own agent with `new_agent`.
    #[serde(default, alias = "agent")]
    pub role: String,
    /// A new agent for this step, when no saved role fits.
    #[serde(default, alias = "custom_agent")]
    pub new_agent: Option<StepAgent>,
    #[serde(alias = "task", alias = "instruction")]
    pub instructions: String,
    /// Artifact ids the step starts from; it is shown their summaries, never their bodies.
    #[serde(
        default,
        alias = "input",
        alias = "artifacts",
        deserialize_with = "one_or_many"
    )]
    pub inputs: Vec<String>,
    #[serde(
        default,
        alias = "dependencies",
        alias = "depends",
        alias = "after",
        deserialize_with = "one_or_many"
    )]
    pub depends_on: Vec<String>,
    #[serde(default, alias = "acceptance_check", alias = "check")]
    pub acceptance: Option<String>,
}

/// Every name [`StepDraft`] reads, aliases included, so a caller can say which fields it ignored.
pub const STEP_DRAFT_FIELDS: &[&str] = &[
    "key",
    "id",
    "step_key",
    "role",
    "agent",
    "new_agent",
    "custom_agent",
    "instructions",
    "task",
    "instruction",
    "inputs",
    "input",
    "artifacts",
    "depends_on",
    "dependencies",
    "depends",
    "after",
    "acceptance",
    "acceptance_check",
    "check",
];

/// Where a step with no `key` takes one from, in order, before falling back to its place.
const KEY_FALLBACKS: &[&str] = &["name", "title", "step", "step_name", "step_id", "label"];

/// Read the `position`th step (from 1) of a `plan_write` call. A field spelt like one a step reads,
/// such as `dependsOn`, is read as that field, since dropping a misspelt `depends_on` would cost
/// the step its inputs. A step with no key takes one from its name, else `step-<position>`: a key
/// is only a handle, and refusing the plan for it cost the planner a turn. Returns the draft and
/// the fields it ignored.
pub fn read_step_draft(
    value: serde_json::Value,
    position: usize,
) -> Result<ReadStep, serde_json::Error> {
    let squash = |name: &str| -> String {
        name.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    let mut ignored = Vec::new();
    let mut names = Vec::new();
    let value = match value {
        serde_json::Value::Object(fields) => {
            // Exact names first, so a field spelt right wins over a near spelling of it.
            let mut read = serde_json::Map::with_capacity(fields.len());
            let mut near = Vec::new();
            for (name, field) in fields {
                if STEP_DRAFT_FIELDS.contains(&name.as_str()) {
                    read.insert(name, field);
                } else {
                    near.push((name, field));
                }
            }
            // A planner writes `new_agent: "Forager Gosling: ..."` meaning a role, or an object with
            // no name; only a readable agent makes one, so anything else is dropped, not refused.
            if read.get("new_agent").is_some_and(|agent| {
                !agent.is_null() && serde_json::from_value::<StepAgent>(agent.clone()).is_err()
            }) {
                read.remove("new_agent");
                ignored.push("new_agent".to_string());
            }
            let mut unread = Vec::new();
            for (name, field) in near {
                match STEP_DRAFT_FIELDS
                    .iter()
                    .find(|known| squash(known) == squash(&name))
                {
                    Some(known) if !read.contains_key(*known) => {
                        read.insert((*known).to_string(), field);
                    }
                    _ => unread.push((name, field)),
                }
            }
            for (name, field) in &unread {
                if KEY_FALLBACKS
                    .iter()
                    .any(|fallback| squash(fallback) == squash(name))
                {
                    match field {
                        serde_json::Value::String(text) => names.push(text.trim().to_string()),
                        serde_json::Value::Number(n) => names.push(format!("step-{n}")),
                        _ => {}
                    }
                }
            }
            let keyed = ["key", "id", "step_key"]
                .iter()
                .any(|name| read.get(*name).is_some_and(|value| !value.is_null()));
            if !keyed {
                let named = KEY_FALLBACKS.iter().find_map(|fallback| {
                    let at = unread
                        .iter()
                        .position(|(name, _)| squash(name) == squash(fallback))?;
                    let key = match &unread[at].1 {
                        serde_json::Value::String(text) if !text.trim().is_empty() => text.clone(),
                        serde_json::Value::Number(n) => format!("step-{n}"),
                        _ => return None,
                    };
                    unread.remove(at);
                    Some(key)
                });
                let key = named.unwrap_or_else(|| format!("step-{position}"));
                read.insert("key".to_string(), serde_json::Value::String(key));
            }
            // A name or title is how other steps' waits may name this one, so it is not ignored.
            ignored.extend(
                unread
                    .into_iter()
                    .map(|(name, _)| name)
                    .filter(|name| !KEY_FALLBACKS.iter().any(|f| squash(f) == squash(name))),
            );
            serde_json::Value::Object(read)
        }
        other => other,
    };
    Ok(ReadStep {
        draft: serde_json::from_value(value)?,
        ignored,
        names,
    })
}

/// One step of a `plan_write` call as read: the draft, the fields it ignored, and the names it
/// went by besides its key, which another step's wait may use.
#[derive(Debug, Clone)]
pub struct ReadStep {
    pub draft: StepDraft,
    pub ignored: Vec<String>,
    pub names: Vec<String>,
}

/// Point waits that name a step by its name or title at that step's key: the planner often waits
/// for "Gather camera events" while keying the step `step-1`.
pub fn match_waits_by_name(steps: &mut [ReadStep]) {
    let slug = |raw: &str| suggest_key(raw).unwrap_or_else(|| raw.trim().to_ascii_lowercase());
    let keys: Vec<String> = steps.iter().map(|step| slug(&step.draft.key)).collect();
    let named: Vec<(String, String)> = steps
        .iter()
        .flat_map(|step| {
            step.names
                .iter()
                .map(|name| (slug(name), step.draft.key.clone()))
        })
        .collect();
    for step in steps.iter_mut() {
        for wait in &mut step.draft.depends_on {
            let wanted = slug(wait);
            if keys.contains(&wanted) {
                continue;
            }
            if let Some((_, key)) = named.iter().find(|(name, _)| *name == wanted) {
                *wait = key.clone();
            }
        }
    }
}

/// An agent a plan makes for one step because no saved role fits. The runner builds it from these
/// and holds its tools to the task's ceiling, so a plan can ask for no more than a saved role has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepAgent {
    pub name: String,
    pub instructions: String,
    #[serde(default, alias = "tool_groups", deserialize_with = "one_or_many")]
    pub tools: Vec<String>,
}

/// A made agent's step names it `new:<name>`, so the card can tell it from a saved role.
pub const NEW_AGENT_PREFIX: &str = "new:";

/// Longest name a made agent may have; it is shown on the card.
const AGENT_NAME_CHARS: usize = 40;

impl StepAgent {
    fn cleaned(self) -> Option<Self> {
        let name: String = self.name.trim().chars().take(AGENT_NAME_CHARS).collect();
        let instructions = self.instructions.trim().to_string();
        let tools = clean_ids(self.tools);
        (!name.is_empty() && !instructions.is_empty()).then_some(Self {
            name,
            instructions,
            tools,
        })
    }
}

/// A small model writes `"a, b"` or `"a"` where a list belongs; keys and ids never hold commas.
fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(OneOrMany::One(text)) => text.split(',').map(str::to_string).collect(),
        Some(OneOrMany::Many(items)) => items,
    })
}

/// One step of a plan. Only [`validate_plan`] makes a new one and only its methods move it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanStep {
    task_id: String,
    key: StepKey,
    ordinal: u32,
    role: String,
    instructions: String,
    inputs: Vec<String>,
    depends_on: Vec<StepKey>,
    acceptance: Option<String>,
    state: StepState,
    /// Attempts begun; a pre-emption gives its attempt back, a crash does not.
    attempts: u32,
    summary: Option<String>,
    /// The last attempt's, kept through a retry so the retry's prompt can carry the reason.
    failure: Option<String>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    /// Set when the plan made this step's agent rather than naming a saved role.
    agent: Option<StepAgent>,
}

/// A stored step's columns, for [`PlanStep::from_parts`]; holding one grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStepParts {
    pub task_id: String,
    pub key: String,
    pub ordinal: u32,
    pub role: String,
    pub instructions: String,
    pub inputs: Vec<String>,
    pub depends_on: Vec<String>,
    pub acceptance: Option<String>,
    pub state: StepState,
    pub attempts: u32,
    pub summary: Option<String>,
    pub failure: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub agent: Option<StepAgent>,
}

impl PlanStep {
    /// Rebuild a stored step; a key or dependency outside the alphabet is an error.
    pub fn from_parts(parts: PlanStepParts) -> Result<Self, PlanError> {
        let key = StepKey::parse(&parts.key)?;
        let role = parts.role.trim();
        if role.is_empty() {
            return Err(PlanError::MissingRole { step: key.into() });
        }
        let instructions = parts.instructions.trim();
        if instructions.is_empty() {
            return Err(PlanError::MissingInstructions { step: key.into() });
        }
        let depends_on = parts
            .depends_on
            .iter()
            .map(|dep| StepKey::parse(dep))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            task_id: parts.task_id,
            key,
            ordinal: parts.ordinal,
            role: role.to_string(),
            instructions: instructions.to_string(),
            inputs: clean_ids(parts.inputs),
            depends_on,
            acceptance: non_blank(parts.acceptance.as_deref()),
            state: parts.state,
            attempts: parts.attempts,
            summary: capped(parts.summary.as_deref()),
            failure: capped(parts.failure.as_deref()),
            started_at: parts.started_at,
            finished_at: parts.finished_at,
            agent: parts.agent.and_then(StepAgent::cleaned),
        })
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// The agent this plan made for the step, when it named no saved role.
    pub fn agent(&self) -> Option<&StepAgent> {
        self.agent.as_ref()
    }

    pub fn key(&self) -> &StepKey {
        &self.key
    }

    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// The saved role's name; the runner resolves it, and an unknown role fails the step.
    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn inputs(&self) -> &[String] {
        &self.inputs
    }

    pub fn depends_on(&self) -> &[StepKey] {
        &self.depends_on
    }

    pub fn acceptance(&self) -> Option<&str> {
        self.acceptance.as_deref()
    }

    pub fn state(&self) -> StepState {
        self.state
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.started_at
    }

    pub fn finished_at(&self) -> Option<DateTime<Utc>> {
        self.finished_at
    }

    /// Begin an attempt: one helper life in a fresh window. Also how an approved step runs.
    pub fn start(&mut self, at: DateTime<Utc>) -> Result<(), StepStateRefused> {
        self.move_to(StepState::Running)?;
        self.attempts = self.attempts.saturating_add(1);
        self.started_at = Some(at);
        self.finished_at = None;
        Ok(())
    }

    /// A household turn stopped the step; it reruns later and this attempt is not counted.
    pub fn preempt(&mut self) -> Result<(), StepStateRefused> {
        self.move_to(StepState::Pending)?;
        self.attempts = self.attempts.saturating_sub(1);
        self.started_at = None;
        Ok(())
    }

    /// Record a running step's hand-back. A verifier's verdict belongs in `outcome` already:
    /// a Completed step never moves again, so it cannot be failed afterwards.
    pub fn record(
        &mut self,
        outcome: &StepOutcome,
        at: DateTime<Utc>,
    ) -> Result<(), StepStateRefused> {
        let next = match outcome.outcome {
            OutcomeKind::Completed => StepState::Completed,
            OutcomeKind::Failed => StepState::Failed,
            OutcomeKind::NeedsApproval => StepState::AwaitingApproval,
        };
        // Explicit: Pending may also become AwaitingApproval, but only a run produces a hand-back.
        if self.state != StepState::Running {
            return Err(self.refused(next));
        }
        self.move_to(next)?;
        self.summary = non_blank(Some(outcome.summary.as_str()));
        self.failure = match outcome.outcome {
            OutcomeKind::Completed => None,
            OutcomeKind::Failed => Some(
                outcome
                    .failure
                    .clone()
                    .unwrap_or_else(|| UNEXPLAINED_FAILURE.to_string()),
            ),
            OutcomeKind::NeedsApproval => outcome.failure.clone(),
        };
        self.finished_at = (next != StepState::AwaitingApproval).then_some(at);
        Ok(())
    }

    /// A failure the runner saw itself, such as a crash, a timeout or a role that is gone.
    pub fn fail(&mut self, reason: &str, at: DateTime<Utc>) -> Result<(), StepStateRefused> {
        self.move_to(StepState::Failed)?;
        self.failure = Some(capped(Some(reason)).unwrap_or_else(|| UNEXPLAINED_FAILURE.into()));
        self.finished_at = Some(at);
        Ok(())
    }

    /// Park a step before it runs, because its role can act on the world.
    pub fn await_approval(&mut self) -> Result<(), StepStateRefused> {
        // Explicit: a running step parks through `record`, with the helper's reason.
        if self.state != StepState::Pending {
            return Err(self.refused(StepState::AwaitingApproval));
        }
        self.move_to(StepState::AwaitingApproval)
    }

    /// The planner or a member decided the step is not needed.
    pub fn skip(&mut self, at: DateTime<Utc>) -> Result<(), StepStateRefused> {
        self.move_to(StepState::Skipped)?;
        self.finished_at = Some(at);
        Ok(())
    }

    fn move_to(&mut self, next: StepState) -> Result<(), StepStateRefused> {
        if !self.state.can_become(next) {
            return Err(self.refused(next));
        }
        self.state = next;
        Ok(())
    }

    fn refused(&self, next: StepState) -> StepStateRefused {
        StepStateRefused {
            step: self.key.to_string(),
            from: self.state,
            to: next,
        }
    }
}

/// A checked plan for one task; only [`validate_plan`] makes one, and the store writes only this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlan {
    task_id: String,
    steps: Vec<PlanStep>,
    kept: Vec<StepKey>,
    relaxed: Vec<RelaxedWait>,
}

/// A wait for a step the plan does not have, kept as a wait for every step before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaxedWait {
    pub step: String,
    pub missing: String,
}

impl ValidatedPlan {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// The new steps, all Pending, in the order the planner wrote them.
    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    /// Steps Completed or Running when checked; new steps may wait for them, so they must stay so.
    pub fn kept_keys(&self) -> &[StepKey] {
        &self.kept
    }

    /// Waits for steps the plan does not have, each now a wait for every step before it.
    pub fn relaxed_waits(&self) -> &[RelaxedWait] {
        &self.relaxed
    }

    /// Whether the plan still fits the stored `(key, state)` pairs; a store calls this inside
    /// the write that replaces the steps, since steps may move after `validate_plan`.
    pub fn check_current<'a>(
        &self,
        stored: impl IntoIterator<Item = (&'a str, StepState)>,
    ) -> Result<(), StalePlan> {
        let stored: HashMap<&str, StepState> = stored.into_iter().collect();
        for key in &self.kept {
            match stored.get(key.as_str()) {
                Some(state) if state.survives_replan() => {}
                now => {
                    return Err(StalePlan {
                        task_id: self.task_id.clone(),
                        step: key.to_string(),
                        now: now.copied(),
                    })
                }
            }
        }
        for step in &self.steps {
            if let Some(&state) = stored.get(step.key.as_str()) {
                if state.survives_replan() {
                    return Err(StalePlan {
                        task_id: self.task_id.clone(),
                        step: step.key.to_string(),
                        now: Some(state),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A plan the store refused because the steps moved after [`validate_plan`] checked it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "the plan for task `{task_id}` is out of date: step `{step}` is now {}. Check it again \
     against the current steps.",
    stale_state(.now)
)]
pub struct StalePlan {
    pub task_id: String,
    pub step: String,
    /// `None` when the step is gone.
    pub now: Option<StepState>,
}

fn stale_state(now: &Option<StepState>) -> &'static str {
    match now {
        Some(state) => state.as_str(),
        None => "gone",
    }
}

/// Check a plan for `task` against `existing`, every step it has now. Completed and Running
/// steps are kept and never rewritten; the rest are replaced, so their keys may be reused.
pub fn validate_plan(
    task: &LongTask,
    drafts: Vec<StepDraft>,
    existing: &[PlanStep],
) -> Result<ValidatedPlan, PlanError> {
    if task.state.is_terminal() {
        return Err(PlanError::TaskFinished { state: task.state });
    }
    if let Some(stray) = existing.iter().find(|step| step.task_id != task.id) {
        return Err(PlanError::StepsOfAnotherTask {
            task: task.id.clone(),
            other: stray.task_id.clone(),
        });
    }
    let mut kept: Vec<&PlanStep> = existing
        .iter()
        .filter(|step| step.state.survives_replan())
        .collect();
    kept.sort_by_key(|step| step.ordinal);
    if drafts.is_empty() && kept.is_empty() {
        return Err(PlanError::EmptyPlan);
    }
    let cap = task.budget.max_steps;
    let requested = kept.len() + drafts.len();
    if requested > cap as usize {
        return Err(PlanError::TooManySteps {
            requested,
            kept: kept.len(),
            cap,
        });
    }

    let kept_states: HashMap<&str, StepState> = kept
        .iter()
        .map(|step| (step.key.as_str(), step.state))
        .collect();
    let mut keys: Vec<StepKey> = Vec::with_capacity(drafts.len());
    for draft in &drafts {
        let key = StepKey::lenient(&draft.key)?;
        if let Some(&state) = kept_states.get(key.as_str()) {
            return Err(PlanError::RewritesSettledStep {
                key: key.into(),
                state,
            });
        }
        if keys.contains(&key) {
            return Err(PlanError::DuplicateKey { key: key.into() });
        }
        keys.push(key);
    }

    // Open steps left out of the new plan are deleted, so only these keys can be waited for.
    let mut known: HashMap<&str, &StepKey> = HashMap::new();
    for key in &keys {
        known.insert(key.as_str(), key);
    }
    for step in &kept {
        known.insert(step.key.as_str(), &step.key);
    }

    let first_ordinal = kept.last().map_or(1, |step| step.ordinal.saturating_add(1));
    let mut steps = Vec::with_capacity(drafts.len());
    let mut relaxed = Vec::new();
    for (offset, (draft, key)) in drafts.into_iter().zip(keys.iter()).enumerate() {
        let agent = draft.new_agent.and_then(StepAgent::cleaned);
        let made_role = agent
            .as_ref()
            .map(|agent| format!("{NEW_AGENT_PREFIX}{}", agent.name));
        let role = made_role.as_deref().unwrap_or(draft.role.trim());
        if role.is_empty() {
            return Err(PlanError::MissingRole {
                step: key.to_string(),
            });
        }
        let instructions = draft.instructions.trim();
        if instructions.is_empty() {
            return Err(PlanError::MissingInstructions {
                step: key.to_string(),
            });
        }
        let mut depends_on: Vec<StepKey> = Vec::with_capacity(draft.depends_on.len());
        for raw in &draft.depends_on {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let near = suggest_key(raw);
            let Some(&dep) = known
                .get(raw)
                .or_else(|| near.as_deref().and_then(|near| known.get(near)))
            else {
                // Refusing the plan over one wait cost the planner its turns: this step waits for
                // every one before it instead, which keeps the order and hands it all their work.
                for earlier in &keys[..offset] {
                    if !depends_on.contains(earlier) {
                        depends_on.push(earlier.clone());
                    }
                }
                relaxed.push(RelaxedWait {
                    step: key.to_string(),
                    missing: raw.to_string(),
                });
                continue;
            };
            if !depends_on.contains(dep) {
                depends_on.push(dep.clone());
            }
        }
        steps.push(PlanStep {
            task_id: task.id.clone(),
            key: key.clone(),
            ordinal: first_ordinal.saturating_add(u32::try_from(offset).unwrap_or(u32::MAX)),
            role: role.to_string(),
            instructions: instructions.to_string(),
            inputs: clean_ids(draft.inputs),
            depends_on,
            acceptance: non_blank(draft.acceptance.as_deref()),
            state: StepState::Pending,
            attempts: 0,
            summary: None,
            failure: None,
            started_at: None,
            finished_at: None,
            agent,
        });
    }

    if let Some(cycle) = find_cycle(&steps) {
        return Err(PlanError::Cycle { cycle });
    }
    Ok(ValidatedPlan {
        task_id: task.id.clone(),
        steps,
        kept: kept.iter().map(|step| step.key.clone()).collect(),
        relaxed,
    })
}

/// A plan that keeps every step as it is and adds `draft` after them, for a step the runner adds
/// itself (a hand-off). Open steps are carried over unchanged: a replan through
/// [`validate_plan`] would turn a failed or skipped one back into Pending.
pub fn validate_appended_step(
    task: &LongTask,
    draft: StepDraft,
    existing: &[PlanStep],
) -> Result<ValidatedPlan, PlanError> {
    if task.state.is_terminal() {
        return Err(PlanError::TaskFinished { state: task.state });
    }
    if let Some(stray) = existing.iter().find(|step| step.task_id != task.id) {
        return Err(PlanError::StepsOfAnotherTask {
            task: task.id.clone(),
            other: stray.task_id.clone(),
        });
    }
    let cap = task.budget.max_steps;
    if existing.len() + 1 > cap as usize {
        return Err(PlanError::TooManySteps {
            requested: existing.len() + 1,
            kept: existing.len(),
            cap,
        });
    }
    let key = StepKey::parse(&draft.key)?;
    if existing.iter().any(|step| step.key == key) {
        return Err(PlanError::DuplicateKey { key: key.into() });
    }
    let role = draft.role.trim();
    if role.is_empty() {
        return Err(PlanError::MissingRole { step: key.into() });
    }
    let instructions = draft.instructions.trim();
    if instructions.is_empty() {
        return Err(PlanError::MissingInstructions { step: key.into() });
    }
    let mut depends_on: Vec<StepKey> = Vec::with_capacity(draft.depends_on.len());
    for raw in draft
        .depends_on
        .iter()
        .map(|raw| raw.trim())
        .filter(|raw| !raw.is_empty())
    {
        let Some(dep) = existing.iter().find(|step| step.key.as_str() == raw) else {
            return Err(PlanError::UnknownDependency {
                step: key.to_string(),
                missing: raw.to_string(),
            });
        };
        if !depends_on.contains(&dep.key) {
            depends_on.push(dep.key.clone());
        }
    }

    let mut ordered: Vec<&PlanStep> = existing.iter().collect();
    ordered.sort_by_key(|step| step.ordinal);
    let ordinal = ordered
        .last()
        .map_or(1, |step| step.ordinal.saturating_add(1));
    let mut steps: Vec<PlanStep> = ordered
        .iter()
        .filter(|step| !step.state.survives_replan())
        .map(|step| (*step).clone())
        .collect();
    steps.push(PlanStep {
        task_id: task.id.clone(),
        key,
        ordinal,
        role: role.to_string(),
        instructions: instructions.to_string(),
        inputs: clean_ids(draft.inputs),
        depends_on,
        acceptance: non_blank(draft.acceptance.as_deref()),
        state: StepState::Pending,
        attempts: 0,
        summary: None,
        failure: None,
        started_at: None,
        finished_at: None,
        agent: None,
    });
    Ok(ValidatedPlan {
        task_id: task.id.clone(),
        steps,
        kept: ordered
            .iter()
            .filter(|step| step.state.survives_replan())
            .map(|step| step.key.clone())
            .collect(),
        relaxed: Vec::new(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unvisited,
    OnPath,
    Done,
}

/// The first cycle among `steps`, as keys from a step round to itself. Waits on kept steps are
/// leaves, since a kept step never waits on a new one.
fn find_cycle(steps: &[PlanStep]) -> Option<Vec<String>> {
    let index: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, step)| (step.key.as_str(), i))
        .collect();
    let mut marks = vec![Mark::Unvisited; steps.len()];
    let mut path = Vec::with_capacity(steps.len());
    (0..steps.len()).find_map(|start| visit(start, steps, &index, &mut marks, &mut path))
}

/// Depth-first; recursion is bounded by the step cap.
fn visit(
    at: usize,
    steps: &[PlanStep],
    index: &HashMap<&str, usize>,
    marks: &mut [Mark],
    path: &mut Vec<usize>,
) -> Option<Vec<String>> {
    match marks[at] {
        Mark::Done => return None,
        Mark::OnPath => {
            let from = path.iter().position(|&i| i == at)?;
            let mut cycle: Vec<String> = path[from..]
                .iter()
                .map(|&i| steps[i].key.to_string())
                .collect();
            cycle.push(steps[at].key.to_string());
            return Some(cycle);
        }
        Mark::Unvisited => {}
    }
    marks[at] = Mark::OnPath;
    path.push(at);
    for dep in &steps[at].depends_on {
        if let Some(&next) = index.get(dep.as_str()) {
            if let Some(cycle) = visit(next, steps, index, marks, path) {
                return Some(cycle);
            }
        }
    }
    path.pop();
    marks[at] = Mark::Done;
    None
}

/// The Pending step to run next: every step it waits for is Completed; lowest ordinal wins.
pub fn next_ready_step(steps: &[PlanStep]) -> Option<&PlanStep> {
    let states: HashMap<&str, StepState> = steps
        .iter()
        .map(|step| (step.key.as_str(), step.state))
        .collect();
    steps
        .iter()
        .filter(|step| step.state == StepState::Pending)
        .filter(|step| {
            step.depends_on
                .iter()
                .all(|dep| states.get(dep.as_str()) == Some(&StepState::Completed))
        })
        .min_by_key(|step| step.ordinal)
}

/// Pending steps that cannot become ready: a step they wait for failed, was skipped, is missing
/// or is blocked itself. Retry a failed step before asking, or its dependents are reported too.
pub fn blocked_steps(steps: &[PlanStep]) -> Vec<&PlanStep> {
    let states: HashMap<&str, StepState> = steps
        .iter()
        .map(|step| (step.key.as_str(), step.state))
        .collect();
    let mut blocked: HashSet<&str> = HashSet::new();
    loop {
        let newly: Vec<&str> = steps
            .iter()
            .filter(|step| step.state == StepState::Pending)
            .filter(|step| !blocked.contains(step.key.as_str()))
            .filter(|step| {
                step.depends_on
                    .iter()
                    .any(|dep| match states.get(dep.as_str()) {
                        None | Some(StepState::Failed | StepState::Skipped) => true,
                        Some(_) => blocked.contains(dep.as_str()),
                    })
            })
            .map(|step| step.key.as_str())
            .collect();
        if newly.is_empty() {
            break;
        }
        blocked.extend(newly);
    }
    let mut out: Vec<&PlanStep> = steps
        .iter()
        .filter(|step| blocked.contains(step.key.as_str()))
        .collect();
    out.sort_by_key(|step| step.ordinal);
    out
}

/// Why a plan was refused. Each message says what to change, or a small model resends it as is.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error(
        "this task is already {state}, so its plan can no longer change. Report what it found \
         instead."
    )]
    TaskFinished { state: LongTaskState },
    #[error(
        "the plan has no steps. Write it again with at least one step, each with a `key`, a \
         `role` and `instructions`."
    )]
    EmptyPlan,
    #[error(
        "the plan has {requested} steps, counting the {kept} already started or finished, but \
         this task may have at most {cap}. Merge small steps, or drop the ones the goal does not \
         need."
    )]
    TooManySteps {
        requested: usize,
        kept: usize,
        cap: u32,
    },
    #[error("{}", invalid_key_message(.key, .suggestion))]
    InvalidKey {
        key: String,
        suggestion: Option<String>,
    },
    #[error("two steps share the key `{key}`. Give every step its own key.")]
    DuplicateKey { key: String },
    #[error(
        "step `{key}` has already {} and cannot change. Leave it out of the new plan, and give \
         any new work a new key.",
        settled_phrase(.state)
    )]
    RewritesSettledStep { key: String, state: StepState },
    #[error("step `{step}` names no role. Say which saved role should do it.")]
    MissingRole { step: String },
    #[error("step `{step}` has no instructions. Say in plain words what that step must do.")]
    MissingInstructions { step: String },
    #[error(
        "step `{step}` waits for `{missing}`, which is not a step in this plan. Wait only for \
         the keys of steps in this plan, or of steps already started or finished."
    )]
    UnknownDependency { step: String, missing: String },
    #[error(
        "these steps wait for each other in a circle, so none of them can ever start: {}. \
         Remove one of those waits.",
        cycle_text(.cycle)
    )]
    Cycle { cycle: Vec<String> },
    #[error("the current steps given for task `{task}` belong to task `{other}`")]
    StepsOfAnotherTask { task: String, other: String },
}

fn invalid_key_message(key: &str, suggestion: &Option<String>) -> String {
    let rule = format!("a step key is 1 to {MAX_STEP_KEY_CHARS} characters of a-z, 0-9 and '-'");
    match (key.is_empty(), suggestion) {
        (true, _) => {
            format!("a step has no `key`: {rule}, such as `find-flights`. Give every step one.")
        }
        (false, Some(suggestion)) => format!(
            "`{key}` is not a usable step key: {rule}. Use `{suggestion}` instead, spelled the \
             same wherever another step waits for it."
        ),
        (false, None) => {
            format!("`{key}` is not a usable step key: {rule}, such as `find-flights`.")
        }
    }
}

fn settled_phrase(state: &StepState) -> &'static str {
    match state {
        StepState::Running => "started",
        _ => "finished",
    }
}

fn cycle_text(cycle: &[String]) -> String {
    cycle
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(" waits for ")
}

// ── The hand-back ───────────────────────────────────────────────────────────

/// What a helper says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Completed,
    Failed,
    NeedsApproval,
}

/// The fixed shape every helper hands back; the planner and the prime see this, never its prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepOutcome {
    outcome: OutcomeKind,
    summary: String,
    artifacts: Vec<String>,
    failure: Option<String>,
    #[serde(skip_serializing_if = "HandBackAsks::is_empty")]
    asks: HandBackAsks,
}

/// Cap on a hand-off's brief, which becomes the next step's instructions.
pub const MAX_HAND_OFF_BRIEF_CHARS: usize = 1_200;

/// What a hand-back asks the runner to do next. Asking grants nothing: the runner checks the
/// agent's hooks and the target's activations before it acts on either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HandBackAsks {
    /// An agent to run next, and what it should do.
    pub hand_to: Option<HandToAsk>,
    /// The goal of a long task to start, for an agent whose hooks allow `start_task`.
    pub start_task: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandToAsk {
    pub agent: String,
    pub brief: Option<String>,
}

impl HandBackAsks {
    pub fn is_empty(&self) -> bool {
        self.hand_to.is_none() && self.start_task.is_none()
    }

    fn from_object(object: &serde_json::Map<String, serde_json::Value>) -> Self {
        let brief = first_present(object, &BRIEF_KEYS)
            .map(value_text)
            .and_then(|text| non_blank(Some(text.as_str())))
            .map(|text| cap_text(&text, MAX_HAND_OFF_BRIEF_CHARS));
        let hand_to = first_present(object, &HAND_TO_KEYS).and_then(|value| match value {
            serde_json::Value::String(agent) => {
                non_blank(Some(agent.as_str())).map(|agent| HandToAsk {
                    agent,
                    brief: brief.clone(),
                })
            }
            serde_json::Value::Object(inner) => inner
                .get("agent")
                .or_else(|| inner.get("name"))
                .and_then(serde_json::Value::as_str)
                .and_then(|agent| non_blank(Some(agent)))
                .map(|agent| HandToAsk {
                    agent,
                    brief: first_present(inner, &BRIEF_KEYS)
                        .map(value_text)
                        .and_then(|text| non_blank(Some(text.as_str())))
                        .map(|text| cap_text(&text, MAX_HAND_OFF_BRIEF_CHARS))
                        .or_else(|| brief.clone()),
                }),
            _ => None,
        });
        let start_task = first_present(object, &START_TASK_KEYS)
            .and_then(serde_json::Value::as_str)
            .and_then(|goal| non_blank(Some(goal)))
            .map(|goal| cap_text(&goal, MAX_STEP_SUMMARY_CHARS));
        Self {
            hand_to,
            start_task,
        }
    }
}

/// Names a small model invents for each field, first match wins. None of them widens anything:
/// the hand-back carries no authority.
const OUTCOME_KEYS: [&str; 3] = ["outcome", "status", "state"];
const SUMMARY_KEYS: [&str; 6] = [
    "summary", "result", "answer", "findings", "report", "message",
];
const ARTIFACT_KEYS: [&str; 4] = ["artifacts", "artifact_ids", "artifact", "outputs"];
const FAILURE_KEYS: [&str; 4] = ["failure", "error", "reason", "problem"];
const HAND_TO_KEYS: [&str; 4] = ["hand_to", "handoff", "hand_off", "next_agent"];
const BRIEF_KEYS: [&str; 3] = ["brief", "hand_to_brief", "handoff_brief"];
const START_TASK_KEYS: [&str; 3] = ["start_task", "new_task", "follow_up_task"];

impl StepOutcome {
    /// Summary and failure are capped at [`MAX_STEP_SUMMARY_CHARS`]; a Completed step keeps no
    /// failure text.
    pub fn new(
        outcome: OutcomeKind,
        summary: &str,
        artifacts: Vec<String>,
        failure: Option<&str>,
    ) -> Self {
        Self {
            outcome,
            summary: cap_text(summary, MAX_STEP_SUMMARY_CHARS),
            artifacts: clean_ids(artifacts),
            failure: match outcome {
                OutcomeKind::Completed => None,
                OutcomeKind::Failed | OutcomeKind::NeedsApproval => capped(failure),
            },
            asks: HandBackAsks::default(),
        }
    }

    pub fn with_asks(mut self, asks: HandBackAsks) -> Self {
        self.asks = asks;
        self
    }

    pub fn with_artifacts(mut self, artifacts: Vec<String>) -> Self {
        self.artifacts = clean_ids(artifacts);
        self
    }

    /// Read a helper's final message: bare JSON, fenced JSON, or JSON inside prose.
    pub fn parse(text: &str) -> Result<Self, OutcomeError> {
        let value = first_json_value(text).ok_or(OutcomeError::NotAnObject)?;
        Self::from_json(&value)
    }

    /// Lenient about field names and spellings, never about the outcome: a missing or unknown
    /// one is an error, since defaulting to Completed would pass off a failed step as done.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, OutcomeError> {
        let object = value.as_object().ok_or(OutcomeError::NotAnObject)?;
        let outcome = match first_present(object, &OUTCOME_KEYS) {
            Some(raw) => parse_outcome_kind(raw)?,
            None => match object.get("success").and_then(serde_json::Value::as_bool) {
                Some(true) => OutcomeKind::Completed,
                Some(false) => OutcomeKind::Failed,
                None => return Err(OutcomeError::MissingOutcome),
            },
        };
        let summary = first_present(object, &SUMMARY_KEYS)
            .map(value_text)
            .unwrap_or_default();
        let artifacts = first_present(object, &ARTIFACT_KEYS)
            .map(artifact_ids)
            .unwrap_or_default();
        let failure = first_present(object, &FAILURE_KEYS).map(value_text);
        Ok(Self::new(outcome, &summary, artifacts, failure.as_deref())
            .with_asks(HandBackAsks::from_object(object)))
    }

    pub fn outcome(&self) -> OutcomeKind {
        self.outcome
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Ids as the helper gave them; the runner checks they are this task's before trusting them.
    pub fn artifacts(&self) -> &[String] {
        &self.artifacts
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn asks(&self) -> &HandBackAsks {
        &self.asks
    }
}

fn first_json_value(text: &str) -> Option<serde_json::Value> {
    let text = text.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        return Some(value);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

fn first_present<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a serde_json::Value> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find(|value| !value.is_null())
}

fn parse_outcome_kind(raw: &serde_json::Value) -> Result<OutcomeKind, OutcomeError> {
    let Some(text) = raw.as_str() else {
        return Err(OutcomeError::UnknownOutcome(raw.to_string()));
    };
    let word: String = text
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '-' { '_' } else { c })
        .collect();
    match word.as_str() {
        "completed" | "complete" | "done" | "success" | "succeeded" | "finished" | "ok" => {
            Ok(OutcomeKind::Completed)
        }
        "failed" | "fail" | "failure" | "error" | "blocked" | "unable" => Ok(OutcomeKind::Failed),
        "needs_approval" | "needsapproval" | "awaiting_approval" | "approval_required"
        | "needs_permission" | "approval" => Ok(OutcomeKind::NeedsApproval),
        _ => Err(OutcomeError::UnknownOutcome(text.trim().to_string())),
    }
}

/// A string as itself; anything else as its JSON, so nothing the helper said is dropped.
fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn artifact_ids(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(text) => text.split(',').map(str::to_string).collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::String(id) => Some(id.clone()),
                serde_json::Value::Object(object) => object
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Why a hand-back could not be read; each message shows the shape to send instead.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OutcomeError {
    #[error(
        "your hand-back is not a JSON object. Reply with only a JSON object like \
         {HAND_BACK_EXAMPLE}"
    )]
    NotAnObject,
    #[error(
        "your hand-back has no `outcome`. Say whether the step is `completed`, `failed` or \
         `needs_approval`, as in {HAND_BACK_EXAMPLE}"
    )]
    MissingOutcome,
    #[error("`{0}` is not an outcome. Say `completed`, `failed` or `needs_approval`.")]
    UnknownOutcome(String),
}

// ── Artifacts ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Note,
    Findings,
    Draft,
    Data,
}

impl ArtifactKind {
    pub const ALL: [ArtifactKind; 4] = [
        ArtifactKind::Note,
        ArtifactKind::Findings,
        ArtifactKind::Draft,
        ArtifactKind::Data,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Note => "note",
            ArtifactKind::Findings => "findings",
            ArtifactKind::Draft => "draft",
            ArtifactKind::Data => "data",
        }
    }

    /// Case and surrounding whitespace are forgiven; an unknown kind is `None`, never `Note`.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }
}

/// A step's output. It has no scope of its own: it belongs to its task, so it is read at the
/// task's scope and deleted with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Artifact {
    id: String,
    task_id: String,
    /// The step that wrote it; plain provenance, since a replan may delete that step.
    step_key: Option<StepKey>,
    kind: ArtifactKind,
    title: String,
    summary: String,
    body: String,
    /// Always derived from `body`, never supplied, so paging and the count agree.
    tokens: u32,
    created_at: DateTime<Utc>,
}

/// A stored artifact's columns, for [`Artifact::from_parts`]; `tokens` is recomputed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactParts {
    pub id: String,
    pub task_id: String,
    pub step_key: Option<String>,
    pub kind: ArtifactKind,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// What a step is shown of an artifact before it decides to page the body in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactSummary {
    pub id: String,
    pub task_id: String,
    pub step_key: Option<StepKey>,
    pub kind: ArtifactKind,
    pub title: String,
    pub summary: String,
    pub tokens: u32,
    pub created_at: DateTime<Utc>,
}

impl Artifact {
    /// A new artifact. Title and summary are folded to one line and capped.
    pub fn new(
        task_id: &str,
        step_key: Option<StepKey>,
        kind: ArtifactKind,
        title: &str,
        summary: &str,
        body: String,
        at: DateTime<Utc>,
    ) -> Result<Self, ArtifactError> {
        Self::from_parts(ArtifactParts {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            step_key: step_key.map(String::from),
            kind,
            title: title.to_string(),
            summary: summary.to_string(),
            body,
            created_at: at,
        })
    }

    /// The one validating path, shared by new and stored artifacts.
    pub fn from_parts(parts: ArtifactParts) -> Result<Self, ArtifactError> {
        let ArtifactParts {
            id,
            task_id,
            step_key,
            kind,
            title,
            summary,
            body,
            created_at,
        } = parts;
        if id.trim().is_empty() {
            return Err(ArtifactError::MissingId);
        }
        if task_id.trim().is_empty() {
            return Err(ArtifactError::MissingTaskId);
        }
        let step_key = match step_key {
            None => None,
            Some(raw) => {
                let parsed = StepKey::parse(&raw);
                match parsed {
                    Ok(key) => Some(key),
                    Err(_) => return Err(ArtifactError::InvalidStepKey { key: raw }),
                }
            }
        };
        let title = one_line(&title);
        if title.is_empty() {
            return Err(ArtifactError::MissingTitle);
        }
        let summary = one_line(&summary);
        if summary.is_empty() {
            return Err(ArtifactError::MissingSummary);
        }
        if body.trim().is_empty() {
            return Err(ArtifactError::EmptyBody);
        }
        let tokens = artifact_tokens(&body);
        Ok(Self {
            id,
            task_id,
            step_key,
            kind,
            title: cap_text(&title, MAX_ARTIFACT_TITLE_CHARS),
            summary: cap_text(&summary, MAX_ARTIFACT_SUMMARY_CHARS),
            body,
            tokens,
            created_at,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn step_key(&self) -> Option<&StepKey> {
        self.step_key.as_ref()
    }

    pub fn kind(&self) -> ArtifactKind {
        self.kind
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn tokens(&self) -> u32 {
        self.tokens
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// The tokens `[from_token, to_token)` of the body; see [`artifact_slice`].
    pub fn slice(&self, from_token: u32, to_token: u32) -> &str {
        artifact_slice(&self.body, from_token, to_token)
    }

    pub fn summary_view(&self) -> ArtifactSummary {
        ArtifactSummary {
            id: self.id.clone(),
            task_id: self.task_id.clone(),
            step_key: self.step_key.clone(),
            kind: self.kind,
            title: self.title.clone(),
            summary: self.summary.clone(),
            tokens: self.tokens,
            created_at: self.created_at,
        }
    }
}

/// Estimated tokens in `text`, rounded up so it never under-states.
pub fn artifact_tokens(text: &str) -> u32 {
    let tokens = text.chars().count().div_ceil(ARTIFACT_CHARS_PER_TOKEN);
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

/// The tokens `[from_token, to_token)` of `body` under the [`ARTIFACT_CHARS_PER_TOKEN`] estimate,
/// cut on char boundaries, so consecutive pages join back into the whole body.
pub fn artifact_slice(body: &str, from_token: u32, to_token: u32) -> &str {
    if to_token <= from_token {
        return "";
    }
    let from_char = (from_token as usize).saturating_mul(ARTIFACT_CHARS_PER_TOKEN);
    let to_char = (to_token as usize).saturating_mul(ARTIFACT_CHARS_PER_TOKEN);
    let start = byte_offset(body, from_char);
    let end = byte_offset(body, to_char);
    &body[start..end]
}

/// Byte offset of the `char_index`th char, or the end when the text is shorter.
fn byte_offset(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(offset, _)| offset)
}

/// Why an artifact could not be written or read back.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArtifactError {
    #[error("an artifact needs a title. Give it a few words saying what it is.")]
    MissingTitle,
    #[error(
        "an artifact needs a one-line summary, so later steps can tell whether to read it. Say \
         in one sentence what it holds."
    )]
    MissingSummary,
    #[error("an artifact needs a body. Write what you found into it.")]
    EmptyBody,
    #[error("`{key}` is not a usable step key for an artifact")]
    InvalidStepKey { key: String },
    #[error("a stored artifact has no id")]
    MissingId,
    #[error("an artifact must belong to a task")]
    MissingTaskId,
}

// ── Text helpers ────────────────────────────────────────────────────────────

/// Cut to `max_chars` on a char boundary, ending with a note so no reader takes it as whole.
fn cap_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let marker = format!(" [cut to {max_chars} characters]");
    let marker_chars = marker.chars().count();
    if marker_chars >= max_chars {
        return text.chars().take(max_chars).collect();
    }
    let mut cut: String = text.chars().take(max_chars - marker_chars).collect();
    let kept = cut.trim_end().len();
    cut.truncate(kept);
    cut.push_str(&marker);
    cut
}

/// Capped step text, `None` when blank.
fn capped(text: Option<&str>) -> Option<String> {
    non_blank(text).map(|text| cap_text(&text, MAX_STEP_SUMMARY_CHARS))
}

fn non_blank(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Trimmed, blanks dropped, first occurrence kept.
fn clean_ids(ids: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        let id = id.trim();
        if !id.is_empty() && !out.iter().any(|seen| seen == id) {
            out.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod test_support {
    use super::*;

    pub(super) fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    pub(super) fn task(max_steps: u32) -> LongTask {
        LongTask::start(
            ProfileScope::Household,
            None,
            "plan the school trip",
            None,
            LongTaskBudget::new(max_steps, None).unwrap(),
            t(0),
        )
        .unwrap()
    }

    pub(super) fn draft(key: &str, deps: &[&str]) -> StepDraft {
        StepDraft {
            key: key.to_string(),
            role: "researcher".to_string(),
            instructions: format!("do {key}"),
            depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
            ..StepDraft::default()
        }
    }

    /// A step already in the ledger, in any state, for tests that need history.
    pub(super) fn existing(
        task: &LongTask,
        key: &str,
        ordinal: u32,
        state: StepState,
        deps: &[&str],
    ) -> PlanStep {
        PlanStep {
            task_id: task.id().to_string(),
            key: StepKey::parse(key).unwrap(),
            ordinal,
            role: "researcher".to_string(),
            instructions: format!("do {key}"),
            inputs: Vec::new(),
            depends_on: deps
                .iter()
                .map(|dep| StepKey::parse(dep).unwrap())
                .collect(),
            acceptance: None,
            state,
            attempts: 0,
            summary: None,
            failure: None,
            started_at: None,
            finished_at: None,
            agent: None,
        }
    }

    pub(super) fn keys(steps: &[&PlanStep]) -> Vec<String> {
        steps.iter().map(|step| step.key().to_string()).collect()
    }
}

#[cfg(test)]
mod task_state_tests {
    use super::test_support::*;
    use super::*;

    /// Written out apart from `can_become`, so adding or dropping an edge fails a named pair.
    const TASK_EDGES: [(LongTaskState, LongTaskState); 21] = {
        use LongTaskState::*;
        [
            (Planning, Running),
            (Planning, Paused),
            (Planning, Completed),
            (Planning, Failed),
            (Planning, Cancelled),
            (Running, Planning),
            (Running, AwaitingApproval),
            (Running, Paused),
            (Running, Completed),
            (Running, Failed),
            (Running, Cancelled),
            (AwaitingApproval, Planning),
            (AwaitingApproval, Running),
            (AwaitingApproval, Paused),
            (AwaitingApproval, Failed),
            (AwaitingApproval, Cancelled),
            (Paused, Planning),
            (Paused, Running),
            (Paused, AwaitingApproval),
            (Paused, Failed),
            (Paused, Cancelled),
        ]
    };

    #[test]
    fn every_task_move_is_exactly_the_written_table() {
        for from in LongTaskState::ALL {
            for to in LongTaskState::ALL {
                assert_eq!(
                    from.can_become(to),
                    TASK_EDGES.contains(&(from, to)),
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn a_finished_task_never_moves_again() {
        let terminal: Vec<LongTaskState> = LongTaskState::ALL
            .into_iter()
            .filter(|state| state.is_terminal())
            .collect();
        assert_eq!(
            terminal,
            vec![
                LongTaskState::Completed,
                LongTaskState::Failed,
                LongTaskState::Cancelled
            ]
        );
        for from in &terminal {
            for to in LongTaskState::ALL {
                assert!(!from.can_become(to), "{from} -> {to}");
            }
        }
        // Control: an always-false `can_become` passes the loop above; every live state can move.
        for from in LongTaskState::ALL.into_iter().filter(|s| !s.is_terminal()) {
            assert!(
                LongTaskState::ALL.into_iter().any(|to| from.can_become(to)),
                "{from} is stuck"
            );
            assert!(
                from.can_become(LongTaskState::Cancelled),
                "{from} cannot be cancelled"
            );
        }
    }

    #[test]
    fn transition_stamps_finished_at_only_on_a_terminal_move_and_refuses_after() {
        let mut task = task(4);
        task.transition_to(LongTaskState::Running, t(10)).unwrap();
        assert_eq!(task.state(), LongTaskState::Running);
        assert_eq!(task.updated_at(), t(10));
        assert_eq!(task.finished_at(), None);

        task.transition_to(LongTaskState::Completed, t(20)).unwrap();
        assert_eq!(task.finished_at(), Some(t(20)));

        let refused = task
            .transition_to(LongTaskState::Running, t(30))
            .unwrap_err();
        assert_eq!(refused.from, LongTaskState::Completed);
        assert_eq!(refused.to, LongTaskState::Running);
        assert_eq!(task.state(), LongTaskState::Completed, "a refusal moved it");
        assert_eq!(task.updated_at(), t(20), "a refusal touched it");
    }

    #[test]
    fn stored_spellings_round_trip_and_nothing_else_parses() {
        for state in LongTaskState::ALL {
            assert_eq!(LongTaskState::parse(state.as_str()), Some(state));
        }
        for state in StepState::ALL {
            assert_eq!(StepState::parse(state.as_str()), Some(state));
        }
        for kind in ArtifactKind::ALL {
            assert_eq!(ArtifactKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(LongTaskState::parse("Running"), None);
        assert_eq!(StepState::parse(""), None);
        assert_eq!(ArtifactKind::parse("report"), None);
        assert_eq!(
            ArtifactKind::parse(" Findings "),
            Some(ArtifactKind::Findings)
        );
    }
}

#[cfg(test)]
mod step_state_tests {
    use super::*;

    const STEP_EDGES: [(StepState, StepState); 11] = {
        use StepState::*;
        [
            (Pending, Running),
            (Pending, AwaitingApproval),
            (Pending, Skipped),
            (Running, Completed),
            (Running, Failed),
            (Running, AwaitingApproval),
            (Running, Pending),
            (Failed, Running),
            (Failed, Skipped),
            (AwaitingApproval, Running),
            (AwaitingApproval, Skipped),
        ]
    };

    #[test]
    fn every_step_move_is_exactly_the_written_table() {
        for from in StepState::ALL {
            for to in StepState::ALL {
                assert_eq!(
                    from.can_become(to),
                    STEP_EDGES.contains(&(from, to)),
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn completed_and_skipped_steps_never_move_and_the_rest_can() {
        for from in [StepState::Completed, StepState::Skipped] {
            for to in StepState::ALL {
                assert!(!from.can_become(to), "{from} -> {to}");
            }
        }
        for from in [
            StepState::Pending,
            StepState::Running,
            StepState::Failed,
            StepState::AwaitingApproval,
        ] {
            assert!(StepState::ALL.into_iter().any(|to| from.can_become(to)));
        }
    }

    #[test]
    fn only_completed_and_running_steps_survive_a_replan() {
        let kept: Vec<StepState> = StepState::ALL
            .into_iter()
            .filter(|state| state.survives_replan())
            .collect();
        assert_eq!(kept, vec![StepState::Running, StepState::Completed]);
    }
}

#[cfg(test)]
mod task_tests {
    use super::test_support::*;
    use super::*;

    fn start(scope: ProfileScope, owner: Option<&str>) -> Result<LongTask, LongTaskError> {
        LongTask::start(
            scope,
            owner.map(str::to_string),
            "  book the dentist  ",
            Some("session-1".to_string()),
            LongTaskBudget::new(5, Some(t(3600))).unwrap(),
            t(0),
        )
    }

    #[test]
    fn a_new_task_starts_planning_with_its_fields_as_given() {
        let task = start(ProfileScope::Owner("liz".into()), Some("liz")).unwrap();
        assert_eq!(task.state(), LongTaskState::Planning);
        assert_eq!(task.goal(), "book the dentist");
        assert_eq!(task.owner_profile_id(), Some("liz"));
        assert_eq!(task.scope(), &ProfileScope::Owner("liz".into()));
        assert_eq!(task.origin_session_id(), Some("session-1"));
        assert_eq!(task.budget().max_steps(), 5);
        assert_eq!(task.budget().deadline(), Some(t(3600)));
        assert_eq!(task.created_at(), t(0));
        assert_eq!(task.updated_at(), t(0));
        assert_eq!(task.finished_at(), None);
        assert!(uuid::Uuid::parse_str(task.id()).is_ok());
        assert_ne!(
            task.id(),
            start(ProfileScope::Household, None).unwrap().id()
        );
    }

    #[test]
    fn the_scope_and_the_owner_must_agree() {
        assert_eq!(
            start(ProfileScope::Owner("liz".into()), Some("jerry")).unwrap_err(),
            LongTaskError::OwnerMismatch {
                owner: "jerry".into(),
                scope_owner: "liz".into()
            }
        );
        assert_eq!(
            start(ProfileScope::Guest, Some("liz")).unwrap_err(),
            LongTaskError::GuestWithOwner {
                owner: "liz".into()
            }
        );
        assert_eq!(
            start(ProfileScope::Owner(" ".into()), None).unwrap_err(),
            LongTaskError::BlankOwner
        );
        // Controls: each refusal above has an accepted neighbour.
        assert_eq!(
            start(ProfileScope::Owner("liz".into()), None)
                .unwrap()
                .owner_profile_id(),
            Some("liz"),
            "an Owner scope names its owner"
        );
        assert_eq!(
            start(ProfileScope::Guest, None).unwrap().owner_profile_id(),
            None
        );
        assert_eq!(
            start(ProfileScope::Household, Some("jerry"))
                .unwrap()
                .owner_profile_id(),
            Some("jerry")
        );
    }

    #[test]
    fn a_goal_a_budget_and_a_future_deadline_are_required() {
        let budget = LongTaskBudget::new(3, None).unwrap();
        assert_eq!(
            LongTask::start(ProfileScope::Household, None, "  ", None, budget, t(0)).unwrap_err(),
            LongTaskError::MissingGoal
        );
        assert_eq!(
            LongTaskBudget::new(0, None).unwrap_err(),
            LongTaskError::MaxStepsOutOfRange {
                requested: 0,
                cap: MAX_LONG_TASK_STEPS
            }
        );
        assert!(LongTaskBudget::new(MAX_LONG_TASK_STEPS + 1, None).is_err());
        assert!(LongTaskBudget::new(MAX_LONG_TASK_STEPS, None).is_ok());
        assert!(LongTaskBudget::new(DEFAULT_LONG_TASK_STEPS, None).is_ok());
        let late = LongTaskBudget::new(3, Some(t(0))).unwrap();
        assert_eq!(
            LongTask::start(ProfileScope::Household, None, "go", None, late, t(0)).unwrap_err(),
            LongTaskError::DeadlineNotInFuture
        );
        assert_eq!(
            LongTask::start(
                ProfileScope::Household,
                None,
                "go",
                Some(" ".into()),
                budget,
                t(0)
            )
            .unwrap_err(),
            LongTaskError::BlankOriginSession
        );
    }

    #[test]
    fn a_task_is_general_until_asked_and_its_kind_reads_back_exactly() {
        let task = LongTask::start(
            ProfileScope::Owner("liz".into()),
            None,
            "compare heat pumps",
            None,
            LongTaskBudget::new(3, None).unwrap(),
            t(0),
        )
        .unwrap();
        assert_eq!(task.kind(), LongTaskKind::General);
        assert_eq!(
            task.with_kind(LongTaskKind::Research).kind(),
            LongTaskKind::Research
        );
        for kind in LongTaskKind::ALL {
            assert_eq!(LongTaskKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(
            LongTaskKind::parse("Research"),
            None,
            "only the stored spelling"
        );
    }

    #[test]
    fn the_planner_decides_the_answer_only_when_the_member_did_not() {
        let task = || {
            LongTask::start(
                ProfileScope::Household,
                None,
                "how warm is it",
                None,
                LongTaskBudget::new(3, None).unwrap(),
                t(0),
            )
            .unwrap()
        };
        let mut auto = task();
        assert_eq!(auto.answer(), AnswerShape::Auto);
        assert!(!auto.decide_answer(AnswerShape::Auto));
        assert!(auto.decide_answer(AnswerShape::Short));
        assert!(!auto.decide_answer(AnswerShape::Report), "decided once");
        assert_eq!(auto.answer(), AnswerShape::Short);

        let mut chosen = task().with_answer(AnswerShape::Report);
        assert!(
            !chosen.decide_answer(AnswerShape::Short),
            "the member's choice stands"
        );
        assert_eq!(chosen.answer(), AnswerShape::Report);

        assert_eq!(AnswerShape::read(" Short "), Some(AnswerShape::Short));
        assert_eq!(AnswerShape::read(""), Some(AnswerShape::Auto));
        assert_eq!(AnswerShape::read("essay"), None);
        assert_eq!(
            AnswerShape::parse("Short"),
            None,
            "only the stored spelling"
        );
    }

    #[test]
    fn the_deadline_passes_at_its_instant() {
        let budget = LongTaskBudget::new(3, Some(t(100))).unwrap();
        assert!(!budget.deadline_passed(t(99)));
        assert!(budget.deadline_passed(t(100)));
        assert!(!LongTaskBudget::new(3, None)
            .unwrap()
            .deadline_passed(t(i64::from(u32::MAX))));
    }

    fn parts(max_steps: u32) -> LongTaskParts {
        LongTaskParts {
            id: "task-1".into(),
            owner_profile_id: Some("liz".into()),
            scope: ProfileScope::Owner("liz".into()),
            goal: "book the dentist".into(),
            origin_session_id: None,
            state: LongTaskState::Paused,
            max_steps,
            deadline: None,
            kind: LongTaskKind::Research,
            answer: AnswerShape::Auto,
            created_at: t(0),
            updated_at: t(5),
            finished_at: None,
        }
    }

    #[test]
    fn a_stored_task_is_rebuilt_through_the_same_checks() {
        let task = LongTask::from_parts(parts(4)).unwrap();
        assert_eq!(task.id(), "task-1");
        assert_eq!(task.state(), LongTaskState::Paused);
        assert_eq!(task.updated_at(), t(5));
        assert_eq!(task.budget().max_steps(), 4);
        assert_eq!(
            task.kind(),
            LongTaskKind::Research,
            "a stored kind survives the rebuild"
        );

        assert_eq!(
            LongTask::from_parts(parts(MAX_LONG_TASK_STEPS + 10))
                .unwrap()
                .budget()
                .max_steps(),
            MAX_LONG_TASK_STEPS,
            "a stored budget above today's cap is narrowed"
        );
        assert!(LongTask::from_parts(parts(0)).is_err());
        let mut mismatched = parts(4);
        mismatched.owner_profile_id = Some("jerry".into());
        assert!(matches!(
            LongTask::from_parts(mismatched),
            Err(LongTaskError::OwnerMismatch { .. })
        ));
        let mut nameless = parts(4);
        nameless.id = String::new();
        assert_eq!(
            LongTask::from_parts(nameless).unwrap_err(),
            LongTaskError::MissingId
        );
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn keys_in_the_alphabet_are_accepted() {
        let longest = "k".repeat(MAX_STEP_KEY_CHARS);
        for key in ["a", "find-flights", "step-2", "9", "-", longest.as_str()] {
            assert_eq!(StepKey::parse(key).unwrap().as_str(), key);
        }
        assert_eq!(
            StepKey::parse("  draft-email \n").unwrap().as_str(),
            "draft-email"
        );
    }

    #[test]
    fn keys_outside_it_are_refused_with_a_usable_suggestion() {
        let cases = [
            ("Find Flights", Some("find-flights")),
            ("step_1", Some("step-1")),
            ("Step 2", Some("step-2")),
            ("café", Some("caf")),
            ("!!!", None),
            ("", None),
        ];
        for (raw, suggestion) in cases {
            assert_eq!(
                StepKey::parse(raw).unwrap_err(),
                PlanError::InvalidKey {
                    key: raw.to_string(),
                    suggestion: suggestion.map(str::to_string)
                },
                "{raw:?}"
            );
            if let Some(suggestion) = suggestion {
                assert!(StepKey::parse(suggestion).is_ok(), "{suggestion} is no key");
            }
        }
        let long = "k".repeat(MAX_STEP_KEY_CHARS + 1);
        let PlanError::InvalidKey { suggestion, .. } = StepKey::parse(&long).unwrap_err() else {
            panic!("a long key was not refused as a key");
        };
        let suggestion = suggestion.unwrap();
        assert_eq!(suggestion.len(), MAX_STEP_KEY_CHARS);
        assert!(StepKey::parse(&suggestion).is_ok());
    }

    #[test]
    fn a_plan_takes_a_misspelt_key_as_its_suggestion_but_never_an_unusable_one() {
        assert_eq!(StepKey::lenient("step_1").unwrap().as_str(), "step-1");
        assert_eq!(
            StepKey::lenient("Find Flights").unwrap().as_str(),
            "find-flights"
        );
        assert_eq!(StepKey::lenient("ok").unwrap().as_str(), "ok");
        assert!(matches!(
            StepKey::lenient("!!!"),
            Err(PlanError::InvalidKey {
                suggestion: None,
                ..
            })
        ));
    }

    #[test]
    fn every_field_a_step_writes_is_one_it_reads() {
        let written = serde_json::to_value(StepDraft::default()).unwrap();
        for name in written.as_object().unwrap().keys() {
            assert!(
                STEP_DRAFT_FIELDS.contains(&name.as_str()),
                "{name} is not listed"
            );
        }
        let extra = serde_json::json!({"key": "a", "role": "r", "instructions": "x", "title": "t"});
        assert_eq!(serde_json::from_value::<StepDraft>(extra).unwrap().key, "a");
    }

    #[test]
    fn the_refusal_names_the_key_to_use() {
        let message = StepKey::parse("Find Flights").unwrap_err().to_string();
        assert!(message.contains("`Find Flights`"), "{message}");
        assert!(message.contains("`find-flights`"), "{message}");
        let message = StepKey::parse("").unwrap_err().to_string();
        assert!(message.contains("has no `key`"), "{message}");
    }

    #[test]
    fn a_key_deserialises_only_through_the_same_check() {
        assert_eq!(
            serde_json::from_str::<StepKey>("\"find-flights\"")
                .unwrap()
                .as_str(),
            "find-flights"
        );
        assert!(serde_json::from_str::<StepKey>("\"Find Flights\"").is_err());
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    #[test]
    fn a_draft_reads_the_canonical_shape() {
        let draft: StepDraft = serde_json::from_str(
            r#"{"key": "compare", "role": "analyst", "instructions": "compare the quotes",
                "inputs": ["a1", "a2"], "depends_on": ["fetch"], "acceptance": "names a winner"}"#,
        )
        .unwrap();
        assert_eq!(
            draft,
            StepDraft {
                key: "compare".into(),
                role: "analyst".into(),
                instructions: "compare the quotes".into(),
                inputs: vec!["a1".into(), "a2".into()],
                depends_on: vec!["fetch".into()],
                acceptance: Some("names a winner".into()),
                new_agent: None,
            }
        );
    }

    #[test]
    fn a_draft_forgives_a_small_models_spellings() {
        let draft: StepDraft = serde_json::from_str(
            r#"{"id": "compare", "agent": "analyst", "task": "compare the quotes",
                "artifacts": "a1, a2", "after": "fetch", "check": null}"#,
        )
        .unwrap();
        assert_eq!(draft.key, "compare");
        assert_eq!(draft.role, "analyst");
        assert_eq!(draft.instructions, "compare the quotes");
        assert_eq!(draft.inputs, vec!["a1".to_string(), " a2".to_string()]);
        assert_eq!(draft.depends_on, vec!["fetch".to_string()]);
        assert_eq!(draft.acceptance, None);
    }

    #[test]
    fn a_misspelt_field_is_read_as_the_one_it_means_and_an_unknown_one_is_named() {
        for spelt in ["dependson", "dependsOn", "Depends-On"] {
            let ReadStep { draft, ignored, .. } = read_step_draft(
                serde_json::json!({
                    "key": "b", "role": "r", "instructions": "go", spelt: ["a"]
                }),
                1,
            )
            .unwrap();
            assert_eq!(
                draft.depends_on,
                vec!["a".to_string()],
                "{spelt} was dropped"
            );
            assert!(ignored.is_empty(), "{spelt}: {ignored:?}");
        }
        let ReadStep { draft, ignored, .. } = read_step_draft(
            serde_json::json!({
                "key": "b", "role": "r", "instructions": "go", "tools": ["giap-system"],
                "depends_on": ["a"], "dependsOn": ["c"]
            }),
            1,
        )
        .unwrap();
        assert_eq!(
            draft.depends_on,
            vec!["a".to_string()],
            "the exact name wins"
        );
        let mut ignored = ignored;
        ignored.sort();
        assert_eq!(ignored, vec!["dependsOn".to_string(), "tools".to_string()]);
        assert!(read_step_draft(serde_json::json!({"key": "b", "role": "r"}), 1).is_err());
    }

    #[test]
    fn an_unreadable_new_agent_is_dropped_and_the_step_kept() {
        for agent in [
            serde_json::json!("Forager Gosling: search the web"),
            serde_json::json!({"instructions": "search", "tools": ["giap-knowledge"]}),
        ] {
            let step = read_step_draft(
                serde_json::json!({"key": "a", "role": "r", "instructions": "go", "new_agent": agent}),
                1,
            )
            .unwrap();
            assert!(step.draft.new_agent.is_none());
            assert_eq!(step.ignored, vec!["new_agent".to_string()]);
        }
    }

    #[test]
    fn a_step_with_no_key_takes_one_from_its_name_or_its_place() {
        let read = |value: serde_json::Value| {
            let step = read_step_draft(value, 3).unwrap();
            (step.draft, step.ignored)
        };
        let (named, ignored) =
            read(serde_json::json!({"name": "Gather Events", "role": "r", "instructions": "go"}));
        assert_eq!(
            named.key, "Gather Events",
            "validate_plan slugs it to gather-events"
        );
        assert!(
            ignored.is_empty(),
            "the name was used, not ignored: {ignored:?}"
        );
        let (numbered, _) = read(serde_json::json!({"step": 2, "role": "r", "instructions": "go"}));
        assert_eq!(numbered.key, "step-2");
        let (placed, _) = read(serde_json::json!({"role": "r", "instructions": "go"}));
        assert_eq!(placed.key, "step-3");
        let (kept, ignored) =
            read(serde_json::json!({"key": "a", "name": "A", "role": "r", "instructions": "go"}));
        assert_eq!(kept.key, "a");
        assert!(ignored.is_empty(), "a name is kept for waits: {ignored:?}");
    }

    #[test]
    fn a_wait_may_name_a_step_by_its_name() {
        let mut steps = vec![
            read_step_draft(
                serde_json::json!({"step": 1, "name": "Gather camera events", "role": "r",
                                   "instructions": "go"}),
                1,
            )
            .unwrap(),
            read_step_draft(
                serde_json::json!({"step": 2, "role": "r", "instructions": "write",
                                   "depends_on": ["gather-camera-events", "step-1"]}),
                2,
            )
            .unwrap(),
        ];
        assert_eq!(steps[0].draft.key, "Gather camera events");
        match_waits_by_name(&mut steps);
        assert_eq!(
            steps[1].draft.depends_on,
            vec![
                "gather-camera-events".to_string(),
                "Gather camera events".to_string()
            ],
            "`step: 1` names the step `step-1` too"
        );
    }
}

#[cfg(test)]
mod plan_tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn a_valid_plan_becomes_pending_steps_in_the_order_written() {
        let task = task(5);
        let mut fetch = draft("fetch", &[]);
        fetch.inputs = vec![" a1 ".into(), "".into(), "a1".into(), "a2".into()];
        fetch.acceptance = Some("   ".into());
        let mut compare = draft("compare", &["fetch", " fetch "]);
        compare.acceptance = Some(" names a winner ".into());
        compare.role = "  analyst ".into();

        let plan = validate_plan(&task, vec![fetch, compare], &[]).unwrap();
        assert_eq!(plan.task_id(), task.id());
        assert!(plan.kept_keys().is_empty());
        let steps = plan.steps();
        assert_eq!(steps.len(), 2);

        assert_eq!(steps[0].key().as_str(), "fetch");
        assert_eq!(steps[0].ordinal(), 1);
        assert_eq!(steps[0].inputs(), ["a1".to_string(), "a2".to_string()]);
        assert_eq!(steps[0].acceptance(), None);
        assert!(steps[0].depends_on().is_empty());

        assert_eq!(steps[1].key().as_str(), "compare");
        assert_eq!(steps[1].ordinal(), 2);
        assert_eq!(steps[1].role(), "analyst");
        assert_eq!(steps[1].acceptance(), Some("names a winner"));
        assert_eq!(steps[1].depends_on(), [StepKey::parse("fetch").unwrap()]);

        for step in steps {
            assert_eq!(step.task_id(), task.id());
            assert_eq!(step.state(), StepState::Pending);
            assert_eq!(step.attempts(), 0);
            assert_eq!(step.summary(), None);
            assert_eq!(step.failure(), None);
            assert_eq!(step.started_at(), None);
            assert_eq!(step.finished_at(), None);
        }
    }

    #[test]
    fn two_steps_may_not_share_a_key() {
        let refused =
            validate_plan(&task(5), vec![draft("a", &[]), draft("a", &[])], &[]).unwrap_err();
        assert_eq!(refused, PlanError::DuplicateKey { key: "a".into() });
    }

    #[test]
    fn a_replan_never_rewrites_a_started_or_finished_step() {
        let task = task(6);
        for state in [StepState::Completed, StepState::Running] {
            let history = [existing(&task, "a", 1, state, &[])];
            assert_eq!(
                validate_plan(&task, vec![draft("a", &[])], &history).unwrap_err(),
                PlanError::RewritesSettledStep {
                    key: "a".into(),
                    state
                }
            );
        }
        // Control: an open step's key is free to reuse, since the replan replaces that step.
        for state in [
            StepState::Pending,
            StepState::Failed,
            StepState::Skipped,
            StepState::AwaitingApproval,
        ] {
            let history = [existing(&task, "a", 1, state, &[])];
            let plan = validate_plan(&task, vec![draft("a", &[])], &history).unwrap();
            assert_eq!(plan.steps()[0].key().as_str(), "a", "{state}");
            assert!(plan.kept_keys().is_empty(), "{state} was kept");
        }
    }

    #[test]
    fn new_steps_follow_the_kept_ones_and_may_wait_for_them() {
        let task = task(6);
        let history = [
            existing(&task, "fetch", 1, StepState::Completed, &[]),
            existing(&task, "read", 4, StepState::Running, &["fetch"]),
            existing(&task, "old", 7, StepState::Failed, &[]),
        ];
        let plan =
            validate_plan(&task, vec![draft("write", &["read", "fetch"])], &history).unwrap();
        assert_eq!(
            plan.kept_keys(),
            [
                StepKey::parse("fetch").unwrap(),
                StepKey::parse("read").unwrap()
            ]
        );
        assert_eq!(
            plan.steps()[0].ordinal(),
            5,
            "after the highest kept ordinal, not the dropped one"
        );
    }

    #[test]
    fn a_wait_for_a_step_the_plan_lacks_becomes_a_wait_for_every_step_before_it() {
        let task = task(6);
        let plan = validate_plan(
            &task,
            vec![draft("a", &[]), draft("b", &[]), draft("c", &["gather"])],
            &[],
        )
        .unwrap();
        let waits: Vec<&str> = plan.steps()[2]
            .depends_on()
            .iter()
            .map(StepKey::as_str)
            .collect();
        assert_eq!(waits, vec!["a", "b"]);
        assert_eq!(
            plan.relaxed_waits(),
            &[RelaxedWait {
                step: "c".into(),
                missing: "gather".into()
            }]
        );
        // The first step has nothing before it, so its wait is dropped. So is a wait for an open
        // step left out of the new plan, which is deleted.
        let history = [existing(&task, "a", 1, StepState::Pending, &[])];
        let plan = validate_plan(&task, vec![draft("b", &["a"])], &history).unwrap();
        assert!(plan.steps()[0].depends_on().is_empty());
        assert_eq!(plan.relaxed_waits().len(), 1);
        // Control: the same wait is fine once `a` is in the plan or finished.
        assert!(validate_plan(&task, vec![draft("a", &[]), draft("b", &["a"])], &[]).is_ok());
        let done = [existing(&task, "a", 1, StepState::Completed, &[])];
        assert!(validate_plan(&task, vec![draft("b", &["a"])], &done).is_ok());
    }

    #[test]
    fn a_circle_of_waits_is_refused_and_named() {
        let task = task(8);
        let refused = validate_plan(
            &task,
            vec![draft("a", &["c"]), draft("b", &["a"]), draft("c", &["b"])],
            &[],
        )
        .unwrap_err();
        let PlanError::Cycle { cycle } = &refused else {
            panic!("not refused as a cycle: {refused:?}");
        };
        assert_eq!(cycle.first(), cycle.last());
        let mut members = cycle.clone();
        members.sort();
        members.dedup();
        assert_eq!(members, vec!["a", "b", "c"]);
        let message = refused.to_string();
        assert!(message.contains("`a`") && message.contains("`b`") && message.contains("`c`"));

        assert_eq!(
            validate_plan(&task, vec![draft("a", &["a"])], &[]).unwrap_err(),
            PlanError::Cycle {
                cycle: vec!["a".into(), "a".into()]
            }
        );
    }

    /// Control for the cycle test: shared waits are not a circle.
    #[test]
    fn a_diamond_of_waits_is_accepted() {
        let plan = validate_plan(
            &task(8),
            vec![
                draft("d", &["b", "c"]),
                draft("b", &["a"]),
                draft("c", &["a"]),
                draft("a", &[]),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(plan.steps().len(), 4);
    }

    #[test]
    fn every_step_needs_a_role_and_instructions() {
        let mut nameless = draft("a", &[]);
        nameless.role = "  ".into();
        assert_eq!(
            validate_plan(&task(3), vec![nameless], &[]).unwrap_err(),
            PlanError::MissingRole { step: "a".into() }
        );
        let mut empty = draft("a", &[]);
        empty.instructions = "\n".into();
        assert_eq!(
            validate_plan(&task(3), vec![empty], &[]).unwrap_err(),
            PlanError::MissingInstructions { step: "a".into() }
        );
    }

    #[test]
    fn the_step_cap_counts_the_steps_already_kept() {
        let task = task(3);
        let history = [existing(&task, "done", 1, StepState::Completed, &[])];
        assert_eq!(
            validate_plan(
                &task,
                vec![draft("a", &[]), draft("b", &[]), draft("c", &[])],
                &history
            )
            .unwrap_err(),
            PlanError::TooManySteps {
                requested: 4,
                kept: 1,
                cap: 3
            }
        );
        // Control: one fewer fits exactly.
        assert!(validate_plan(&task, vec![draft("a", &[]), draft("b", &[])], &history).is_ok());
        // Dropped open steps do not count.
        let open = [
            existing(&task, "x", 1, StepState::Failed, &[]),
            existing(&task, "y", 2, StepState::Pending, &[]),
        ];
        assert!(validate_plan(
            &task,
            vec![draft("a", &[]), draft("b", &[]), draft("c", &[])],
            &open
        )
        .is_ok());
    }

    #[test]
    fn an_empty_plan_is_refused_unless_it_only_drops_open_work() {
        let task = task(3);
        assert_eq!(
            validate_plan(&task, vec![], &[]).unwrap_err(),
            PlanError::EmptyPlan
        );
        let history = [
            existing(&task, "done", 1, StepState::Completed, &[]),
            existing(&task, "later", 2, StepState::Pending, &[]),
        ];
        let plan = validate_plan(&task, vec![], &history).unwrap();
        assert!(plan.steps().is_empty());
        assert_eq!(plan.kept_keys(), [StepKey::parse("done").unwrap()]);
    }

    #[test]
    fn a_finished_task_takes_no_plan() {
        let mut task = task(3);
        task.transition_to(LongTaskState::Cancelled, t(1)).unwrap();
        assert_eq!(
            validate_plan(&task, vec![draft("a", &[])], &[]).unwrap_err(),
            PlanError::TaskFinished {
                state: LongTaskState::Cancelled
            }
        );
    }

    #[test]
    fn a_plan_goes_stale_when_a_step_it_relied_on_moves() {
        let task = task(6);
        let history = [
            existing(&task, "fetch", 1, StepState::Running, &[]),
            existing(&task, "later", 2, StepState::Pending, &[]),
        ];
        let plan = validate_plan(
            &task,
            vec![draft("later", &[]), draft("write", &["fetch"])],
            &history,
        )
        .unwrap();

        // Control: as checked, and after the kept step finishes, the plan still fits.
        let as_checked = [("fetch", StepState::Running), ("later", StepState::Pending)];
        assert_eq!(plan.check_current(as_checked), Ok(()));
        assert_eq!(
            plan.check_current([
                ("fetch", StepState::Completed),
                ("later", StepState::Failed)
            ]),
            Ok(())
        );

        let preempted = plan
            .check_current([("fetch", StepState::Pending), ("later", StepState::Pending)])
            .unwrap_err();
        assert_eq!(preempted.step, "fetch");
        assert_eq!(preempted.now, Some(StepState::Pending));

        let gone = plan
            .check_current([("later", StepState::Pending)])
            .unwrap_err();
        assert_eq!((gone.step.as_str(), gone.now), ("fetch", None));

        let started = plan
            .check_current([("fetch", StepState::Running), ("later", StepState::Running)])
            .unwrap_err();
        assert_eq!(
            (started.step.as_str(), started.now),
            ("later", Some(StepState::Running))
        );
    }

    #[test]
    fn another_tasks_steps_are_refused_as_history() {
        let mine = task(3);
        let theirs = task(3);
        let history = [existing(&theirs, "a", 1, StepState::Completed, &[])];
        assert!(matches!(
            validate_plan(&mine, vec![draft("b", &[])], &history),
            Err(PlanError::StepsOfAnotherTask { .. })
        ));
    }

    #[test]
    fn every_refusal_says_what_to_do_next() {
        let refusals = [
            PlanError::TaskFinished {
                state: LongTaskState::Completed,
            },
            PlanError::EmptyPlan,
            PlanError::TooManySteps {
                requested: 9,
                kept: 2,
                cap: 8,
            },
            PlanError::DuplicateKey { key: "a".into() },
            PlanError::RewritesSettledStep {
                key: "a".into(),
                state: StepState::Running,
            },
            PlanError::MissingRole { step: "a".into() },
            PlanError::MissingInstructions { step: "a".into() },
            PlanError::UnknownDependency {
                step: "b".into(),
                missing: "a".into(),
            },
            PlanError::Cycle {
                cycle: vec!["a".into(), "a".into()],
            },
        ];
        for refusal in refusals {
            let message = refusal.to_string();
            let last = message.rsplit(". ").next().unwrap_or_default();
            let imperative = [
                "Report", "Write", "Merge", "Give", "Leave", "Say", "Wait", "Remove",
            ];
            assert!(
                imperative.iter().any(|verb| last.starts_with(verb)),
                "no instruction at the end of: {message}"
            );
        }
        assert!(PlanError::RewritesSettledStep {
            key: "a".into(),
            state: StepState::Running
        }
        .to_string()
        .contains("already started"));
        assert!(PlanError::RewritesSettledStep {
            key: "a".into(),
            state: StepState::Completed
        }
        .to_string()
        .contains("already finished"));
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn the_next_step_is_the_lowest_pending_one_whose_waits_are_all_completed() {
        let task = task(8);
        let steps = vec![
            existing(&task, "done", 1, StepState::Completed, &[]),
            existing(&task, "waits-on-running", 2, StepState::Pending, &["busy"]),
            existing(&task, "busy", 3, StepState::Running, &[]),
            existing(&task, "ready-late", 6, StepState::Pending, &["done"]),
            existing(&task, "ready-early", 5, StepState::Pending, &[]),
        ];
        assert_eq!(
            next_ready_step(&steps).unwrap().key().as_str(),
            "ready-early",
            "a lower ordinal waiting on a Running step is not ready"
        );
    }

    #[test]
    fn nothing_is_ready_when_nothing_pending_has_its_waits_met() {
        let task = task(8);
        let steps = vec![
            existing(&task, "a", 1, StepState::AwaitingApproval, &[]),
            existing(&task, "b", 2, StepState::Pending, &["a"]),
            existing(&task, "c", 3, StepState::Completed, &[]),
        ];
        assert!(next_ready_step(&steps).is_none());
        assert!(next_ready_step(&[]).is_none());
    }

    #[test]
    fn a_step_whose_wait_failed_was_skipped_or_is_missing_is_blocked_transitively() {
        let task = task(10);
        let steps = vec![
            existing(&task, "failed", 1, StepState::Failed, &[]),
            existing(&task, "skipped", 2, StepState::Skipped, &[]),
            existing(&task, "on-failed", 3, StepState::Pending, &["failed"]),
            existing(&task, "on-skipped", 4, StepState::Pending, &["skipped"]),
            existing(&task, "on-blocked", 5, StepState::Pending, &["on-failed"]),
            existing(&task, "on-missing", 6, StepState::Pending, &["gone"]),
        ];
        let blocked = blocked_steps(&steps);
        assert_eq!(
            keys(&blocked),
            vec!["on-failed", "on-skipped", "on-blocked", "on-missing"]
        );
    }

    /// Control: a Pending step that is merely waiting is not blocked.
    #[test]
    fn a_step_waiting_on_live_work_is_not_blocked() {
        let task = task(10);
        let steps = vec![
            existing(&task, "running", 1, StepState::Running, &[]),
            existing(&task, "parked", 2, StepState::AwaitingApproval, &[]),
            existing(&task, "pending", 3, StepState::Pending, &[]),
            existing(&task, "done", 4, StepState::Completed, &[]),
            existing(&task, "w1", 5, StepState::Pending, &["running", "done"]),
            existing(&task, "w2", 6, StepState::Pending, &["parked"]),
            existing(&task, "w3", 7, StepState::Pending, &["pending"]),
            existing(&task, "failed", 8, StepState::Failed, &["w1"]),
        ];
        assert!(
            blocked_steps(&steps).is_empty(),
            "{:?}",
            keys(&blocked_steps(&steps))
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::test_support::*;
    use super::*;

    fn pending() -> PlanStep {
        existing(&task(4), "a", 1, StepState::Pending, &[])
    }

    fn outcome(kind: OutcomeKind, failure: Option<&str>) -> StepOutcome {
        StepOutcome::new(kind, "found three quotes", vec!["art-1".into()], failure)
    }

    #[test]
    fn starting_counts_an_attempt_and_a_preemption_gives_it_back() {
        let mut step = pending();
        step.start(t(1)).unwrap();
        assert_eq!(step.state(), StepState::Running);
        assert_eq!(step.attempts(), 1);
        assert_eq!(step.started_at(), Some(t(1)));

        step.preempt().unwrap();
        assert_eq!(step.state(), StepState::Pending);
        assert_eq!(
            step.attempts(),
            0,
            "a household turn is not the step's failure"
        );
        assert_eq!(step.started_at(), None);

        step.start(t(2)).unwrap();
        step.fail("the pond restarted mid-step", t(3)).unwrap();
        assert_eq!(step.attempts(), 1, "a crash does count");
        step.start(t(4)).unwrap();
        assert_eq!(step.attempts(), 2);
        assert_eq!(step.finished_at(), None);
        assert_eq!(
            step.failure(),
            Some("the pond restarted mid-step"),
            "the retry's prompt needs the reason"
        );
    }

    #[test]
    fn a_hand_back_settles_a_running_step() {
        let mut done = pending();
        done.start(t(1)).unwrap();
        done.record(&outcome(OutcomeKind::Completed, Some("ignored")), t(2))
            .unwrap();
        assert_eq!(done.state(), StepState::Completed);
        assert_eq!(done.summary(), Some("found three quotes"));
        assert_eq!(done.failure(), None);
        assert_eq!(done.finished_at(), Some(t(2)));

        let mut failed = pending();
        failed.start(t(1)).unwrap();
        failed
            .record(&outcome(OutcomeKind::Failed, None), t(2))
            .unwrap();
        assert_eq!(failed.state(), StepState::Failed);
        assert_eq!(failed.failure(), Some(UNEXPLAINED_FAILURE));
        assert_eq!(failed.finished_at(), Some(t(2)));

        let mut parked = pending();
        parked.start(t(1)).unwrap();
        parked
            .record(
                &outcome(OutcomeKind::NeedsApproval, Some("it books a table")),
                t(2),
            )
            .unwrap();
        assert_eq!(parked.state(), StepState::AwaitingApproval);
        assert_eq!(parked.failure(), Some("it books a table"));
        assert_eq!(parked.finished_at(), None, "a parked step has not finished");
    }

    #[test]
    fn only_a_running_step_takes_a_hand_back() {
        // Pending -> AwaitingApproval is a real edge, so this checks the explicit guard.
        let mut step = pending();
        assert_eq!(
            step.record(&outcome(OutcomeKind::NeedsApproval, None), t(1))
                .unwrap_err(),
            StepStateRefused {
                step: "a".into(),
                from: StepState::Pending,
                to: StepState::AwaitingApproval
            }
        );
        assert_eq!(step.state(), StepState::Pending);
    }

    #[test]
    fn a_finished_step_cannot_be_skipped_restarted_or_failed() {
        let mut step = pending();
        step.start(t(1)).unwrap();
        step.record(&outcome(OutcomeKind::Completed, None), t(2))
            .unwrap();
        let before = step.clone();
        assert!(step.skip(t(3)).is_err());
        assert!(step.start(t(3)).is_err());
        assert!(step.fail("verifier disagreed", t(3)).is_err());
        assert_eq!(step, before, "a refused move changed the step");
    }

    #[test]
    fn only_a_pending_step_is_parked_before_it_runs() {
        let mut step = pending();
        step.await_approval().unwrap();
        assert_eq!(step.state(), StepState::AwaitingApproval);
        step.start(t(1)).unwrap();
        assert_eq!(step.state(), StepState::Running, "approval runs it");
        assert!(step.await_approval().is_err());
        assert_eq!(step.state(), StepState::Running);
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn the_canonical_hand_back_parses_exactly() {
        let parsed = StepOutcome::parse(
            r#"{"outcome": "completed", "summary": "two quotes found", "artifacts": ["a1", "a2"], "failure": null}"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            StepOutcome::new(
                OutcomeKind::Completed,
                "two quotes found",
                vec!["a1".into(), "a2".into()],
                None
            )
        );
        assert_eq!(parsed.artifacts(), ["a1".to_string(), "a2".to_string()]);
    }

    #[test]
    fn a_missing_outcome_is_an_error_never_completed() {
        for text in [
            r#"{"summary": "looks done to me", "artifacts": ["a1"]}"#,
            r#"{"outcome": null, "summary": "done"}"#,
            "{}",
        ] {
            assert_eq!(
                StepOutcome::parse(text).unwrap_err(),
                OutcomeError::MissingOutcome,
                "{text}"
            );
        }
        assert_eq!(
            StepOutcome::parse(r#"{"outcome": "partial", "summary": "half"}"#).unwrap_err(),
            OutcomeError::UnknownOutcome("partial".into())
        );
        assert!(matches!(
            StepOutcome::parse(r#"{"outcome": true}"#),
            Err(OutcomeError::UnknownOutcome(_))
        ));
        assert_eq!(
            StepOutcome::parse("I finished the step.").unwrap_err(),
            OutcomeError::NotAnObject
        );
        assert_eq!(
            StepOutcome::parse("[1, 2]").unwrap_err(),
            OutcomeError::NotAnObject
        );
    }

    #[test]
    fn a_small_models_field_names_and_spellings_are_recovered() {
        let parsed = StepOutcome::parse(
            r#"{"status": "Needs Approval", "result": "ready to book", "artifact_ids": "a1, a2", "error": "booking spends money"}"#,
        )
        .unwrap();
        assert_eq!(parsed.outcome(), OutcomeKind::NeedsApproval);
        assert_eq!(parsed.summary(), "ready to book");
        assert_eq!(parsed.artifacts(), ["a1".to_string(), "a2".to_string()]);
        assert_eq!(parsed.failure(), Some("booking spends money"));

        let failed =
            StepOutcome::parse(r#"{"success": false, "reason": "site was down"}"#).unwrap();
        assert_eq!(failed.outcome(), OutcomeKind::Failed);
        assert_eq!(failed.failure(), Some("site was down"));

        for (word, kind) in [
            ("done", OutcomeKind::Completed),
            ("SUCCESS", OutcomeKind::Completed),
            ("failed", OutcomeKind::Failed),
            ("needs-approval", OutcomeKind::NeedsApproval),
        ] {
            let text = format!(r#"{{"state": "{word}"}}"#);
            assert_eq!(StepOutcome::parse(&text).unwrap().outcome(), kind, "{word}");
        }
    }

    #[test]
    fn a_hand_back_inside_a_fence_or_prose_is_found() {
        let fenced =
            "Here it is:\n```json\n{\"outcome\": \"failed\", \"failure\": \"no results\"}\n```";
        assert_eq!(
            StepOutcome::parse(fenced).unwrap().outcome(),
            OutcomeKind::Failed
        );
    }

    #[test]
    fn artifacts_are_trimmed_deduplicated_and_read_from_objects() {
        let parsed = StepOutcome::parse(
            r#"{"outcome": "done", "artifacts": [" a1 ", "", "a1", {"id": "a2"}, 7]}"#,
        )
        .unwrap();
        assert_eq!(parsed.artifacts(), ["a1".to_string(), "a2".to_string()]);
    }

    #[test]
    fn a_long_summary_is_cut_on_a_char_boundary_and_says_so() {
        let long = "水".repeat(MAX_STEP_SUMMARY_CHARS * 2);
        let outcome = StepOutcome::new(OutcomeKind::Completed, &long, vec![], None);
        let summary = outcome.summary();
        assert!(
            summary.chars().count() <= MAX_STEP_SUMMARY_CHARS,
            "{}",
            summary.chars().count()
        );
        assert!(summary.ends_with(&format!("[cut to {MAX_STEP_SUMMARY_CHARS} characters]")));
        assert!(summary.starts_with(&"水".repeat(100)));

        let failure = "x".repeat(MAX_STEP_SUMMARY_CHARS + 1);
        let failed = StepOutcome::new(OutcomeKind::Failed, "", vec![], Some(&failure));
        assert!(failed.failure().unwrap().chars().count() <= MAX_STEP_SUMMARY_CHARS);

        // Control: a summary under the cap is left whole.
        let short = "a".repeat(MAX_STEP_SUMMARY_CHARS);
        assert_eq!(
            StepOutcome::new(OutcomeKind::Completed, &short, vec![], None).summary(),
            short
        );
    }

    #[test]
    fn a_completed_hand_back_keeps_no_failure() {
        let parsed = StepOutcome::parse(r#"{"outcome": "completed", "failure": "minor"}"#).unwrap();
        assert_eq!(parsed.failure(), None);
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::test_support::*;
    use super::*;

    fn artifact(title: &str, summary: &str, body: &str) -> Result<Artifact, ArtifactError> {
        Artifact::new(
            "task-1",
            Some(StepKey::parse("fetch").unwrap()),
            ArtifactKind::Findings,
            title,
            summary,
            body.to_string(),
            t(0),
        )
    }

    #[test]
    fn tokens_count_chars_not_bytes_and_round_up() {
        assert_eq!(artifact_tokens(""), 0);
        assert_eq!(artifact_tokens("abcd"), 1);
        assert_eq!(artifact_tokens("abcde"), 2);
        // Five 3-byte chars: bytes / 4 would say 4.
        assert_eq!(artifact_tokens(&"水".repeat(5)), 2);
        assert_eq!(artifact("t", "s", "abcdefghi").unwrap().tokens(), 3);
    }

    #[test]
    fn pages_join_back_into_the_whole_body() {
        let body = "Lunch at 水 the café, then 12 km by bus. ".repeat(9);
        let total = artifact_tokens(&body);
        for page in [1, 3, 7, total] {
            let mut joined = String::new();
            let mut from = 0;
            while from < total {
                joined.push_str(artifact_slice(&body, from, from + page));
                from += page;
            }
            assert_eq!(joined, body, "page size {page}");
        }
    }

    #[test]
    fn slices_past_the_end_or_backwards_are_empty() {
        let body = "abcdefgh";
        assert_eq!(artifact_slice(body, 0, 1), "abcd");
        assert_eq!(artifact_slice(body, 1, 100), "efgh");
        assert_eq!(artifact_slice(body, 2, 3), "");
        assert_eq!(artifact_slice(body, 1, 1), "");
        assert_eq!(artifact_slice(body, 1, 0), "");
        assert_eq!(artifact_slice(body, u32::MAX - 1, u32::MAX), "");
    }

    #[test]
    fn title_and_summary_are_one_capped_line() {
        let made = artifact(
            "  Quotes\nfrom three   garages ",
            &format!("first line\n{}", "y".repeat(MAX_ARTIFACT_SUMMARY_CHARS)),
            "body",
        )
        .unwrap();
        assert_eq!(made.title(), "Quotes from three garages");
        assert!(!made.summary().contains('\n'));
        assert!(made.summary().starts_with("first line y"));
        assert!(made.summary().chars().count() <= MAX_ARTIFACT_SUMMARY_CHARS);
        let long_title = artifact(&"t".repeat(MAX_ARTIFACT_TITLE_CHARS * 2), "s", "b").unwrap();
        assert!(long_title.title().chars().count() <= MAX_ARTIFACT_TITLE_CHARS);
    }

    #[test]
    fn an_artifact_needs_a_title_a_summary_and_a_body() {
        assert_eq!(
            artifact(" ", "s", "b").unwrap_err(),
            ArtifactError::MissingTitle
        );
        assert_eq!(
            artifact("t", "\n", "b").unwrap_err(),
            ArtifactError::MissingSummary
        );
        assert_eq!(
            artifact("t", "s", "  ").unwrap_err(),
            ArtifactError::EmptyBody
        );
        let made = artifact("t", "s", "b").unwrap();
        assert!(uuid::Uuid::parse_str(made.id()).is_ok());
        assert_eq!(made.task_id(), "task-1");
        assert_eq!(made.step_key().map(StepKey::as_str), Some("fetch"));
        assert_eq!(made.kind(), ArtifactKind::Findings);
        assert_eq!(made.created_at(), t(0));
    }

    #[test]
    fn a_stored_artifact_recomputes_its_tokens_and_its_view_carries_no_body() {
        let stored = Artifact::from_parts(ArtifactParts {
            id: "art-1".into(),
            task_id: "task-1".into(),
            step_key: None,
            kind: ArtifactKind::Data,
            title: "rows".into(),
            summary: "twelve rows".into(),
            body: "x".repeat(41),
            created_at: t(9),
        })
        .unwrap();
        assert_eq!(stored.tokens(), 11);
        let view = stored.summary_view();
        assert_eq!(
            view,
            ArtifactSummary {
                id: "art-1".into(),
                task_id: "task-1".into(),
                step_key: None,
                kind: ArtifactKind::Data,
                title: "rows".into(),
                summary: "twelve rows".into(),
                tokens: 11,
                created_at: t(9),
            }
        );
        assert!(matches!(
            Artifact::from_parts(ArtifactParts {
                step_key: Some("Bad Key".into()),
                ..ArtifactParts {
                    id: "art-2".into(),
                    task_id: "task-1".into(),
                    step_key: None,
                    kind: ArtifactKind::Note,
                    title: "t".into(),
                    summary: "s".into(),
                    body: "b".into(),
                    created_at: t(0),
                }
            }),
            Err(ArtifactError::InvalidStepKey { .. })
        ));
    }
}
