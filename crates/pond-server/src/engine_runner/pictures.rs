//! What capture and ledger jobs share, and the tick's care of the pictures themselves: a picture
//! past its time loses its file, a file no capture names goes, and a money picture held while
//! Budget was off is read once it is on.

use std::collections::HashSet;

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};

use pond_core::models::domain::message::ImageAttachment;
use pond_core::shared::domain::long_task::LongTask;
use pond_core::user_data::domain::capture::Capture;
use pond_core::user_data::domain::engine_job::{EngineJob, JobKind, JobState};
use pond_core::user_data::domain::profile::Profile;
use pond_core::user_data::domain::specialist::SpecialistId;

use super::finish::close;
use super::inline::failed;
use super::{EngineDeps, ModelStop, PictureDeps, READS_NO_PICTURES};

/// A file younger than this may belong to a capture whose row is still being written.
const ORPHAN_AFTER: Duration = Duration::days(1);
/// How far back a settled capture or ledger job's task is looked for, to delete it.
const SETTLED_TASKS_SCANNED: Duration = Duration::days(1);
const JOBS_SCANNED: usize = 100;

/// Whether this pond can read a picture now: somewhere to keep it and a model that sees.
pub(super) async fn can_read(deps: &EngineDeps) -> bool {
    deps.pictures.is_some() && deps.model.reads_pictures().await
}

fn store_of(deps: &EngineDeps) -> Result<&PictureDeps, ModelStop> {
    deps.pictures
        .as_ref()
        .ok_or_else(|| failed("this pond has nowhere to keep pictures"))
}

/// The capture a capture or ledger job reads, while it is still its member's.
pub(super) async fn capture_of(
    deps: &EngineDeps,
    job: &EngineJob,
) -> anyhow::Result<Option<Capture>> {
    let found = deps.captures.find(&job.subject).await?;
    Ok(found.filter(|capture| job.profile_id.as_deref() == Some(capture.profile_id.as_str())))
}

/// The job's capture and its picture as the model is shown it.
pub(super) async fn picture_of(
    deps: &EngineDeps,
    job: &EngineJob,
) -> Result<(Capture, ImageAttachment), ModelStop> {
    if !deps.model.reads_pictures().await {
        return Err(failed(READS_NO_PICTURES));
    }
    let store = store_of(deps)?;
    let capture = capture_of(deps, job)
        .await
        .map_err(failed)?
        .ok_or_else(|| failed("the picture was deleted"))?;
    let file = capture
        .picture_file
        .as_deref()
        .ok_or_else(|| failed("its picture is gone"))?;
    let (bytes, mime_type) = store
        .store
        .read(file)
        .await
        .map_err(failed)?
        .ok_or_else(|| failed("its picture is gone"))?;
    let picture = ImageAttachment {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime_type,
    };
    Ok((capture, picture))
}

/// Whether the capture's picture can still be read from the store.
pub(super) async fn picture_kept(deps: &EngineDeps, capture: &Capture) -> bool {
    match (&deps.pictures, capture.picture_file.as_deref()) {
        (Some(pictures), Some(file)) => matches!(pictures.store.read(file).await, Ok(Some(_))),
        _ => false,
    }
}

/// Deletes a capture whole: its picture, then its row.
pub(super) async fn forget_capture(deps: &EngineDeps, capture: &Capture) -> anyhow::Result<()> {
    // The picture first: a failure part-way leaves a row with no picture, never a picture
    // with no row.
    if let (Some(pictures), Some(file)) = (&deps.pictures, capture.picture_file.as_deref()) {
        pictures.store.delete(file).await?;
    }
    deps.captures
        .delete(&capture.profile_id, &capture.id)
        .await?;
    Ok(())
}

/// Wakes the embedder, so a capture just read is searchable.
pub(super) fn ring_indexer(deps: &EngineDeps) {
    if let Some(bell) = deps.pictures.as_ref().and_then(|p| p.indexed.as_ref()) {
        bell.notify_one();
    }
}

/// Settles a capture or ledger job and deletes its task: the model's replies about a picture
/// are kept nowhere but in what the settle saved.
pub(super) async fn close_and_forget_task(
    deps: &EngineDeps,
    task: &LongTask,
    job: &mut EngineJob,
    state: JobState,
    outcome: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    close(deps, task, job, state, outcome, now).await?;
    if let Err(e) = deps.tasks.delete_task(task.id()).await {
        tracing::warn!(task = %task.id(), error = %e, "a settled picture job's task stayed");
    }
    Ok(())
}

