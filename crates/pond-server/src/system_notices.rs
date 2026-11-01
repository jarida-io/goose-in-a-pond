//! Where `serve` notices what the household should hear about while away from the dashboard, and
//! hands it to the notification pipeline. What each notice says, who hears it, the household's
//! switch and the flap damping are pond-core's (`mcp::domain::system_notice`,
//! `mcp::services::system_notices`); the catalogue is `docs/architecture/notifications.md`.
//!
//! - schedule and rule runs: [`spawn_schedule_result_notices`]
//! - reminders that became proposals: [`spawn_proposal_notices`]
//! - parts of the Pond that did not start: [`announce_startup_problems`]
//! - a low disk: [`spawn_disk_watch`]
//! - remote access stopping, or needing a sign-in: [`spawn_remote_access_watch`]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_core::mcp::domain::system_notice::{StartupProblems, SystemEvent};
use pond_core::mcp::ports::notification::{
    MemberDelivery, MemberNotifier, Notification, NotificationSender, OpenScreen,
};
use pond_core::mcp::services::system_notices::{
    disk_is_low, OutageGate, OutageNotice, SystemNotices,
};
use pond_core::user_data::domain::proposal::Proposal;
use pond_core::user_data::domain::schedule::{
    RunStatus, Schedule, ScheduleResultEvent, TaskKind, TriggerAction,
};
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::settings::SettingsRepository;

// Schedule and rule runs

/// The phone notification for a finished schedule run, or `None` for one still running or one
/// skipped, which a household member chose or nobody asked for.
///
/// It names the schedule and the run in its `open` hint, so a tap opens the schedules screen.
pub fn schedule_result_notification(ev: &ScheduleResultEvent) -> Option<Notification> {
    let (category, body) = match ev.status {
        RunStatus::Completed => ("info", ev.result.clone().unwrap_or_default()),
        RunStatus::Failed => (
            "alert",
            ev.error
                .clone()
                .unwrap_or_else(|| "Task failed".to_string()),
        ),
        RunStatus::Running | RunStatus::Skipped => return None,
    };
    let open = OpenScreen::Schedule {
        schedule_id: ev.schedule_id.clone(),
        run_id: ev.run_id.clone(),
    };
    Some(Notification {
        id: uuid::Uuid::new_v4().to_string(),
        target: "broadcast".to_string(),
        category: category.to_string(),
        title: format!("Schedule: {}", ev.schedule_label),
        body,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: Some(open.data()),
    })
}

/// Why a finished run is not announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultSilence {
    /// The household switched off "Tell me when a task finishes" (`schedule_result_notify`).
    SwitchedOff,
    /// A rule with a `Notify` action ran: that action already told the household, in the words
    /// they gave it, so "Schedule: ... notified: ..." would say it twice. A failure still speaks.
    RuleSpokeForItself,
}

/// Whether `kind` is a rule or routine that tells the household itself.
fn notifies_itself(kind: &TaskKind) -> bool {
    let actions = match kind {
        TaskKind::SensorTrigger(spec) => &spec.actions,
        TaskKind::Routine { actions } => actions,
        _ => return false,
    };
    actions
        .iter()
        .any(|a| matches!(a, TriggerAction::Notify { .. }))
}

async fn result_silence(
    ev: &ScheduleResultEvent,
    settings: &dyn SettingsRepository,
    scheduler: Option<&dyn SchedulerPort>,
) -> Option<ResultSilence> {
    match settings.get().await {
        Ok(s) if !s.schedule_result_notify => return Some(ResultSilence::SwitchedOff),
        Ok(_) => {}
        Err(e) => tracing::warn!(
            kind = "schedule_notice_settings_unreadable",
            schedule = %ev.schedule_id,
            error = %e,
            "could not read whether finished tasks are announced; announcing this one"
        ),
    }
    if ev.status != RunStatus::Completed {
        return None;
    }
    let tasks = scheduler?.list_tasks().await.ok()?;
    let task = tasks.iter().find(|t| t.id == ev.schedule_id)?;
    notifies_itself(&task.kind).then_some(ResultSilence::RuleSpokeForItself)
}

