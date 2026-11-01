//! Long-task MCP servers. `giap-tasks` is the household's: start, check and stop a task.
//! `giap-task-work` is a helper's: its own task's artifacts, plan and report, reached through the
//! run it is bound to, never through a task id the model names.
//!
//! Deps are read on every call: goose may spawn a server before pond-server installs them.

use chrono::Utc;
use pond_core::mcp::domain::tool_group::{TASKS_EXTENSION, TASK_WORK_EXTENSION};
use pond_core::shared::domain::long_task::{
    match_waits_by_name, next_ready_step, read_step_draft, validate_plan, AnswerShape, Artifact,
    ArtifactKind, ArtifactSummary, LongTask, LongTaskBudget, LongTaskState, LongTaskStateRefused,
    OutcomeKind, PlanStep, StalePlan, StepDraft, StepOutcome, StepState, DEFAULT_LONG_TASK_STEPS,
};
use pond_core::shared::domain::orchestration::DelegationAuthority;
use pond_core::shared::ports::long_task::{LongTaskOwnerFilter, LongTaskRepository};
use pond_core::shared::services::child_sessions::{process_child_sessions, ChildSessionRegistry};
use pond_core::shared::services::long_task_open::{open_task, OpeningBrief};
use pond_core::shared::services::long_task_queue::{place_of, run_order, waiting_line};
use pond_core::shared::services::long_task_roles::WORKER_ROLE;
use pond_core::shared::services::long_task_runner::{clip_brief, result_artifact};
use pond_core::shared::services::step_bindings::{
    process_step_bindings, StepBinding, StepBindings, StepWork,
};
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::domain::profile::ProfileScope;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData, Implementation, InitializeResult, Meta,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::io::DuplexStream;
use tokio::sync::Notify;

/// Largest part of an artifact one `artifact_read` returns, leaving most of a helper's window
/// for its own work; a guess, never measured.
pub const ARTIFACT_READ_WINDOW_TOKENS: u32 = 1_500;

/// How much of a goal or a step's instructions a status line repeats.
const GOAL_PREVIEW_CHARS: usize = 120;
const INSTRUCTIONS_PREVIEW_CHARS: usize = 160;

// ── Deps ────────────────────────────────────────────────────────────────────

/// What both servers need; none of it exists when goose registers them.
#[derive(Clone)]
pub struct LongTaskDeps {
    pub repo: Arc<dyn LongTaskRepository>,
    /// The SAME registry the goose adapter publishes each live turn into, or every call refuses.
    pub authorities: Arc<TurnAuthorityRegistry>,
    /// Woken when a task starts or is cancelled, so the runner need not poll for either.
    pub wake: Option<Arc<Notify>>,
}

impl LongTaskDeps {
    pub fn new(
        repo: Arc<dyn LongTaskRepository>,
        authorities: Arc<TurnAuthorityRegistry>,
        wake: Option<Arc<Notify>>,
    ) -> Self {
        Self {
            repo,
            authorities,
            wake,
        }
    }

    fn wake_runner(&self) {
        if let Some(wake) = &self.wake {
            wake.notify_one();
        }
    }
}

static LONG_TASK_DEPS: OnceLock<LongTaskDeps> = OnceLock::new();

/// Install the deps once, after the goose adapter exists; later calls are ignored.
pub fn init_long_task_deps(deps: LongTaskDeps) {
    let _ = LONG_TASK_DEPS.set(deps);
}

pub fn installed_long_task_deps() -> Option<LongTaskDeps> {
    LONG_TASK_DEPS.get().cloned()
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_tasks_server(reader: DuplexStream, writer: DuplexStream) {
    crate::serve_builtin(TASKS_EXTENSION, TasksMcpServer::new(), reader, writer);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_task_work_server(reader: DuplexStream, writer: DuplexStream) {
    crate::serve_builtin(
        TASK_WORK_EXTENSION,
        TaskWorkMcpServer::new(),
        reader,
        writer,
    );
}

// ── Refusals ────────────────────────────────────────────────────────────────

/// Why a long-task call did nothing. Returned as tool SUCCESS text: a small model retries a
/// protocol error verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// The deps were never installed.
    Unavailable {
        trace: &'static str,
    },
    /// No `_meta` or no live top-level turn; one sentence for all, so they look the same.
    NotHere {
        trace: &'static str,
    },
    Guest,
    /// A task-work call that resolves to no bound run; one sentence for every way that happens.
    NotInAStep {
        trace: &'static str,
    },
    Malformed(String),
    /// No task this caller may see. A foreign id reads the same as an unknown one.
    UnknownTask {
        trace: &'static str,
    },
    AlreadyFinished(LongTaskState),
    /// Not in this task, which also covers another task's artifact.
    UnknownArtifact(String),
    UnknownArtifacts(Vec<String>),
    NotThePlanner,
    /// The planner and the checker save nothing of their own; only a step's worker does.
    NotAWorker,
    /// The planner's hand-back is its plan.
    PlannerDoesNotReport,
    /// The run ended between resolving the binding and recording the report.
    ReportTooLate,
    /// A domain refusal, whose text is already written for the model.
    Rejected(String),
    Store(String),
}

impl Refusal {
    /// The model-facing text; each ends with what to do next.
    pub fn message(&self) -> String {
        match self {
            Refusal::Unavailable { .. } => {
                "Long tasks are not available on this pond right now.".to_string()
            }
            Refusal::NotHere { .. } => "Long tasks are not available in this conversation. Do \
                 the work yourself, or tell the user it cannot be started here."
                .to_string(),
            Refusal::Guest => "Long tasks can be started only once this conversation is \
                 identified as a member of the household. Answer directly instead."
                .to_string(),
            Refusal::NotInAStep { .. } => "This tool works only inside a step of a long task, \
                 and this conversation is not one."
                .to_string(),
            Refusal::Malformed(detail) => detail.clone(),
            Refusal::UnknownTask { .. } => "There is no task by that id for you. Call \
                 task_status with no task_id to see your tasks."
                .to_string(),
            Refusal::AlreadyFinished(state) => {
                format!("That task is already {state}, so there is nothing to stop.")
            }
            Refusal::UnknownArtifact(id) => format!(
                "There is no artifact `{id}` in this task. Call artifact_list to see the ids you \
                 can read."
            ),
            Refusal::UnknownArtifacts(ids) => format!(
                "These artifact ids are not in this task: {}. Name only ids artifact_write \
                 returned, or call artifact_list, then report again.",
                quoted(ids)
            ),
            Refusal::NotThePlanner => "Only the planner may change this task's plan. Do your own \
                 step, then call step_report."
                .to_string(),
            Refusal::NotAWorker => "Only a step's worker saves artifacts. If you are planning, \
                 write the steps with plan_write; if you are checking, call step_report."
                .to_string(),
            Refusal::PlannerDoesNotReport => "The planner does not report a step. Write the plan \
                 with plan_write; that is your whole job."
                .to_string(),
            Refusal::ReportTooLate => {
                "This step has already ended, so the report was not kept.".to_string()
            }
            Refusal::Rejected(text) => text.clone(),
            Refusal::Store(detail) => format!(
                "The task's records could not be read or written ({detail}), so nothing was \
                 done. Do not guess at what they hold."
            ),
        }
    }

    /// A stable label for the log line. Never shown to the model.
    pub fn trace(&self) -> &'static str {
        match self {
            Refusal::Unavailable { trace }
            | Refusal::NotHere { trace }
            | Refusal::NotInAStep { trace }
            | Refusal::UnknownTask { trace } => trace,
            Refusal::Guest => "guest",
            Refusal::Malformed(_) => "malformed_request",
            Refusal::AlreadyFinished(_) => "already_finished",
            Refusal::UnknownArtifact(_) => "unknown_artifact",
            Refusal::UnknownArtifacts(_) => "unknown_artifacts",
            Refusal::NotThePlanner => "not_the_planner",
            Refusal::NotAWorker => "not_a_worker",
            Refusal::PlannerDoesNotReport => "planner_does_not_report",
            Refusal::ReportTooLate => "report_too_late",
            Refusal::Rejected(_) => "rejected",
            Refusal::Store(_) => "store_failed",
        }
    }
}

fn store(e: anyhow::Error) -> Refusal {
    Refusal::Store(e.to_string())
}

fn quoted(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Wrap an answer or a refusal as tool success, logging the refusal.
const REFUSAL_TRACE_CHARS: usize = 400;

fn answer(tool: &'static str, outcome: Result<String, Refusal>) -> CallToolResult {
    let text = match outcome {
        Ok(text) => text,
        Err(refusal) => {
            let message = refusal.message();
            // The model's own session is not kept, so this is the only record of what it was told.
            let said: String = message.chars().take(REFUSAL_TRACE_CHARS).collect();
            tracing::warn!(
                target: "giap::trace",
                kind = "long_task_tool_refused",
                tool,
                reason = refusal.trace(),
                message = %said,
                "a long-task tool call was refused"
            );
            message
        }
    };
    CallToolResult::success(vec![ContentBlock::text(text)])
}

// ── Lenient arguments ───────────────────────────────────────────────────────

/// Remove the first present alias holding a non-blank string.
fn take_alias(extra: &mut HashMap<String, Value>, aliases: &[&str]) -> Option<String> {
    for alias in aliases {
        if let Some(value) = extra.get(*alias).and_then(Value::as_str) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                extra.remove(*alias);
                return Some(value);
            }
        }
    }
    None
}

fn non_blank(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// A whole number of tokens from a number or a numeric string; anything else refuses.
fn token_count(value: Option<Value>, field: &str) -> Result<Option<u32>, Refusal> {
    let parsed = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.trim().parse::<u64>().ok(),
        Some(_) => None,
    };
    parsed
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .map(Some)
        .ok_or_else(|| {
            Refusal::Malformed(format!(
                "`{field}` must be a whole number of tokens, 0 or more."
            ))
        })
}

// ── giap-tasks: the household's side ───────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TaskStartParams {
    /// What the finished work should give the person, in one or two sentences.
    pub goal: Option<String>,
    /// Everything from this conversation the helpers need that the goal leaves out: who it is for,
    /// dates and places, budget, diets and allergies, what was decided or ruled out. They see
    /// nothing of the conversation but this and the goal.
    pub brief: Option<String>,
    /// short or report, only when the person said how much they want: short is the answer in a
    /// few sentences, report is the whole of it in sections. Leave it out otherwise.
    pub answer: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

