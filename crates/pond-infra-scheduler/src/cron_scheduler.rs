//! `SchedulerPort` on `tokio-cron-scheduler`, with tasks persisted to JSON and rebuilt on start.

use crate::run_history::JsonRunHistory;
use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::schedule::{
    RunStatus, Schedule, ScheduleRun, TaskKind, CRON_ONCE,
};
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job, JobScheduler};

// ── Persisted record ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTask {
    id: String,
    label: String,
    cron: String,
    /// IANA timezone.  Defaults to "UTC" for legacy tasks.
    #[serde(default = "default_timezone")]
    timezone: String,
    /// `None` only in legacy files; filled from `payload` during rehydration.
    #[serde(default)]
    kind: Option<TaskKind>,
    /// Legacy field — kept for backward-compat deserialization.
    #[serde(default)]
    payload: Option<serde_json::Value>,
    paused: bool,
    #[serde(default)]
    created_at: Option<chrono::DateTime<Utc>>,
    /// When the task last fired (not finished). See [`durable_fire_stamp`] for when it hits disk.
    #[serde(default)]
    last_run: Option<chrono::DateTime<Utc>>,
    /// Fire once at this instant, then delete; `None` for a recurring task.
    #[serde(default)]
    fire_at: Option<chrono::DateTime<Utc>>,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

// ── In-memory state ───────────────────────────────────────────────────────────

struct TaskEntry {
    persisted: PersistedTask,
    job_id: uuid::Uuid,
    currently_running: bool,
    last_run: Option<chrono::DateTime<Utc>>,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct CronSchedulerAdapter {
    scheduler: JobScheduler,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    persist: Arc<SnapshotWriter>,
    executor: Arc<dyn ScheduleExecutor>,
    run_history: Arc<JsonRunHistory>,
    /// Optional broadcast sender for schedule result events (SSE delivery).
    result_tx: Option<
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    >,
}

impl CronSchedulerAdapter {
    /// Creates the scheduler, rehydrating tasks from `persist_path`; history goes to `runs_path`.
    pub async fn new(
        persist_path: PathBuf,
        runs_path: PathBuf,
        executor: Arc<dyn ScheduleExecutor>,
    ) -> Result<Self> {
        Self::with_options(persist_path, runs_path, executor, None, 50).await
    }

    pub async fn with_options(
        persist_path: PathBuf,
        runs_path: PathBuf,
        executor: Arc<dyn ScheduleExecutor>,
        result_tx: Option<
            tokio::sync::broadcast::Sender<
                pond_core::user_data::domain::schedule::ScheduleResultEvent,
            >,
        >,
        max_runs_per_task: u32,
    ) -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        let tasks: Arc<Mutex<HashMap<String, TaskEntry>>> = Arc::new(Mutex::new(HashMap::new()));
        let run_history = Arc::new(JsonRunHistory::new(runs_path, max_runs_per_task).await?);

        let adapter = Self {
            scheduler,
            tasks,
            persist: Arc::new(SnapshotWriter::new(persist_path)),
            executor,
            run_history,
            result_tx,
        };

        adapter.rehydrate().await?;

        adapter.scheduler.start().await?;

        Ok(adapter)
    }

    // ── Persistence helpers ───────────────────────────────────────────────────

    async fn save(&self) -> Result<()> {
        let guard = self.tasks.lock().await;
        // Ticket under the tasks lock: it orders snapshots by when their contents were read.
        let seq = self.persist.ticket();
        let records: Vec<PersistedTask> = guard.values().map(|e| e.persisted.clone()).collect();
        drop(guard);

        self.persist.publish(seq, &records).await
    }

    /// Records that `id` fired, at run start so a crash mid-run still counts as a fire.
    /// Takes the pieces, not `&self`: both fire paths run in a spawned task.
    async fn stamp_fire(
        tasks: &Arc<Mutex<HashMap<String, TaskEntry>>>,
        persist: &Arc<SnapshotWriter>,
        id: &str,
    ) {
        let now = Utc::now();
        let (seq, records): (u64, Vec<PersistedTask>) = {
            let mut guard = tasks.lock().await;
            let Some(entry) = guard.get_mut(id) else {
                return;
            };
            // Memory for every kind (the UI shows it); `durable_fire_stamp` gates only the write.
            entry.last_run = Some(now);
            entry.persisted.last_run = Some(now);
            if !durable_fire_stamp(&Self::resolve_kind(entry)) {
                return;
            }
            // Ticket under the lock, as `save` does, so a racing API edit can't land older state.
            (
                persist.ticket(),
                guard.values().map(|e| e.persisted.clone()).collect(),
            )
        };

        if let Err(e) = persist.publish(seq, &records).await {
            // Warn: a lost stamp re-fires the rule after the next restart.
            tracing::warn!(task = %id, error = %e, "could not persist fire stamp");
        }
    }

    async fn rehydrate(&self) -> Result<()> {
        let path = self.persist.path();
        if !path.exists() {
            return Ok(());
        }

        let json = tokio::fs::read_to_string(path).await?;
        let records: Vec<PersistedTask> = match serde_json::from_str(&json) {
            Ok(records) => records,
            Err(e) => {
                // An Err here means 503 on every schedule endpoint, every boot. Start empty, but
                // move the file aside: it's the household's only copy of its automations.
                let kept = quarantine_unreadable(path).await;
                let kept = kept
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "NOT SAVED".to_string());
                tracing::error!(
                    path = %path.display(),
                    kept = %kept,
                    error = %e,
                    "schedules.json is unreadable: starting with NO schedules and NO rules"
                );
                return Ok(());
            }
        };

        let mut migrated = false;
        for mut record in records {
            // Legacy tasks have `payload` but no `kind`.
            if record.kind.is_none() {
                record.kind = Some(migrate_kind_from_payload(&record));
                migrated = true;
            }
            if record.created_at.is_none() {
                record.created_at = Some(Utc::now());
                migrated = true;
            }

            // Paused tasks and event rules get the nil "no job" id. Test `fire_at` before `cron`:
            // parsing a one-shot's `"@once"` fails `rehydrate()` and with it every schedule.
            let job_id = if record.paused {
                uuid::Uuid::nil()
            } else {
                let kind = record.kind.clone().unwrap();
                if kind.is_event_triggered() {
                    uuid::Uuid::nil()
                } else if let Some(at) = record.fire_at {
                    self.add_one_shot_to_scheduler(&record.id, at, kind).await?
                } else {
                    self.add_job_to_scheduler(&record.id, &record.cron, kind)
                        .await?
                }
            };

            // One insert for paused and active tasks alike, so neither can drop the fire stamp.
            let last_run = record.last_run;
            let mut guard = self.tasks.lock().await;
            guard.insert(
                record.id.clone(),
                TaskEntry {
                    persisted: record,
                    job_id,
                    currently_running: false,
                    last_run,
                },
            );
        }