/// Announce each finished schedule or rule run to the household's phones, unless the household
/// switched that off or the run was a rule or routine that already notified.
pub fn spawn_schedule_result_notices(
    mut results: tokio::sync::broadcast::Receiver<ScheduleResultEvent>,
    sender: Arc<dyn NotificationSender>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    scheduler: Option<Arc<dyn SchedulerPort>>,
) {
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            let ev = match results.recv().await {
                Ok(ev) => ev,
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(
                        kind = "schedule_notice_lagged",
                        missed,
                        "schedule results arrived faster than they were announced; some were skipped"
                    );
                    continue;
                }
                Err(RecvError::Closed) => break,
            };
            // A running schedule is not news.
            let Some(notification) = schedule_result_notification(&ev) else {
                continue;
            };
            if let Some(silence) =
                result_silence(&ev, settings.as_ref(), scheduler.as_deref()).await
            {
                tracing::debug!(
                    kind = "schedule_notice_skipped",
                    schedule = %ev.schedule_id,
                    reason = ?silence,
                    "finished run not announced"
                );
                continue;
            }
            match sender.broadcast(notification).await {
                Ok(()) => tracing::info!(
                    kind = "schedule_notice_sent",
                    schedule = %ev.schedule_id,
                    run = %ev.run_id,
                    "finished run announced to the household"
                ),
                Err(e) => tracing::warn!(
                    kind = "schedule_notice_failed",
                    schedule = %ev.schedule_id,
                    error = %e,
                    "finished run could not be announced"
                ),
            }
        }
    });
}

// Long tasks

/// The owner's notice that a long task they started has ended. `info` for an answer, `alert`
/// when it could not finish; a tap opens chat, where `task_status` gives the detail.
pub fn long_task_notification(
    task_id: &str,
    goal: &str,
    finished: bool,
    body: String,
) -> Notification {
    let mut fields = serde_json::Map::new();
    fields.insert("long_task_id".into(), task_id.into());
    let title = if finished {
        "Task done"
    } else {
        "Task stopped"
    };
    Notification {
        id: format!("long-task-{task_id}"),
        target: String::new(),
        category: if finished { "info" } else { "alert" }.to_string(),
        title: format!("{title}: {}", truncate_goal(goal)),
        body,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: Some(OpenScreen::Chat.merged_into(fields)),
    }
}

fn truncate_goal(goal: &str) -> String {
    const MAX: usize = 60;
    match goal.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}...", goal[..at].trim_end()),
        None => goal.to_string(),
    }
}

// Missed runs

/// The household's question about a missed run, opening the schedule where it is answered.
/// `action_required`, not `alert`: it waits for a decision and is not read aloud. The id is the
/// schedule and the missed time, so asking again after a restart repeats the same notice.
pub fn missed_run_notification(schedule: &Schedule) -> Option<Notification> {
    let missed = schedule.missed_run?;
    let local = pond_core::shared::services::chat::LocalTimeOfDay::in_zone(
        &schedule.timezone,
        missed.due_at,
    );
    let at = format!("{:02}:{:02}", local.minutes() / 60, local.minutes() % 60);
    Some(Notification {
        id: format!("missed-{}-{}", schedule.id, missed.due_at.timestamp()),
        target: "broadcast".to_string(),
        category: "action_required".to_string(),
        title: schedule.label.clone(),
        body: format!("{} didn't run at {at}. Run it now?", schedule.label),
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: Some(
            OpenScreen::ScheduleDetail {
                schedule_id: schedule.id.clone(),
            }
            .data(),
        ),
    })
}

/// Ask every paired phone about each missed run waiting for an answer, as the pond starts.
pub async fn announce_missed_runs(scheduler: &dyn SchedulerPort, sender: &dyn NotificationSender) {
    let tasks = match scheduler.list_tasks().await {
        Ok(tasks) => tasks,
        Err(e) => {
            tracing::warn!(kind = "missed_run_notice_failed", error = %e, "could not read schedules to ask about missed runs");
            return;
        }
    };
    for notification in tasks.iter().filter_map(missed_run_notification) {
        if let Err(e) = sender.broadcast(notification).await {
            tracing::warn!(kind = "missed_run_notice_failed", error = %e, "could not ask about a missed run");
        }
    }
}

// Proposals