const GOAL_ALIASES: [&str; 5] = ["task", "description", "request", "instructions", "prompt"];

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TaskIdParams {
    /// The task's id, from task_start or task_status.
    pub task_id: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

const TASK_ID_ALIASES: [&str; 4] = ["id", "task", "taskId", "task-id"];

fn task_id_of(mut params: TaskIdParams) -> Option<String> {
    non_blank(params.task_id.take()).or_else(|| take_alias(&mut params.extra, &TASK_ID_ALIASES))
}

/// Whether `scope` reaches `task`, with every scoped read's shape: a member reaches their own
/// tasks and unowned ones, the household all of them, a guest none.
pub fn scope_reaches(scope: &ProfileScope, task: &LongTask) -> bool {
    match scope {
        ProfileScope::Owner(id) => task.owner_profile_id().is_none_or(|owner| owner == id),
        ProfileScope::Household => true,
        ProfileScope::Guest => false,
    }
}

#[derive(Clone)]
pub struct TasksMcpServer {
    /// Tests only; in production the installed deps are read on every call.
    fixed: Option<LongTaskDeps>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

impl Default for TasksMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl TasksMcpServer {
    /// All tools, without constructing the server; the generated `tool_router()` is private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new() -> Self {
        Self {
            fixed: None,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Start a long task: work too big for one reply, such as researching and comparing options or \
going through a long document. The pond plans it and works on it step by step while the house is \
quiet, then tells the person on their phone. `goal` = what the finished work should give the person. Not for \
anything you can answer now.")]
    async fn task_start(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<TaskStartParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("task_start");
        Ok(answer(
            "task_start",
            self.run_start(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
How a long task is going: its state, how many steps are done and, once finished, its result. \
Give `task_id` for one task, or leave it out to list the person's tasks still running.")]
    async fn task_status(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<TaskIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("task_status");
        Ok(answer(
            "task_status",
            self.run_status(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Stop a long task for good, by `task_id`. Only when the person asks to stop or cancel it.")]
    async fn task_cancel(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<TaskIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("task_cancel");
        Ok(answer(
            "task_cancel",
            self.run_cancel(&ctx.meta, params.0).await,
        ))
    }
}

impl TasksMcpServer {
    #[cfg(test)]
    fn with_deps(deps: Option<LongTaskDeps>) -> Self {
        Self {
            fixed: deps,
            tool_router: Self::tool_router(),
        }
    }

    fn deps(&self) -> Option<LongTaskDeps> {
        self.fixed.clone().or_else(installed_long_task_deps)
    }

    /// The calling turn, refused unless it is a live, identified, top-level one.
    fn caller(&self, meta: &Meta) -> Result<(LongTaskDeps, DelegationAuthority), Refusal> {
        let Some(deps) = self.deps() else {
            return Err(Refusal::Unavailable {
                trace: "deps_not_installed",
            });
        };
        let Some(session) = crate::session_from_meta(meta) else {
            return Err(Refusal::NotHere {
                trace: "no_engine_session",
            });
        };
        let Some(authority) = deps.authorities.authority_for_engine_session(&session) else {
            return Err(Refusal::NotHere {
                trace: "no_live_turn",
            });
        };
        // Published authorities are roots; refuse anything else rather than trust that.
        if !authority.depth().is_root() {
            return Err(Refusal::NotHere {
                trace: "not_a_top_level_turn",
            });
        }
        if authority.profile_scope().excludes_everything() {
            return Err(Refusal::Guest);
        }
        Ok((deps, authority))
    }

    async fn run_start(&self, meta: &Meta, mut params: TaskStartParams) -> Result<String, Refusal> {
        let (deps, authority) = self.caller(meta)?;
        let goal = non_blank(params.goal.take())
            .or_else(|| take_alias(&mut params.extra, &GOAL_ALIASES))
            .ok_or_else(|| {
                Refusal::Malformed(
                    "task_start needs a `goal`: what the finished work should give the person, \
                     in one or two sentences."
                        .to_string(),
                )
            })?;
        let budget = LongTaskBudget::new(DEFAULT_LONG_TASK_STEPS, None)
            .map_err(|e| Refusal::Rejected(e.to_string()))?;
        let answer = non_blank(params.answer.take())
            .or_else(|| take_alias(&mut params.extra, &ANSWER_ALIASES))
            .and_then(|raw| AnswerShape::read(&raw))
            .unwrap_or_default();
        let task = LongTask::start(
            authority.profile_scope().clone(),
            None,
            &goal,
            Some(authority.session_id().to_string()),
            budget,
            Utc::now(),
        )
        .map_err(|e| Refusal::Rejected(e.to_string()))?
        .with_answer(answer);
        let brief = params
            .brief
            .as_deref()
            .and_then(clip_brief)
            .map(|body| OpeningBrief {
                summary: "What the conversation said that the goal leaves out",
                body,
            });
        let task = open_task(deps.repo.as_ref(), task, brief)
            .await
            .map_err(store)?;
        deps.wake_runner();
        let open = deps
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, false)
            .await
            .map_err(store)?;
        let place = place_of(&run_order(&open), task.id(), task.owner_profile_id());
        tracing::info!(
            target: "giap::trace",
            kind = "long_task_started",
            task_id = %task.id(),
            origin_session_id = %authority.session_id(),
            max_steps = task.budget().max_steps(),
            position = place.as_ref().map(|p| p.position),
        );
        let waiting = place
            .as_ref()
            .and_then(waiting_line)
            .map(|line| format!(" {line}"))
            .unwrap_or_default();
        Ok(format!(
            "Started task {}. The pond works on it while the household is quiet and tells the \
             person on their phone when it is done. Tell the person it has started; task_status \
             with this id says how far it has got.{waiting}",
            task.id()
        ))
    }

    async fn run_status(&self, meta: &Meta, params: TaskIdParams) -> Result<String, Refusal> {
        let (deps, authority) = self.caller(meta)?;
        status_text(&deps, authority.profile_scope(), task_id_of(params)).await
    }

    async fn run_cancel(&self, meta: &Meta, params: TaskIdParams) -> Result<String, Refusal> {
        let (deps, authority) = self.caller(meta)?;
        let task_id = task_id_of(params).ok_or_else(|| {
            Refusal::Malformed(
                "task_cancel needs the `task_id` of the task to stop. Call task_status to find it."
                    .to_string(),
            )
        })?;
        let task = reached_task(&deps, authority.profile_scope(), &task_id).await?;
        match deps
            .repo
            .set_task_state(task.id(), LongTaskState::Cancelled, Utc::now())
            .await
        {
            Ok(Some(cancelled)) => {
                deps.wake_runner();
                tracing::info!(
                    target: "giap::trace",
                    kind = "long_task_cancelled",
                    task_id = %cancelled.id(),
                );
                Ok(format!(
                    "Cancelled task {}: {}. Nothing more will run for it.",
                    cancelled.id(),
                    clip(cancelled.goal(), GOAL_PREVIEW_CHARS)
                ))
            }
            Ok(None) => Err(Refusal::UnknownTask {
                trace: "deleted_while_cancelling",
            }),
            Err(e) => match e.downcast_ref::<LongTaskStateRefused>() {
                Some(refused) => Err(Refusal::AlreadyFinished(refused.from)),
                None => Err(store(e)),
            },
        }
    }
}

/// One task if this scope reaches it; a foreign id and an unknown one read the same.
/// What `task_status` answers: one task, or every task still running for this person.
async fn status_text(
    deps: &LongTaskDeps,
    scope: &ProfileScope,
    task_id: Option<String>,
) -> Result<String, Refusal> {
    if let Some(task_id) = task_id {
        let task = reached_task(deps, scope, &task_id).await?;
        let steps = deps.repo.list_steps(task.id()).await.map_err(store)?;
        let result = result_of(deps.repo.as_ref(), task.id()).await?;
        return Ok(task_summary_for_notice(&task, &steps, result.as_ref()));
    }
    let tasks = reached_tasks(deps, scope).await?;
    if tasks.is_empty() {
        return Ok(NO_TASKS_RUNNING.to_string());
    }
    let mut lines = vec![format!("Long tasks still running: {}", tasks.len())];
    for task in &tasks {
        let steps = deps.repo.list_steps(task.id()).await.map_err(store)?;
        lines.push(task_line(task, &steps));
    }
    Ok(lines.join("\n"))
}

const NO_TASKS_RUNNING: &str = "There are no long tasks running for this person.";

/// `task_status`'s answer for a call that reached `check_task` instead: the prime often asks it
/// about a task `task_start` began. `None` when the id is no long task of theirs, or none runs.
pub(crate) async fn long_task_status_for(
    scope: &ProfileScope,
    task_id: Option<&str>,
) -> Option<String> {
    long_task_status_with(&installed_long_task_deps()?, scope, task_id).await
}

async fn long_task_status_with(
    deps: &LongTaskDeps,
    scope: &ProfileScope,
    task_id: Option<&str>,
) -> Option<String> {
    status_text(deps, scope, task_id.map(str::to_string))
        .await
        .ok()
        .filter(|text| text != NO_TASKS_RUNNING)
}

async fn reached_task(
    deps: &LongTaskDeps,
    scope: &ProfileScope,
    task_id: &str,
) -> Result<LongTask, Refusal> {
    let Some(task) = deps.repo.get_task(task_id).await.map_err(store)? else {
        return Err(Refusal::UnknownTask {
            trace: "no_such_task",
        });
    };
    if !scope_reaches(scope, &task) {
        return Err(Refusal::UnknownTask {
            trace: "task_of_another_member",
        });
    }
    Ok(task)
}

/// Open tasks this scope reaches, newest first.
async fn reached_tasks(
    deps: &LongTaskDeps,
    scope: &ProfileScope,
) -> Result<Vec<LongTask>, Refusal> {
    let mut tasks = match scope {
        ProfileScope::Owner(id) => {
            let mut tasks = deps
                .repo
                .list_tasks(LongTaskOwnerFilter::Member(id), false)
                .await
                .map_err(store)?;
            tasks.extend(
                deps.repo
                    .list_tasks(LongTaskOwnerFilter::Unowned, false)
                    .await
                    .map_err(store)?,
            );
            tasks
        }
        ProfileScope::Household => deps
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, false)
            .await
            .map_err(store)?,
        ProfileScope::Guest => Vec::new(),
    };
    tasks.sort_by_key(|task| std::cmp::Reverse(task.created_at()));
    Ok(tasks)
}

/// The task's answer, by the runner's own rule, so status and the runner never disagree.
async fn result_of(
    repo: &dyn LongTaskRepository,
    task_id: &str,
) -> Result<Option<ArtifactSummary>, Refusal> {
    let artifacts = repo.list_artifacts(task_id).await.map_err(store)?;
    Ok(result_artifact(&artifacts).cloned())
}

/// What the household is told about one task: state, progress, the step under way, and the
/// result once there is one. The completion notice uses it too, so the two never disagree.
pub fn task_summary_for_notice(
    task: &LongTask,
    steps: &[PlanStep],
    result: Option<&ArtifactSummary>,
) -> String {
    let mut lines = vec![
        format!(
            "Task {}: {}",
            task.id(),
            clip(task.goal(), GOAL_PREVIEW_CHARS)
        ),
        format!("{}. {}", state_phrase(task.state()), progress(steps)),
    ];
    if !task.state().is_terminal() {
        if let Some(step) = current_step(task, steps) {
            lines.push(format!(
                "Now on step `{}`: {}",
                step.key(),
                clip(step.instructions(), INSTRUCTIONS_PREVIEW_CHARS)
            ));
        }
    }
    if task.state() == LongTaskState::Failed {
        if let Some(step) = steps
            .iter()
            .rev()
            .find(|step| step.state() == StepState::Failed)
        {
            lines.push(format!(
                "Step `{}` failed: {}",
                step.key(),
                step.failure().unwrap_or("no reason was given")
            ));
        }
    }
    match result {
        Some(result) => lines.push(format!(
            "Result (artifact {}): {}",
            result.id, result.summary
        )),
        None if task.state() == LongTaskState::Completed => {
            lines.push("It finished without saving a result.".to_string())
        }
        None => {}
    }
    lines.join("\n")
}

/// One line per task in a listing.
fn task_line(task: &LongTask, steps: &[PlanStep]) -> String {
    format!(
        "- {}: {} ({}; {})",
        task.id(),
        clip(task.goal(), GOAL_PREVIEW_CHARS),
        state_phrase(task.state()).to_lowercase(),
        progress(steps).trim_end_matches('.').to_lowercase()
    )
}

fn state_phrase(state: LongTaskState) -> &'static str {
    match state {
        LongTaskState::Planning => "Planning its steps",
        LongTaskState::Running => "Working on it",
        LongTaskState::AwaitingApproval => "Waiting for a decision",
        LongTaskState::Paused => "Paused",
        LongTaskState::Completed => "Finished",
        LongTaskState::Failed => "Failed",
        LongTaskState::Cancelled => "Cancelled",
    }
}

fn progress(steps: &[PlanStep]) -> String {
    if steps.is_empty() {
        return "No plan yet.".to_string();
    }
    let done = steps
        .iter()
        .filter(|step| step.state() == StepState::Completed)
        .count();
    let skipped = steps
        .iter()
        .filter(|step| step.state() == StepState::Skipped)
        .count();
    if skipped == 0 {
        format!("{done} of {} steps done.", steps.len())
    } else {
        format!("{done} of {} steps done, {skipped} skipped.", steps.len())
    }
}

/// The step that is running, else the one waiting on a decision, else the next to run.
fn current_step<'a>(task: &LongTask, steps: &'a [PlanStep]) -> Option<&'a PlanStep> {
    steps
        .iter()
        .find(|step| step.state() == StepState::Running)
        .or_else(|| {
            (task.state() == LongTaskState::AwaitingApproval)
                .then(|| {
                    steps
                        .iter()
                        .find(|step| step.state() == StepState::AwaitingApproval)
                })
                .flatten()
        })
        .or_else(|| next_ready_step(steps))
}

/// At most `max` chars on a char boundary, marked when cut.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

#[tool_handler]
impl ServerHandler for TasksMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                TASKS_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP long tasks: work too big for one reply. A task is planned and worked \
                 through step by step while the household is quiet, then reported back in the \
                 conversation that started it. Start one only when the person asks for that \
                 kind of work.",
            )
    }
}

// ── giap-task-work: a helper's side ────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ArtifactReadParams {
    /// The artifact's id, from the brief or artifact_list.
    pub id: Option<String>,
    /// Where to start, in tokens. Default 0.
    #[schemars(with = "Option<u32>")]
    pub from_token: Option<Value>,
    /// Where to stop, in tokens. Default and limit: 1500 tokens after from_token.
    #[schemars(with = "Option<u32>")]
    pub to_token: Option<Value>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

const ARTIFACT_ID_ALIASES: [&str; 2] = ["artifact_id", "artifact"];
const FROM_ALIASES: [&str; 3] = ["from", "start", "offset"];
const TO_ALIASES: [&str; 2] = ["to", "end"];

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ArtifactWriteParams {
    /// One of: note, findings, draft, data. Default note.
    pub kind: Option<String>,
    /// A few words saying what it is.
    pub title: Option<String>,
    /// One sentence saying what it holds, so later steps can tell whether to read it.
    pub summary: Option<String>,
    /// The content itself.
    #[schemars(with = "Option<String>")]
    pub body: Option<Value>,
    /// true on your step's last save when the step is done: it also reports the step completed,
    /// so you need no step_report.
    #[schemars(with = "Option<bool>")]
    pub finish: Option<Value>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

const TITLE_ALIASES: [&str; 1] = ["name"];
const ANSWER_ALIASES: [&str; 3] = ["answer_shape", "length", "shape"];
/// The most steps a short answer's plan may have: one to find it, a second for another place.
const MAX_SHORT_STEPS: usize = 2;
const FINISH_ALIASES: [&str; 3] = ["done", "final", "last"];

/// A yes from a bool or the words a model writes for one; anything else is no.
fn says_yes(value: Option<Value>) -> bool {
    match value {
        Some(Value::Bool(yes)) => yes,
        Some(Value::String(text)) => {
            matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "yes")
        }
        _ => false,
    }
}
const SUMMARY_ALIASES: [&str; 2] = ["description", "one_line"];
const BODY_ALIASES: [&str; 3] = ["content", "text", "contents"];

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct StepReportParams {
    /// completed or failed.
    #[schemars(with = "Option<String>")]
    pub outcome: Option<Value>,
    /// What you did or found, in a few sentences.
    #[schemars(with = "Option<String>")]
    pub summary: Option<Value>,
    /// Why, if it failed.
    #[schemars(with = "Option<String>")]
    pub failure: Option<Value>,
    /// Ids of the artifacts you saved.
    #[schemars(with = "Option<Vec<String>>")]
    pub artifacts: Option<Value>,
    /// Only if your instructions name agents you may hand work to: which one should go next.
    #[schemars(with = "Option<String>")]
    pub hand_to: Option<Value>,
    /// What that agent should do, when you name one.
    #[schemars(with = "Option<String>")]
    pub brief: Option<Value>,
    /// Only if your instructions allow it: a goal for a new long task that follows from this one.
    #[schemars(with = "Option<String>")]
    pub start_task: Option<Value>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanWriteParams {
    /// Every step still to do, in order.
    #[schemars(with = "Option<Vec<PlanStepSchema>>")]
    pub steps: Option<Value>,
    /// short or report: how long the person wants the answer, when the brief asks you to decide.
    #[schemars(with = "Option<String>")]
    pub answer: Option<Value>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

/// The shape `plan_write` advertises for one step. Only its schema is used: each step is parsed
/// as a `StepDraft`, one at a time, so one bad step is a message rather than a protocol error.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct PlanStepSchema {
    /// Short name, unique in the task: lowercase letters, digits and dashes.
    key: String,
    /// The saved role that does this step.
    role: String,
    /// What this step must do, complete on its own.
    instructions: String,
    /// Ids of artifacts this step needs.
    inputs: Option<Vec<String>>,
    /// Keys of steps that must finish first.
    depends_on: Option<Vec<String>>,
    /// One sentence a checker can verify.
    acceptance: Option<String>,
}

/// A helper call resolved to its run: the root session and what that run is bound to.
struct Helper {
    deps: LongTaskDeps,
    root: String,
    binding: StepBinding,
}

#[derive(Clone)]
pub struct TaskWorkMcpServer {
    /// Tests only; in production the installed deps are read on every call.
    fixed: Option<LongTaskDeps>,
    bindings: Arc<StepBindings>,
    children: Arc<ChildSessionRegistry>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

impl Default for TaskWorkMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl TaskWorkMcpServer {
    /// All tools, without constructing the server; the generated `tool_router()` is private.
    #[cfg(test)]
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    /// Bound to the process's registries, which the orchestrator and the runner fill.
    pub fn new() -> Self {
        Self {
            fixed: None,
            bindings: process_step_bindings(),
            children: process_child_sessions(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
List the artifacts saved in this task: id, kind, size in tokens, title and a one-line summary. \
Read one with artifact_read.")]
    async fn artifact_list(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("artifact_list");
        Ok(answer("artifact_list", self.run_list(&ctx.meta).await))
    }

    #[tool(description = "\
Read part of an artifact by `id`: up to 1500 tokens from `from_token` (default 0). It says how \
many tokens remain, so read a long one part by part.")]
    async fn artifact_read(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ArtifactReadParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("artifact_read");
        Ok(answer(
            "artifact_read",
            self.run_read(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Save what this step found, for later steps to read: `kind` (note, findings, draft or data), a \
short `title`, a one-line `summary` and the `body`. Returns the artifact's id. On the step's last \
save, set `finish` to true to report the step completed in the same call.")]
    async fn artifact_write(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ArtifactWriteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("artifact_write");
        Ok(answer(
            "artifact_write",
            self.run_write(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Finish your step, once, last. `outcome` = completed or failed; `summary` = what you did or \
found, in a few sentences; `failure` = why, if it failed; `artifacts` = ids of what you saved.")]
    async fn step_report(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<StepReportParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("step_report");
        Ok(answer(
            "step_report",
            self.run_report(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Write this task's plan: every step still to do, in order. Each step names a listed `role`, or \
carries a `new_agent` (name, instructions, tools) when none is for it. Give `answer`, short or \
report, when the brief asks you to decide it. Steps already started or finished stay as they are. \
If it is refused, fix what the message names and call it again.")]
    async fn plan_write(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<PlanWriteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("plan_write");
        Ok(answer(
            "plan_write",
            self.run_plan(&ctx.meta, params.0).await,
        ))
    }
}

impl TaskWorkMcpServer {
    #[cfg(test)]
    fn with_parts(
        deps: Option<LongTaskDeps>,
        bindings: Arc<StepBindings>,
        children: Arc<ChildSessionRegistry>,
    ) -> Self {
        Self {
            fixed: deps,
            bindings,
            children,
            tool_router: Self::tool_router(),
        }
    }

    fn deps(&self) -> Option<LongTaskDeps> {
        self.fixed.clone().or_else(installed_long_task_deps)
    }

    /// The calling helper's run. The task comes from here and never from the arguments.
    fn helper(&self, meta: &Meta) -> Result<Helper, Refusal> {
        let Some(deps) = self.deps() else {
            return Err(Refusal::Unavailable {
                trace: "deps_not_installed",
            });
        };
        let Some(session) = crate::session_from_meta(meta) else {
            return Err(Refusal::NotInAStep {
                trace: "no_engine_session",
            });
        };
        // `binding_for_helper` in two halves, since `step_report` needs the root as well.
        let Some(root) = self.children.parent_of(&session) else {
            return Err(Refusal::NotInAStep {
                trace: "not_a_live_child",
            });
        };
        let Some(binding) = self.bindings.binding_for(&root) else {
            return Err(Refusal::NotInAStep {
                trace: "run_not_bound",
            });
        };
        Ok(Helper {
            deps,
            root,
            binding,
        })
    }

    async fn run_list(&self, meta: &Meta) -> Result<String, Refusal> {
        let helper = self.helper(meta)?;
        let artifacts = helper
            .deps
            .repo
            .list_artifacts(&helper.binding.task_id)
            .await
            .map_err(store)?;
        if artifacts.is_empty() {
            return Ok("This task has no saved artifacts yet.".to_string());
        }
        let mut lines = vec!["Artifacts in this task:".to_string()];
        for artifact in &artifacts {
            let from = artifact
                .step_key
                .as_ref()
                .map(|key| format!(", from step `{key}`"))
                .unwrap_or_default();
            lines.push(format!(
                "- `{}` ({}, {} tokens{from}) {}: {}",
                artifact.id,
                artifact.kind.as_str(),
                artifact.tokens,
                artifact.title,
                artifact.summary
            ));
        }
        Ok(lines.join("\n"))
    }

    async fn run_read(
        &self,
        meta: &Meta,
        mut params: ArtifactReadParams,
    ) -> Result<String, Refusal> {
        let helper = self.helper(meta)?;
        let id = non_blank(params.id.take())
            .or_else(|| take_alias(&mut params.extra, &ARTIFACT_ID_ALIASES))
            .ok_or_else(|| {
                Refusal::Malformed(
                    "artifact_read needs the `id` of an artifact. Call artifact_list to see them."
                        .to_string(),
                )
            })?;
        let from_raw = params
            .from_token
            .take()
            .or_else(|| take_value(&mut params.extra, &FROM_ALIASES));
        let to_raw = params
            .to_token
            .take()
            .or_else(|| take_value(&mut params.extra, &TO_ALIASES));
        let from = token_count(from_raw, "from_token")?.unwrap_or(0);
        let asked_to = token_count(to_raw, "to_token")?;

        let Some(artifact) = helper
            .deps
            .repo
            .get_artifact(&helper.binding.task_id, &id)
            .await
            .map_err(store)?
        else {
            return Err(Refusal::UnknownArtifact(id));
        };
        Ok(read_window(&artifact, from, asked_to))
    }

    async fn run_write(
        &self,
        meta: &Meta,
        mut params: ArtifactWriteParams,
    ) -> Result<String, Refusal> {
        let helper = self.helper(meta)?;
        if !matches!(helper.binding.work, StepWork::Do(_)) {
            return Err(Refusal::NotAWorker);
        }
        let kind = match non_blank(params.kind.take()) {
            None => ArtifactKind::Note,
            Some(raw) => ArtifactKind::parse(&raw).ok_or_else(|| {
                Refusal::Malformed(format!(
                    "`{raw}` is not a kind of artifact. Use note, findings, draft or data."
                ))
            })?,
        };
        let title = non_blank(params.title.take())
            .or_else(|| take_alias(&mut params.extra, &TITLE_ALIASES))
            .unwrap_or_default();
        let summary = non_blank(params.summary.take())
            .or_else(|| take_alias(&mut params.extra, &SUMMARY_ALIASES))
            .unwrap_or_default();
        let body = params
            .body
            .take()
            .or_else(|| take_value(&mut params.extra, &BODY_ALIASES))
            .map(|value| match value {
                Value::String(text) => text,
                other => other.to_string(),
            })
            .map(|text| absolute_links(&text))
            .unwrap_or_default();
        let finish = says_yes(
            params
                .finish
                .take()
                .or_else(|| take_value(&mut params.extra, &FINISH_ALIASES)),
        );

        let artifact = Artifact::new(
            &helper.binding.task_id,
            helper.binding.work.step_key().cloned(),
            kind,
            &title,
            &summary,
            body,
            Utc::now(),
        )
        .map_err(|e| Refusal::Rejected(e.to_string()))?;
        helper
            .deps
            .repo
            .insert_artifact(&artifact)
            .await
            .map_err(store)?;
        self.bindings.note_artifact(&helper.root, artifact.id());
        if !finish {
            return Ok(format!(
                "Saved artifact `{}` ({} tokens). Name it in step_report's artifacts so later \
                 steps can find it, or save your last artifact with finish true.",
                artifact.id(),
                artifact.tokens()
            ));
        }
        let said = if summary.is_empty() { &title } else { &summary };
        let outcome = StepOutcome::new(
            OutcomeKind::Completed,
            said,
            self.bindings.written_by(&helper.root),
            None,
        );
        if !self.bindings.report(&helper.root, outcome) {
            return Err(Refusal::ReportTooLate);
        }
        Ok(format!(
            "Saved artifact `{}` and recorded the step as completed. Your step is done; stop here.",
            artifact.id()
        ))
    }

    async fn run_report(&self, meta: &Meta, params: StepReportParams) -> Result<String, Refusal> {
        let helper = self.helper(meta)?;
        if helper.binding.work == StepWork::Plan {
            return Err(Refusal::PlannerDoesNotReport);
        }
        let mut outcome = StepOutcome::from_json(&report_object(params))
            .map_err(|e| Refusal::Rejected(e.to_string()))?;
        if !outcome.artifacts().is_empty() {
            let held = helper
                .deps
                .repo
                .list_artifacts(&helper.binding.task_id)
                .await
                .map_err(store)?;
            let written = self.bindings.written_by(&helper.root);
            // Reported before saving anything: say what to do, not only which names were wrong.
            let named = name_artifacts(outcome.artifacts(), &held, &written).map_err(|_| {
                Refusal::Rejected(
                    "You have saved nothing yet, so there is no artifact to name. Save your work \
                     with artifact_write and finish true: that also reports the step."
                        .to_string(),
                )
            })?;
            outcome = outcome.with_artifacts(named);
        }
        let said = outcome_phrase(outcome.outcome());
        if !self.bindings.report(&helper.root, outcome) {
            return Err(Refusal::ReportTooLate);
        }
        Ok(format!(
            "Recorded: the step {said}. Your step is done; stop here."
        ))
    }

    async fn run_plan(&self, meta: &Meta, mut params: PlanWriteParams) -> Result<String, Refusal> {
        let helper = self.helper(meta)?;
        if helper.binding.work != StepWork::Plan {
            return Err(Refusal::NotThePlanner);
        }
        let raw = params
            .steps
            .take()
            .or_else(|| take_value(&mut params.extra, &["plan"]));
        let (drafts, ignored) = plan_drafts(raw)?;
        let answer = params
            .answer
            .take()
            .or_else(|| take_value(&mut params.extra, &ANSWER_ALIASES))
            .and_then(|value| value.as_str().and_then(AnswerShape::read));

        let repo = &helper.deps.repo;
        let Some(task) = repo
            .get_task(&helper.binding.task_id)
            .await
            .map_err(store)?
        else {
            return Err(Refusal::Rejected(
                "This task no longer exists, so there is nothing to plan. Stop here.".to_string(),
            ));
        };
        let current = repo.list_steps(task.id()).await.map_err(store)?;
        // Told to plan one step for a short answer, the planner still wrote three, and the answer
        // took minutes more; one refusal costs less.
        let shape = match task.answer() {
            AnswerShape::Auto => answer.unwrap_or(AnswerShape::Auto),
            chosen => chosen,
        };
        if shape == AnswerShape::Short && drafts.len() > MAX_SHORT_STEPS {
            return Err(Refusal::Rejected(format!(
                "This is a short answer, and you planned {} steps. Plan one step that finds the \
                 answer and saves it as the Result, or two when it needs two different places \
                 looked in, then call plan_write again.",
                drafts.len()
            )));
        }
        let plan =
            validate_plan(&task, drafts, &current).map_err(|e| Refusal::Rejected(e.to_string()))?;
        // Before the steps, so the first worker is briefed for the answer the planner chose.
        if let Some(answer) = answer {
            repo.decide_task_answer(task.id(), answer)
                .await
                .map_err(store)?;
        }
        if let Err(e) = repo.replace_open_steps(&plan).await {
            return Err(match e.downcast_ref::<StalePlan>() {
                Some(stale) => Refusal::Rejected(stale.to_string()),
                None => store(e),
            });
        }

        let new: Vec<String> = plan
            .steps()
            .iter()
            .map(|step| step.key().to_string())
            .collect();
        let kept: Vec<String> = plan.kept_keys().iter().map(|key| key.to_string()).collect();
        let mut text = if new.is_empty() {
            "Plan saved, with no new steps.".to_string()
        } else {
            format!("Plan saved. Steps to do, in order: {}.", quoted(&new))
        };
        if !kept.is_empty() {
            text.push_str(&format!(
                " Kept as they are, already started or finished: {}.",
                quoted(&kept)
            ));
        }
        for wait in plan.relaxed_waits() {
            text.push_str(&format!(
                " `{}` waited for `{}`, which is not a step here, so it waits for every step \
                 before it.",
                wait.step, wait.missing
            ));
        }
        if !ignored.is_empty() {
            text.push_str(&format!(
                " Ignored, as no step reads them: {}.",
                quoted(&ignored)
            ));
            if ignored.iter().any(|name| name.contains("tool")) {
                text.push_str(
                    " A step's tools come from its role; give a step other tools with `new_agent`.",
                );
            }
        }
        Ok(text)
    }
}

/// Remove the first present, non-null alias, whatever its type.
fn take_value(extra: &mut HashMap<String, Value>, aliases: &[&str]) -> Option<Value> {
    for alias in aliases {
        if extra.get(*alias).is_some_and(|value| !value.is_null()) {
            return extra.remove(*alias);
        }
    }
    None
}

/// The report as one JSON object, so `StepOutcome::from_json` applies its aliases to every key.
fn report_object(params: StepReportParams) -> Value {
    let StepReportParams {
        outcome,
        summary,
        failure,
        artifacts,
        hand_to,
        brief,
        start_task,
        extra,
    } = params;
    let mut object: serde_json::Map<String, Value> = extra.into_iter().collect();
    for (key, value) in [
        ("outcome", outcome),
        ("summary", summary),
        ("failure", failure),
        ("artifacts", artifacts),
        ("hand_to", hand_to),
        ("brief", brief),
        ("start_task", start_task),
    ] {
        if let Some(value) = value {
            object.insert(key.to_string(), value);
        }
    }
    Value::Object(object)
}

fn outcome_phrase(outcome: OutcomeKind) -> &'static str {
    match outcome {
        OutcomeKind::Completed => "completed",
        OutcomeKind::Failed => "failed",
        OutcomeKind::NeedsApproval => "needs a person's approval",
    }
}

/// A Markdown link whose target is a bare web address, `[Guide](rome2rio.com/x)`, with `https://`
/// put in front: the model shortens the address the search gave, and the card would open it as a
/// path on the pond.
fn absolute_links(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 16);
    let mut rest = body;
    while let Some(at) = rest.find("](") {
        let start = at + 2;
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(')').unwrap_or(rest.len());
        let target = &rest[..end];
        // `rome2rio.com, 2023-04-14`: the address, then words the model added to it.
        let (address, extra) = match target.find([',', ' ']) {
            Some(at) => (&target[..at], target[at..].trim_start_matches([',', ' '])),
            None => (target, ""),
        };
        let host = address.split('/').next().unwrap_or("");
        let bare = !address.contains("://")
            && !address.starts_with(['/', '#', '.'])
            && !address.starts_with("mailto:")
            && host.contains('.')
            && host
                .rsplit('.')
                .next()
                .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()));
        rest = &rest[end..];
        if !bare {
            out.push_str(target);
            continue;
        }
        out.push_str("https://");
        out.push_str(address);
        if !extra.is_empty() && rest.starts_with(')') {
            out.push_str(&format!(") ({extra}"));
        }
    }
    out.push_str(rest);
    out
}

/// A report's artifacts as ids. A model names one by its title, or by a placeholder it made up;
/// refusing that cost a whole try, so a title is read as its artifact, and anything else as what
/// this run saved. Only a run that saved nothing is refused, with the names it got wrong.
fn name_artifacts(
    asked: &[String],
    held: &[ArtifactSummary],
    written: &[String],
) -> Result<Vec<String>, Vec<String>> {
    let mut ids = Vec::with_capacity(asked.len());
    let mut unknown = Vec::new();
    for raw in asked {
        let wanted = raw.trim();
        let found = held
            .iter()
            .find(|a| a.id == wanted)
            .or_else(|| {
                held.iter()
                    .filter(|a| a.title.trim().eq_ignore_ascii_case(wanted))
                    .max_by_key(|a| a.created_at)
            })
            .map(|a| a.id.clone());
        match found {
            Some(id) if !ids.contains(&id) => ids.push(id),
            Some(_) => {}
            None => unknown.push(raw.clone()),
        }
    }
    if unknown.is_empty() {
        return Ok(ids);
    }
    if written.is_empty() {
        return Err(unknown);
    }
    for id in written {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    Ok(ids)
}

/// Each step parsed on its own, so every unreadable step is named in one refusal. Also returns the
/// fields no step reads, which are ignored rather than refused.
fn plan_drafts(raw: Option<Value>) -> Result<(Vec<StepDraft>, Vec<String>), Refusal> {
    let shape = "plan_write needs `steps`: a list of steps, each with `key`, `role` (or \
                 `new_agent`) and `instructions`.";
    let Some(mut value) = raw else {
        return Err(Refusal::Malformed(shape.to_string()));
    };
    // A small model sometimes sends the list as a JSON string.
    if let Value::String(text) = &value {
        value = serde_json::from_str(text).map_err(|_| Refusal::Malformed(shape.to_string()))?;
    }
    let items = match value {
        Value::Array(items) => items,
        object @ Value::Object(_) => vec![object],
        _ => return Err(Refusal::Malformed(shape.to_string())),
    };
    let mut read = Vec::with_capacity(items.len());
    let mut unreadable = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        let had: Vec<String> = item
            .as_object()
            .map(|fields| fields.keys().cloned().collect())
            .unwrap_or_default();
        match read_step_draft(item, i + 1) {
            Ok(step) => {
                for name in &step.ignored {
                    if !ignored.contains(name) {
                        ignored.push(name.clone());
                    }
                }
                read.push(step);
            }
            Err(e) if had.is_empty() => {
                unreadable.push(format!("step {} cannot be read: {e}", i + 1))
            }
            Err(e) => unreadable.push(format!(
                "step {} cannot be read: {e} (it has {})",
                i + 1,
                quoted(&had)
            )),
        }
    }
    if !unreadable.is_empty() {
        let fine = if read.is_empty() {
            String::new()
        } else {
            " The other steps are fine; send them again unchanged with these fixed.".to_string()
        };
        return Err(Refusal::Malformed(format!(
            "{}. Each step takes `key`, `role` or `new_agent`, `instructions`, and optionally \
             `inputs`, `depends_on` and `acceptance`.{fine}",
            capitalised(&unreadable.join("; "))
        )));
    }
    match_waits_by_name(&mut read);
    let drafts = read
        .into_iter()
        .map(|step| {
            let mut draft = step.draft;
            // A step that names neither a role nor an agent is the general hand's.
            if draft.role.trim().is_empty() && draft.new_agent.is_none() {
                draft.role = WORKER_ROLE.to_string();
            }
            draft
        })
        .collect();
    Ok((drafts, ignored))
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The part of `artifact` to return and how to ask for the next one.
fn read_window(artifact: &Artifact, from: u32, asked_to: Option<u32>) -> String {
    let total = artifact.tokens();
    if from >= total {
        return format!(
            "Artifact `{}` has {total} tokens, so nothing starts at token {from}. Read from \
             from_token 0.",
            artifact.id()
        );
    }
    let limit = from.saturating_add(ARTIFACT_READ_WINDOW_TOKENS);
    let to = asked_to.unwrap_or(limit).min(limit).min(total);
    if to <= from {
        return format!(
            "`to_token` must be after `from_token` ({from}). Leave it out to read the next \
             {ARTIFACT_READ_WINDOW_TOKENS} tokens."
        );
    }
    let text = artifact.slice(from, to);
    let remaining = total - to;
    let tail = if remaining == 0 {
        "That is the end of it.".to_string()
    } else {
        format!(
            "{remaining} tokens remain. For the next part, call artifact_read with from_token \
             {to}."
        )
    };
    format!(
        "Artifact `{}`, \"{}\": tokens {from} to {to} of {total}.\n\n{text}\n\n{tail}",
        artifact.id(),
        artifact.title()
    )
}

#[tool_handler]
impl ServerHandler for TaskWorkMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                TASK_WORK_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP task work: the tools for one step of a long task. They always act on the \
                 task this step belongs to. Read only what the step needs, save what later steps \
                 need, and finish with step_report.",
            )
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::shared::domain::long_task::StepKey;
    use pond_core::shared::mocks::mock_long_task::MockLongTaskRepository;
    use pond_core::shared::services::child_sessions::ChildSessionLease;
    use pond_core::shared::services::long_task_roles::PLANNER_ROLE_RECIPE;
    use pond_core::shared::services::long_task_runner::RESULT_TITLE;
    use pond_core::shared::services::step_bindings::StepBindingLease;
    use pond_core::shared::services::turn_authority::TurnAuthorityLease;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn meta_for(engine_session: &str) -> Meta {
        let mut m = Meta::new();
        m.0.insert(
            crate::SESSION_ID_META_KEY.to_string(),
            Value::String(engine_session.to_string()),
        );
        m
    }

    fn params<T: serde::de::DeserializeOwned>(json: Value) -> T {
        serde_json::from_value(json).expect("the params accept any object")
    }

    /// One pond's worth of fresh registries; nothing here touches the process globals.
    struct Pond {
        repo: Arc<MockLongTaskRepository>,
        authorities: Arc<TurnAuthorityRegistry>,
        wake: Arc<Notify>,
        bindings: Arc<StepBindings>,
        children: Arc<ChildSessionRegistry>,
    }

    impl Pond {
        fn new() -> Self {
            Self {
                repo: Arc::new(MockLongTaskRepository::new()),
                authorities: Arc::new(TurnAuthorityRegistry::new()),
                wake: Arc::new(Notify::new()),
                bindings: Arc::new(StepBindings::new()),
                children: Arc::new(ChildSessionRegistry::new()),
            }
        }

        fn deps(&self) -> LongTaskDeps {
            LongTaskDeps::new(
                self.repo.clone(),
                self.authorities.clone(),
                Some(self.wake.clone()),
            )
        }

        fn prime(&self) -> TasksMcpServer {
            TasksMcpServer::with_deps(Some(self.deps()))
        }

        fn worker(&self) -> TaskWorkMcpServer {
            TaskWorkMcpServer::with_parts(
                Some(self.deps()),
                self.bindings.clone(),
                self.children.clone(),
            )
        }

        /// A live turn; hold the lease, or the authority is gone and every check passes vacuously.
        fn turn(
            &self,
            engine: &str,
            giap_session: &str,
            scope: ProfileScope,
        ) -> TurnAuthorityLease {
            self.authorities.publish(
                engine,
                DelegationAuthority::root(giap_session, scope, BTreeSet::new()),
                Default::default(),
            )
        }

        /// A helper `engine` working for run `root` on `task_id`.
        fn helper(
            &self,
            engine: &str,
            root: &str,
            task_id: &str,
            work: StepWork,
        ) -> (StepBindingLease, ChildSessionLease) {
            let binding = self.bindings.bind(
                root,
                StepBinding {
                    task_id: task_id.to_string(),
                    scope: ProfileScope::Household,
                    work,
                },
            );
            (binding, self.children.bind(engine, root))
        }

        async fn task(&self, scope: ProfileScope, goal: &str) -> LongTask {
            let task = LongTask::start(
                scope,
                None,
                goal,
                None,
                LongTaskBudget::new(6, None).unwrap(),
                Utc::now(),
            )
            .unwrap();
            self.repo.create_task(&task).await.unwrap();
            task
        }

        async fn plan(&self, task: &LongTask, keys: &[&str]) {
            let drafts = keys
                .iter()
                .map(|key| StepDraft {
                    key: key.to_string(),
                    role: "long-task-worker".to_string(),
                    instructions: format!("do {key}"),
                    ..StepDraft::default()
                })
                .collect();
            let current = self.repo.list_steps(task.id()).await.unwrap();
            let plan = validate_plan(task, drafts, &current).unwrap();
            self.repo.replace_open_steps(&plan).await.unwrap();
        }

        async fn woken(&self) -> bool {
            tokio::time::timeout(Duration::from_millis(50), self.wake.notified())
                .await
                .is_ok()
        }
    }

    fn work(key: &str) -> StepWork {
        StepWork::Do(StepKey::parse(key).unwrap())
    }

    // ── giap-tasks ─────────────────────────────────────────────────────────

    /// Asserted on the strings: the property is what the model can tell apart.
    #[tokio::test]
    async fn every_way_of_not_being_a_live_turn_reads_the_same() {
        let pond = Pond::new();
        let prime = pond.prime();
        let goal = || params::<TaskStartParams>(serde_json::json!({"goal": "compare plumbers"}));

        let no_meta = prime.run_start(&Meta::new(), goal()).await.unwrap_err();
        let no_turn = prime
            .run_start(&meta_for("engine-1"), goal())
            .await
            .unwrap_err();
        assert_eq!(no_meta.message(), no_turn.message());
        assert_ne!(no_meta.trace(), no_turn.trace());

        assert!(
            installed_long_task_deps().is_none(),
            "a test installed the process deps, so the next check reads them"
        );
        let unavailable = TasksMcpServer::with_deps(None)
            .run_start(&meta_for("engine-1"), goal())
            .await
            .unwrap_err();
        assert_eq!(unavailable.trace(), "deps_not_installed");
        assert!(!unavailable.message().contains('\n'));

        assert!(pond
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap()
            .is_empty());
        assert!(!pond.woken().await, "a refused start woke the runner");
    }

    #[tokio::test]
    async fn the_primes_brief_is_saved_for_every_helper_and_a_blank_one_is_not() {
        let pond = Pond::new();
        let prime = pond.prime();
        let _liz = pond.turn("engine-liz", "giap-liz", ProfileScope::Owner("liz".into()));
        let started = prime
            .run_start(
                &meta_for("engine-liz"),
                params(serde_json::json!({
                    "goal": "a four-day Mombasa plan",
                    "brief": "Four friends, 12-15 December. Liz is lactose intolerant. No flights."
                })),
            )
            .await
            .unwrap();
        let tasks = pond
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap();
        assert!(started.contains(tasks[0].id()), "{started}");
        let artifacts = pond.repo.list_artifacts(tasks[0].id()).await.unwrap();
        let brief = pond_core::shared::services::long_task_runner::brief_artifact(&artifacts)
            .expect("the brief is saved as the task's own artifact");
        let body = pond
            .repo
            .get_artifact(tasks[0].id(), &brief.id)
            .await
            .unwrap()
            .unwrap();
        assert!(body.body().contains("lactose intolerant"));

        prime
            .run_start(
                &meta_for("engine-liz"),
                params(serde_json::json!({"goal": "compare plumbers", "brief": "  "})),
            )
            .await
            .unwrap();
        let tasks = pond
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap();
        let plumbers = tasks
            .iter()
            .find(|t| t.goal() == "compare plumbers")
            .unwrap();
        assert!(pond
            .repo
            .list_artifacts(plumbers.id())
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_guest_cannot_start_a_task_and_a_member_can() {
        let pond = Pond::new();
        let prime = pond.prime();
        let _guest = pond.turn("engine-guest", "giap-guest", ProfileScope::Guest);
        let refused = prime
            .run_start(
                &meta_for("engine-guest"),
                params(serde_json::json!({"goal": "compare plumbers"})),
            )
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::Guest);
        assert!(pond
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap()
            .is_empty());
        assert!(!pond.woken().await);

        // Control: the same call from a member's turn starts a task.
        let _liz = pond.turn("engine-liz", "giap-liz", ProfileScope::Owner("liz".into()));
        let started = prime
            .run_start(
                &meta_for("engine-liz"),
                params(serde_json::json!({"goal": "compare plumbers"})),
            )
            .await
            .unwrap();
        let tasks = pond
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert!(started.contains(task.id()), "{started}");
        assert_eq!(task.state(), LongTaskState::Planning);
        assert_eq!(task.goal(), "compare plumbers");
        assert_eq!(task.owner_profile_id(), Some("liz"));
        assert_eq!(task.scope(), &ProfileScope::Owner("liz".into()));
        assert_eq!(task.origin_session_id(), Some("giap-liz"));
        assert_eq!(task.budget().max_steps(), DEFAULT_LONG_TASK_STEPS);
        assert!(pond.woken().await, "a started task did not wake the runner");
    }

    #[tokio::test]
    async fn a_start_without_a_goal_is_refused_and_an_alias_is_recovered() {
        let pond = Pond::new();
        let prime = pond.prime();
        let _turn = pond.turn("engine-1", "giap-1", ProfileScope::Household);
        let refused = prime
            .run_start(
                &meta_for("engine-1"),
                params(serde_json::json!({"goal": "  "})),
            )
            .await
            .unwrap_err();
        assert!(matches!(refused, Refusal::Malformed(_)));
        assert!(refused.message().contains("`goal`"));

        prime
            .run_start(
                &meta_for("engine-1"),
                params(serde_json::json!({"task": "read the lease"})),
            )
            .await
            .unwrap();
        let tasks = pond
            .repo
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap();
        assert_eq!(tasks[0].goal(), "read the lease");
        assert_eq!(
            tasks[0].owner_profile_id(),
            None,
            "a household task has no owner"
        );
    }

    #[tokio::test]
    async fn a_long_task_asked_about_through_check_task_gets_its_status() {
        let pond = Pond::new();
        let deps = pond.deps();
        let liz = ProfileScope::Owner("liz".into());
        let hers = pond.task(liz.clone(), "plan the school trip").await;
        let his = pond
            .task(ProfileScope::Owner("jerry".into()), "his task")
            .await;

        let one = long_task_status_with(&deps, &liz, Some(hers.id()))
            .await
            .unwrap();
        assert!(one.contains("plan the school trip"), "{one}");
        let listed = long_task_status_with(&deps, &liz, None).await.unwrap();
        assert!(
            listed.contains(hers.id()) && !listed.contains(his.id()),
            "{listed}"
        );

        assert_eq!(
            long_task_status_with(&deps, &liz, Some(his.id())).await,
            None,
            "not hers"
        );
        assert_eq!(
            long_task_status_with(&deps, &liz, Some("no-such-task")).await,
            None
        );
        let nobody = ProfileScope::Owner("ola".into());
        assert_eq!(
            long_task_status_with(&deps, &nobody, None).await,
            None,
            "with nothing running, check_task keeps its own refusal"
        );
    }

    #[tokio::test]
    async fn status_lists_only_the_tasks_the_caller_reaches() {
        let pond = Pond::new();
        let prime = pond.prime();
        let hers = pond
            .task(ProfileScope::Owner("liz".into()), "her task")
            .await;
        let his = pond
            .task(ProfileScope::Owner("jerry".into()), "his task")
            .await;
        let shared = pond.task(ProfileScope::Household, "shared task").await;
        let _liz = pond.turn("engine-liz", "giap-liz", ProfileScope::Owner("liz".into()));

        let listed = prime
            .run_status(&meta_for("engine-liz"), TaskIdParams::default())
            .await
            .unwrap();
        assert!(listed.contains(hers.id()), "{listed}");
        assert!(listed.contains(shared.id()), "{listed}");
        assert!(
            !listed.contains(his.id()),
            "Jerry's task reached Liz:\n{listed}"
        );

        // A foreign id and an unknown one read the same.
        let foreign = prime
            .run_status(
                &meta_for("engine-liz"),
                params(serde_json::json!({"task_id": his.id()})),
            )
            .await
            .unwrap_err();
        let unknown = prime
            .run_status(
                &meta_for("engine-liz"),
                params(serde_json::json!({"task_id": "no-such-task"})),
            )
            .await
            .unwrap_err();
        assert_eq!(foreign.message(), unknown.message());
        assert_eq!(foreign.trace(), "task_of_another_member");
        assert_eq!(unknown.trace(), "no_such_task");

        // Control: her own id, under an alias, answers.
        let own = prime
            .run_status(
                &meta_for("engine-liz"),
                params(serde_json::json!({"id": hers.id()})),
            )
            .await
            .unwrap();
        assert!(own.contains("her task"), "{own}");

        // The household reaches every task.
        let _house = pond.turn("engine-house", "giap-house", ProfileScope::Household);
        let all = prime
            .run_status(&meta_for("engine-house"), TaskIdParams::default())
            .await
            .unwrap();
        for task in [&hers, &his, &shared] {
            assert!(all.contains(task.id()), "{all}");
        }
    }

    #[tokio::test]
    async fn status_reports_progress_the_step_under_way_and_the_result() {
        let pond = Pond::new();
        let prime = pond.prime();
        let task = pond.task(ProfileScope::Household, "compare plumbers").await;
        pond.repo
            .set_task_state(task.id(), LongTaskState::Running, Utc::now())
            .await
            .unwrap();
        pond.plan(&task, &["fetch", "compare"]).await;
        let mut fetch = pond.repo.list_steps(task.id()).await.unwrap()[0].clone();
        fetch.start(Utc::now()).unwrap();
        pond.repo.update_step(&fetch).await.unwrap();
        fetch
            .record(
                &StepOutcome::new(OutcomeKind::Completed, "three quotes", vec![], None),
                Utc::now(),
            )
            .unwrap();
        pond.repo.update_step(&fetch).await.unwrap();
        let result = Artifact::new(
            task.id(),
            Some(StepKey::parse("compare").unwrap()),
            ArtifactKind::Findings,
            RESULT_TITLE,
            "Otieno is cheapest at 4,500 KES",
            "the full comparison".to_string(),
            Utc::now(),
        )
        .unwrap();
        pond.repo.insert_artifact(&result).await.unwrap();

        let _turn = pond.turn("engine-1", "giap-1", ProfileScope::Household);
        let status = prime
            .run_status(
                &meta_for("engine-1"),
                params(serde_json::json!({"task_id": task.id()})),
            )
            .await
            .unwrap();
        assert!(status.contains("1 of 2 steps done"), "{status}");
        assert!(status.contains("Now on step `compare`"), "{status}");
        assert!(status.contains("Otieno is cheapest"), "{status}");
        assert!(status.contains(result.id()), "{status}");
    }

    #[tokio::test]
    async fn cancel_stops_only_a_task_the_caller_reaches() {
        let pond = Pond::new();
        let prime = pond.prime();
        let his = pond
            .task(ProfileScope::Owner("jerry".into()), "his task")
            .await;
        let hers = pond
            .task(ProfileScope::Owner("liz".into()), "her task")
            .await;
        let _liz = pond.turn("engine-liz", "giap-liz", ProfileScope::Owner("liz".into()));

        let refused = prime
            .run_cancel(
                &meta_for("engine-liz"),
                params(serde_json::json!({"task_id": his.id()})),
            )
            .await
            .unwrap_err();
        assert!(matches!(refused, Refusal::UnknownTask { .. }));
        assert_eq!(
            pond.repo.get_task(his.id()).await.unwrap().unwrap().state(),
            LongTaskState::Planning,
            "Liz cancelled Jerry's task"
        );
        assert!(!pond.woken().await);

        // Control: her own task stops, and the runner is told.
        let said = prime
            .run_cancel(
                &meta_for("engine-liz"),
                params(serde_json::json!({"task_id": hers.id()})),
            )
            .await
            .unwrap();
        assert!(said.contains("Cancelled"), "{said}");
        assert_eq!(
            pond.repo
                .get_task(hers.id())
                .await
                .unwrap()
                .unwrap()
                .state(),
            LongTaskState::Cancelled
        );
        assert!(pond.woken().await);

        let again = prime
            .run_cancel(
                &meta_for("engine-liz"),
                params(serde_json::json!({"task_id": hers.id()})),
            )
            .await
            .unwrap_err();
        assert_eq!(again, Refusal::AlreadyFinished(LongTaskState::Cancelled));
        assert!(matches!(
            prime
                .run_cancel(&meta_for("engine-liz"), TaskIdParams::default())
                .await,
            Err(Refusal::Malformed(_))
        ));
    }

    #[test]
    fn a_member_reaches_their_own_and_unowned_tasks_and_a_guest_none() {
        let owned = |owner: Option<&str>| {
            let scope = match owner {
                Some(id) => ProfileScope::Owner(id.to_string()),
                None => ProfileScope::Household,
            };
            LongTask::start(
                scope,
                None,
                "g",
                None,
                LongTaskBudget::new(2, None).unwrap(),
                Utc::now(),
            )
            .unwrap()
        };
        let liz = ProfileScope::Owner("liz".into());
        assert!(scope_reaches(&liz, &owned(Some("liz"))));
        assert!(scope_reaches(&liz, &owned(None)));
        assert!(!scope_reaches(&liz, &owned(Some("jerry"))));
        assert!(scope_reaches(
            &ProfileScope::Household,
            &owned(Some("jerry"))
        ));
        for task in [owned(Some("liz")), owned(None)] {
            assert!(!scope_reaches(&ProfileScope::Guest, &task));
        }
    }

    // ── giap-task-work ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_call_that_is_not_a_bound_helper_is_refused_in_one_sentence() {
        let pond = Pond::new();
        let worker = pond.worker();
        let task = pond.task(ProfileScope::Household, "compare plumbers").await;

        let no_meta = worker.run_list(&Meta::new()).await.unwrap_err();
        let not_a_child = worker.run_list(&meta_for("engine-x")).await.unwrap_err();
        let _unbound_child = pond.children.bind("engine-orphan", "root-with-no-binding");
        let unbound = worker
            .run_list(&meta_for("engine-orphan"))
            .await
            .unwrap_err();
        for refusal in [&no_meta, &not_a_child, &unbound] {
            assert_eq!(refusal.message(), no_meta.message());
            assert!(!refusal.message().contains(". "), "more than one sentence");
        }
        assert_eq!(unbound.trace(), "run_not_bound");

        // A top-level turn's session is not a helper's, even with a live authority.
        let _turn = pond.turn("engine-prime", "giap-1", ProfileScope::Household);
        assert!(worker.run_list(&meta_for("engine-prime")).await.is_err());

        // Control: a bound child is answered.
        let _run = pond.helper("engine-helper", "root-1", task.id(), work("fetch"));
        assert!(worker.run_list(&meta_for("engine-helper")).await.is_ok());

        let unavailable =
            TaskWorkMcpServer::with_parts(None, pond.bindings.clone(), pond.children.clone())
                .run_list(&meta_for("engine-helper"))
                .await
                .unwrap_err();
        assert_eq!(unavailable.trace(), "deps_not_installed");
    }

    #[tokio::test]
    async fn a_helper_writes_lists_and_pages_through_its_own_tasks_artifacts() {
        let pond = Pond::new();
        let worker = pond.worker();
        let task = pond.task(ProfileScope::Household, "read the lease").await;
        let other = pond.task(ProfileScope::Household, "another task").await;
        let _run = pond.helper("engine-helper", "root-1", task.id(), work("read-part-1"));
        let meta = meta_for("engine-helper");

        // 1,750 tokens: more than one window.
        let body = "The tenant pays rent monthly. ".repeat(233) + "Done.";
        let saved = worker
            .run_write(
                &meta,
                params(serde_json::json!({
                    "kind": "findings",
                    "title": "Lease terms",
                    "summary": "what the lease says about rent",
                    "body": body,
                    "task_id": other.id(),
                })),
            )
            .await
            .unwrap();
        let stored = pond.repo.list_artifacts(task.id()).await.unwrap();
        assert_eq!(stored.len(), 1, "{saved}");
        let artifact = &stored[0];
        assert!(saved.contains(&artifact.id), "{saved}");
        assert_eq!(
            artifact.step_key.as_ref().map(StepKey::as_str),
            Some("read-part-1")
        );
        assert!(
            pond.repo
                .list_artifacts(other.id())
                .await
                .unwrap()
                .is_empty(),
            "a model-supplied task_id chose where the artifact went"
        );

        let listed = worker.run_list(&meta).await.unwrap();
        assert!(
            listed.contains(&artifact.id) && listed.contains("Lease terms"),
            "{listed}"
        );

        let total = artifact.tokens;
        assert!(
            total > ARTIFACT_READ_WINDOW_TOKENS,
            "the fixture fits one window"
        );
        let first = worker
            .run_read(&meta, params(serde_json::json!({"id": artifact.id})))
            .await
            .unwrap();
        assert!(
            first.contains(&format!(
                "tokens 0 to {ARTIFACT_READ_WINDOW_TOKENS} of {total}"
            )),
            "{first}"
        );
        assert!(
            first.contains(&format!("from_token {ARTIFACT_READ_WINDOW_TOKENS}")),
            "{first}"
        );
        let second = worker
            .run_read(
                &meta,
                params(serde_json::json!({"artifact_id": artifact.id, "from": "1500"})),
            )
            .await
            .unwrap();
        assert!(second.contains("That is the end of it."), "{second}");

        // The two parts are the whole body.
        let part = |text: &str| -> String {
            let start = text.find("\n\n").unwrap() + 2;
            let end = text.rfind("\n\n").unwrap();
            text[start..end].to_string()
        };
        assert_eq!(part(&first) + &part(&second), body);

        let past_the_end = worker
            .run_read(
                &meta,
                params(serde_json::json!({"id": artifact.id, "from_token": total})),
            )
            .await
            .unwrap();
        assert!(
            past_the_end.contains("Read from from_token 0"),
            "{past_the_end}"
        );
        assert!(matches!(
            worker
                .run_read(
                    &meta,
                    params(serde_json::json!({"id": artifact.id, "from_token": -3}))
                )
                .await,
            Err(Refusal::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn a_helper_cannot_read_another_tasks_artifact() {
        let pond = Pond::new();
        let worker = pond.worker();
        let mine = pond.task(ProfileScope::Household, "mine").await;
        let theirs = pond
            .task(ProfileScope::Owner("jerry".into()), "theirs")
            .await;
        let secret = Artifact::new(
            theirs.id(),
            None,
            ArtifactKind::Note,
            "Jerry's notes",
            "private",
            "the body".to_string(),
            Utc::now(),
        )
        .unwrap();
        pond.repo.insert_artifact(&secret).await.unwrap();
        let _mine = pond.helper("engine-mine", "root-mine", mine.id(), work("a"));

        let refused = worker
            .run_read(
                &meta_for("engine-mine"),
                params(serde_json::json!({"id": secret.id()})),
            )
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::UnknownArtifact(secret.id().to_string()));
        let listed = worker.run_list(&meta_for("engine-mine")).await.unwrap();
        assert!(!listed.contains(secret.id()), "{listed}");

        // Control: a helper of the owning task reads it.
        let _theirs = pond.helper("engine-theirs", "root-theirs", theirs.id(), work("a"));
        assert!(worker
            .run_read(
                &meta_for("engine-theirs"),
                params(serde_json::json!({"id": secret.id()}))
            )
            .await
            .unwrap()
            .contains("the body"));
    }

    #[tokio::test]
    async fn a_report_waits_on_the_binding_and_is_never_defaulted() {
        let pond = Pond::new();
        let worker = pond.worker();
        let task = pond.task(ProfileScope::Household, "compare plumbers").await;
        let (run, _child) = pond.helper("engine-helper", "root-1", task.id(), work("fetch"));
        let meta = meta_for("engine-helper");

        let refused = worker
            .run_report(&meta, params(serde_json::json!({"summary": "looks done"})))
            .await
            .unwrap_err();
        assert!(matches!(refused, Refusal::Rejected(_)));
        assert!(
            refused.message().contains("`outcome`"),
            "{}",
            refused.message()
        );
        assert_eq!(run.take_report(), None, "a report with no outcome was kept");

        let phantom = worker
            .run_report(
                &meta,
                params(serde_json::json!({"outcome": "completed", "artifacts": ["made-up"]})),
            )
            .await
            .unwrap_err();
        assert!(
            phantom.message().contains("saved nothing yet")
                && phantom.message().contains("finish true"),
            "{}",
            phantom.message()
        );
        assert_eq!(run.take_report(), None);

        let saved = Artifact::new(
            task.id(),
            None,
            ArtifactKind::Findings,
            "Quotes",
            "three quotes",
            "body".to_string(),
            Utc::now(),
        )
        .unwrap();
        pond.repo.insert_artifact(&saved).await.unwrap();
        let said = worker
            .run_report(
                &meta,
                params(serde_json::json!({
                    "status": "done",
                    "result": "found three quotes",
                    "artifacts": [saved.id()],
                })),
            )
            .await
            .unwrap();
        assert!(said.contains("completed"), "{said}");
        let report = run.take_report().expect("the report reached the runner");
        assert_eq!(report.outcome(), OutcomeKind::Completed);
        assert_eq!(report.summary(), "found three quotes");
        assert_eq!(report.artifacts(), [saved.id().to_string()]);

        drop(run);
        assert!(matches!(
            worker
                .run_report(&meta, params(serde_json::json!({"outcome": "failed"})))
                .await,
            Err(Refusal::NotInAStep { .. })
        ));
    }

    #[tokio::test]
    async fn only_the_planner_writes_the_plan() {
        let pond = Pond::new();
        let worker = pond.worker();
        let task = pond.task(ProfileScope::Household, "compare plumbers").await;
        let steps = serde_json::json!({"steps": [
            {"key": "fetch", "role": "long-task-worker", "instructions": "find three plumbers"},
            {"key": "compare", "role": "long-task-worker", "instructions": "compare them",
             "depends_on": ["fetch"]},
        ]});

        let _doer = pond.helper("engine-doer", "root-doer", task.id(), work("fetch"));
        let refused = worker
            .run_plan(&meta_for("engine-doer"), params(steps.clone()))
            .await
            .unwrap_err();
        assert_eq!(refused, Refusal::NotThePlanner);
        assert!(pond.repo.list_steps(task.id()).await.unwrap().is_empty());

        // Control: the planner's identical call is written.
        let _planner = pond.helper("engine-plan", "root-plan", task.id(), StepWork::Plan);
        let said = worker
            .run_plan(&meta_for("engine-plan"), params(steps))
            .await
            .unwrap();
        assert!(said.contains("`fetch`, `compare`"), "{said}");
        let keys: Vec<String> = pond
            .repo
            .list_steps(task.id())
            .await
            .unwrap()
            .iter()
            .map(|step| step.key().to_string())
            .collect();
        assert_eq!(keys, vec!["fetch", "compare"]);
    }

    #[tokio::test]
    async fn a_refused_plan_says_why_and_writes_nothing() {
        let pond = Pond::new();
        let worker = pond.worker();
        let task = pond.task(ProfileScope::Household, "compare plumbers").await;
        let _planner = pond.helper("engine-plan", "root-plan", task.id(), StepWork::Plan);
        let meta = meta_for("engine-plan");

        let circle = worker
            .run_plan(
                &meta,
                params(serde_json::json!({"steps": [
                    {"key": "a", "role": "r", "instructions": "x", "depends_on": ["b"]},
                    {"key": "b", "role": "r", "instructions": "y", "depends_on": ["a"]},
                ]})),
            )
            .await
            .unwrap_err();
        assert!(circle.message().contains("circle"), "{}", circle.message());
        let unreadable = worker
            .run_plan(
                &meta,
                params(serde_json::json!({"steps": [
                    {"key": "a", "role": "r", "instructions": "x"},
                    {"key": "b", "role": "r", "instructions": 5},
                    {"key": "c", "role": "r"},
                ]})),
            )
            .await
            .unwrap_err()
            .message();
        assert!(
            unreadable.starts_with("Step 2 cannot be read"),
            "{unreadable}"
        );
        assert!(unreadable.contains("step 3 cannot be read"), "{unreadable}");
        assert!(!unreadable.contains("step 1"), "{unreadable}");
        assert!(unreadable.contains("other steps are fine"), "{unreadable}");
        assert!(matches!(
            worker.run_plan(&meta, PlanWriteParams::default()).await,
            Err(Refusal::Malformed(_))
        ));
        assert!(pond.repo.list_steps(task.id()).await.unwrap().is_empty());

        // A list sent as a JSON string is still a list.
        let stringified = serde_json::json!({
            "steps": r#"[{"key": "a", "role": "r", "instructions": "x"}]"#
        });
        worker.run_plan(&meta, params(stringified)).await.unwrap();
        assert_eq!(pond.repo.list_steps(task.id()).await.unwrap().len(), 1);

        // Fields no step reads are ignored and named, and a misspelt key is taken as its slug.
        let saved = worker
            .run_plan(
                &meta,
                params(serde_json::json!({"steps": [
                    {"key": "Find_Events", "role": "r", "instructions": "x", "title": "Find",
                     "tools": ["giap-system"]},
                    {"key": "sum", "role": "r", "instructions": "y", "depends_on": ["Find_Events"]},
                ]})),
            )
            .await
            .unwrap();
        assert!(saved.contains("`find-events`, `sum`"), "{saved}");
        assert!(
            saved.contains("Ignored, as no step reads them: `tools`"),
            "{saved}"
        );
        assert!(saved.contains("`new_agent`"), "{saved}");
        let steps = pond.repo.list_steps(task.id()).await.unwrap();
        assert_eq!(steps[1].depends_on()[0].as_str(), "find-events");

        // The planner's reading of the ask is kept, and a later one does not overturn it.
        for answer in ["Short", "report"] {
            worker
                .run_plan(
                    &meta,
                    params(serde_json::json!({"answer": answer, "steps": [
                        {"role": "r", "instructions": "x"},
                    ]})),
                )
                .await
                .unwrap();
        }
        let task = pond.repo.get_task(task.id()).await.unwrap().unwrap();
        assert_eq!(task.answer(), AnswerShape::Short);
        let steps = pond.repo.list_steps(task.id()).await.unwrap();
        assert_eq!(
            steps[0].key().as_str(),
            "step-1",
            "a step with no key takes its place"
        );

        // A short answer's plan is one step, or two; a longer one is sent back with what to do.
        let long = worker
            .run_plan(
                &meta,
                params(serde_json::json!({"steps": [
                    {"role": "r", "instructions": "x"},
                    {"role": "r", "instructions": "y"},
                    {"role": "r", "instructions": "z"},
                ]})),
            )
            .await
            .unwrap_err()
            .message();
        assert!(
            long.contains("short answer, and you planned 3 steps"),
            "{long}"
        );
    }

    #[test]
    fn every_withheld_task_tool_is_one_the_server_offers_and_each_role_keeps_its_own() {
        use pond_core::shared::services::long_task_roles::{PLANNER_ROLE, VERIFIER_ROLE};
        use pond_core::shared::services::long_task_runner::tools_withheld;
        let offered: Vec<String> = TaskWorkMcpServer::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| format!("{TASK_WORK_EXTENSION}__{}", tool.name))
            .collect();
        for role in [PLANNER_ROLE, VERIFIER_ROLE, WORKER_ROLE] {
            for tool in tools_withheld(role) {
                assert!(
                    offered.iter().any(|o| o == tool),
                    "{role} withholds {tool}, no such tool"
                );
            }
        }
        let keeps = |role: &str, tool: &str| {
            !tools_withheld(role).contains(&format!("{TASK_WORK_EXTENSION}__{tool}").as_str())
        };
        assert!(keeps(PLANNER_ROLE, "plan_write"));
        assert!(keeps(VERIFIER_ROLE, "step_report"));
        assert!(keeps(WORKER_ROLE, "artifact_write") && keeps(WORKER_ROLE, "step_report"));
        assert!(!keeps(PLANNER_ROLE, "artifact_write") && !keeps(WORKER_ROLE, "plan_write"));
    }

    #[test]
    fn a_bare_web_address_in_a_link_gets_its_scheme_and_nothing_else_changes() {
        assert_eq!(
            absolute_links("Train [Rome2rio](rome2rio.com/s/Nairobi) or [KR](https://krc.co.ke)."),
            "Train [Rome2rio](https://rome2rio.com/s/Nairobi) or [KR](https://krc.co.ke)."
        );
        assert_eq!(
            absolute_links("[Guide](rome2rio.com, 2023-04-14) says so."),
            "[Guide](https://rome2rio.com) (2023-04-14) says so."
        );
        for kept in [
            "[snap](/api/v1/camera/porch/snapshot)",
            "[top](#result)",
            "[mail](mailto:a@b.co)",
            "[file](report.v2)",
            "[odd](not a link)",
            "[odd](see rome2rio.com)",
            "no link (rome2rio.com) here",
        ] {
            assert_eq!(absolute_links(kept), kept);
        }
    }

    #[test]
    fn a_report_may_name_its_artifacts_by_title_or_fall_back_to_what_the_run_saved() {
        let summary = |id: &str, title: &str, at: i64| ArtifactSummary {
            id: id.into(),
            task_id: "t".into(),
            step_key: None,
            kind: ArtifactKind::Draft,
            title: title.into(),
            summary: String::new(),
            tokens: 10,
            created_at: chrono::DateTime::from_timestamp(1_790_000_000 + at, 0).unwrap(),
        };
        let held = [
            summary("a1", "Draft: Camera Events", 0),
            summary("a2", "Result", 5),
        ];
        let asked = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();

        assert_eq!(
            name_artifacts(&asked(&["a2", "draft: camera events"]), &held, &[]),
            Ok(asked(&["a2", "a1"]))
        );
        assert_eq!(
            name_artifacts(
                &asked(&["the_artifact_id_from_previous_write_call"]),
                &held,
                &asked(&["a1"])
            ),
            Ok(asked(&["a1"])),
            "a placeholder reads as what this run saved"
        );
        assert_eq!(
            name_artifacts(&asked(&["made-up"]), &held, &[]),
            Err(asked(&["made-up"])),
            "a run that saved nothing is told which names were wrong"
        );
    }

    #[test]
    fn a_task_summary_says_where_a_task_has_got() {
        let task = LongTask::start(
            ProfileScope::Household,
            None,
            "compare plumbers",
            None,
            LongTaskBudget::new(4, None).unwrap(),
            Utc::now(),
        )
        .unwrap();
        let fresh = task_summary_for_notice(&task, &[], None);
        assert!(
            fresh.contains("Planning its steps. No plan yet."),
            "{fresh}"
        );
        assert!(fresh.contains(task.id()));

        let mut done = task.clone();
        done.transition_to(LongTaskState::Running, Utc::now())
            .unwrap();
        done.transition_to(LongTaskState::Completed, Utc::now())
            .unwrap();
        assert!(task_summary_for_notice(&done, &[], None).contains("without saving a result"));
    }

    #[test]
    fn the_result_title_is_the_one_the_planner_is_told_to_use() {
        assert!(
            PLANNER_ROLE_RECIPE.contains(&format!("titled \"{RESULT_TITLE}\"")),
            "task_status looks for an artifact the planner is never told to write"
        );
    }

    #[test]
    fn each_server_offers_exactly_its_own_tools() {
        let names = |tools: Vec<rmcp::model::Tool>| -> BTreeSet<String> {
            tools
                .into_iter()
                .map(|tool| tool.name.to_string())
                .collect()
        };
        let expected = |list: &[&str]| -> BTreeSet<String> {
            list.iter().map(|name| name.to_string()).collect()
        };
        assert_eq!(
            names(TasksMcpServer::tool_defs()),
            expected(&["task_start", "task_status", "task_cancel"])
        );
        assert_eq!(
            names(TaskWorkMcpServer::tool_defs()),
            expected(&[
                "artifact_list",
                "artifact_read",
                "artifact_write",
                "step_report",
                "plan_write"
            ])
        );
    }

    /// The description is prompt text and cannot name the const, so it is checked against it.
    #[test]
    fn the_read_window_the_model_is_told_is_the_one_applied() {
        let read = TaskWorkMcpServer::tool_defs()
            .into_iter()
            .find(|tool| tool.name == "artifact_read")
            .expect("artifact_read is offered");
        let description = read.description.as_deref().unwrap_or_default();
        assert!(
            description.contains(&format!("up to {ARTIFACT_READ_WINDOW_TOKENS} tokens")),
            "{description}"
        );
    }
}
