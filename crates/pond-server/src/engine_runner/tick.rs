//! The engine's minute tick: what each member's stored state calls for, queued and opened with
//! no model call.

use std::time::Duration;

use chrono::{DateTime, Utc};

use pond_core::shared::domain::long_task::LongTaskState;
use pond_core::user_data::domain::anchor::{
    Anchor, AnchorKind, AnchorOrigin, AnchorState, Deliverable, DeliverableState,
};
use pond_core::user_data::domain::engine_job::{
    is_engine_task, EngineJob, JobClass, JobKind, JobState,
};
use pond_core::user_data::domain::profile::{ProfileScope, Role};
use pond_core::user_data::services::engine::recipes::{deliverable_title, standing_of, Standing};
use pond_core::user_data::services::engine::tick::{plan_tick, TickAction, TickSnapshot};
use pond_core::user_data::services::fact_sheet::{is_due, request_rebuild, REBUILD_AFTER_MEMORIES};

use super::{deliverable, inline, interests, open, pictures, EngineDeps};

const TICK_EVERY: Duration = Duration::from_secs(60);
/// The title of the standing anchor every member has.
pub const EVERYDAY_TITLE: &str = "Everyday";
/// Pending captures read per tick.
const CAPTURES_READ: usize = 50;
/// Newest memories read to count those saved since the last rebuild; shared ones sit among them.
const MEMORIES_READ: usize = REBUILD_AFTER_MEMORIES * 4;
/// A member's jobs looked through for those of an anchor that ended.
const JOBS_SCANNED: usize = 500;
const EVERY_STATE: [AnchorState; 5] = [
    AnchorState::Candidate,
    AnchorState::Active,
    AnchorState::Ended,
    AnchorState::Stopped,
    AnchorState::Faded,
];

/// What one tick did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Jobs added to the queue.
    pub queued: usize,
    /// Queued jobs given their long task.
    pub opened: usize,
    pub faded: usize,
    pub ended: usize,
    /// Open jobs closed because their task was stopped, failed or removed.
    pub closed: usize,
    /// Picture files deleted: past their time, or named by no capture.
    pub pictures_deleted: usize,
    /// Anchors made for confirmed interests, and stopped for avoided ones.
    pub interests_followed: usize,
    /// Questions asked about guessed interests.
    pub interests_asked: usize,
}

impl TickReport {
    fn did_something(&self) -> bool {
        *self != Self::default()
    }
}

/// Ticks for as long as the pond runs.
pub async fn run_ticks(deps: EngineDeps) {
    loop {
        tokio::time::sleep(TICK_EVERY).await;
        tick(&deps, Utc::now()).await;
    }
}

/// One tick: closes jobs whose task died, queues what each member's state calls for, fades and
/// ends anchors, and opens a long task for every queued job.
pub async fn tick(deps: &EngineDeps, now: DateTime<Utc>) -> TickReport {
    let mut report = TickReport {
        closed: close_dead_jobs(deps, now).await,
        ..TickReport::default()
    };
    let members = match deps.profiles.list().await {
        Ok(profiles) => profiles,
        Err(e) => {
            tracing::warn!(error = %e, "the engine could not read the household's members");
            Vec::new()
        }
    };
    report.pictures_deleted = pictures::tend(deps, &members, now).await;
    let open_jobs = deps.jobs.open().await.unwrap_or_default();
    for member in members.iter().filter(|p| p.role != Role::Guest) {
        if let Err(e) = tick_member(deps, &member.id, &open_jobs, now, &mut report).await {
            tracing::warn!(member = %member.id, error = %e, "the engine's tick skipped a member");
        }
    }
    report.opened = open::open_queued(deps, now).await;
    if report.did_something() {
        tracing::info!(
            target: "giap::trace",
            kind = "engine_tick",
            queued = report.queued,
            opened = report.opened,
            faded = report.faded,
            ended = report.ended,
            closed = report.closed,
            pictures_deleted = report.pictures_deleted,
            interests_followed = report.interests_followed,
            interests_asked = report.interests_asked,
            "the engine's tick changed something"
        );
    }
    report
}

/// Closes the open jobs whose long task was cancelled, failed or removed.
async fn close_dead_jobs(deps: &EngineDeps, now: DateTime<Utc>) -> usize {
    let mut closed = 0;
    for job in deps.jobs.open().await.unwrap_or_default() {
        let Some(task_id) = job.task_id.as_deref() else {
            continue;
        };
        let Ok(task) = deps.tasks.get_task(task_id).await else {
            continue;
        };
        let (state, outcome) = match task.map(|task| task.state()) {
            None => (JobState::Cancelled, "Stopped: its task was removed."),
            Some(LongTaskState::Cancelled) => (JobState::Cancelled, "Stopped before it finished."),
            Some(LongTaskState::Failed) => (JobState::Failed, "Failed: its task could not finish."),
            Some(_) => continue,
        };
        // Read again: the runner settles a job before it ends the task, so a job still open
        // now is one nobody settled.
        let Ok(Some(mut job)) = deps.jobs.get(&job.id).await else {
            continue;
        };
        if !job.state.is_open() {
            continue;
        }
        deliverable::rest(deps, &job, now).await;
        if job.settle(state, outcome, now).is_ok() && deps.jobs.save(&job).await.is_ok() {
            closed += 1;
        }
    }
    closed
}