/// The member's notification for a proposal awaiting their decision. `action_required`, not
/// `alert`: the speech gate voices only `alert`, and suggestions shouldn't be read aloud. A tap
/// opens chat.
pub fn proposal_notification(proposal_id: &str, summary: String, rationale: &str) -> Notification {
    let mut fields = serde_json::Map::new();
    fields.insert("proposal_id".into(), proposal_id.into());
    fields.insert("rationale".into(), rationale.into());
    Notification {
        id: format!("proposal-{proposal_id}"),
        target: String::new(),
        category: "action_required".to_string(),
        title: "A suggestion".to_string(),
        body: summary,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: Some(OpenScreen::Chat.merged_into(fields)),
    }
}

/// Tell one member about one proposal on their own phones, never the household's.
pub async fn notify_proposal(notifier: &dyn MemberNotifier, proposal: &Proposal) -> MemberDelivery {
    let member = proposal.audience().profile_id();
    let delivery = notifier
        .notify_member(
            member,
            proposal_notification(proposal.id(), proposal.summary(), proposal.rationale()),
        )
        .await;
    match &delivery {
        MemberDelivery::Reached(devices) => tracing::info!(
            kind = "proposal_notice_sent",
            proposal = %proposal.id(),
            member = %member,
            devices = devices.len(),
            "proposal announced to its member"
        ),
        MemberDelivery::NoPhone => tracing::info!(
            kind = "proposal_notice_no_phone",
            proposal = %proposal.id(),
            member = %member,
            "proposal waits in the app: its member has no paired phone"
        ),
        MemberDelivery::Failed(why) => tracing::warn!(
            kind = "proposal_notice_failed",
            proposal = %proposal.id(),
            member = %member,
            error = %why,
            "proposal could not be announced to its member"
        ),
    }
    delivery
}

/// Announce each proposal sent down `proposals` to its member. The channel lets a producer that
/// starts before the notification pipeline exists (the memory-extraction loop) hand them over.
pub fn spawn_proposal_notices(
    mut proposals: tokio::sync::mpsc::Receiver<Proposal>,
    notifier: Arc<dyn MemberNotifier>,
) {
    tokio::spawn(async move {
        while let Some(proposal) = proposals.recv().await {
            notify_proposal(notifier.as_ref(), &proposal).await;
        }
    });
}

// Startup

/// Where the last startup notice is remembered, so a Pond restarting in a loop says it once.
const STARTUP_NOTICE_FILE: &str = ".startup_notice.json";
/// How long the same startup problems stay announced before a restart repeats them.
const STARTUP_NOTICE_REPEAT_SECS: u64 = 24 * 60 * 60;

#[derive(serde::Serialize, serde::Deserialize)]
struct LastStartupNotice {
    fingerprint: String,
    at_unix: u64,
}

/// Whether problems with `fingerprint` should be announced now, given the last announcement.
fn startup_notice_due(last: Option<&LastStartupNotice>, fingerprint: &str, now_unix: u64) -> bool {
    match last {
        Some(last) if last.fingerprint == fingerprint => {
            now_unix.saturating_sub(last.at_unix) >= STARTUP_NOTICE_REPEAT_SECS
        }
        _ => true,
    }
}

/// Tell the household which parts of the Pond did not start, once per distinct set of problems a
/// day. A clean start forgets the last notice, so the same problem coming back is news again.
pub async fn announce_startup_problems(
    problems: StartupProblems,
    data_dir: &Path,
    notices: &SystemNotices,
) {
    let path = data_dir.join(STARTUP_NOTICE_FILE);
    if problems.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!(
                kind = "startup_notice_cleared",
                "clean start: the last startup problems are behind us"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                kind = "startup_notice_clear_failed",
                error = %e,
                "could not forget the last startup notice"
            ),
        }
        return;
    }
    let fingerprint = problems.fingerprint();
    let last: Option<LastStartupNotice> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let now_unix = chrono::Utc::now().timestamp().max(0) as u64;
    if !startup_notice_due(last.as_ref(), &fingerprint, now_unix) {
        tracing::info!(
            kind = "startup_notice_repeat_skipped",
            problems = %fingerprint,
            "the same startup problems were announced within the day; not repeating them"
        );
        return;
    }
    notices
        .announce(SystemEvent::StartupProblems(problems))
        .await;
    let record = LastStartupNotice {
        fingerprint,
        at_unix: now_unix,
    };
    let written = serde_json::to_vec(&record)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&path, bytes));
    if let Err(e) = written {
        tracing::warn!(
            kind = "startup_notice_record_failed",
            error = %e,
            "could not remember this startup notice; a restart may repeat it"
        );
    }
}

