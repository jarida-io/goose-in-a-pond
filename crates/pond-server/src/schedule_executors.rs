//! Scheduler executors; `DeferredExecutor` lets the scheduler exist before the agent does.

use anyhow::{bail, Result};
use async_trait::async_trait;
use pond_audio_out::AudioBus;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::AgentRequest;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::schedule::{
    routine_rejection, ActionsFailed, TaskKind, TriggerAction,
};
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use std::sync::Arc;
use tokio::sync::{OnceCell, Semaphore};

// ── AgentScheduleExecutor ────────────────────────────────────────────────────

/// Runs scheduled tasks: an agent prompt, a webhook, or a sensor rule's list of actions.
pub struct AgentScheduleExecutor {
    agent: Arc<dyn Agent>,
    session_storage: Arc<dyn SessionStorage>,
    http_client: reqwest::Client,
    /// Actuation seam for rule actions. `None` → device actions report failure.
    device_control: Option<Arc<dyn DeviceControlPort>>,
    /// Limit concurrent scheduled runs to avoid starving interactive chat.
    semaphore: Semaphore,
    /// Speaks a finished `AgentPrompt` response, gated by `speakable_now`; `None` without TTS.
    audio_bus: Option<AudioBus>,
    settings_repo: Option<Arc<dyn SettingsRepository>>,
}

impl AgentScheduleExecutor {
    pub fn new(
        agent: Arc<dyn Agent>,
        session_storage: Arc<dyn SessionStorage>,
        device_control: Option<Arc<dyn DeviceControlPort>>,
        max_concurrent: u32,
        voice_output: Option<Arc<dyn VoiceOutput>>,
        settings_repo: Option<Arc<dyn SettingsRepository>>,
    ) -> Self {
        Self {
            agent,
            session_storage,
            http_client: pond_net::client(),
            device_control,
            semaphore: Semaphore::new(max_concurrent.max(1) as usize),
            audio_bus: voice_output.map(AudioBus::new),
            settings_repo,
        }
    }

    /// Checks quiet hours and `unprompted_speech_enabled` only; not `speak_unprompted`'s
    /// presence gate, as schedules have no owner. Member-level gating is a known gap. Quiet hours
    /// are the household's, so they are read in its time zone, the one schedules fire in unless
    /// they name another, not in the machine's, which on a headless pond is often UTC.
    fn speakable_now(settings: &pond_core::user_data::domain::settings::Settings) -> bool {
        if !settings.unprompted_speech_enabled {
            return false;
        }
        let now = pond_core::shared::services::chat::LocalTimeOfDay::in_zone(
            &settings.timezone,
            chrono::Utc::now(),
        );
        !quiet_hours_cover_now(
            &settings.quiet_hours_start,
            &settings.quiet_hours_end,
            u32::from(now.minutes() / 60),
            u32::from(now.minutes() % 60),
        )
    }

    /// Prompt the agent in an ephemeral session; the caller (`execute`) holds the semaphore.
    async fn run_agent_prompt(&self, task_id: &str, prompt: &str) -> Result<String> {
        let session_id = format!("sched-{}-{}", task_id, chrono::Utc::now().timestamp());
        let _ = self
            .session_storage
            .create_session(session_id.clone())
            .await;

        let should_speak = match (&self.audio_bus, &self.settings_repo) {
            (Some(_), Some(repo)) => match repo.get().await {
                Ok(settings) if Self::speakable_now(&settings) => true,
                Ok(_) => {
                    tracing::debug!(
                        "[scheduler] task {task_id}: not speaking (consent off or quiet hours)"
                    );
                    false
                }
                Err(e) => {
                    tracing::warn!(
                        "[scheduler] task {task_id}: could not read settings, staying silent: {e}"
                    );
                    false
                }
            },
            _ => false,
        };

        let request = AgentRequest {
            message: prompt.to_string(),
            session_id,
            model_role: "task".to_string(),
            images: vec![],
            voice_mode: should_speak,
            canvas_mode: false,
            // Schedules have no owner, so a reminder is household context.
            profile_scope: ProfileScope::Household,
            // Nobody is in the room for a scheduled task.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
            suggestion_context: None,
            deep_research_task: None,
        };

        tracing::info!("[scheduler] executing prompt for task {task_id} (speak={should_speak})");

        if should_speak {
            if let Some(bus) = &self.audio_bus {
                match self.agent.chat_stream(request.clone()).await {
                    Ok(stream) => {
                        tracing::info!(
                            "[scheduler] task {task_id}: streaming tokens into audio bus"
                        );
                        let full_text = bus.speak_stream(stream, true).await?;
                        tracing::info!(
                            "[scheduler] task {task_id} completed ({} chars, speech finished)",
                            full_text.len()
                        );
                        return Ok(full_text);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[scheduler] task {task_id}: chat_stream failed ({e}), falling back to batch chat"
                        );
                    }
                }
            }
        }

