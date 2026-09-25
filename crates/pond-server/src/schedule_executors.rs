//! Scheduler executors; `DeferredExecutor` lets the scheduler exist before the agent does.

use anyhow::{bail, Result};
use async_trait::async_trait;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::AgentRequest;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::schedule::{TaskKind, TriggerAction};
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
    voice_output: Option<Arc<dyn VoiceOutput>>,
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
            http_client: reqwest::Client::new(),
            device_control,
            semaphore: Semaphore::new(max_concurrent.max(1) as usize),
            voice_output,
            settings_repo,
        }
    }

    /// Checks quiet hours and `unprompted_speech_enabled` only; not `speak_unprompted`'s
    /// presence gate, as schedules have no owner. Member-level gating is a known gap.
    fn speakable_now(settings: &pond_core::user_data::domain::settings::Settings) -> bool {
        if !settings.unprompted_speech_enabled {
            return false;
        }
        let now = chrono::Local::now();
        !quiet_hours_cover_now(
            &settings.quiet_hours_start,
            &settings.quiet_hours_end,
            chrono::Timelike::hour(&now),
            chrono::Timelike::minute(&now),
        )
    }

    /// Prompt the agent in an ephemeral session; the caller (`execute`) holds the semaphore.
    async fn run_agent_prompt(&self, task_id: &str, prompt: &str) -> Result<String> {
        let session_id = format!("sched-{}-{}", task_id, chrono::Utc::now().timestamp());
        let _ = self
            .session_storage
            .create_session(session_id.clone())
            .await;

        let request = AgentRequest {
            message: prompt.to_string(),
            session_id,
            model_role: "task".to_string(),
            images: vec![],
            voice_mode: false,
            canvas_mode: false,
            // Schedules have no owner, so a reminder is household context.
            profile_scope: ProfileScope::Household,
            // Nobody is in the room for a scheduled task.
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        };

        tracing::info!("[scheduler] executing prompt for task {task_id}");
        let response = self.agent.chat(request).await?;
        tracing::info!(
            "[scheduler] task {task_id} completed ({} chars)",
            response.text.len()
        );

        if let (Some(voice), Some(settings_repo)) = (&self.voice_output, &self.settings_repo) {
            match settings_repo.get().await {
                Ok(settings) if Self::speakable_now(&settings) => {
                    tracing::info!("[scheduler] task {task_id}: speaking response aloud");
                    if let Err(e) = voice.speak(&response.text).await {
                        tracing::warn!("[scheduler] task {task_id}: TTS failed: {e}");
                    }
                }
                Ok(_) => tracing::debug!(
                    "[scheduler] task {task_id}: not speaking (consent off or quiet hours)"
                ),
                Err(e) => tracing::warn!(
                    "[scheduler] task {task_id}: could not read settings, staying silent: {e}"
                ),
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
}

#[async_trait]
impl ScheduleExecutor for AgentScheduleExecutor {
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String> {
        let _permit = self.semaphore.acquire().await?;

        match kind {
            TaskKind::AgentPrompt { prompt } => self.run_agent_prompt(task_id, prompt).await,
            TaskKind::Webhook { webhook_url } => {
                tracing::info!("[scheduler] firing webhook for task {task_id}: {webhook_url}");
                // A user-typed URL is the most direct exfiltration path: `begin` gates it and
                // `finish` records every outcome, timeouts included.
                let call = pond_core::shared::services::egress::begin(webhook_url, "POST")
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