/// Deletes the pictures whose time is up and the files no capture names any more, such as a
/// removed member's, and returns how many. `members` is every profile: a file is an orphan
/// only when none names it.
pub(super) async fn tend(deps: &EngineDeps, members: &[Profile], now: DateTime<Utc>) -> usize {
    for member in members {
        forget_settled_tasks(deps, &member.id, now).await;
    }
    let Some(pictures) = &deps.pictures else {
        return 0;
    };
    let mut deleted = 0;
    for mut capture in deps.captures.pictures_due(now).await.unwrap_or_default() {
        let Some(file) = capture.picture_file.clone() else {
            continue;
        };
        if let Err(e) = pictures.store.delete(&file).await {
            tracing::warn!(error = %e, "a picture past its time could not be deleted");
            continue;
        }
        capture.forget_picture(now);
        if let Err(e) = deps.captures.save(&capture).await {
            tracing::warn!(error = %e, "a capture whose picture went could not be saved");
        }
        deleted += 1;
    }
    deleted + forget_orphans(deps, pictures, members, now).await
}

async fn forget_orphans(
    deps: &EngineDeps,
    pictures: &PictureDeps,
    members: &[Profile],
    now: DateTime<Utc>,
) -> usize {
    let old: Vec<String> = match pictures.store.kept().await {
        Ok(kept) => kept
            .into_iter()
            .filter(|picture| now - picture.saved_at >= ORPHAN_AFTER)
            .map(|picture| picture.file)
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "the kept pictures could not be listed");
            return 0;
        }
    };
    // With no member read there is nothing to tell a kept file from an orphan by.
    if old.is_empty() || members.is_empty() {
        return 0;
    }
    let mut named = HashSet::new();
    for member in members {
        let (Ok(read), Ok(held)) = (
            deps.captures.list(&member.id).await,
            deps.captures.hot(&member.id).await,
        ) else {
            // A list that could not be read would make every one of its files look orphaned.
            return 0;
        };
        named.extend(read.into_iter().chain(held).filter_map(|c| c.picture_file));
    }
    let mut deleted = 0;
    for file in old.iter().filter(|file| !named.contains(*file)) {
        match pictures.store.delete(file).await {
            Ok(()) => deleted += 1,
            Err(e) => tracing::warn!(error = %e, "a picture no capture names could not be deleted"),
        }
    }
    if deleted > 0 {
        tracing::info!(
            target: "giap::trace",
            kind = "engine_pictures_orphaned",
            deleted,
            "pictures no capture named were deleted"
        );
    }
    deleted
}

/// Deletes the tasks a member's settled capture and ledger jobs still have, such as one a
/// route called off when the member deleted the picture.
async fn forget_settled_tasks(deps: &EngineDeps, member: &str, now: DateTime<Utc>) {
    let jobs = deps
        .jobs
        .recent(member, JOBS_SCANNED)
        .await
        .unwrap_or_default();
    let mut deleted = 0;
    for job in jobs.iter().filter(|job| {
        matches!(job.kind, JobKind::Capture | JobKind::Ledger)
            && !job.state.is_open()
            && job
                .finished_at
                .is_some_and(|at| now - at < SETTLED_TASKS_SCANNED)
    }) {
        let Some(task_id) = job.task_id.as_deref() else {
            continue;
        };
        if matches!(deps.tasks.get_task(task_id).await, Ok(Some(_)))
            && matches!(deps.tasks.delete_task(task_id).await, Ok(true))
        {
            deleted += 1;
        }
    }
    if deleted > 0 {
        tracing::info!(
            target: "giap::trace",
            kind = "engine_picture_tasks_forgotten",
            deleted,
            "the tasks of settled picture jobs were deleted"
        );
    }
}

/// Queues a ledger job for each money picture the member holds once their Budget agent is on;
/// returns how many.
pub(super) async fn read_held(
    deps: &EngineDeps,
    member: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    if !can_read(deps).await
        || !deps
            .agents
            .switch(member, SpecialistId::Budget)
            .await?
            .is_on()
    {
        return Ok(0);
    }
    let mut queued = 0;
    for capture in deps.captures.hot(member).await? {
        queued += usize::from(queue_ledger(deps, &capture, now).await?);
    }
    Ok(queued)
}

/// Queues the job that reads a held money picture into the ledger; `false` when one is open.
pub(super) async fn queue_ledger(
    deps: &EngineDeps,
    capture: &Capture,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    if deps.jobs.open_for_capture(&capture.id).await?.is_some() {
        return Ok(false);
    }
    let job = EngineJob::for_capture(&capture.profile_id, &capture.id, JobKind::Ledger, now);
    deps.jobs.add(&job).await?;
    Ok(true)
}