// Disk

const DISK_POLL: Duration = Duration::from_secs(5 * 60);
/// Two readings in a row, so a download that is about to be deleted does not alarm anyone.
const DISK_GRACE: Duration = DISK_POLL;
const DISK_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);
const DISK_COMPONENT: &str = "disk";

/// The low-disk state machine, apart from the clock and the filesystem.
struct DiskWatch {
    gate: OutageGate,
    low: bool,
}

impl DiskWatch {
    fn new() -> Self {
        Self {
            gate: OutageGate::new(DISK_GRACE, DISK_COOLDOWN),
            low: false,
        }
    }

    fn observe(&mut self, free_bytes: u64, total_bytes: u64, now: Instant) -> Option<SystemEvent> {
        self.low = disk_is_low(free_bytes, total_bytes, self.low);
        match self.gate.observe(DISK_COMPONENT, !self.low, now)? {
            OutageNotice::Down => Some(SystemEvent::DiskLow {
                free_bytes,
                total_bytes,
            }),
            OutageNotice::Recovered => Some(SystemEvent::DiskRecovered { free_bytes }),
        }
    }
}

/// Watch free space on the disk holding `data_dir` (models, recordings, both databases).
pub fn spawn_disk_watch(data_dir: PathBuf, notices: Arc<SystemNotices>) {
    tokio::spawn(async move {
        let mut watch = DiskWatch::new();
        let mut unreadable_reported = false;
        let mut interval = tokio::time::interval(DISK_POLL);
        loop {
            interval.tick().await;
            let reading = fs2::available_space(&data_dir)
                .and_then(|free| fs2::total_space(&data_dir).map(|total| (free, total)));
            let (free, total) = match reading {
                Ok(reading) => reading,
                Err(e) => {
                    // Once: the next reading fails the same way, every five minutes.
                    if !unreadable_reported {
                        unreadable_reported = true;
                        tracing::warn!(
                            kind = "disk_watch_unreadable",
                            path = %data_dir.display(),
                            error = %e,
                            "cannot read free disk space; a full disk will not be announced"
                        );
                    }
                    continue;
                }
            };
            unreadable_reported = false;
            if let Some(event) = watch.observe(free, total, Instant::now()) {
                notices.announce(event).await;
            }
        }
    });
}

// Remote access

const REMOTE_POLL: Duration = Duration::from_secs(60);
/// Long enough for the helper's own restart and a coordinator hiccup.
const REMOTE_GRACE: Duration = Duration::from_secs(5 * 60);
const REMOTE_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const REMOTE_COMPONENT: &str = "remote_access";

/// One look at remote access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAccessReading {
    /// The household switched it on.
    pub enabled: bool,
    /// It has reached `Running` on this Pond before, so the household is registered and this is
    /// not a set-up still in progress.
    pub registered: bool,
    /// The networking helper's state: `Running`, `Starting`, `NeedsLogin`, `Unavailable`, ...
    pub state: String,
}

/// The remote-access state machine, apart from the clock and the helper.
struct RemoteAccessWatch {
    gate: OutageGate,
}

impl RemoteAccessWatch {
    fn new() -> Self {
        Self {
            gate: OutageGate::new(REMOTE_GRACE, REMOTE_COOLDOWN),
        }
    }

    fn observe(&mut self, reading: &RemoteAccessReading, now: Instant) -> Option<SystemEvent> {
        // Switched off, or never set up: the household's own doing, so neither down nor back.
        if !reading.enabled || !reading.registered {
            self.gate.forget(REMOTE_COMPONENT);
            return None;
        }
        let running = reading.state == "Running";
        match self.gate.observe(REMOTE_COMPONENT, running, now)? {
            OutageNotice::Recovered => Some(SystemEvent::RemoteAccessRestored),
            OutageNotice::Down
                if matches!(reading.state.as_str(), "NeedsLogin" | "NeedsMachineAuth") =>
            {
                Some(SystemEvent::RemoteAccessNeedsSignIn)
            }
            OutageNotice::Down => Some(SystemEvent::RemoteAccessDown),
        }
    }
}