        if migrated {
            self.save().await?;
        }

        Ok(())
    }

    // ── Internal job management ───────────────────────────────────────────────

    async fn add_job_to_scheduler(
        &self,
        task_id: &str,
        cron: &str,
        kind: TaskKind,
    ) -> Result<uuid::Uuid> {
        let executor = self.executor.clone();
        let tasks = self.tasks.clone();
        let run_history = self.run_history.clone();
        let result_tx = self.result_tx.clone();
        let persist = self.persist.clone();
        let id = task_id.to_string();

        let job = Job::new_async(cron, move |_uuid, _lock| {
            let ctx = TaskRunContext {
                executor: executor.clone(),
                tasks: tasks.clone(),
                run_history: run_history.clone(),
                result_tx: result_tx.clone(),
                persist: persist.clone(),
                id: id.clone(),
                kind: kind.clone(),
            };
            Box::pin(async move { ctx.run().await })
        })
        .map_err(|e| anyhow::anyhow!("invalid cron expression '{cron}': {e}"))?;

        let job_id = self.scheduler.add(job).await?;
        Ok(job_id)
    }

    /// Registers a task that fires once at `at`, then removes itself. A past `at` fires now:
    /// a timer the pond slept through is late, not cancelled.
    async fn add_one_shot_to_scheduler(
        &self,
        task_id: &str,
        at: chrono::DateTime<Utc>,
        kind: TaskKind,
    ) -> Result<uuid::Uuid> {
        let delay = (at - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);

        let executor = self.executor.clone();
        let tasks = self.tasks.clone();
        let run_history = self.run_history.clone();
        let result_tx = self.result_tx.clone();
        let persist = self.persist.clone();
        let id = task_id.to_string();

        let job = Job::new_one_shot_async(delay, move |_uuid, _lock| {
            let ctx = TaskRunContext {
                executor: executor.clone(),
                tasks: tasks.clone(),
                run_history: run_history.clone(),
                result_tx: result_tx.clone(),
                persist: persist.clone(),
                id: id.clone(),
                kind: kind.clone(),
            };
            Box::pin(async move {
                let tasks = ctx.tasks.clone();
                let persist = ctx.persist.clone();
                let id = ctx.id.clone();
                ctx.run().await;
                // Delete, don't pause: spent one-shots clutter `list_schedules` for the model,
                // and a resume would target a moment already past.
                let records = {
                    let mut guard = tasks.lock().await;
                    guard.remove(&id);
                    guard
                        .values()
                        .map(|e| e.persisted.clone())
                        .collect::<Vec<_>>()
                };
                let seq = persist.ticket();
                if let Err(e) = persist.publish(seq, &records).await {
                    tracing::warn!("one-shot {id} fired but could not be removed from disk: {e}");
                }
            })
        })
        .map_err(|e| anyhow::anyhow!("could not schedule a one-shot for {at}: {e}"))?;

        let job_id = self.scheduler.add(job).await?;
        Ok(job_id)
    }

    fn resolve_kind(entry: &TaskEntry) -> TaskKind {
        entry
            .persisted
            .kind
            .clone()
            .unwrap_or_else(|| migrate_kind_from_payload(&entry.persisted))
    }

    fn to_schedule(entry: &TaskEntry) -> Schedule {
        let next_run = if entry.persisted.paused {
            None
        } else if let Some(at) = entry.persisted.fire_at {
            // `compute_next_run` can't parse the "@once" sentinel and would report "never runs".
            Some(at)
        } else {
            compute_next_run(&entry.persisted.cron, &entry.persisted.timezone)
        };
        Schedule {
            id: entry.persisted.id.clone(),
            label: entry.persisted.label.clone(),
            cron: entry.persisted.cron.clone(),
            fire_at: entry.persisted.fire_at,
            timezone: entry.persisted.timezone.clone(),
            kind: Self::resolve_kind(entry),
            paused: entry.persisted.paused,
            currently_running: entry.currently_running,
            last_run: entry.last_run,
            next_run,
            created_at: entry.persisted.created_at.unwrap_or_else(Utc::now),
        }
    }
}

/// Next fire time of `cron_expr` in IANA `timezone` (UTC if unparseable); `None` if invalid.
fn compute_next_run(cron_expr: &str, timezone: &str) -> Option<chrono::DateTime<Utc>> {
    // croner 3 accepts 5- and 6-field cron by default. Keep croner on `tokio-cron-scheduler`'s
    // major: that copy decides when jobs fire, this one what `next_run` promises.
    let cron: croner::Cron = cron_expr.parse().ok()?;
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    let now = Utc::now().with_timezone(&tz);
    match cron.find_next_occurrence(&now, false) {
        Ok(dt) => Some(dt.with_timezone(&Utc)),
        Err(e) => {
            tracing::debug!(
                cron_expr,
                error = %e,
                "Failed to compute next_run for cron expression"
            );
            None
        }
    }
}

/// Refuses a kind the scheduler must not store; every create and update passes here.
fn validate_kind(kind: &TaskKind) -> Result<()> {
    if let TaskKind::SensorTrigger(spec) = kind {
        spec.validate()
            .map_err(|rejected| anyhow::anyhow!("{rejected}"))?;
    }
    Ok(())
}

/// Whether a fire alone is worth a file rewrite: only cooldown rules read the stamp back, and
/// the cooldown caps those writes; other stamps ride along on the next save, sparing flash.
fn durable_fire_stamp(kind: &TaskKind) -> bool {
    matches!(kind, TaskKind::SensorTrigger(spec) if spec.cooldown_secs > 0)
}

/// Publishes `schedules.json`: one writer at a time, each through its own scratch file, and in
/// ticket (content) order, so a slow writer can't publish older state over newer.
struct SnapshotWriter {
    path: PathBuf,
    next_seq: AtomicU64,
    /// Highest published ticket. Held across write and rename so nothing lands after the check.
    published: Mutex<u64>,
}