        // An engine failure is an `Err` from the adapter, never reply text, so the run is failed.
        let response = match self.agent.chat(request).await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    kind = "schedule_prompt_failed",
                    task_id = %task_id,
                    error = %e,
                    "[scheduler] task {task_id} failed"
                );
                return Err(e);
            }
        };
        tracing::info!(
            kind = "schedule_prompt_completed",
            task_id = %task_id,
            chars = response.text.len(),
            "[scheduler] task {task_id} completed ({} chars)",
            response.text.len()
        );

        if should_speak {
            if let Some(bus) = &self.audio_bus {
                tracing::info!("[scheduler] task {task_id}: speaking response aloud");
                if let Err(e) = bus.speak_text(&response.text, true).await {
                    tracing::warn!("[scheduler] task {task_id}: TTS failed: {e}");
                }
            }
        }

        Ok(response.text)
    }

    /// Run one sensor-rule action, returning a short outcome summary.
    async fn run_trigger_action(&self, task_id: &str, action: &TriggerAction) -> Result<String> {
        match action {
            TriggerAction::AgentPrompt { prompt } => self.run_agent_prompt(task_id, prompt).await,
            TriggerAction::DevicePower { device_id, on } => match &self.device_control {
                Some(dc) => {
                    dc.set_power(device_id, *on).await?;
                    Ok(format!(
                        "{device_id} switched {}",
                        if *on { "on" } else { "off" }
                    ))
                }
                None => bail!("device control not available"),
            },
            TriggerAction::Notify { title, body } => {
                // Process-global, set at startup before any rule can fire.
                match pond_mcp_server::notification_sender() {
                    Some(sender) => {
                        let n = pond_core::mcp::ports::notification::Notification {
                            id: uuid::Uuid::new_v4().to_string(),
                            target: "broadcast".to_string(),
                            category: "alert".to_string(),
                            title: title.clone(),
                            body: body.clone(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            data: None,
                        };
                        sender.broadcast(n).await?;
                        Ok(format!("notified: {title}"))
                    }
                    None => bail!("notification sender not available"),
                }
            }
        }
    }

    /// Run every action of a routine, even after one fails: a dead plug must not leave the
    /// lights off. Fails if any action failed, naming those.
    async fn run_routine(&self, task_id: &str, actions: &[TriggerAction]) -> Result<String> {
        if let Some(why) = routine_rejection(actions) {
            bail!("task {task_id}: {why}");
        }
        let mut outcomes = Vec::with_capacity(actions.len());
        let mut failed = Vec::new();
        for action in actions {
            match self.run_trigger_action(task_id, action).await {
                Ok(outcome) => outcomes.push(outcome),
                Err(e) => {
                    tracing::warn!("[routine] task {task_id} action failed: {e}");
                    let what = action_target(action);
                    outcomes.push(format!("{what} failed: {e}"));
                    failed.push(what);
                }
            }
        }
        let outcomes = outcomes.join(" · ");
        if failed.is_empty() {
            return Ok(outcomes);
        }
        Err(ActionsFailed {
            error: format!("did not run: {}", failed.join(", ")),
            outcomes,
        }
        .into())
    }
}

/// What an action acts on, to name it when it fails.
fn action_target(action: &TriggerAction) -> String {
    match action {
        TriggerAction::DevicePower { device_id, .. } => device_id.clone(),
        TriggerAction::Notify { .. } => "the notification".to_string(),
        TriggerAction::AgentPrompt { .. } => "the assistant".to_string(),
    }
}

