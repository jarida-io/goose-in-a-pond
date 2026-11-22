//! SQLite-backed [`EngineJobRepository`] over the `engine_jobs` table.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::engine_job::{
    sort_for_running, EngineJob, JobClass, JobKind, JobState,
};
use pond_core::user_data::ports::engine_jobs::EngineJobRepository;

use crate::sqlite_rows::{
    count_in, json_in, json_of, maybe_stamp, maybe_time_of, parse_time, stamp, time_of, word_in,
};

pub struct SqliteEngineJobRepository {
    pool: Pool<Sqlite>,
}

impl SqliteEngineJobRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, task_id, profile_id, kind, class, anchor_id, deliverable_id, \
     subject, due_at, state, rounds, objections_json, outcome, created_at, started_at, \
     finished_at";

const OPEN: &str = "state IN ('queued', 'running')";

fn job_of(row: &SqliteRow) -> Result<EngineJob> {
    Ok(EngineJob {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        profile_id: row.try_get("profile_id")?,
        kind: word_in(row, "kind", JobKind::parse)?,
        class: word_in(row, "class", JobClass::parse)?,
        anchor_id: row.try_get("anchor_id")?,
        deliverable_id: row.try_get("deliverable_id")?,
        subject: row.try_get("subject")?,
        due_at: maybe_time_of(row, "due_at")?,
        state: word_in(row, "state", JobState::parse)?,
        rounds: count_in(row, "rounds")?,
        objections: json_in(row, "objections_json")?,
        outcome: row.try_get("outcome")?,
        created_at: time_of(row, "created_at")?,
        started_at: maybe_time_of(row, "started_at")?,
        finished_at: maybe_time_of(row, "finished_at")?,
    })
}

fn in_running_order(rows: &[SqliteRow]) -> Result<Vec<EngineJob>> {
    let mut jobs = rows.iter().map(job_of).collect::<Result<Vec<_>>>()?;
    sort_for_running(&mut jobs);
    Ok(jobs)
}