impl SnapshotWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            // Start at 1: 0 means "nothing published yet".
            next_seq: AtomicU64::new(1),
            published: Mutex::new(0),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Call while holding the lock the records are read under, or it orders writers, not contents.
    fn ticket(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Publishes `records` under ticket `seq`, unless something newer is already on disk.
    async fn publish(&self, seq: u64, records: &[PersistedTask]) -> Result<()> {
        let mut published = self.published.lock().await;
        if *published > seq {
            // Snapshots are the whole map, so the newer one on disk supersedes this.
            return Ok(());
        }
        write_snapshot(&self.path, &temp_path(&self.path, seq), records).await?;
        *published = seq;
        Ok(())
    }
}

/// Scratch path for one write, unique by ticket and pid. A sibling of the target, because
/// `rename(2)` is only atomic within one filesystem.
fn temp_path(persist_path: &Path, seq: u64) -> PathBuf {
    let name = persist_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("schedules.json");
    persist_path.with_file_name(format!(".{name}.{}.{seq}.tmp", std::process::id()))
}

/// Write the task list through `tmp`, then publish it with a rename.
async fn write_snapshot(persist_path: &Path, tmp: &Path, records: &[PersistedTask]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let json = serde_json::to_string_pretty(records)?;
    if let Some(parent) = persist_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent).await?;
    }

    let staged = async {
        let mut file = tokio::fs::File::create(tmp).await?;
        file.write_all(json.as_bytes()).await?;
        // Sync before the rename, or a power cut can publish an empty or torn file.
        file.sync_all().await
    }
    .await;
    if let Err(e) = staged {
        // Nothing was published, so leave nothing behind either.
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(e.into());
    }

    tokio::fs::rename(tmp, persist_path).await?;

    // The rename already took effect, so a failed dir flush only costs power-cut durability.
    if let Some(parent) = persist_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Err(e) = fsync_dir(parent).await {
            tracing::debug!(dir = %parent.display(), error = %e, "could not flush the schedules directory");
        }
    }
    Ok(())
}

/// `fsync` a directory, so a rename inside it survives a power cut.
async fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::File::open(dir)?.sync_all())
        .await
        .map_err(std::io::Error::other)?
}

/// Renames an unreadable `schedules.json` aside (never deletes it); `None` if that failed too.
async fn quarantine_unreadable(persist_path: &Path) -> Option<PathBuf> {
    let name = persist_path.file_name().and_then(|n| n.to_str())?;
    let kept = persist_path.with_file_name(format!(
        "{name}.unreadable-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.6fZ")
    ));
    match tokio::fs::rename(persist_path, &kept).await {
        Ok(()) => Some(kept),
        Err(e) => {
            tracing::error!(error = %e, "could not move the unreadable schedules file aside");
            None
        }
    }
}

/// Infer `TaskKind` from a legacy `payload` field.
fn migrate_kind_from_payload(record: &PersistedTask) -> TaskKind {
    if let Some(payload) = &record.payload {
        if let Some(url) = payload.get("webhook_url").and_then(|v| v.as_str()) {
            return TaskKind::Webhook {
                webhook_url: url.to_string(),
            };
        }
        if let Some(prompt) = payload.get("prompt").and_then(|v| v.as_str()) {
            return TaskKind::AgentPrompt {
                prompt: prompt.to_string(),
            };
        }
    }
    TaskKind::AgentPrompt {
        prompt: record.label.clone(),
    }
}

// ── SchedulerPort impl ────────────────────────────────────────────────────────

#[async_trait]
impl SchedulerPort for CronSchedulerAdapter {
    async fn create_task(&self, req: CreateScheduleRequest) -> Result<Schedule> {
        // Validated at the store: the `create_sensor_rule` MCP tool bypasses the API checks.
        validate_kind(&req.kind)?;

        {
            let guard = self.tasks.lock().await;
            if guard.contains_key(&req.id) {
                bail!("task '{}' already exists", req.id);
            }
        }

        // `once` resolves to the cron's next occurrence; below, only `fire_at` marks a one-shot.
        let fire_at = req.fire_at.or_else(|| {
            if req.once {
                compute_next_run(&req.cron, &req.timezone)
            } else {
                None
            }
        });
        if req.fire_at.is_none() && req.once && fire_at.is_none() {
            bail!(
                "cannot create a one-shot: '{}' is not a valid cron expression \
                 to derive a next occurrence from",
                req.cron
            );
        }
        let cron = if fire_at.is_some() {
            CRON_ONCE.to_string()
        } else {
            req.cron.clone()
        };

        let record = PersistedTask {
            id: req.id.clone(),
            label: req.label.clone(),
            cron: cron.clone(),
            timezone: req.timezone.clone(),
            kind: Some(req.kind.clone()),
            payload: None,
            paused: false,
            created_at: Some(Utc::now()),
            last_run: None,
            fire_at,
        };

        // Event rules get no job (the rules engine fires them via `run_now`); nil id means no job.
        let job_id = if req.kind.is_event_triggered() {
            uuid::Uuid::nil()
        } else if let Some(at) = fire_at {
            self.add_one_shot_to_scheduler(&req.id, at, req.kind.clone())
                .await?
        } else {
            self.add_job_to_scheduler(&req.id, &cron, req.kind.clone())
                .await?
        };

        let next_run = if req.kind.is_event_triggered() {
            None
        } else if let Some(at) = fire_at {
            Some(at)
        } else {
            compute_next_run(&cron, &req.timezone)
        };
        let schedule = Schedule {
            id: req.id.clone(),
            label: req.label,
            cron,
            fire_at,
            timezone: req.timezone,
            kind: req.kind,
            last_run: None,
            next_run,
            paused: false,
            currently_running: false,
            created_at: record.created_at.unwrap(),
        };

        {
            let mut guard = self.tasks.lock().await;
            guard.insert(
                req.id,
                TaskEntry {
                    persisted: record,
                    job_id,
                    currently_running: false,
                    last_run: None,
                },
            );
        }

        self.save().await?;
        Ok(schedule)
    }

    async fn list_tasks(&self) -> Result<Vec<Schedule>> {
        let guard = self.tasks.lock().await;
        let tasks = guard.values().map(Self::to_schedule).collect();
        Ok(tasks)
    }

