//! Scheduler executors; `DeferredExecutor` lets the scheduler exist before the agent does.

use anyhow::{bail, Result};
use async_trait::async_trait;
use pond_audio_out::Announcer;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::AgentRequest;
use pond_core::shared::domain::turn_failure::TurnFailure;
use pond_core::user_data::domain::schedule::{
    routine_rejection, ActionsFailed, TaskKind, TriggerAction,
};
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::identity_resolution;
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
    /// Names devices in a run's result and its notification; ids without it.
    device_registry: Option<Arc<dyn DeviceRegistry + Send + Sync>>,
    /// Limit concurrent scheduled runs to avoid starving interactive chat.
    semaphore: Semaphore,
    /// Speaks a finished `AgentPrompt` response, gated by `speakable_now`; `None` without TTS.
    announcer: Option<Announcer>,
    settings_repo: Option<Arc<dyn SettingsRepository>>,
    /// Counted each run: a schedule has no owner, so its scope is a turn nobody identifies.
    profiles: Option<Arc<dyn ProfileRepository + Send + Sync>>,
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
            device_registry: None,
            semaphore: Semaphore::new(max_concurrent.max(1) as usize),
            announcer: voice_output.map(Announcer::new),
            settings_repo,
            profiles: None,
        }
    }

    pub fn with_profiles(mut self, profiles: Arc<dyn ProfileRepository + Send + Sync>) -> Self {
        self.profiles = Some(profiles);
        self
    }

    pub fn with_device_registry(mut self, registry: Arc<dyn DeviceRegistry + Send + Sync>) -> Self {
        self.device_registry = Some(registry);
        self
    }

    /// A device as people know it: its name, or its id when it has none or the registry is down.
    async fn device_name(&self, device_id: &str) -> String {
        let Some(registry) = &self.device_registry else {
            return device_id.to_string();
        };
        match registry.get_device(device_id).await {
            Ok(Some(device)) if !device.name.trim().is_empty() => device.name,
            Ok(_) => device_id.to_string(),
            Err(e) => {
                tracing::debug!(error = %e, "[scheduler] device name unavailable; using its id");
                device_id.to_string()
            }
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

        let should_speak = match (&self.announcer, &self.settings_repo) {
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
            audio: Vec::new(),
            voice_mode: should_speak,
            canvas_mode: false,
            // Schedules have no owner: on a pond of two members a run must not read either's memories.
            profile_scope: identity_resolution::scope_without_evidence(self.profiles.as_deref())
                .await,
            // Nobody is in the room for a scheduled task.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
            suggestion_context: None,
            deep_research_task: None,
            interview: None,
        };

        tracing::info!("[scheduler] executing prompt for task {task_id} (speak={should_speak})");

        // A run that speaks goes through `chat` too, and is spoken only once it has an answer.
        // Streaming the tokens into the speaker spoke goose's "Ran into this error: ..." prose
        // aloud and recorded it, or an empty reply, as a completed run (the Orin's Daily Brief,
        // 2026-10-29 to 2026-11-02): only `chat` checks the turn's provider failure and its text.
        // The tool calls take most of a run, so the first word comes only seconds later.
        //
        // An engine failure is an `Err` from the adapter, never reply text, so the run is failed.
        // The scheduler stores the error's text as the run's, which the household reads. A
        // `TurnFailure`'s text is its plain sentence; the adapter logged the engine's detail.
        let response = match self.agent.chat(request).await {
            Ok(response) => response,
            Err(e) => {
                let failure = e.downcast_ref::<TurnFailure>().copied();
                tracing::warn!(
                    kind = "schedule_prompt_failed",
                    task_id = %task_id,
                    failure = ?failure,
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
            if let Some(announcer) = &self.announcer {
                tracing::info!("[scheduler] task {task_id}: speaking response aloud");
                if let Err(e) = announcer.speak_text(&response.text).await {
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
                        "{} switched {}",
                        self.device_name(device_id).await,
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

    /// What an action acts on, to name it when it fails.
    async fn action_target(&self, action: &TriggerAction) -> String {
        match action {
            TriggerAction::DevicePower { device_id, .. } => self.device_name(device_id).await,
            TriggerAction::Notify { .. } => "the notification".to_string(),
            TriggerAction::AgentPrompt { .. } => "the assistant".to_string(),
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
                    let what = self.action_target(action).await;
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

    /// An engine whose turn failed: the adapter reports it as an `Err` carrying a `TurnFailure`.
    struct FailingEngine(TurnFailure);

    #[async_trait]
    impl Agent for FailingEngine {
        async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
            Err(self.0.into())
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            unimplemented!("a schedule runs through chat, spoken or not")
        }
    }

    /// An engine whose turn answered `self.0`.
    struct AnsweringEngine(&'static str);

    #[async_trait]
    impl Agent for AnsweringEngine {
        async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
            Ok(AgentResponse {
                text: self.0.to_string(),
                metadata: Default::default(),
            })
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            unimplemented!("a schedule runs through chat, spoken or not")
        }
    }

    /// A speaker that counts what it was asked to say.
    #[derive(Default)]
    struct CountingSpeaker(std::sync::atomic::AtomicUsize);

    #[async_trait]
    impl VoiceOutput for CountingSpeaker {
        async fn speak(&self, _text: &str) -> Result<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    /// Settings under which a schedule speaks now: unprompted speech on, quiet hours elsewhere.
    /// The settings mock keeps only some fields, and not these.
    struct SpeakingSettings(pond_core::user_data::domain::settings::Settings);

    impl SpeakingSettings {
        fn now() -> Self {
            let mut settings = pond_core::user_data::domain::settings::Settings {
                unprompted_speech_enabled: true,
                timezone: "UTC".to_string(),
                ..Default::default()
            };
            let now = chrono::Utc::now();
            settings.quiet_hours_start = (now + chrono::Duration::hours(2))
                .format("%H:%M")
                .to_string();
            settings.quiet_hours_end = (now + chrono::Duration::hours(3))
                .format("%H:%M")
                .to_string();
            Self(settings)
        }
    }

    #[async_trait]
    impl SettingsRepository for SpeakingSettings {
        async fn get(&self) -> Result<pond_core::user_data::domain::settings::Settings> {
            Ok(self.0.clone())
        }
        async fn update(&self, _: &pond_core::user_data::domain::settings::Settings) -> Result<()> {
            unimplemented!("a schedule only reads settings")
        }
        async fn get_key(&self, _: &str) -> Result<Option<String>> {
            unimplemented!("a schedule only reads settings")
        }
        async fn set_key(&self, _: &str, _: String) -> Result<()> {
            unimplemented!("a schedule only reads settings")
        }
    }

    async fn run_spoken(agent: Arc<dyn Agent>, speaker: Arc<CountingSpeaker>) -> Result<String> {
        AgentScheduleExecutor::new(
            agent,
            Arc::new(InMemorySessionStorage::new()),
            None,
            1,
            Some(speaker),
            Some(Arc::new(SpeakingSettings::now())),
        )
        .execute(
            "daily-brief",
            &TaskKind::AgentPrompt {
                prompt: "Give me my daily brief".to_string(),
            },
        )
        .await
    }

    /// The Orin's Daily Brief of 2026-11-02 streamed goose's "Ran into this error: ..." into the
    /// speaker and recorded it as the completed run's result; on 2026-10-29 to -31 it recorded an
    /// empty result as completed. A spoken run is now a `chat` turn: failed, and silent.
    #[tokio::test]
    async fn a_spoken_run_whose_turn_failed_is_a_failed_run_and_says_nothing() {
        let speaker = Arc::new(CountingSpeaker::default());
        for failure in [TurnFailure::Engine, TurnFailure::NoReplyAfterTools] {
            let err = run_spoken(Arc::new(FailingEngine(failure)), speaker.clone())
                .await
                .expect_err("a failed turn is a failed run");
            assert_eq!(err.to_string(), failure.user_message());
        }
        assert_eq!(speaker.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_spoken_run_speaks_the_answer_it_records() {
        let speaker = Arc::new(CountingSpeaker::default());
        let result = run_spoken(
            Arc::new(AnsweringEngine("Mainly clear today.")),
            speaker.clone(),
        )
        .await
        .expect("an answered turn is a completed run");
        assert_eq!(result, "Mainly clear today.");
        assert!(speaker.0.load(std::sync::atomic::Ordering::SeqCst) > 0);
    }

    async fn run_failing(failure: TurnFailure) -> anyhow::Error {
        let executor = AgentScheduleExecutor::new(
            Arc::new(FailingEngine(failure)),
            Arc::new(InMemorySessionStorage::new()),
            None,
            1,
            None,
            None,
        );
        executor
            .execute(
                "task-1",
                &TaskKind::AgentPrompt {
                    prompt: "Water the plants".to_string(),
                },
            )
            .await
            .expect_err("a failed turn must not be recorded as a completed run")
    }

    /// The scheduler stores `error.to_string()` as the run's error, which the dashboard and the
    /// phone show; the Orin's run of 2026-10-27 stored llama.cpp's "null result" there.
    #[tokio::test]
    async fn a_model_that_could_not_load_is_a_failed_run_with_a_plain_sentence() {
        let err = run_failing(TurnFailure::ModelUnavailable).await;
        assert_eq!(
            err.to_string(),
            TurnFailure::ModelUnavailable.user_message()
        );
        assert_eq!(
            err.downcast_ref::<TurnFailure>(),
            Some(&TurnFailure::ModelUnavailable),
            "the kind stays typed for anything downstream"
        );
    }

    #[tokio::test]
    async fn a_turn_that_called_tools_but_never_answered_is_a_failed_run_with_a_plain_sentence() {
        let err = run_failing(TurnFailure::NoReplyAfterTools).await;
        assert_eq!(
            err.to_string(),
            TurnFailure::NoReplyAfterTools.user_message()
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
            Arc::new(FailingEngine(TurnFailure::Engine)),
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

    /// Names two devices; any other has no row.
    struct NamedDevices;

    #[async_trait]
    impl DeviceRegistry for NamedDevices {
        async fn register(
            &self,
            _: pond_core::user_data::ports::device_registry::RegisterDeviceRequest,
        ) -> Result<pond_core::user_data::ports::device_registry::Device> {
            unimplemented!("a routine only reads names")
        }
        async fn list_devices(
            &self,
        ) -> Result<Vec<pond_core::user_data::ports::device_registry::Device>> {
            unimplemented!("a routine reads one device at a time")
        }
        async fn get_device(
            &self,
            id: &str,
        ) -> Result<Option<pond_core::user_data::ports::device_registry::Device>> {
            let name = match id {
                "kitchen-lights" => "Kitchen",
                "water-heater-plug" => "Water heater",
                _ => return Ok(None),
            };
            Ok(Some(pond_core::user_data::ports::device_registry::Device {
                id: id.to_string(),
                name: name.to_string(),
                device_type: "light".to_string(),
                hostname: None,
                ip_address: None,
                capabilities: vec![],
                registered_at: "2026-11-05T00:00:00Z".to_string(),
                last_seen: None,
                is_online: true,
                room_id: None,
            }))
        }
        async fn unregister(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn rename(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_routine_names_devices_as_people_know_them() {
        let executor = AgentScheduleExecutor::new(
            Arc::new(FailingEngine(TurnFailure::Engine)),
            Arc::new(InMemorySessionStorage::new()),
            Some(Arc::new(OfflinePlug::default())),
            1,
            None,
            None,
        )
        .with_device_registry(Arc::new(NamedDevices));
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
            .expect_err("the water heater is offline");
        let failed = err.downcast_ref::<ActionsFailed>().expect("outcomes kept");

        assert_eq!(failed.error, "did not run: Water heater");
        assert!(
            failed.outcomes.contains("Kitchen switched on"),
            "{}",
            failed.outcomes
        );
        // No row: its id is all there is to say.
        assert!(
            failed.outcomes.contains("porch-light switched on"),
            "{}",
            failed.outcomes
        );
    }

    #[tokio::test]
    async fn a_routine_whose_devices_all_answer_completes() {
        let executor = AgentScheduleExecutor::new(
            Arc::new(FailingEngine(TurnFailure::Engine)),
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
