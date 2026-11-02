//! Runs a household's agents on their schedules and on the bus events they name. Each run is a
//! one-step long task, queued here and worked by the long-task runner on the inference lane, so
//! nothing here holds the model. The decisions are pond-core's
//! (`shared::services::agent_activations`); this file reads, writes and waits.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use pond_adapters_weather::WeatherProvider;
use pond_core::mcp::ports::notification::{Notification, OpenScreen};
use pond_core::shared::domain::long_task::{LongTask, LongTaskState, PlanStep, StepOutcome};
use pond_core::shared::domain::orchestration::{AgentRole, RolePersonalData};
use pond_core::shared::domain::role_block::BeforeHook;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::ports::long_task::LongTaskRepository;
use pond_core::shared::services::agent_activations::{
    self as activations, ActivationAgent, ActivationBook, AfterAction, BeforeHookContext, Firing,
    FiringReason, PresenceBoard, PresenceLine,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;

/// How often schedules and daily times are checked; a run may start up to this late.
const CLOCK_SECS: u64 = 30;

/// How often recipes are re-read, so an edited agent's activations apply within a minute.
const RELOAD_SECS: u64 = 60;

/// A before hook waits this long for the weather, then says it could not read it.
const WEATHER_TIMEOUT: Duration = Duration::from_secs(5);

/// Schedules read to find today's; more than a household keeps in a day.
const SCHEDULE_LOOKAHEAD: u32 = 32;

/// Shared between the loop, which records presence off the bus, and the before hooks.
pub type SharedPresence = Arc<RwLock<PresenceBoard>>;

pub struct ActivationDeps {
    pub settings: Arc<dyn SettingsRepository + Send + Sync>,
    pub recipes: Arc<dyn AgentRecipeRepository + Send + Sync>,
    pub tasks: Arc<dyn LongTaskRepository>,
    pub profiles: Arc<dyn ProfileRepository + Send + Sync>,
    pub bus: Arc<dyn EventBus>,
    /// The long-task runner's nudge bell: it keeps every lane gate, so a run waits for quiet.
    pub wake: Arc<tokio::sync::Notify>,
    pub presence: SharedPresence,
}

/// What the settings allow, re-read on every clock tick.
struct Gate {
    enabled: bool,
    timezone: String,
}

impl Gate {
    fn closed() -> Self {
        Self {
            enabled: false,
            timezone: "UTC".to_string(),
        }
    }
}

/// The loop: bus events and a clock tick, cooldowns in memory, tasks through the ledger.
pub async fn run_agent_activations(deps: ActivationDeps) {
    let mut events = deps.bus.subscribe();
    let mut clock = tokio::time::interval(Duration::from_secs(CLOCK_SECS));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut book = ActivationBook::default();
    let mut agents: Vec<ActivationAgent> = Vec::new();
    let mut loaded_at: Option<Instant> = None;
    let mut gate = Gate::closed();
    let mut warned: HashMap<String, u64> = HashMap::new();
    tracing::info!(
        "agent activations active - off unless `agent_activations_enabled`, `long_tasks_enabled` \
         and `ext_orchestrator_enabled`"
    );

    loop {
        let event = tokio::select! {
            next = events.next() => match next {
                Some(event) => Some(event),
                None => {
                    tracing::debug!("agent activations: the event bus closed - stopping");
                    return;
                }
            },
            _ = clock.tick() => None,
        };
        // Recorded whatever the switches say, so a before hook switched on later is not blind.
        if let Some(BusEvent::Presence(presence)) = &event {
            if let Ok(mut board) = deps.presence.write() {
                board.record(presence);
            }
        }

        if event.is_none() {
            let next = match deps.settings.get().await {
                Ok(s) => Gate {
                    enabled: s.agent_activations_enabled
                        && s.long_tasks_enabled
                        && s.ext_orchestrator_enabled,
                    timezone: s.timezone,
                },
                Err(e) => {
                    tracing::debug!(error = %e, "agent activations: settings read failed");
                    Gate::closed()
                }
            };
            // A clock kept while off would fire every time it missed the moment it is back on.
            if !next.enabled || next.timezone != gate.timezone {
                book.reset_clocks();
            }
            gate = next;
            if !gate.enabled {
                continue;
            }
            if loaded_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(RELOAD_SECS)) {
                if let Some(loaded) = load_agents(deps.recipes.as_ref(), &mut warned).await {
                    agents = loaded;
                    book.retain(&activations::live_keys(&agents));
                }
                loaded_at = Some(Instant::now());
            }
        }
        if !gate.enabled || agents.is_empty() {
            continue;
        }

        let now = Utc::now();
        let firings = match &event {
            Some(event) => {
                activations::firings_for_event(&agents, event, &gate.timezone, &mut book, now)
            }
            None => activations::firings_for_clock(&agents, &gate.timezone, &mut book, now),
        };
        for firing in firings {
            queue(&deps, &agents, &firing, &mut book).await;
        }
    }
}