/// Makes sure the member has their standing everyday anchor; one they stopped stays stopped.
async fn ensure_everyday(
    deps: &EngineDeps,
    member: &str,
    held: &[Anchor],
    now: DateTime<Utc>,
) -> anyhow::Result<Option<Anchor>> {
    let has_one = held.iter().any(|anchor| {
        anchor.profile_id == member && standing_of(anchor) == Some(Standing::Everyday)
    });
    if has_one {
        return Ok(None);
    }
    let everyday = Anchor::new(
        member,
        AnchorKind::Standing,
        EVERYDAY_TITLE,
        AnchorOrigin::Goose,
        now,
    )
    .ok_or_else(|| anyhow::anyhow!("the everyday anchor has no title"))?;
    Ok(deps.anchors.add(&everyday).await?.then_some(everyday))
}

/// Whether the member's fact sheet is due a rebuild.
async fn fact_sheet_due(deps: &EngineDeps, member: &str, now: DateTime<Utc>) -> bool {
    let last = deps
        .jobs
        .last_finished(member, JobKind::FactSheet)
        .await
        .unwrap_or_default();
    let since = match last {
        None => 0,
        Some(at) => deps
            .memories
            .search_recent(&ProfileScope::Owner(member.to_string()), MEMORIES_READ)
            .await
            .unwrap_or_default()
            .iter()
            .filter(|memory| memory.profile_id.as_deref() == Some(member) && memory.created_at > at)
            .count(),
    };
    is_due(last, since, now)
}

async fn tick_member(
    deps: &EngineDeps,
    member: &str,
    open_jobs: &[EngineJob],
    now: DateTime<Utc>,
    report: &mut TickReport,
) -> anyhow::Result<()> {
    let mut anchors = deps.anchors.list(member, &EVERY_STATE).await?;
    anchors.extend(ensure_everyday(deps, member, &anchors, now).await?);
    match interests::follow(deps, member, &mut anchors, open_jobs, now).await {
        Ok(followed) => report.interests_followed += followed.anchored + followed.stopped,
        Err(e) => {
            tracing::warn!(member = %member, error = %e, "the engine could not follow a member's interests")
        }
    }
    anchors.retain(|anchor| AnchorState::LIVE.contains(&anchor.state));
    let mut deliverables = Vec::new();
    for anchor in anchors.iter().filter(|anchor| anchor.profile_id == member) {
        for mut made in deps.anchors.deliverables_of(&anchor.id).await? {
            // Being made by no job: the job was called off or lost between two writes.
            let stranded = made.state == DeliverableState::Making
                && !open_jobs
                    .iter()
                    .any(|job| job.deliverable_id.as_deref() == Some(made.id.as_str()));
            if stranded {
                deliverable::release(deps, &made.id, now).await;
                made = deps
                    .anchors
                    .find_deliverable(&made.id)
                    .await?
                    .unwrap_or(made);
            }
            deliverables.push(made);
        }
    }
    let pending = if inline::runs(JobKind::Capture) && pictures::can_read(deps).await {
        deps.captures.pending(CAPTURES_READ).await?
    } else {
        Vec::new()
    };
    let actions = plan_tick(
        &TickSnapshot {
            profile_id: member,
            anchors: &anchors,
            deliverables: &deliverables,
            open_jobs,
            fact_sheet_due: fact_sheet_due(deps, member, now).await,
            pending_captures: &pending,
        },
        now,
    );
    for action in actions {
        let did = carry_out(deps, member, &anchors, &deliverables, &action, now, report).await;
        if let Err(e) = did {
            tracing::warn!(member = %member, ?action, error = %e, "the engine's tick could not carry out an action");
        }
    }
    match interests::offer(deps, member, now).await {
        Ok(asked) => report.interests_asked += asked,
        Err(e) => {
            tracing::warn!(member = %member, error = %e, "the engine could not offer a member's interests")
        }
    }
    match pictures::read_held(deps, member, now).await {
        Ok(queued) => report.queued += queued,
        Err(e) => {
            tracing::warn!(member = %member, error = %e, "the engine could not queue a held picture's reading")
        }
    }
    Ok(())
}

fn anchor_in<'a>(anchors: &'a [Anchor], id: &str) -> anyhow::Result<&'a Anchor> {
    anchors
        .iter()
        .find(|anchor| anchor.id == id)
        .ok_or_else(|| anyhow::anyhow!("the anchor is not among the member's"))
}

