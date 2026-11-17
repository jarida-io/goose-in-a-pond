//! Asking for a deliverable to be made again, whoever asks: a member's tap, an answer, or a
//! job the heartbeat left itself.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};

use super::tick::class_of;
use crate::user_data::domain::anchor::{Anchor, Deliverable};
use crate::user_data::domain::engine_job::{EngineJob, JobClass, JobState};
use crate::user_data::ports::engine_jobs::EngineJobRepository;

/// Serialises "has this deliverable an open job?" with queueing one, so two asks queue one job.
static REFRESH_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Queues a refresh of `deliverable` as a job with no task, which the engine's tick opens. A
/// deliverable never has two open jobs: one already open is returned, moved up to `class`
/// while it still waits.
pub async fn request_refresh(
    jobs: &dyn EngineJobRepository,
    anchor: &Anchor,
    deliverable: &Deliverable,
    class: JobClass,
    now: DateTime<Utc>,
) -> anyhow::Result<EngineJob> {
    let _held = REFRESH_LOCK.lock().await;
    if let Some(mut job) = jobs.open_for_deliverable(&deliverable.id).await? {
        if job.state == JobState::Queued && class < job.class {
            job.class = class;
            jobs.save(&job).await?;
        }
        return Ok(job);
    }
    let job = EngineJob::for_deliverable(
        &anchor.profile_id,
        &anchor.id,
        &deliverable.id,
        &deliverable.title,
        class,
        class_of(anchor).1,
        now,
    );
    jobs.add(&job).await?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::{AnchorKind, AnchorOrigin, DeliverableKind};
    use crate::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn summit() -> (Anchor, Deliverable) {
        let mut summit = Anchor::new(
            "jerry",
            AnchorKind::Event,
            "AI Summit Nairobi",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        summit.starts_at = Some(now() + Duration::days(17));
        let dossier = Deliverable::new(
            &summit.id,
            DeliverableKind::Dossier,
            "dossier",
            "Dossier for AI Summit Nairobi",
            now(),
        )
        .unwrap();
        (summit, dossier)
    }

    #[tokio::test]
    async fn a_deliverable_gets_one_open_job_however_often_a_refresh_is_asked_for() {
        let jobs = InMemoryEngineJobRepository::new();
        let (summit, dossier) = summit();
        let first = request_refresh(&jobs, &summit, &dossier, JobClass::Deadline, now())
            .await
            .unwrap();
        assert_eq!(first.state, JobState::Queued);
        assert_eq!(first.task_id, None);
        assert_eq!(first.class, JobClass::Deadline);
        assert_eq!(first.due_at, summit.starts_at);
        assert_eq!(first.deliverable_id.as_deref(), Some(dossier.id.as_str()));
        assert_eq!(first.profile_id.as_deref(), Some("jerry"));

        let again = request_refresh(&jobs, &summit, &dossier, JobClass::Deadline, now())
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(jobs.open().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_waiting_job_moves_up_to_a_sooner_class_and_never_down() {
        let jobs = InMemoryEngineJobRepository::new();
        let (summit, dossier) = summit();
        let queued = request_refresh(&jobs, &summit, &dossier, JobClass::Deadline, now())
            .await
            .unwrap();

        let asked = request_refresh(&jobs, &summit, &dossier, JobClass::Asked, now())
            .await
            .unwrap();
        assert_eq!(asked.id, queued.id);
        assert_eq!(asked.class, JobClass::Asked);

        let later = request_refresh(&jobs, &summit, &dossier, JobClass::Open, now())
            .await
            .unwrap();
        assert_eq!(later.class, JobClass::Asked);
        assert_eq!(
            jobs.get(&queued.id).await.unwrap().unwrap().class,
            JobClass::Asked
        );
    }

    #[tokio::test]
    async fn a_running_job_keeps_its_class() {
        let jobs = InMemoryEngineJobRepository::new();
        let (summit, dossier) = summit();
        let mut running = request_refresh(&jobs, &summit, &dossier, JobClass::Open, now())
            .await
            .unwrap();
        running.start(Some("engine-1"), now()).unwrap();
        jobs.save(&running).await.unwrap();

        let asked = request_refresh(&jobs, &summit, &dossier, JobClass::Asked, now())
            .await
            .unwrap();
        assert_eq!(asked.id, running.id);
        assert_eq!(asked.class, JobClass::Open);
    }
}