#[async_trait]
impl ScheduleExecutor for AgentScheduleExecutor {
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String> {
        let _permit = self.semaphore.acquire().await?;

        match kind {
            TaskKind::AgentPrompt { prompt } => self.run_agent_prompt(task_id, prompt).await,
            TaskKind::Routine { actions } => self.run_routine(task_id, actions).await,
            TaskKind::Webhook { webhook_url } => {
                tracing::info!("[scheduler] firing webhook for task {task_id}: {webhook_url}");
                // A user-typed URL is the most direct exfiltration path: `begin` gates it and
                // `finish` records every outcome, timeouts included.
                let call = pond_core::shared::services::egress::begin_for(
                    webhook_url,
                    "POST",
                    pond_core::shared::services::egress::EgressPurpose::Schedules,
                    "",
                )
                .map_err(|e| anyhow::anyhow!("task {task_id}: {e}"))?;

                let sent = self.http_client.post(webhook_url).send().await;
                call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));

                let resp =
                    sent.map_err(|e| anyhow::anyhow!("task {task_id}: webhook POST failed: {e}"))?;

                let status = resp.status();
                if !status.is_success() {
                    bail!("task {task_id}: webhook returned {status}");
                }
                Ok(format!("Webhook returned {status}"))
            }
            TaskKind::SensorTrigger(spec) => {
                // Fails only when no action succeeded.
                let mut summaries = Vec::with_capacity(spec.actions.len());
                let mut any_ok = false;
                for action in &spec.actions {
                    match self.run_trigger_action(task_id, action).await {
                        Ok(s) => {
                            any_ok = true;
                            summaries.push(s);
                        }
                        Err(e) => {
                            tracing::warn!("[rules] task {task_id} action failed: {e}");
                            summaries.push(format!("action failed: {e}"));
                        }
                    }
                }
                if spec.actions.is_empty() {
                    bail!("task {task_id}: sensor rule has no actions");
                }
                if !any_ok {
                    bail!(
                        "task {task_id}: all rule actions failed: {}",
                        summaries.join("; ")
                    );
                }
                Ok(summaries.join(" · "))
            }
        }
    }
}