    async fn delete_task(&self, id: &str) -> Result<()> {
        let job_id = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .remove(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            entry.job_id
        };

        if job_id != uuid::Uuid::nil() {
            self.scheduler.remove(&job_id).await?;
        }

        self.save().await?;
        Ok(())
    }

    async fn pause_task(&self, id: &str) -> Result<()> {
        let job_id = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if entry.persisted.paused {
                return Ok(());
            }
            entry.persisted.paused = true;
            let jid = entry.job_id;
            entry.job_id = uuid::Uuid::nil();
            jid
        };

        if job_id != uuid::Uuid::nil() {
            self.scheduler.remove(&job_id).await?;
        }

        self.save().await?;
        Ok(())
    }

    async fn resume_task(&self, id: &str) -> Result<()> {
        let (cron, kind) = {
            let guard = self.tasks.lock().await;
            let entry = guard
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if !entry.persisted.paused {
                return Ok(());
            }
            (entry.persisted.cron.clone(), Self::resolve_kind(entry))
        };

        // Event rules have no job to re-register; the rules engine checks `paused` itself.
        let job_id = if kind.is_event_triggered() {
            uuid::Uuid::nil()
        } else {
            self.add_job_to_scheduler(id, &cron, kind).await?
        };

        {
            let mut guard = self.tasks.lock().await;
            if let Some(entry) = guard.get_mut(id) {
                entry.persisted.paused = false;
                entry.job_id = job_id;
            }
        }

        self.save().await?;
        Ok(())
    }

    async fn run_now(&self, id: &str) -> Result<()> {
        use pond_core::user_data::domain::schedule::ScheduleResultEvent;

        let (kind, label) = {
            let guard = self.tasks.lock().await;
            let entry = guard
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            (Self::resolve_kind(entry), entry.persisted.label.clone())
        };

        let executor = self.executor.clone();
        let tasks = self.tasks.clone();
        let run_history = self.run_history.clone();
        let result_tx = self.result_tx.clone();
        let persist = self.persist.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            {
                let mut guard = tasks.lock().await;
                if let Some(entry) = guard.get_mut(&id) {
                    entry.currently_running = true;
                }
            }
            // Rules fire via `run_now`; their cooldown reads this stamp back after a restart.
            CronSchedulerAdapter::stamp_fire(&tasks, &persist, &id).await;

            let run_id = run_history.record_start(&id).await;
            let start = std::time::Instant::now();

            // Broadcast "started" event so clients see progress immediately
            if let Some(tx) = &result_tx {
                let _ = tx.send(ScheduleResultEvent {
                    schedule_id: id.clone(),
                    schedule_label: label.clone(),
                    run_id: run_id.clone(),
                    status: RunStatus::Running,
                    result: None,
                    error: None,
                    duration_ms: None,
                });
            }

            let result = executor.execute(&id, &kind).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            let (status, result_text, error_text) = match &result {
                Ok(text) => {
                    run_history
                        .record_finish(&run_id, RunStatus::Completed, Some(text.clone()), None)
                        .await;
                    (RunStatus::Completed, Some(text.clone()), None)
                }
                Err(e) => {
                    tracing::error!("run_now task {id} failed: {e}");
                    run_history
                        .record_finish(&run_id, RunStatus::Failed, None, Some(e.to_string()))
                        .await;
                    (RunStatus::Failed, None, Some(e.to_string()))
                }
            };

            if let Some(tx) = &result_tx {
                let _ = tx.send(ScheduleResultEvent {
                    schedule_id: id.clone(),
                    schedule_label: label,
                    run_id: run_id.clone(),
                    status,
                    result: result_text,
                    error: error_text,
                    duration_ms: Some(duration_ms),
                });
            }

            // Mark not-running. `last_run` was stamped at fire time above.
            {
                let mut guard = tasks.lock().await;
                if let Some(entry) = guard.get_mut(&id) {
                    entry.currently_running = false;
                }
            }
        });

        Ok(())
    }

    async fn update_task(&self, id: &str, req: UpdateScheduleRequest) -> Result<Schedule> {
        // Validated at the store, as in `create_task`.
        if let Some(kind) = &req.kind {
            validate_kind(kind)?;
        }

        // `fire_at` (given, or derived via `once`) makes a one-shot with the `"@once"` cron;
        // a real `cron` alone makes it recurring again and clears `fire_at`.
        let shape_changed = req.cron.is_some() || req.fire_at.is_some() || req.once;

        // Read current state and apply non-shape changes first.
        let (old_job_id, new_cron, new_fire_at, new_kind, was_paused) = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;

            if let Some(label) = &req.label {
                entry.persisted.label = label.clone();
            }
            if let Some(tz) = &req.timezone {
                entry.persisted.timezone = tz.clone();
            }
            if let Some(kind) = &req.kind {
                entry.persisted.kind = Some(kind.clone());
            }

            let old_job_id = entry.job_id;
            let paused = entry.persisted.paused;
            // Use the NEW shape for scheduling but don't commit it to metadata yet.
            let (cron, fire_at) = if let Some(at) = req.fire_at {
                (CRON_ONCE.to_string(), Some(at))
            } else if req.once {
                let basis_cron = req.cron.as_deref().unwrap_or(&entry.persisted.cron);
                let basis_tz = req.timezone.as_deref().unwrap_or(&entry.persisted.timezone);
                let at = compute_next_run(basis_cron, basis_tz).ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot switch '{id}' to a one-shot: '{basis_cron}' is not a valid \
                         cron expression to derive a next occurrence from"
                    )
                })?;
                (CRON_ONCE.to_string(), Some(at))
            } else if let Some(cron) = &req.cron {
                (cron.clone(), None)
            } else {
                (entry.persisted.cron.clone(), entry.persisted.fire_at)
            };
            let kind = Self::resolve_kind(entry);
            (old_job_id, cron, fire_at, kind, paused)
        };

        // Create the new job first, so a bad cron leaves the old job running.
        if shape_changed && !was_paused {
            let new_job_id = if let Some(at) = new_fire_at {
                self.add_one_shot_to_scheduler(id, at, new_kind).await?
            } else {
                self.add_job_to_scheduler(id, &new_cron, new_kind).await?
            };
            if old_job_id != uuid::Uuid::nil() {
                let _ = self.scheduler.remove(&old_job_id).await;
            }
            let mut guard = self.tasks.lock().await;
            if let Some(entry) = guard.get_mut(id) {
                entry.persisted.cron = new_cron;
                entry.persisted.fire_at = new_fire_at;
                entry.job_id = new_job_id;
            }
        } else if shape_changed && was_paused {
            // Schedule is paused — just update the stored shape (no active job to replace).
            let mut guard = self.tasks.lock().await;
            if let Some(entry) = guard.get_mut(id) {
                entry.persisted.cron = new_cron;
                entry.persisted.fire_at = new_fire_at;
            }
        }

        self.save().await?;

        let guard = self.tasks.lock().await;
        let entry = guard
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
        Ok(Self::to_schedule(entry))
    }

    async fn get_runs(&self, schedule_id: &str, limit: u32) -> Result<Vec<ScheduleRun>> {
        Ok(self.run_history.get_runs(schedule_id, limit).await)
    }

    async fn list_upcoming(&self, limit: u32) -> Result<Vec<Schedule>> {
        let guard = self.tasks.lock().await;
        let mut schedules: Vec<Schedule> = guard
            .values()
            .filter(|e| !e.persisted.paused)
            .map(Self::to_schedule)
            .collect();
        // Sort by next fire time (soonest first); schedules without next_run sort last.
        schedules.sort_by(|a, b| match (&a.next_run, &b.next_run) {
            (Some(a_next), Some(b_next)) => a_next.cmp(b_next),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.created_at.cmp(&b.created_at),
        });
        schedules.truncate(limit as usize);
        Ok(schedules)
    }

    async fn set_executor(&self, _executor: Arc<dyn ScheduleExecutor>) -> Result<()> {
        // No-op: the executor is injected at construction via `DeferredExecutor`.
        Ok(())
    }
}