/// Active recipes whose roles run on their own; `None` when the store could not be read.
async fn load_agents(
    recipes: &(dyn AgentRecipeRepository + Send + Sync),
    warned: &mut HashMap<String, u64>,
) -> Option<Vec<ActivationAgent>> {
    let list = match recipes.list().await {
        Ok(list) => list,
        Err(e) => {
            tracing::debug!(error = %e, "agent activations: recipes could not be read");
            return None;
        }
    };
    let mut agents = Vec::new();
    for recipe in list.into_iter().filter(|r| r.active) {
        match AgentRole::from_recipe_yaml(&recipe.name, &recipe.yaml) {
            Ok(Some(role)) if role.runs_on_its_own() => agents.push(ActivationAgent {
                role,
                owner_profile_id: recipe.owner_profile_id,
            }),
            Ok(_) => {}
            Err(e) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                recipe.yaml.hash(&mut hasher);
                let seen = hasher.finish();
                // Once per edit, not once a minute.
                if warned.insert(recipe.name.clone(), seen) != Some(seen) {
                    tracing::warn!(
                        recipe = %recipe.name,
                        error = %e,
                        "agent activations: this recipe's role does not parse, so its schedules \
                         and events are off until it is fixed"
                    );
                }
            }
        }
    }
    Some(agents)
}

/// Queues one firing as a one-step task, unless its last run is still open or nobody owns it.
async fn queue(
    deps: &ActivationDeps,
    agents: &[ActivationAgent],
    firing: &Firing,
    book: &mut ActivationBook,
) {
    let Some(agent) = agents.iter().find(|a| a.role.name() == firing.agent) else {
        return;
    };
    if let Some(open) = book.task_for(&firing.key) {
        match deps.tasks.get_task(open).await {
            Ok(Some(task)) if !task.state().is_terminal() => {
                tracing::debug!(
                    agent = %firing.agent,
                    task = %task.id(),
                    "agent activation skipped: its last run has not finished"
                );
                return;
            }
            Ok(_) => {}
            // Unknown is treated as open: a pile of queued runs is worse than one missed.
            Err(e) => {
                tracing::debug!(error = %e, "agent activation skipped: the ledger could not be read");
                return;
            }
        }
    }

    let member = match &firing.reason {
        FiringReason::Event(matched) => matched.member.clone(),
        FiringReason::Schedule { .. } | FiringReason::Daily { .. } => None,
    };
    let owner = activations::activation_owner(member.as_deref(), agent.owner_profile_id.as_deref());
    let members = match deps.profiles.list().await {
        Ok(members) => members,
        Err(e) => {
            tracing::debug!(error = %e, "agent activation skipped: members could not be read");
            return;
        }
    };
    let Some(scope) = activations::activation_scope(owner.as_deref(), members.len()) else {
        tracing::info!(
            target: "giap::trace",
            kind = "agent_activation_unowned",
            agent = %firing.agent,
            "agent activation skipped: the recipe has no owner and the event names nobody, so \
             there is no member to run it for"
        );
        return;
    };
    let member_name = member
        .as_deref()
        .and_then(|id| members.iter().find(|p| p.id == id))
        .map(|p| p.display_name.clone());
    let reason = activations::firing_reason_line(&firing.reason, member_name.as_deref());
    let instructions = activations::activation_instructions(&firing.brief, &reason);

    let now = Utc::now();
    let (task, plan) =
        match activations::activation_task(&agent.role, &instructions, scope, owner, now) {
            Ok(built) => built,
            Err(e) => {
                tracing::warn!(agent = %firing.agent, error = %e, "agent activation refused");
                return;
            }
        };
    if let Err(e) = deps.tasks.create_task(&task).await {
        tracing::warn!(agent = %firing.agent, error = %e, "agent activation: the task did not save");
        return;
    }
    // Paused until its step is stored, so the runner never plans it; cancelled if that fails.
    let stored = match deps.tasks.replace_open_steps(&plan).await {
        Ok(()) => deps
            .tasks
            .set_task_state(task.id(), LongTaskState::Running, Utc::now())
            .await
            .map(|moved| moved.is_some()),
        Err(e) => Err(e),
    };
    if !matches!(stored, Ok(true)) {
        if let Err(e) = &stored {
            tracing::warn!(agent = %firing.agent, error = %e, "agent activation: the run did not queue");
        }
        let _ = deps
            .tasks
            .set_task_state(task.id(), LongTaskState::Cancelled, Utc::now())
            .await;
        return;
    }
    book.note_task(&firing.key, task.id());
    deps.wake.notify_one();
    tracing::info!(
        target: "giap::trace",
        kind = "agent_activation",
        agent = %firing.agent,
        task = %task.id(),
        member = task.owner_profile_id().unwrap_or("-"),
        "an agent's activation queued its run"
    );
}