/// Queues the one job that makes or refreshes a deliverable; `false` when it already has one.
async fn queue_deliverable(
    deps: &EngineDeps,
    member: &str,
    anchor: &Anchor,
    deliverable: &Deliverable,
    class: JobClass,
    due_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    if deps
        .jobs
        .open_for_deliverable(&deliverable.id)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    let job = EngineJob::for_deliverable(
        member,
        &anchor.id,
        &deliverable.id,
        &deliverable.title,
        class,
        due_at,
        now,
    );
    deps.jobs.add(&job).await?;
    Ok(true)
}

async fn carry_out(
    deps: &EngineDeps,
    member: &str,
    anchors: &[Anchor],
    deliverables: &[Deliverable],
    action: &TickAction,
    now: DateTime<Utc>,
    report: &mut TickReport,
) -> anyhow::Result<()> {
    match action {
        TickAction::QueueQuestions {
            anchor_id,
            class,
            due_at,
        } => {
            let anchor = anchor_in(anchors, anchor_id)?;
            let mut job =
                EngineJob::new(JobKind::Questions, *class, Some(member), &anchor.title, now);
            job.anchor_id = Some(anchor.id.clone());
            job.due_at = *due_at;
            deps.jobs.add(&job).await?;
            report.queued += 1;
        }
        TickAction::QueueSlot {
            anchor_id,
            slot,
            class,
            due_at,
        } => {
            let anchor = anchor_in(anchors, anchor_id)?;
            let held = deliverables
                .iter()
                .find(|d| d.anchor_id == anchor.id && d.slot == slot.slot);
            let deliverable = match held {
                Some(held) => held.clone(),
                None => {
                    let title = deliverable_title(anchor, slot);
                    let made = Deliverable::new(&anchor.id, slot.kind, slot.slot, &title, now)
                        .ok_or_else(|| anyhow::anyhow!("the slot's deliverable has no title"))?;
                    anyhow::ensure!(
                        deps.anchors.add_deliverable(&made).await?,
                        "the slot already has a deliverable"
                    );
                    made
                }
            };
            if queue_deliverable(deps, member, anchor, &deliverable, *class, *due_at, now).await? {
                report.queued += 1;
            }
        }
        TickAction::QueueRefresh {
            anchor_id,
            deliverable_id,
            class,
            due_at,
        } => {
            let anchor = anchor_in(anchors, anchor_id)?;
            let deliverable = deliverables
                .iter()
                .find(|d| &d.id == deliverable_id)
                .ok_or_else(|| anyhow::anyhow!("the deliverable is not among the member's"))?;
            if queue_deliverable(deps, member, anchor, deliverable, *class, *due_at, now).await? {
                report.queued += 1;
            }
        }
        TickAction::QueueFactSheet => {
            request_rebuild(deps.jobs.as_ref(), member, JobClass::Upkeep, now).await?;
            report.queued += 1;
        }
        TickAction::QueueCapture { capture_id } => {
            if deps.jobs.open_for_capture(capture_id).await?.is_none() {
                let job = EngineJob::for_capture(member, capture_id, JobKind::Capture, now);
                deps.jobs.add(&job).await?;
                report.queued += 1;
            }
        }
        TickAction::FadeAnchor { anchor_id } => {
            let mut anchor = anchor_in(anchors, anchor_id)?.clone();
            anchor.fade(now)?;
            deps.anchors.save(&anchor).await?;
            report.faded += 1;
        }
        TickAction::EndAnchor { anchor_id } => {
            end_anchor(deps, anchor_in(anchors, anchor_id)?, now).await?;
            report.ended += 1;
        }
    }
    Ok(())
}

/// Ends an anchor whose time has passed: its deliverables are archived with their bodies, its
/// open jobs stop, and the long tasks its jobs ran as go, with what they saved.
async fn end_anchor(deps: &EngineDeps, anchor: &Anchor, now: DateTime<Utc>) -> anyhow::Result<()> {
    let mut ended = anchor.clone();
    ended.end(now)?;
    deps.anchors.save(&ended).await?;

    let mut tasks: Vec<String> = Vec::new();
    for mut deliverable in deps.anchors.deliverables_of(&anchor.id).await? {
        tasks.extend(deliverable.task_id.take());
        if deliverable.archive(now).is_ok() {
            deps.anchors.save_deliverable(&deliverable).await?;
        }
    }
    let of_anchor = |job: &EngineJob| job.anchor_id.as_deref() == Some(anchor.id.as_str());
    for mut job in deps
        .jobs
        .recent(&anchor.profile_id, JOBS_SCANNED)
        .await?
        .into_iter()
        .filter(of_anchor)
    {
        tasks.extend(job.task_id.clone());
        if job.state.is_open()
            && job
                .settle(JobState::Cancelled, "Stopped: its anchor ended.", now)
                .is_ok()
        {
            deps.jobs.save(&job).await?;
        }
    }
    tasks.sort();
    tasks.dedup();
    for task_id in tasks.iter().filter(|id| is_engine_task(id)) {
        deps.tasks.delete_task(task_id).await?;
    }
    Ok(())
}
