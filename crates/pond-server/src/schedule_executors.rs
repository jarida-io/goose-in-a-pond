//! Scheduler executors; `DeferredExecutor` lets the scheduler exist before the agent does.

use anyhow::{bail, Result};
use async_trait::async_trait;
use pond_audio_out::AudioBus;
use pond_core::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification};
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::AgentRequest;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::schedule::{TaskKind, TriggerAction};
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::profile::ProfileRepository;
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
    /// Names the member a reminder is for, when it is said aloud.
    profiles: Option<Arc<dyn ProfileRepository>>,
    /// Read when a reminder fires: delivery is installed after the scheduler starts.
    member_notifier: fn() -> Option<Arc<dyn MemberNotifier>>,
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
            profiles: None,
            member_notifier: || None,
        }
    }

    /// Deliver members' reminders to their own phones; without this they fail undelivered.
    pub fn with_member_delivery(
        mut self,
        profiles: Arc<dyn ProfileRepository>,
        member_notifier: fn() -> Option<Arc<dyn MemberNotifier>>,
    ) -> Self {
        self.profiles = Some(profiles);
        self.member_notifier = member_notifier;
        self
    }

    /// Whether a scheduled task may speak now; logs why not.
    async fn may_speak(&self, task_id: &str) -> bool {
        let (Some(_), Some(repo)) = (&self.audio_bus, &self.settings_repo) else {
            return false;
        };
        match repo.get().await {
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
        }
    }

    /// Checks quiet hours and `unprompted_speech_enabled` only; not `speak_unprompted`'s
    /// presence gate, so nothing checks who is in the room. Quiet hours are the household's, so
    /// they are read in its time zone, the one schedules fire in unless they name another, not in
    /// the machine's, which on a headless pond is often UTC.
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

    /// A fresh pond-authored session for one run of `task_id`.
    async fn run_session(&self, task_id: &str) -> String {
        let session_id = format!("sched-{}-{}", task_id, chrono::Utc::now().timestamp());
        let _ = self
            .session_storage
            .create_session(session_id.clone())
            .await;
        session_id
    }

    /// Prompt the agent in an ephemeral session; the caller (`execute`) holds the semaphore.
    async fn run_agent_prompt(&self, task_id: &str, prompt: &str) -> Result<String> {
        let session_id = self.run_session(task_id).await;

        let should_speak = self.may_speak(task_id).await;

        let request = AgentRequest {
            message: prompt.to_string(),
            session_id,
            model_role: "task".to_string(),
            images: vec![],
            voice_mode: should_speak,
            canvas_mode: false,
            // A household schedule has no owner, so its prompt runs in household context.
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

    /// Run a member's reminder as them and send the reply to their own phones. Aloud, and in the
    /// run's result, which the household can read, it says only who it was for and where it went.
    async fn run_member_reminder(
        &self,
        task_id: &str,
        profile_id: &str,
        prompt: &str,
    ) -> Result<String> {
        let session_id = self.run_session(task_id).await;
        let reply = self
            .agent
            .chat(AgentRequest {
                message: prompt.to_string(),
                session_id,
                model_role: "task".to_string(),
                images: vec![],
                voice_mode: false,
                canvas_mode: false,
                profile_scope: ProfileScope::Owner(profile_id.to_string()),
                profile_context: None,
                tool_group_allowlist: None,
                warmup: false,
                suggestion_context: None,
            })
            .await?;

        let delivery = match (self.member_notifier)() {
            Some(notifier) => {
                let notification = Notification {
                    id: uuid::Uuid::new_v4().to_string(),
                    target: profile_id.to_string(),
                    category: "action_required".to_string(),
                    title: "Reminder".to_string(),
                    body: reply.text,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    data: None,
                };
                notifier.notify_member(profile_id, notification).await
            }
            None => MemberDelivery::Failed("member delivery is not set up".to_string()),
        };

        let name = self.member_name(profile_id).await;
        if self.may_speak(task_id).await {
            if let Some(bus) = &self.audio_bus {
                let line = reminder_announcement(name.as_deref(), &delivery);
                if let Err(e) = bus.speak_text(&line, true).await {
                    tracing::warn!("[scheduler] task {task_id}: TTS failed: {e}");
                }
            }
        }
        reminder_outcome(name.as_deref().unwrap_or("its member"), delivery)
    }

    /// What the member is called aloud: their preferred name, else their display name.
    async fn member_name(&self, profile_id: &str) -> Option<String> {
        let profile = self.profiles.as_ref()?.get(profile_id).await.ok()??;
        profile
            .preferences
            .get("preferred_name")
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or_else(|| Some(profile.display_name.trim().to_string()))
            .filter(|n| !n.is_empty())
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
}

#[async_trait]
impl ScheduleExecutor for AgentScheduleExecutor {
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String> {
        let _permit = self.semaphore.acquire().await?;

        match kind {
            TaskKind::AgentPrompt { prompt } => self.run_agent_prompt(task_id, prompt).await,
            TaskKind::MemberReminder { profile_id, prompt } => {
                self.run_member_reminder(task_id, profile_id, prompt).await
            }
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

/// The line said aloud when a member's reminder fires: who it is for, never what it says.
fn reminder_announcement(name: Option<&str>, delivery: &MemberDelivery) -> String {
    let (whom, phone) = match name {
        Some(name) => (format!("{name}, you have a reminder"), "your phone"),
        None => (
            "There's a reminder for someone here".to_string(),
            "their phone",
        ),
    };
    match delivery {
        MemberDelivery::Reached(_) => format!("{whom}. I've sent it to {phone}."),
        MemberDelivery::NoPhone | MemberDelivery::Failed(_) => {
            format!("{whom}, but I couldn't send it to {phone}.")
        }
    }
}

/// The run's result, which the household can read: where the reminder went, never what it said.
fn reminder_outcome(name: &str, delivery: MemberDelivery) -> Result<String> {
    match delivery {
        MemberDelivery::Reached(_) => Ok(format!("Sent to {name}'s phone")),
        MemberDelivery::NoPhone => {
            bail!("{name} has no paired phone, so the reminder was not delivered")
        }
        MemberDelivery::Failed(why) => {
            bail!("the reminder could not be sent to {name}'s phone: {why}")
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
    use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use pond_core::models::ports::agent::{AgentResponse, AgentStreamEvent};
    use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;

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

    /// Answers every turn with the same reply and keeps the requests it was given.
    #[derive(Default)]
    struct RecordingEngine(std::sync::Mutex<Vec<AgentRequest>>);

    #[async_trait]
    impl Agent for RecordingEngine {
        async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
            self.0.lock().unwrap().push(request);
            Ok(AgentResponse {
                text: "Take two of the white tablets with water.".to_string(),
                metadata: Default::default(),
            })
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            unimplemented!("a reminder runs through chat")
        }
    }

    static WITH_PHONE: std::sync::OnceLock<Arc<MockMemberNotifier>> = std::sync::OnceLock::new();
    static WITHOUT_PHONE: std::sync::OnceLock<Arc<MockMemberNotifier>> = std::sync::OnceLock::new();

    /// An executor delivering through `notifier`, and the id of its member Grace.
    async fn reminder_executor(
        engine: Arc<RecordingEngine>,
        notifier: fn() -> Option<Arc<dyn MemberNotifier>>,
    ) -> (AgentScheduleExecutor, String) {
        use pond_core::user_data::domain::profile::CreateProfileRequest;
        use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
        let profiles = Arc::new(MockProfileRepository::new());
        let grace = profiles
            .create(CreateProfileRequest {
                display_name: "Grace".into(),
                avatar_emoji: String::new(),
            })
            .await
            .unwrap();
        let executor = AgentScheduleExecutor::new(
            engine,
            Arc::new(InMemorySessionStorage::new()),
            None,
            1,
            None,
            None,
        )
        .with_member_delivery(profiles, notifier);
        (executor, grace.id)
    }

    fn reminder_for(profile_id: &str) -> TaskKind {
        TaskKind::MemberReminder {
            profile_id: profile_id.to_string(),
            prompt: "Remind me to take my evening tablets".to_string(),
        }
    }

    #[tokio::test]
    async fn a_members_reminder_runs_as_them_and_goes_only_to_their_phone() {
        let engine = Arc::new(RecordingEngine::default());
        let (executor, grace) = reminder_executor(engine.clone(), || {
            WITH_PHONE
                .get()
                .cloned()
                .map(|n| n as Arc<dyn MemberNotifier>)
        })
        .await;
        let notifier = WITH_PHONE
            .get_or_init(|| Arc::new(MockMemberNotifier::new().with_devices(&grace, &["phone-1"])));

        let result = executor
            .execute("task-1", &reminder_for(&grace))
            .await
            .unwrap();

        let requests = engine.0.lock().unwrap().clone();
        assert_eq!(
            requests[0].profile_scope,
            ProfileScope::Owner(grace.clone())
        );
        assert!(!requests[0].voice_mode, "the reply must not be spoken");
        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, grace);
        assert_eq!(sent[0].1.body, "Take two of the white tablets with water.");
        assert_eq!(result, "Sent to Grace's phone");
    }

    #[tokio::test]
    async fn a_reminder_that_reaches_no_phone_fails_without_saying_what_it_was() {
        let engine = Arc::new(RecordingEngine::default());
        WITHOUT_PHONE.get_or_init(|| Arc::new(MockMemberNotifier::new()));
        let (executor, grace) = reminder_executor(engine, || {
            WITHOUT_PHONE
                .get()
                .cloned()
                .map(|n| n as Arc<dyn MemberNotifier>)
        })
        .await;

        let err = executor
            .execute("task-1", &reminder_for(&grace))
            .await
            .expect_err("an undelivered reminder is a failed run");
        assert!(err.to_string().contains("no paired phone"), "{err}");
        assert!(!err.to_string().contains("tablets"), "{err}");
    }

    #[test]
    fn the_spoken_line_says_who_and_where_never_what() {
        let reached = MemberDelivery::Reached(vec!["phone-1".into()]);
        assert_eq!(
            reminder_announcement(Some("Grace"), &reached),
            "Grace, you have a reminder. I've sent it to your phone."
        );
        assert_eq!(
            reminder_announcement(Some("Grace"), &MemberDelivery::NoPhone),
            "Grace, you have a reminder, but I couldn't send it to your phone."
        );
        assert_eq!(
            reminder_announcement(None, &reached),
            "There's a reminder for someone here. I've sent it to their phone."
        );
    }
}