// ── Before hooks ────────────────────────────────────────────────────────────

/// Where the before hooks read from; each source is optional and a missing one says so.
pub struct BeforeHookSources {
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    pub weather: Option<Arc<dyn WeatherProvider>>,
    pub presence: SharedPresence,
    pub profiles: Arc<dyn ProfileRepository + Send + Sync>,
    pub settings: Arc<dyn SettingsRepository + Send + Sync>,
}

impl BeforeHookSources {
    /// The block `role`'s before hooks add to a step's brief for a run of a task at `scope`;
    /// empty without hooks, so a role that declares none pays no reads.
    pub async fn block_for(
        &self,
        role: &AgentRole,
        scope: &ProfileScope,
        now: DateTime<Utc>,
    ) -> String {
        let hooks = &role.hooks().before;
        if hooks.is_empty() {
            return String::new();
        }
        // The run's own scope: a role that denies personal data runs as a guest.
        let guest = ProfileScope::Guest;
        let scope = match role.personal_data() {
            RolePersonalData::Deny => &guest,
            RolePersonalData::Inherit => scope,
        };
        let context = self.gather(hooks, scope, now).await;
        activations::render_before_hooks(hooks, &context, scope)
    }

    pub async fn gather(
        &self,
        hooks: &[BeforeHook],
        scope: &ProfileScope,
        now: DateTime<Utc>,
    ) -> BeforeHookContext {
        let timezone = self
            .settings
            .get()
            .await
            .map(|s| s.timezone)
            .unwrap_or_else(|_| "UTC".to_string());
        // Not read at all for a run that may not see it.
        let private = scope.excludes_everything();
        let mut context = BeforeHookContext::default();
        if hooks.contains(&BeforeHook::Today) {
            context.date = Some(activations::local_date_label(now, &timezone));
            if !private {
                context.schedule = self.today(&timezone, now).await;
            }
        }
        if hooks.contains(&BeforeHook::Weather) {
            context.weather = self.weather_line().await;
        }
        if hooks.contains(&BeforeHook::Presence) && !private {
            context.presence = self.presence_lines(&timezone, now).await;
        }
        context
    }

    async fn today(&self, timezone: &str, now: DateTime<Utc>) -> Vec<String> {
        let Some(scheduler) = &self.scheduler else {
            return Vec::new();
        };
        let upcoming = match scheduler.list_upcoming(SCHEDULE_LOOKAHEAD).await {
            Ok(upcoming) => upcoming,
            Err(e) => {
                tracing::debug!(error = %e, "before hook: schedules could not be read");
                return Vec::new();
            }
        };
        upcoming
            .into_iter()
            .filter(|s| !s.paused)
            .filter_map(|s| {
                let at = s.next_run?;
                activations::same_local_day(at, now, timezone).then(|| {
                    format!(
                        "{} {}",
                        activations::local_clock_label(at, now, timezone),
                        s.label
                    )
                })
            })
            .collect()
    }

    async fn weather_line(&self) -> Option<String> {
        let weather = self.weather.as_ref()?;
        match tokio::time::timeout(WEATHER_TIMEOUT, weather.current()).await {
            Ok(Ok(now)) => Some(format!(
                "{}, {:.0}\u{00B0}C (feels like {:.0}\u{00B0}C), wind {:.0} km/h, in {}",
                now.description,
                now.temperature_c,
                now.feels_like_c,
                now.wind_speed_kmh,
                now.location_name
            )),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "before hook: the weather could not be read");
                None
            }
            Err(_) => {
                tracing::debug!("before hook: the weather took too long");
                None
            }
        }
    }

    async fn presence_lines(&self, timezone: &str, now: DateTime<Utc>) -> Vec<PresenceLine> {
        let seen = match self.presence.read() {
            Ok(board) => board.snapshot(),
            Err(_) => return Vec::new(),
        };
        let mut lines = Vec::with_capacity(seen.len());
        for (profile_id, home, at) in seen {
            // A member deleted since is nobody to report.
            if let Ok(Some(member)) = self.profiles.get(&profile_id).await {
                lines.push(PresenceLine {
                    name: member.display_name,
                    home,
                    since: activations::local_clock_label(at, now, timezone),
                });
            }
        }
        lines
    }
}

// ── After hooks ─────────────────────────────────────────────────────────────