/// Watch remote access through `read`, which must be cheap: it runs every minute.
pub fn spawn_remote_access_watch<F>(read: F, notices: Arc<SystemNotices>)
where
    F: Fn() -> RemoteAccessReading + Send + 'static,
{
    tokio::spawn(async move {
        let mut watch = RemoteAccessWatch::new();
        let mut interval = tokio::time::interval(REMOTE_POLL);
        loop {
            interval.tick().await;
            if let Some(event) = watch.observe(&read(), Instant::now()) {
                notices.announce(event).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use pond_core::mcp::mocks::mock_notification_sender::{Delivery, MockNotificationSender};
    use pond_core::user_data::domain::proposal::{BusEventRef, ProposalAudience};
    use pond_core::user_data::domain::schedule::{
        SensorTriggerSpec, TriggerCondition, TriggerSource, TriggerSourceKind,
    };
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
    use pond_core::user_data::ports::scheduler::{CreateScheduleRequest, UpdateScheduleRequest};

    const MIN: Duration = Duration::from_secs(60);
    const GB: u64 = 1_000_000_000;

    fn finished(schedule_id: &str, status: RunStatus) -> ScheduleResultEvent {
        ScheduleResultEvent {
            schedule_id: schedule_id.into(),
            schedule_label: "water the plants".into(),
            run_id: "run-1".into(),
            status,
            result: Some("done".into()),
            error: Some("the pump is off".into()),
            duration_ms: Some(10),
        }
    }

    #[test]
    fn a_finished_schedule_notification_opens_its_schedule() {
        let hint = serde_json::json!({"open": "schedule", "schedule_id": "s-1", "run_id": "run-1"});

        let done = schedule_result_notification(&finished("s-1", RunStatus::Completed))
            .expect("a completed run is user-facing");
        assert_eq!(done.category, "info");
        assert_eq!(done.title, "Schedule: water the plants");
        assert_eq!(done.body, "done");
        assert_eq!(done.data, Some(hint.clone()));

        let failed = schedule_result_notification(&finished("s-1", RunStatus::Failed))
            .expect("a failed run is user-facing");
        assert_eq!(failed.category, "alert");
        assert_eq!(failed.body, "the pump is off");
        assert_eq!(failed.data, Some(hint));
    }

    #[test]
    fn a_running_schedule_sends_no_notification() {
        assert!(schedule_result_notification(&finished("s-1", RunStatus::Running)).is_none());
        assert!(schedule_result_notification(&finished("s-1", RunStatus::Skipped)).is_none());
    }

    #[test]
    fn a_proposal_notification_keeps_its_fields_and_opens_chat() {
        let n = proposal_notification("p-1", "Water the plants?".into(), "It is dry");
        assert_eq!(n.id, "proposal-p-1");
        assert_eq!(n.category, "action_required");
        assert_eq!(n.body, "Water the plants?");
        assert_eq!(
            n.data,
            Some(
                serde_json::json!({"proposal_id": "p-1", "rationale": "It is dry", "open": "chat"})
            )
        );
    }

    fn rule(actions: Vec<TriggerAction>) -> TaskKind {
        TaskKind::SensorTrigger(SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Camera,
                device_id: None,
                signal: None,
            },
            condition: TriggerCondition::default(),
            actions,
            cooldown_secs: 60,
        })
    }

    #[test]
    fn only_a_rule_or_routine_with_a_notify_action_speaks_for_itself() {
        assert!(notifies_itself(&rule(vec![TriggerAction::Notify {
            title: "Someone at the door".into(),
            body: "front camera".into(),
        }])));
        assert!(!notifies_itself(&rule(vec![TriggerAction::DevicePower {
            device_id: "porch".into(),
            on: true,
        }])));
        assert!(!notifies_itself(&TaskKind::AgentPrompt {
            prompt: "water the plants".into()
        }));
        assert!(notifies_itself(&TaskKind::Routine {
            actions: vec![TriggerAction::Notify {
                title: "Lights off".into(),
                body: "kitchen".into(),
            }]
        }));
    }

    /// A scheduler that knows one task; nothing else on the port is used here.
    struct OneTask(pond_core::user_data::domain::schedule::Schedule);

    #[async_trait::async_trait]
    impl SchedulerPort for OneTask {
        async fn create_task(
            &self,
            _: CreateScheduleRequest,
        ) -> anyhow::Result<pond_core::user_data::domain::schedule::Schedule> {
            anyhow::bail!("not used")
        }
        async fn list_tasks(
            &self,
        ) -> anyhow::Result<Vec<pond_core::user_data::domain::schedule::Schedule>> {
            Ok(vec![self.0.clone()])
        }
        async fn delete_task(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }
        async fn pause_task(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }
        async fn resume_task(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }
        async fn run_now(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("not used")
        }
        async fn update_task(
            &self,
            _: &str,
            _: UpdateScheduleRequest,
        ) -> anyhow::Result<pond_core::user_data::domain::schedule::Schedule> {
            anyhow::bail!("not used")
        }
        async fn get_runs(
            &self,
            _: &str,
            _: u32,
        ) -> anyhow::Result<Vec<pond_core::user_data::domain::schedule::ScheduleRun>> {
            Ok(vec![])
        }
        async fn list_upcoming(
            &self,
            _: u32,
        ) -> anyhow::Result<Vec<pond_core::user_data::domain::schedule::Schedule>> {
            Ok(vec![])
        }
        async fn set_executor(
            &self,
            _: Arc<dyn pond_core::user_data::ports::schedule_execution::ScheduleExecutor>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn task(id: &str, kind: TaskKind) -> OneTask {
        OneTask(pond_core::user_data::domain::schedule::Schedule {
            ask_if_missed: false,
            missed_run: None,
            id: id.into(),
            label: "door rule".into(),
            cron: String::new(),
            fire_at: None,
            timezone: "UTC".into(),
            kind,
            paused: false,
            currently_running: false,
            last_run: None,
            next_run: None,
            created_at: chrono::Utc::now(),
        })
    }

    #[tokio::test]
    async fn a_missed_run_is_asked_about_on_every_phone_at_its_local_time() {
        let mut missed = task(
            "morning",
            TaskKind::AgentPrompt {
                prompt: "good morning".into(),
            },
        );
        missed.0.label = "Morning routine".into();
        missed.0.timezone = "Africa/Nairobi".into();
        missed.0.missed_run = Some(pond_core::user_data::domain::schedule::MissedRun {
            due_at: "2026-10-29T03:30:00Z".parse().unwrap(),
        });
        let sender = MockNotificationSender::new();

        announce_missed_runs(&missed, &sender).await;

        let sent = sender.sent();
        assert_eq!(sent.len(), 1);
        assert!(matches!(sent[0].0, Delivery::Broadcast));
        let n = &sent[0].1;
        assert_eq!(n.category, "action_required");
        assert_eq!(n.body, "Morning routine didn't run at 06:30. Run it now?");
        assert_eq!(
            n.data,
            Some(serde_json::json!({"open": "schedule", "schedule_id": "morning"}))
        );
    }

    #[tokio::test]
    async fn a_schedule_with_nothing_missed_asks_nothing() {
        let sender = MockNotificationSender::new();
        announce_missed_runs(&task("morning", rule(vec![])), &sender).await;
        assert!(sender.sent().is_empty());
    }

    #[tokio::test]
    async fn a_rule_that_notified_is_not_announced_twice_but_its_failure_is() {
        let settings = MockSettingsRepository::new();
        let scheduler = task(
            "rule-1",
            rule(vec![TriggerAction::Notify {
                title: "t".into(),
                body: "b".into(),
            }]),
        );
        assert_eq!(
            result_silence(
                &finished("rule-1", RunStatus::Completed),
                &settings,
                Some(&scheduler)
            )
            .await,
            Some(ResultSilence::RuleSpokeForItself)
        );
        assert_eq!(
            result_silence(
                &finished("rule-1", RunStatus::Failed),
                &settings,
                Some(&scheduler)
            )
            .await,
            None
        );
        // Any other schedule's result is announced.
        assert_eq!(
            result_silence(
                &finished("other", RunStatus::Completed),
                &settings,
                Some(&scheduler)
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn switching_off_task_results_silences_every_run() {
        let settings = MockSettingsRepository::new();
        settings
            .set_key("schedule_result_notify", "false".into())
            .await
            .unwrap();
        for status in [RunStatus::Completed, RunStatus::Failed] {
            assert_eq!(
                result_silence(&finished("s-1", status), &settings, None).await,
                Some(ResultSilence::SwitchedOff)
            );
        }
    }

    #[tokio::test]
    async fn the_result_task_announces_to_the_household_and_honours_the_switch() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        let sender = Arc::new(MockNotificationSender::new());
        let settings = Arc::new(MockSettingsRepository::new());
        spawn_schedule_result_notices(rx, sender.clone(), settings.clone(), None);

        tx.send(finished("s-1", RunStatus::Running)).unwrap();
        tx.send(finished("s-1", RunStatus::Completed)).unwrap();
        eventually(|| sender.sent().len() == 1).await;
        assert_eq!(sender.sent()[0].0, Delivery::Broadcast);

        settings
            .set_key("schedule_result_notify", "false".into())
            .await
            .unwrap();
        tx.send(finished("s-2", RunStatus::Completed)).unwrap();
        // A marker after it: once the marker is skipped too, the one before it was.
        tx.send(finished("s-3", RunStatus::Failed)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sender.sent().len(), 1, "switched off, nothing more is sent");
    }

    async fn eventually(check: impl Fn() -> bool) {
        for _ in 0..100 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out");
    }

    fn proposal_for(member: &str) -> Proposal {
        Proposal::expiring_after(
            "p-1",
            BusEventRef::new(
                "reminder",
                Some("sess-1".into()),
                Some("dentist".into()),
                chrono::Utc::now(),
            )
            .unwrap(),
            "Jerry mentioned the dentist",
            TaskKind::AgentPrompt {
                prompt: "Remind Jerry about the dentist".into(),
            },
            ProposalAudience::for_member(member).unwrap(),
            0.9,
            chrono::Utc::now(),
            chrono::Duration::hours(12),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_proposal_reaches_only_its_member_and_opens_chat() {
        let notifier = MockMemberNotifier::new().with_devices("jerry", &["jerry-phone"]);
        let delivery = notify_proposal(&notifier, &proposal_for("jerry")).await;
        assert_eq!(
            delivery,
            MemberDelivery::Reached(vec!["jerry-phone".into()])
        );
        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "jerry");
        assert_eq!(sent[0].1.id, "proposal-p-1");
        assert_eq!(sent[0].1.category, "action_required");
        let data = sent[0].1.data.as_ref().unwrap();
        assert_eq!(data["open"], "chat");
        assert_eq!(data["proposal_id"], "p-1");
    }

    #[tokio::test]
    async fn proposals_sent_down_the_channel_are_announced() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("jerry", &["jerry-phone"]));
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        spawn_proposal_notices(rx, notifier.clone());
        tx.send(proposal_for("jerry")).await.unwrap();
        eventually(|| notifier.sent().len() == 1).await;
    }

    #[test]
    fn the_same_startup_problems_are_announced_once_a_day() {
        let last = LastStartupNotice {
            fingerprint: "asr=true;tts=false;extensions=".into(),
            at_unix: 1_000_000,
        };
        assert!(startup_notice_due(None, &last.fingerprint, 1_000_000));
        assert!(!startup_notice_due(
            Some(&last),
            &last.fingerprint,
            1_000_000 + 60
        ));
        assert!(startup_notice_due(
            Some(&last),
            &last.fingerprint,
            1_000_000 + STARTUP_NOTICE_REPEAT_SECS
        ));
        assert!(startup_notice_due(
            Some(&last),
            "asr=true;tts=true;extensions=",
            1_000_000 + 60
        ));
    }

    async fn notices() -> (SystemNotices, Arc<MockNotificationSender>) {
        let sender = Arc::new(MockNotificationSender::new());
        let settings = Arc::new(MockSettingsRepository::new());
        (SystemNotices::new(sender.clone(), settings), sender)
    }

    #[tokio::test]
    async fn a_restart_loop_announces_its_problems_once_and_a_clean_start_resets_that() {
        let dir = tempfile::tempdir().unwrap();
        let (notices, sender) = notices().await;
        let problems = StartupProblems {
            extensions: vec!["weather".into()],
            ..Default::default()
        };
        announce_startup_problems(problems.clone(), dir.path(), &notices).await;
        announce_startup_problems(problems.clone(), dir.path(), &notices).await;
        assert_eq!(sender.sent().len(), 1, "the second start repeats nothing");

        announce_startup_problems(StartupProblems::default(), dir.path(), &notices).await;
        assert!(!dir.path().join(STARTUP_NOTICE_FILE).exists());
        announce_startup_problems(problems, dir.path(), &notices).await;
        assert_eq!(
            sender.sent().len(),
            2,
            "after a clean start, the problem coming back is news"
        );
        assert_eq!(
            sender.sent()[1].1.data.as_ref().unwrap()["kind"],
            "startup_problems"
        );
    }

    #[test]
    fn a_disk_that_fills_is_announced_once_and_its_recovery_once() {
        let mut watch = DiskWatch::new();
        let t0 = Instant::now();
        assert_eq!(watch.observe(10 * GB, 64 * GB, t0), None);
        assert_eq!(
            watch.observe(GB, 64 * GB, t0 + 5 * MIN),
            None,
            "one reading is not enough"
        );
        assert_eq!(
            watch.observe(GB, 64 * GB, t0 + 10 * MIN),
            Some(SystemEvent::DiskLow {
                free_bytes: GB,
                total_bytes: 64 * GB
            })
        );
        assert_eq!(watch.observe(GB, 64 * GB, t0 + 15 * MIN), None);
        // Just above the mark is not yet recovered: no flapping at the boundary.
        assert_eq!(watch.observe(3_500_000_000, 64 * GB, t0 + 20 * MIN), None);
        assert_eq!(
            watch.observe(8 * GB, 64 * GB, t0 + 25 * MIN),
            Some(SystemEvent::DiskRecovered { free_bytes: 8 * GB })
        );
    }

    fn reading(enabled: bool, registered: bool, state: &str) -> RemoteAccessReading {
        RemoteAccessReading {
            enabled,
            registered,
            state: state.into(),
        }
    }

    #[test]
    fn remote_access_that_stops_is_announced_after_the_grace_and_its_return_once() {
        let mut watch = RemoteAccessWatch::new();
        let t0 = Instant::now();
        assert_eq!(watch.observe(&reading(true, true, "Running"), t0), None);
        assert_eq!(
            watch.observe(&reading(true, true, "Unavailable"), t0 + MIN),
            None
        );
        assert_eq!(
            watch.observe(&reading(true, true, "Unavailable"), t0 + 6 * MIN),
            Some(SystemEvent::RemoteAccessDown)
        );
        assert_eq!(
            watch.observe(&reading(true, true, "Unavailable"), t0 + 7 * MIN),
            None
        );
        assert_eq!(
            watch.observe(&reading(true, true, "Running"), t0 + 8 * MIN),
            Some(SystemEvent::RemoteAccessRestored)
        );
    }

    #[test]
    fn a_restart_blip_says_nothing() {
        let mut watch = RemoteAccessWatch::new();
        let t0 = Instant::now();
        watch.observe(&reading(true, true, "Running"), t0);
        assert_eq!(
            watch.observe(&reading(true, true, "Starting"), t0 + MIN),
            None
        );
        assert_eq!(
            watch.observe(&reading(true, true, "Running"), t0 + 2 * MIN),
            None
        );
    }

    #[test]
    fn a_signed_out_node_asks_for_a_sign_in() {
        let mut watch = RemoteAccessWatch::new();
        let t0 = Instant::now();
        watch.observe(&reading(true, true, "NeedsLogin"), t0);
        assert_eq!(
            watch.observe(&reading(true, true, "NeedsLogin"), t0 + 5 * MIN),
            Some(SystemEvent::RemoteAccessNeedsSignIn)
        );
    }

    #[test]
    fn switching_remote_access_off_or_never_finishing_set_up_is_not_an_outage() {
        let mut watch = RemoteAccessWatch::new();
        let t0 = Instant::now();
        // Set-up still waiting for its sign-in: the household's own pending step.
        watch.observe(&reading(true, false, "NeedsLogin"), t0);
        assert_eq!(
            watch.observe(&reading(true, false, "NeedsLogin"), t0 + 30 * MIN),
            None
        );
        // An announced outage, then the household switches it off: no "working again".
        watch.observe(&reading(true, true, "Unavailable"), t0 + 31 * MIN);
        assert_eq!(
            watch.observe(&reading(true, true, "Unavailable"), t0 + 36 * MIN),
            Some(SystemEvent::RemoteAccessDown)
        );
        assert_eq!(
            watch.observe(&reading(false, true, "Stopped"), t0 + 37 * MIN),
            None
        );
        assert_eq!(
            watch.observe(&reading(false, true, "Stopped"), t0 + 90 * MIN),
            None
        );
    }
}
