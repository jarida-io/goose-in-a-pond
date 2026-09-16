//! Schedule executors — bridge between the scheduler infrastructure and the
//! agent / webhook execution targets.
//!
//! `AgentScheduleExecutor` sends prompts to the LLM agent and collects the
//! response.  `DeferredExecutor` wraps a `OnceCell` so the scheduler can be
//! constructed before the agent exists (breaking the circular init dependency).

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

/// Executes scheduled tasks by dispatching to the LLM agent, an HTTP webhook,
/// or — for sensor-triggered rules (#92) — a list of actions (agent prompt,
/// device control, notification).
pub struct AgentScheduleExecutor {
    agent: Arc<dyn Agent>,
    session_storage: Arc<dyn SessionStorage>,
    http_client: reqwest::Client,
    /// Actuation seam for rule actions. `None` → device actions report failure.
    device_control: Option<Arc<dyn DeviceControlPort>>,
    /// Limit concurrent scheduled runs to avoid starving interactive chat.
    semaphore: Semaphore,
    /// Speaks a completed `AgentPrompt` task's response out loud, gated by
    /// `speakable_now` below. `None` when no TTS engine is available (see
    /// `speakable_now`'s doc comment for why this does NOT reuse
    /// `ChatService::speak_unprompted`).
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

    /// Whether a scheduled `AgentPrompt`'s response may be spoken aloud right
    /// now.
    ///
    /// This is NOT `ChatService::speak_unprompted` — that gate requires an
    /// [`ProfileScope::Owner`] audience with recent presence evidence, and a
    /// schedule has neither: schedules are not owned by a household member
    /// (see the comment on `profile_scope` in `run_agent_prompt`), so there
    /// is nobody to check presence for. What DOES still apply, because it
    /// isn't about who's in the room: quiet hours (never interrupt sleep for
    /// something nobody asked to hear right now) and the household's
    /// `unprompted_speech_enabled` consent toggle (the same switch that
    /// governs every other proactive utterance). Member-presence gating for
    /// scheduled reminders needs schedules to carry an owner, which they
    /// don't today — a real gap, not one this function papers over.
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

    /// Send `prompt` to the agent in an ephemeral session. Shared by the
    /// `AgentPrompt` kind and the rule `AgentPrompt` action (no recursion —
    /// the semaphore is held once by `execute`).
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
            // A scheduled task has no speaker to identify -- nobody is in the
            // room. It inherits nothing: an explicit Household scope, because
            // a reminder the household set up is household context. Per-member
            // schedules would need an owner on the schedule itself, which does
            // not exist (there is no schedules table at all; the scheduler is
            // in-process).
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
                // Resolved at fire time via the process-global (set during
                // startup, long before any rule can fire) — same pattern as
                // the `send_notification` MCP tool.
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
                // PAI-2 P5. A scheduled webhook POSTs to a URL the user typed
                // in: the most direct exfiltration path in the tree. `begin`
                // gates and starts the clock, `finish` records the outcome
                // either way, so a webhook that times out is still in the feed.
                //
                // There used to be a second, never-constructed executor in
                // pond-infra-scheduler carrying a copy of this gating. It was
                // deleted rather than kept in sync: two paths to gate is how
                // one of them ends up ungated.
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
                // Run every action; report per-action outcomes. The run only
                // counts as failed when *no* action succeeded.
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

/// `true` when `(hour, minute)` falls inside the `[start, end)` quiet window.
/// Wraps midnight when `start > end` (the normal case, e.g. `22:00`/`07:00`).
/// An unparseable bound is treated as "quiet all day" — same "on unreadable
/// input, do less" rule as `ChatService::quiet_hours_cover`, which this
/// mirrors but does not call (that one is private to `pond-core` and typed
/// around `UnpromptedUtterance`'s member-audience shape, which schedules
/// don't have — see `AgentScheduleExecutor::speakable_now`).
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

    /// Fill the executor.  May only be called once; subsequent calls are no-ops.
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