/// Whether `(hour, minute)` is inside `[start, end)`, wrapping midnight when `start > end`.
/// An unparseable bound means quiet all day, like `ChatService::quiet_hours_cover`.
fn quiet_hours_cover_now(start: &str, end: &str, hour: u32, minute: u32) -> bool {
    let parse = |s: &str| chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M").ok();
    let (Some(start), Some(end)) = (parse(start), parse(end)) else {
        return true;
    };
    let now = match chrono::NaiveTime::from_hms_opt(hour, minute, 0) {
        Some(t) => t,
        None => return true,
    };
    if start == end {
        true
    } else if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

// ── DeferredExecutor ─────────────────────────────────────────────────────────

/// A `ScheduleExecutor` backed by a `OnceCell`.
///
/// Created empty during startup, then filled with a real executor after the
/// agent is constructed. This breaks the circular init dependency:
///
/// ```text
/// deferred_exec = DeferredExecutor::new()       // empty
/// scheduler     = CronSchedulerAdapter::new(..., deferred_exec.clone())
/// agent         = build_goose_backend(..., scheduler.clone(), ...)
/// real_exec     = AgentScheduleExecutor::new(agent, session_storage)
/// deferred_exec.init(real_exec)                  // ← filled
/// ```
pub struct DeferredExecutor {
    inner: OnceCell<Arc<dyn ScheduleExecutor>>,
}

impl DeferredExecutor {
    pub fn new() -> Self {
        Self {
            inner: OnceCell::new(),
        }
    }

    /// First call wins; later calls are no-ops.
    pub async fn init(&self, executor: Arc<dyn ScheduleExecutor>) {
        let _ = self.inner.set(executor);
    }
}

#[async_trait]
impl ScheduleExecutor for DeferredExecutor {
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String> {
        match self.inner.get() {
            Some(exec) => exec.execute(task_id, kind).await,
            None => bail!(
                "task {task_id}: schedule executor not yet initialized \
                 (server is still starting up)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::BoxStream;
    use pond_core::models::ports::agent::{AgentResponse, AgentStreamEvent};
    use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
    use pond_core::user_data::ports::device_control::{DeviceControlOutcome, DeviceStatePatch};

    /// An engine whose model call failed: the adapter reports it as an `Err`.
    struct FailingEngine;

    #[async_trait]
    impl Agent for FailingEngine {
        async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
            bail!("The model could not run: Execution error: null result from llama cpp")
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            unimplemented!("a schedule runs through chat")
        }
    }

    #[tokio::test]
    async fn an_errored_turn_is_a_failed_run_carrying_the_error() {
        let executor = AgentScheduleExecutor::new(
            Arc::new(FailingEngine),
            Arc::new(InMemorySessionStorage::new()),
            None,
            1,
            None,
            None,
        );
        let err = executor
            .execute(
                "task-1",
                &TaskKind::AgentPrompt {
                    prompt: "Water the plants".to_string(),
                },
            )
            .await
            .expect_err("an engine failure must not be recorded as a completed run");
        assert_eq!(
            err.to_string(),
            "The model could not run: Execution error: null result from llama cpp"
        );
    }

    /// Switches every device but `water-heater-plug`, which is offline; keeps what it switched.
    #[derive(Default)]
    struct OfflinePlug(std::sync::Mutex<Vec<String>>);

    #[async_trait]
    impl DeviceControlPort for OfflinePlug {
        async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
            if device_id == "water-heater-plug" {
                bail!("device offline");
            }
            self.0.lock().unwrap().push(device_id.to_string());
            Ok(DeviceControlOutcome::new(
                device_id,
                DeviceStatePatch {
                    on: Some(on),
                    ..Default::default()
                },
            ))
        }
        async fn set_brightness(&self, _: &str, _: u8) -> Result<DeviceControlOutcome> {
            unimplemented!("a routine here only switches power")
        }
        async fn set_target_temp(&self, _: &str, _: f32) -> Result<DeviceControlOutcome> {
            unimplemented!("a routine here only switches power")
        }
        async fn set_locked(&self, _: &str, _: bool) -> Result<DeviceControlOutcome> {
            unimplemented!("a routine here only switches power")
        }
    }

    fn power(device_id: &str) -> TriggerAction {
        TriggerAction::DevicePower {
            device_id: device_id.to_string(),
            on: true,
        }
    }

    #[tokio::test]
    async fn a_routine_runs_every_action_without_the_assistant_and_names_what_failed() {
        let devices = Arc::new(OfflinePlug::default());
        let executor = AgentScheduleExecutor::new(
            Arc::new(FailingEngine),
            Arc::new(InMemorySessionStorage::new()),
            Some(devices.clone()),
            1,
            None,
            None,
        );
        let routine = TaskKind::Routine {
            actions: vec![
                power("water-heater-plug"),
                power("kitchen-lights"),
                power("porch-light"),
            ],
        };

        let err = executor
            .execute("morning", &routine)
            .await
            .expect_err("a routine with a failed device is a failed run");

        assert_eq!(
            *devices.0.lock().unwrap(),
            ["kitchen-lights", "porch-light"]
        );
        let failed = err
            .downcast_ref::<ActionsFailed>()
            .expect("the outcomes travel with it");
        assert_eq!(failed.error, "did not run: water-heater-plug");
        assert!(
            failed.outcomes.contains("kitchen-lights switched on"),
            "{}",
            failed.outcomes
        );
        assert!(
            failed.outcomes.contains("water-heater-plug failed"),
            "{}",
            failed.outcomes
        );
    }

    #[tokio::test]
    async fn a_routine_whose_devices_all_answer_completes() {
        let executor = AgentScheduleExecutor::new(
            Arc::new(FailingEngine),
            Arc::new(InMemorySessionStorage::new()),
            Some(Arc::new(OfflinePlug::default())),
            1,
            None,
            None,
        );
        let routine = TaskKind::Routine {
            actions: vec![power("kitchen-lights")],
        };
        assert_eq!(
            executor.execute("morning", &routine).await.unwrap(),
            "kitchen-lights switched on"
        );
    }
}