/// What applying a completed step's after-hook actions needs from the server.
pub struct AfterHookDeps<'a> {
    pub tasks: &'a dyn LongTaskRepository,
    pub sender: &'a BroadcastNotificationSender,
    /// The household's roles, as the runner offers them, for a hand-off's target.
    pub roles: &'a [AgentRole],
    /// Rung when a follow-up task is started, so the runner looks sooner.
    pub wake: Option<&'a tokio::sync::Notify>,
}

/// Applies each action [`activations::after_hook_actions`] chose for `step`, in order. A failed
/// action is logged and the rest still run: the step itself already succeeded.
pub async fn apply_after_actions(
    actions: &[AfterAction],
    task: &LongTask,
    step: &PlanStep,
    outcome: &StepOutcome,
    deps: &AfterHookDeps<'_>,
) {
    for action in actions {
        match action {
            AfterAction::NotifyOwner(text) => {
                let Some(owner) = task.owner_profile_id() else {
                    continue;
                };
                let report = deps
                    .sender
                    .send_to_profile(owner, agent_notification(task.id(), step.role(), text))
                    .await;
                tracing::info!(
                    task = %task.id(),
                    agent = %step.role(),
                    member = %owner,
                    queued = report.queued.len(),
                    failed = report.failed.len(),
                    "an agent's run ended; its owner was told"
                );
            }
            AfterAction::SaveArtifact => {
                match activations::hook_result_artifact(task, step, outcome, Utc::now()) {
                    Ok(artifact) => {
                        if let Err(e) = deps.tasks.insert_artifact(&artifact).await {
                            tracing::warn!(task = %task.id(), error = %e, "after hook: the result did not save");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(task = %task.id(), error = %e, "after hook: no result to save")
                    }
                }
            }
            AfterAction::StartTask(goal) => {
                match activations::follow_up_task(task, goal, Utc::now()) {
                    Ok(follow_up) => match deps.tasks.create_task(&follow_up).await {
                        Ok(()) => {
                            if let Some(wake) = deps.wake {
                                wake.notify_one();
                            }
                            tracing::info!(
                                target: "giap::trace",
                                kind = "agent_follow_up_task",
                                task = %task.id(),
                                follow_up = %follow_up.id(),
                                agent = %step.role(),
                                "an agent started a long task"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(task = %task.id(), error = %e, "after hook: the follow-up task did not save")
                        }
                    },
                    Err(e) => {
                        tracing::warn!(task = %task.id(), error = %e, "after hook: the follow-up task was refused")
                    }
                }
            }
            AfterAction::HandTo { agent, brief } => {
                hand_on(task, step, agent, brief, deps).await;
            }
        }
    }
}

async fn hand_on(
    task: &LongTask,
    step: &PlanStep,
    agent: &str,
    brief: &str,
    deps: &AfterHookDeps<'_>,
) {
    let (steps, artifacts) = match (
        deps.tasks.list_steps(task.id()).await,
        deps.tasks.list_artifacts(task.id()).await,
    ) {
        (Ok(steps), Ok(artifacts)) => (steps, artifacts),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!(task = %task.id(), error = %e, "hand-off: the ledger could not be read");
            return;
        }
    };
    // The stored step, which carries the summary the next agent is told.
    let source = steps.iter().find(|s| s.key() == step.key()).unwrap_or(step);
    let plan = match activations::plan_hand_off(
        task, &steps, source, agent, brief, deps.roles, &artifacts,
    ) {
        Ok(plan) => plan,
        Err(refused) => {
            tracing::info!(
                target: "giap::trace",
                kind = "agent_hand_off_refused",
                task = %task.id(),
                from = %step.role(),
                to = %agent,
                reason = %refused,
                "a hand-off was refused"
            );
            return;
        }
    };
    match deps.tasks.replace_open_steps(&plan).await {
        Ok(()) => tracing::info!(
            target: "giap::trace",
            kind = "agent_hand_off",
            task = %task.id(),
            from = %step.role(),
            to = %agent,
            "a run handed its work on"
        ),
        Err(e) => {
            tracing::warn!(task = %task.id(), error = %e, "hand-off: the next step did not save")
        }
    }
}

/// The owner's notice that one of their agents finished a run; a tap opens chat, where
/// `task_status` gives the detail.
pub fn agent_notification(task_id: &str, agent: &str, summary: &str) -> Notification {
    let mut fields = serde_json::Map::new();
    fields.insert("long_task_id".into(), task_id.into());
    Notification {
        id: format!("agent-run-{task_id}"),
        target: String::new(),
        category: "info".to_string(),
        title: format!("From {agent}"),
        body: summary.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        data: Some(OpenScreen::Chat.merged_into(fields)),
    }
}