#[async_trait]
impl EngineJobRepository for SqliteEngineJobRepository {
    async fn add(&self, job: &EngineJob) -> Result<()> {
        sqlx::query(
            "INSERT INTO engine_jobs (id, task_id, profile_id, kind, class, anchor_id, \
             deliverable_id, subject, due_at, state, rounds, objections_json, outcome, \
             created_at, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&job.id)
        .bind(&job.task_id)
        .bind(&job.profile_id)
        .bind(job.kind.as_str())
        .bind(job.class.as_str())
        .bind(&job.anchor_id)
        .bind(&job.deliverable_id)
        .bind(&job.subject)
        .bind(maybe_stamp(job.due_at))
        .bind(job.state.as_str())
        .bind(i64::from(job.rounds))
        .bind(json_of(&job.objections)?)
        .bind(&job.outcome)
        .bind(stamp(job.created_at))
        .bind(maybe_stamp(job.started_at))
        .bind(maybe_stamp(job.finished_at))
        .execute(&self.pool)
        .await
        .context("log an engine job")?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<EngineJob>> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM engine_jobs WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("read an engine job")?;
        row.as_ref().map(job_of).transpose()
    }

    async fn by_task(&self, task_id: &str) -> Result<Option<EngineJob>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM engine_jobs WHERE task_id = ? ORDER BY rowid DESC LIMIT 1"
        ))
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .context("read the engine job of a long task")?;
        row.as_ref().map(job_of).transpose()
    }

    async fn save(&self, job: &EngineJob) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE engine_jobs SET task_id = ?, profile_id = ?, kind = ?, class = ?, \
             anchor_id = ?, deliverable_id = ?, subject = ?, due_at = ?, state = ?, rounds = ?, \
             objections_json = ?, outcome = ?, started_at = ?, finished_at = ? WHERE id = ?",
        )
        .bind(&job.task_id)
        .bind(&job.profile_id)
        .bind(job.kind.as_str())
        .bind(job.class.as_str())
        .bind(&job.anchor_id)
        .bind(&job.deliverable_id)
        .bind(&job.subject)
        .bind(maybe_stamp(job.due_at))
        .bind(job.state.as_str())
        .bind(i64::from(job.rounds))
        .bind(json_of(&job.objections)?)
        .bind(&job.outcome)
        .bind(maybe_stamp(job.started_at))
        .bind(maybe_stamp(job.finished_at))
        .bind(&job.id)
        .execute(&self.pool)
        .await
        .context("save an engine job")?;
        Ok(done.rows_affected() > 0)
    }

    async fn open(&self) -> Result<Vec<EngineJob>> {
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM engine_jobs WHERE {OPEN}"))
            .fetch_all(&self.pool)
            .await
            .context("list the open engine jobs")?;
        in_running_order(&rows)
    }

    async fn open_by_class(&self, class: JobClass) -> Result<Vec<EngineJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM engine_jobs WHERE {OPEN} AND class = ?"
        ))
        .bind(class.as_str())
        .fetch_all(&self.pool)
        .await
        .context("list the open engine jobs of a class")?;
        in_running_order(&rows)
    }

    async fn open_for_deliverable(&self, deliverable_id: &str) -> Result<Option<EngineJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM engine_jobs WHERE {OPEN} AND deliverable_id = ?"
        ))
        .bind(deliverable_id)
        .fetch_all(&self.pool)
        .await
        .context("find a deliverable's open job")?;
        Ok(in_running_order(&rows)?.into_iter().next())
    }

    async fn open_for_capture(&self, capture_id: &str) -> Result<Option<EngineJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM engine_jobs \
             WHERE {OPEN} AND kind IN ('capture', 'ledger') AND subject = ?"
        ))
        .bind(capture_id)
        .fetch_all(&self.pool)
        .await
        .context("find a capture's open job")?;
        Ok(in_running_order(&rows)?.into_iter().next())
    }

    async fn open_of_kind(&self, profile_id: &str, kind: JobKind) -> Result<Vec<EngineJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM engine_jobs WHERE {OPEN} AND profile_id = ? AND kind = ?"
        ))
        .bind(profile_id)
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await
        .context("list a member's open engine jobs of a kind")?;
        in_running_order(&rows)
    }

    async fn count_open(&self, profile_id: &str, class: JobClass) -> Result<usize> {
        let open: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM engine_jobs WHERE {OPEN} AND profile_id = ? AND class = ?"
        ))
        .bind(profile_id)
        .bind(class.as_str())
        .fetch_one(&self.pool)
        .await
        .context("count a member's open engine jobs of a class")?;
        Ok(open.max(0) as usize)
    }

    async fn recent(&self, profile_id: &str, limit: usize) -> Result<Vec<EngineJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM engine_jobs WHERE profile_id = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT ?"
        ))
        .bind(profile_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("list a member's recent engine jobs")?;
        rows.iter().map(job_of).collect()
    }

    async fn last_finished(
        &self,
        profile_id: &str,
        kind: JobKind,
    ) -> Result<Option<DateTime<Utc>>> {
        let latest: Option<String> = sqlx::query_scalar(
            "SELECT MAX(finished_at) FROM engine_jobs WHERE profile_id = ? AND kind = ?",
        )
        .bind(profile_id)
        .bind(kind.as_str())
        .fetch_one(&self.pool)
        .await
        .context("read when a member's last engine job of a kind finished")?;
        latest.as_deref().map(parse_time).transpose()
    }

    async fn prune_finished(&self, before: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query(
            "DELETE FROM engine_jobs WHERE finished_at IS NOT NULL AND finished_at < ?",
        )
        .bind(stamp(before))
        .execute(&self.pool)
        .await
        .context("prune finished engine jobs")?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::domain::anchor::{Objection, ObjectionKind};
    use pond_core::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;

    fn job(member: &str, kind: JobKind, class: JobClass, minutes_old: i64) -> EngineJob {
        EngineJob::new(
            kind,
            class,
            Some(member),
            "a job",
            noon() - Duration::minutes(minutes_old),
        )
    }

    async fn a_job_round_trips_through_its_run(repo: &dyn EngineJobRepository) {
        let mut making = EngineJob::for_deliverable(
            "jerry",
            "a1",
            "d1",
            "Summit dossier",
            JobClass::Deadline,
            Some(noon() + Duration::days(4)),
            noon(),
        );
        repo.add(&making).await.unwrap();
        assert_eq!(repo.get(&making.id).await.unwrap(), Some(making.clone()));
        assert!(repo.add(&making).await.is_err(), "a job is logged once");
        assert_eq!(
            repo.open_for_deliverable("d1").await.unwrap(),
            Some(making.clone())
        );

        making.start(Some("engine-1"), noon()).unwrap();
        making.count_round(vec![Objection::new(
            ObjectionKind::Grounding,
            "the keynote time",
            "cite the programme",
        )]);
        assert!(repo.save(&making).await.unwrap());
        assert_eq!(
            repo.by_task("engine-1").await.unwrap(),
            Some(making.clone())
        );
        assert_eq!(repo.by_task("engine-2").await.unwrap(), None);

        making
            .settle(
                JobState::Unsure,
                "Shown with one objection",
                noon() + Duration::hours(1),
            )
            .unwrap();
        assert!(repo.save(&making).await.unwrap());
        assert_eq!(repo.open_for_deliverable("d1").await.unwrap(), None);
        assert_eq!(
            repo.last_finished("jerry", JobKind::Deliverable)
                .await
                .unwrap(),
            Some(noon() + Duration::hours(1))
        );
        assert_eq!(
            repo.last_finished("jerry", JobKind::FactSheet)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            repo.recent("jerry", 10).await.unwrap(),
            vec![making.clone()]
        );

        let mut ghost = making.clone();
        ghost.id = "never-logged".into();
        assert!(!repo.save(&ghost).await.unwrap());
    }

    async fn open_jobs_are_found_by_class_kind_and_what_they_serve(repo: &dyn EngineJobRepository) {
        let mut soon = job("jerry", JobKind::Deliverable, JobClass::Deadline, 1);
        soon.due_at = Some(noon() + Duration::days(1));
        let mut later = job("liz", JobKind::Deliverable, JobClass::Deadline, 40);
        later.due_at = Some(noon() + Duration::days(9));
        let sheet = job("jerry", JobKind::FactSheet, JobClass::Upkeep, 3);
        let asked = job("liz", JobKind::Deliverable, JobClass::Asked, 2);
        let idle_one = job("jerry", JobKind::Deliverable, JobClass::Open, 20);
        let idle_two = job("jerry", JobKind::Deliverable, JobClass::Open, 10);
        let reading = EngineJob::for_capture("jerry", "c1", JobKind::Capture, noon());
        let mut settled = job("jerry", JobKind::FactSheet, JobClass::Upkeep, 90);
        settled
            .settle(JobState::Passed, "41 lines", noon())
            .unwrap();
        let household = EngineJob::new(
            JobKind::Consolidation,
            JobClass::Upkeep,
            None,
            "Nightly pass",
            noon() - Duration::minutes(50),
        );
        for one in [
            &soon, &later, &sheet, &asked, &idle_one, &idle_two, &reading, &settled, &household,
        ] {
            repo.add(one).await.unwrap();
        }

        let open = repo.open().await.unwrap();
        let order: Vec<&str> = open.iter().map(|j| j.id.as_str()).collect();
        let expected: Vec<&str> = [
            &asked, &soon, &later, &household, &sheet, &reading, &idle_one, &idle_two,
        ]
        .iter()
        .map(|j| j.id.as_str())
        .collect();
        assert_eq!(order, expected);
        let deadlines = repo.open_by_class(JobClass::Deadline).await.unwrap();
        assert_eq!(deadlines, vec![soon.clone(), later.clone()]);

        assert_eq!(
            repo.open_of_kind("jerry", JobKind::FactSheet)
                .await
                .unwrap(),
            vec![sheet.clone()],
            "the settled one is not open"
        );
        assert!(repo
            .open_of_kind("liz", JobKind::FactSheet)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(repo.count_open("jerry", JobClass::Open).await.unwrap(), 2);
        assert_eq!(repo.count_open("liz", JobClass::Open).await.unwrap(), 0);
        assert_eq!(
            repo.open_for_capture("c1").await.unwrap(),
            Some(reading.clone())
        );
        assert_eq!(repo.open_for_capture("c2").await.unwrap(), None);

        assert_eq!(
            repo.recent("liz", 10).await.unwrap(),
            vec![asked.clone(), later.clone()],
            "never another member's log"
        );
        assert_eq!(repo.recent("jerry", 2).await.unwrap().len(), 2);

        assert_eq!(
            repo.prune_finished(noon() + Duration::hours(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(repo.get(&settled.id).await.unwrap(), None);
        assert_eq!(
            repo.open().await.unwrap().len(),
            8,
            "open jobs are never pruned"
        );
    }

    async fn sqlite() -> SqliteEngineJobRepository {
        SqliteEngineJobRepository::new(pool().await)
    }

    #[tokio::test]
    async fn an_engine_job_round_trips_through_its_run() {
        a_job_round_trips_through_its_run(&sqlite().await).await;
        a_job_round_trips_through_its_run(&InMemoryEngineJobRepository::new()).await;
    }

    #[tokio::test]
    async fn open_engine_jobs_are_found_by_class_kind_and_what_they_serve() {
        open_jobs_are_found_by_class_kind_and_what_they_serve(&sqlite().await).await;
        open_jobs_are_found_by_class_kind_and_what_they_serve(&InMemoryEngineJobRepository::new())
            .await;
    }

    #[tokio::test]
    async fn a_members_engine_jobs_go_with_the_member_and_a_household_pass_stays() {
        let repo = sqlite().await;
        repo.add(&job("liz", JobKind::FactSheet, JobClass::Upkeep, 1))
            .await
            .unwrap();
        repo.add(&EngineJob::new(
            JobKind::Consolidation,
            JobClass::Upkeep,
            None,
            "Nightly pass",
            noon(),
        ))
        .await
        .unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&repo.pool)
            .await
            .unwrap();
        let left = repo.open().await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].profile_id, None);
    }
}