// ── Shutdown ──────────────────────────────────────────────────────────────────

impl Drop for CronSchedulerAdapter {
    fn drop(&mut self) {
        let _now = Utc::now();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingExecutor(Arc<AtomicU32>);

    #[async_trait]
    impl ScheduleExecutor for CountingExecutor {
        async fn execute(&self, _id: &str, _kind: &TaskKind) -> Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    async fn make_scheduler(dir: &std::path::Path) -> CronSchedulerAdapter {
        let counter = Arc::new(AtomicU32::new(0));
        let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter));
        CronSchedulerAdapter::new(
            dir.join("schedules.json"),
            dir.join("schedule_runs.json"),
            exec,
        )
        .await
        .expect("scheduler init failed")
    }

    fn create_req(id: &str, cron: &str) -> CreateScheduleRequest {
        CreateScheduleRequest {
            id: id.to_string(),
            label: format!("Test task {id}"),
            cron: cron.to_string(),
            fire_at: None,
            once: false,
            timezone: "UTC".to_string(),
            kind: TaskKind::AgentPrompt {
                prompt: "Hello".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn create_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("t1", "0 0 0 * * *"))
            .await
            .unwrap();
        sched
            .create_task(create_req("t2", "0 0 1 * * *"))
            .await
            .unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.id == "t1"));
        assert!(tasks.iter().any(|t| t.id == "t2"));
    }

    #[tokio::test]
    async fn delete_task() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("del", "0 0 2 * * *"))
            .await
            .unwrap();
        sched.delete_task("del").await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        assert!(sched.delete_task("ghost").await.is_err());
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("p1", "0 0 3 * * *"))
            .await
            .unwrap();
        sched.pause_task("p1").await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert!(tasks.iter().find(|t| t.id == "p1").unwrap().paused);

        sched.resume_task("p1").await.unwrap();
        let tasks = sched.list_tasks().await.unwrap();
        assert!(!tasks.iter().find(|t| t.id == "p1").unwrap().paused);
    }

    #[tokio::test]
    async fn invalid_cron_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        let err = sched.create_task(create_req("bad", "not a cron")).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn persistence_round_trip() {
        let tmp = tempfile::tempdir().unwrap();

        {
            let sched = make_scheduler(tmp.path()).await;
            sched
                .create_task(create_req("persist1", "0 0 4 * * *"))
                .await
                .unwrap();
        }

        // Rehydrate from disk
        let sched2 = make_scheduler(tmp.path()).await;
        let tasks = sched2.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "persist1");
        assert_eq!(tasks[0].timezone, "UTC");
    }

    #[tokio::test]
    async fn duplicate_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        sched
            .create_task(create_req("dup", "0 0 5 * * *"))
            .await
            .unwrap();
        assert!(sched
            .create_task(create_req("dup", "0 0 6 * * *"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn next_run_is_populated() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        let schedule = sched
            .create_task(create_req("nr1", "0 0 12 * * *"))
            .await
            .unwrap();

        assert!(
            schedule.next_run.is_some(),
            "next_run should be computed for an active schedule"
        );

        let next = schedule.next_run.unwrap();
        assert!(
            next > chrono::Utc::now(),
            "next_run should be in the future, got {next}"
        );
    }

    #[tokio::test]
    async fn paused_schedule_has_no_next_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("pnr", "0 0 12 * * *"))
            .await
            .unwrap();
        sched.pause_task("pnr").await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        let task = tasks.iter().find(|t| t.id == "pnr").unwrap();
        assert!(
            task.next_run.is_none(),
            "paused schedule should have no next_run"
        );
    }

    #[tokio::test]
    async fn list_upcoming_sorted_by_next_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        // The 6am task must sort before the noon one.
        sched
            .create_task(create_req("noon", "0 0 12 * * *"))
            .await
            .unwrap();
        sched
            .create_task(create_req("morning", "0 0 6 * * *"))
            .await
            .unwrap();

        let upcoming = sched.list_upcoming(10).await.unwrap();
        assert_eq!(upcoming.len(), 2);

        assert!(upcoming[0].next_run.is_some());
        assert!(upcoming[1].next_run.is_some());

        assert!(
            upcoming[0].next_run.unwrap() <= upcoming[1].next_run.unwrap(),
            "upcoming schedules should be sorted by next_run"
        );
    }

    #[test]
    fn compute_next_run_valid_cron() {
        let next = super::compute_next_run("0 0 12 * * *", "UTC");
        assert!(next.is_some(), "valid cron should produce a next_run");
        assert!(next.unwrap() > chrono::Utc::now());
    }

    #[test]
    fn compute_next_run_invalid_cron() {
        let next = super::compute_next_run("not a cron", "UTC");
        assert!(next.is_none(), "invalid cron should return None");
    }

    #[test]
    fn compute_next_run_honors_the_schedules_own_timezone() {
        // 14:00 in Africa/Nairobi (UTC+3, no DST) is 11:00 UTC.
        let next = super::compute_next_run("0 0 14 * * *", "Africa/Nairobi")
            .expect("valid cron should produce a next_run");
        assert_eq!(next.format("%H:%M").to_string(), "11:00", "{next}");
    }

    #[test]
    fn compute_next_run_falls_back_to_utc_for_an_unrecognised_timezone() {
        let next = super::compute_next_run("0 0 14 * * *", "Not/A/Zone")
            .expect("an unparseable timezone must not make an otherwise-valid cron unparseable");
        assert_eq!(next.format("%H:%M").to_string(), "14:00", "{next}");
    }

    #[tokio::test]
    async fn update_task_changes_fields() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("upd1", "0 0 8 * * *"))
            .await
            .unwrap();

        let updated = sched
            .update_task(
                "upd1",
                UpdateScheduleRequest {
                    label: Some("Updated label".to_string()),
                    cron: Some("0 30 9 * * *".to_string()),
                    timezone: Some("Africa/Nairobi".to_string()),
                    kind: None,
                    fire_at: None,
                    once: false,
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.label, "Updated label");
        assert_eq!(updated.cron, "0 30 9 * * *");
        assert_eq!(updated.timezone, "Africa/Nairobi");
        // Kind unchanged
        match &updated.kind {
            TaskKind::AgentPrompt { prompt } => assert_eq!(prompt, "Hello"),
            other => panic!("expected AgentPrompt, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn switching_a_daily_schedule_to_once_actually_converts_it() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("cadence1", "0 0 9 * * *"))
            .await
            .unwrap();

        let updated = sched
            .update_task(
                "cadence1",
                UpdateScheduleRequest {
                    once: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(
            updated.cron,
            pond_core::user_data::domain::schedule::CRON_ONCE,
            "a one-shot's cron must be the sentinel, not the old recurring pattern"
        );
        assert!(
            updated.fire_at.is_some(),
            "switching to once must set a concrete fire_at"
        );
        assert_eq!(
            updated.next_run, updated.fire_at,
            "a one-shot's next_run IS its fire_at"
        );

        // And back: a real cron without `once` clears `fire_at`.
        let reverted = sched
            .update_task(
                "cadence1",
                UpdateScheduleRequest {
                    cron: Some("0 0 9 * * *".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(reverted.cron, "0 0 9 * * *");
        assert!(
            reverted.fire_at.is_none(),
            "switching back to a real cron must clear the stored fire_at"
        );
    }

    #[tokio::test]
    async fn switching_to_once_with_an_unparseable_cron_is_refused() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("cadence2", "0 0 9 * * *"))
            .await
            .unwrap();

        let err = sched
            .update_task(
                "cadence2",
                UpdateScheduleRequest {
                    cron: Some("not a cron".to_string()),
                    once: true,
                    ..Default::default()
                },
            )
            .await
            .expect_err("an unparseable cron cannot yield a next occurrence");
        assert!(err.to_string().contains("one-shot"), "{err}");

        // The schedule must be untouched by the failed attempt.
        let tasks = sched.list_tasks().await.unwrap();
        let task = tasks.iter().find(|t| t.id == "cadence2").unwrap();
        assert_eq!(task.cron, "0 0 9 * * *");
        assert!(task.fire_at.is_none());
    }

    #[tokio::test]
    async fn a_one_shot_survives_a_restart_instead_of_taking_the_scheduler_down() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        {
            let sched = make_scheduler(tmp.path()).await;
            sched
                .create_task(create_req("cadence3", "0 0 9 * * *"))
                .await
                .unwrap();
            sched
                .update_task(
                    "cadence3",
                    UpdateScheduleRequest {
                        once: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }

        // Simulate a restart: a fresh adapter rehydrating from the same schedules.json.
        let restarted = make_scheduler(tmp.path()).await;
        let tasks = restarted.list_tasks().await.unwrap();
        let task = tasks.iter().find(|t| t.id == "cadence3").unwrap();
        assert_eq!(task.cron, pond_core::user_data::domain::schedule::CRON_ONCE);
        assert!(
            task.fire_at.is_some(),
            "the one-shot instant must survive rehydration"
        );
    }

    #[tokio::test]
    async fn update_with_invalid_cron_preserves_old_schedule() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("atomic1", "0 0 8 * * *"))
            .await
            .unwrap();

        // Attempt to update with an invalid cron — should fail without breaking the schedule.
        let result = sched
            .update_task(
                "atomic1",
                UpdateScheduleRequest {
                    label: None,
                    cron: Some("every morning at 9".to_string()),
                    timezone: None,
                    kind: None,
                    fire_at: None,
                    once: false,
                },
            )
            .await;

        assert!(result.is_err(), "invalid cron should produce an error");

        // The schedule should still exist with the ORIGINAL cron.
        let tasks = sched.list_tasks().await.unwrap();
        let task = tasks.iter().find(|t| t.id == "atomic1").unwrap();
        assert_eq!(
            task.cron, "0 0 8 * * *",
            "original cron should be preserved"
        );
        assert!(!task.paused, "schedule should still be active");
    }

    #[tokio::test]
    async fn update_nonexistent_errors() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        assert!(sched
            .update_task("ghost", UpdateScheduleRequest::default())
            .await
            .is_err());
    }

    // ── Rules that can never fire are refused at the STORE ───────────────

    fn rule_req(
        id: &str,
        actions: Vec<pond_core::user_data::domain::schedule::TriggerAction>,
    ) -> CreateScheduleRequest {
        use pond_core::user_data::domain::schedule::{
            SensorTriggerSpec, TriggerCondition, TriggerSource, TriggerSourceKind,
        };
        CreateScheduleRequest {
            fire_at: None,
            once: false,
            id: id.to_string(),
            label: format!("rule {id}"),
            cron: "@event".to_string(),
            timezone: "UTC".to_string(),
            kind: TaskKind::SensorTrigger(SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: Some("backyard-pir".into()),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions,
                cooldown_secs: 60,
            }),
        }
    }

    fn notify() -> Vec<pond_core::user_data::domain::schedule::TriggerAction> {
        vec![
            pond_core::user_data::domain::schedule::TriggerAction::Notify {
                title: "Motion".into(),
                body: "Backyard".into(),
            },
        ]
    }

    #[tokio::test]
    async fn a_rule_that_can_never_fire_is_refused_at_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        // Control: a valid rule is accepted, so the refusals below are about the spec.
        sched.create_task(rule_req("good", notify())).await.unwrap();

        let err = sched
            .create_task(rule_req("actionless", vec![]))
            .await
            .expect_err(
                "a rule with no actions was stored: the MCP tool writes through this port                  without passing the API, so a check that lives only in routes.rs lets the                  model create the rules a person is refused",
            );
        assert!(err.to_string().contains("action"), "{err}");
        assert_eq!(sched.list_tasks().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_update_cannot_smuggle_in_a_rule_that_can_never_fire() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        sched.create_task(rule_req("r", notify())).await.unwrap();

        let bad = rule_req("r", vec![]).kind;
        let err = sched
            .update_task(
                "r",
                UpdateScheduleRequest {
                    kind: Some(bad),
                    ..Default::default()
                },
            )
            .await
            .expect_err("update is another door onto a stored rule");
        assert!(err.to_string().contains("action"), "{err}");

        // And the good rule is still the one stored.
        let tasks = sched.list_tasks().await.unwrap();
        match &tasks[0].kind {
            TaskKind::SensorTrigger(spec) => assert_eq!(spec.actions.len(), 1),
            other => panic!("expected the rule, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rule_stored_before_the_check_existed_still_loads() {
        // Rehydration doesn't validate: refusing would drop someone's automation on upgrade.
        let tmp = tempfile::tempdir().unwrap();
        let stored = serde_json::json!([{
            "id": "legacy-rule",
            "label": "Actionless",
            "cron": "@event",
            "timezone": "UTC",
            "kind": {"type": "sensor_trigger", "source": {"kind": "sensor"}, "actions": []},
            "paused": false
        }]);
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string_pretty(&stored).unwrap(),
        )
        .await
        .unwrap();

        let sched = make_scheduler(tmp.path()).await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1, "an existing rule must survive the upgrade");
    }

    // ── The file every schedule lives in ─────────────────────────────────

    fn snapshot_of(ids: &[&str], pad: usize) -> Vec<PersistedTask> {
        ids.iter()
            .map(|id| PersistedTask {
                fire_at: None,
                id: (*id).to_string(),
                label: format!("label of {id} {}", "x".repeat(pad)),
                cron: "0 0 4 * * *".into(),
                timezone: "UTC".into(),
                kind: Some(TaskKind::AgentPrompt {
                    prompt: "back up".into(),
                }),
                payload: None,
                paused: false,
                created_at: Some(Utc::now()),
                last_run: None,
            })
            .collect()
    }

    #[test]
    fn no_two_writes_share_a_scratch_file() {
        let target = std::path::Path::new("/var/pond/schedules.json");
        let a = temp_path(target, 1);
        let b = temp_path(target, 2);
        assert_ne!(
            a, b,
            "two writes share a scratch file: whichever renames first \
             publishes a mixture of both, and the other goes on writing into \
             the live schedules.json afterwards"
        );

        for p in [&a, &b] {
            assert_eq!(
                p.parent(),
                target.parent(),
                "the scratch file left the published file's directory: the \
                 rename is no longer guaranteed to be atomic"
            );
            assert_ne!(p, target, "the scratch file IS the published file");
        }
    }

    #[tokio::test]
    async fn a_stale_snapshot_does_not_overwrite_a_newer_one() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = SnapshotWriter::new(tmp.path().join("schedules.json"));

        let old_seq = writer.ticket();
        let new_seq = writer.ticket();
        let newer = snapshot_of(&["a", "b"], 4);
        let older = snapshot_of(&["a"], 4);

        writer.publish(new_seq, &newer).await.unwrap();
        writer.publish(old_seq, &older).await.unwrap();

        let on_disk = tokio::fs::read_to_string(writer.path()).await.unwrap();
        let records: Vec<PersistedTask> = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(
            records.len(),
            2,
            "the older snapshot was published over the newer one: {on_disk}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_writers_leave_one_whole_snapshot() {
        // Smoke test only; `no_two_writes_share_a_scratch_file` is what guards the property.
        let tmp = tempfile::tempdir().unwrap();
        let writer = Arc::new(SnapshotWriter::new(tmp.path().join("schedules.json")));

        let mut handles = Vec::new();
        let mut expected = 0usize;
        for i in 0..32u64 {
            // The last ticket is the big snapshot, so the winner is visible.
            let records = if i % 2 == 1 {
                snapshot_of(&["a", "b", "c", "d", "e", "f", "g", "h"], 512)
            } else {
                snapshot_of(&["a"], 1)
            };
            let seq = writer.ticket();
            expected = records.len();
            let w = writer.clone();
            handles.push(tokio::spawn(async move {
                w.publish(seq, &records).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let on_disk = tokio::fs::read_to_string(writer.path()).await.unwrap();
        let records: Vec<PersistedTask> = serde_json::from_str(&on_disk).unwrap_or_else(|e| {
            panic!(
                "schedules.json does not parse after concurrent writes ({e}): that is \
                 every schedule and every rule in the pond, not one cooldown. The file \
                 was {} bytes",
                on_disk.len()
            )
        });
        assert_eq!(
            records.len(),
            expected,
            "the last-read snapshot is not the one on disk"
        );
        // Vacuity control: the payloads differ in length, so the check above is about which won.
        assert_ne!(snapshot_of(&["a"], 1).len(), expected);

        // And nothing was left behind for the next writer to find.
        let mut leftovers = tokio::fs::read_dir(tmp.path()).await.unwrap();
        while let Some(e) = leftovers.next_entry().await.unwrap() {
            let name = e.file_name().to_string_lossy().to_string();
            assert!(!name.ends_with(".tmp"), "scratch file left behind: {name}");
        }
    }

    #[tokio::test]
    async fn an_unreadable_schedules_file_is_kept_and_the_pond_still_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("schedules.json");
        let corrupt = "[{\"id\":\"nightly-backup\",\"label\":\"Backup\"}, {\"id\":";
        tokio::fs::write(&path, corrupt).await.unwrap();

        let counter = Arc::new(AtomicU32::new(0));
        let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter));
        let sched = CronSchedulerAdapter::new(path.clone(), tmp.path().join("runs.json"), exec)
            .await
            .expect(
                "an unreadable schedules.json failed the whole scheduler: pond-server \
                 turns that into `scheduler = None`, so every schedule endpoint answers \
                 503 for the life of the install and the next boot does it again",
            );
        assert!(sched.list_tasks().await.unwrap().is_empty());

        let mut kept = None;
        let mut entries = tokio::fs::read_dir(tmp.path()).await.unwrap();
        while let Some(e) = entries.next_entry().await.unwrap() {
            if e.file_name()
                .to_string_lossy()
                .contains("schedules.json.unreadable-")
            {
                kept = Some(e.path());
            }
        }
        let kept = kept.expect("the unreadable file must be kept, not deleted");
        assert_eq!(
            tokio::fs::read_to_string(&kept).await.unwrap(),
            corrupt,
            "the quarantined copy is not the file that failed to parse"
        );
    }

    #[tokio::test]
    async fn a_paused_rules_fire_stamp_survives_a_restart() {
        // The fixture is what `pause_task` writes after a fire: paused, with the last stamp.
        let tmp = tempfile::tempdir().unwrap();
        let fired_at = Utc::now() - chrono::Duration::seconds(45);
        let stored = serde_json::json!([{
            "id": "paused-rule",
            "label": "Backyard motion",
            "cron": "@event",
            "timezone": "UTC",
            "kind": {
                "type": "sensor_trigger",
                "source": {"kind": "sensor", "device_id": "backyard-pir", "signal": "motion"},
                "actions": [{"type": "notify", "title": "Motion", "body": "Backyard"}],
                "cooldown_secs": 3600
            },
            "paused": true,
            "last_run": fired_at,
        }]);
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string_pretty(&stored).unwrap(),
        )
        .await
        .unwrap();

        let sched = make_scheduler(tmp.path()).await;
        let tasks = sched.list_tasks().await.unwrap();
        let rule = tasks
            .iter()
            .find(|t| t.id == "paused-rule")
            .expect("the paused rule survives the restart");
        assert!(
            rule.paused,
            "the fixture has to BE paused or it is not testing the paused path"
        );
        assert_eq!(
            rule.last_run,
            Some(fired_at),
            "rehydration dropped a PAUSED rule's fire stamp: resume it and the \
             hour-long cooldown reads as never-fired, so the next matching \
             event fires it"
        );

        // And a resume keeps it — `resume_task` rewrites the file.
        sched.resume_task("paused-rule").await.unwrap();
        let after = sched.list_tasks().await.unwrap();
        let rule = after.iter().find(|t| t.id == "paused-rule").unwrap();
        assert!(!rule.paused);
        assert_eq!(rule.last_run, Some(fired_at), "resume dropped the stamp");
    }

    #[tokio::test]
    async fn backward_compat_legacy_payload() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a legacy schedules.json without kind/timezone fields.
        let legacy = serde_json::json!([{
            "id": "legacy1",
            "label": "Old webhook task",
            "cron": "0 0 8 * * *",
            "payload": {"webhook_url": "https://example.com/hook"},
            "paused": false
        }]);
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .await
        .unwrap();

        let sched = make_scheduler(tmp.path()).await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "legacy1");
        assert_eq!(tasks[0].timezone, "UTC"); // default
        match &tasks[0].kind {
            TaskKind::Webhook { webhook_url } => {
                assert_eq!(webhook_url, "https://example.com/hook");
            }
            other => panic!("expected Webhook kind, got {other:?}"),
        }
    }
}

/// What one task fire needs, shared by cron jobs and one-shots.
struct TaskRunContext {
    executor: Arc<dyn ScheduleExecutor>,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    run_history: Arc<JsonRunHistory>,
    result_tx: Option<
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    >,
    persist: Arc<SnapshotWriter>,
    id: String,
    kind: TaskKind,
}

impl TaskRunContext {
    async fn run(self) {
        use pond_core::user_data::domain::schedule::ScheduleResultEvent;
        let TaskRunContext {
            executor,
            tasks,
            run_history,
            result_tx,
            persist,
            id,
            kind,
        } = self;

        let label = {
            let guard = tasks.lock().await;
            guard
                .get(&id)
                .map(|e| e.persisted.label.clone())
                .unwrap_or_default()
        };

        {
            let mut guard = tasks.lock().await;
            if let Some(entry) = guard.get_mut(&id) {
                entry.currently_running = true;
            }
        }
        CronSchedulerAdapter::stamp_fire(&tasks, &persist, &id).await;

        let run_id = run_history.record_start(&id).await;
        let start = std::time::Instant::now();

        // Broadcast "started" event so clients see progress immediately
        if let Some(tx) = &result_tx {
            let _ = tx.send(ScheduleResultEvent {
                schedule_id: id.clone(),
                schedule_label: label.clone(),
                run_id: run_id.clone(),
                status: RunStatus::Running,
                result: None,
                error: None,
                duration_ms: None,
            });
        }

        let result = executor.execute(&id, &kind).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        let (status, result_text, error_text) = match &result {
            Ok(text) => {
                run_history
                    .record_finish(&run_id, RunStatus::Completed, Some(text.clone()), None)
                    .await;
                (RunStatus::Completed, Some(text.clone()), None)
            }
            Err(e) => {
                tracing::error!("Scheduled task {id} failed: {e}");
                run_history
                    .record_finish(&run_id, RunStatus::Failed, None, Some(e.to_string()))
                    .await;
                (RunStatus::Failed, None, Some(e.to_string()))
            }
        };

        // Broadcast result event (for SSE / desktop notifications)
        if let Some(tx) = &result_tx {
            let _ = tx.send(ScheduleResultEvent {
                schedule_id: id.clone(),
                schedule_label: label,
                run_id: run_id.clone(),
                status,
                result: result_text,
                error: error_text,
                duration_ms: Some(duration_ms),
            });
        }

        // Mark not-running. `last_run` was stamped at fire time above.
        {
            let mut guard = tasks.lock().await;
            if let Some(entry) = guard.get_mut(&id) {
                entry.currently_running = false;
            }
        }
    }
}
