//! In-memory [`EngineJobRepository`], ordered as the SQLite adapter orders.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::engine_job::{sort_for_running, EngineJob, JobClass, JobKind};
use crate::user_data::ports::engine_jobs::EngineJobRepository;

#[derive(Default)]
pub struct InMemoryEngineJobRepository {
    jobs: Mutex<Vec<EngineJob>>,
}

impl InMemoryEngineJobRepository {
    pub fn new() -> Self {
        Self::default()
    }

    fn open_where(&self, keep: impl Fn(&EngineJob) -> bool) -> Vec<EngineJob> {
        let mut found: Vec<EngineJob> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| j.state.is_open() && keep(j))
            .cloned()
            .collect();
        sort_for_running(&mut found);
        found
    }
}

#[async_trait]
impl EngineJobRepository for InMemoryEngineJobRepository {
    async fn add(&self, job: &EngineJob) -> Result<()> {
        let mut jobs = self.jobs.lock().unwrap();
        anyhow::ensure!(
            jobs.iter().all(|j| j.id != job.id),
            "an engine job with that id is already stored"
        );
        jobs.push(job.clone());
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<EngineJob>> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .find(|j| j.id == id)
            .cloned())
    }

    async fn by_task(&self, task_id: &str) -> Result<Option<EngineJob>> {
        let jobs = self.jobs.lock().unwrap();
        Ok(jobs
            .iter()
            .rev()
            .find(|j| j.task_id.as_deref() == Some(task_id))
            .cloned())
    }

    async fn save(&self, job: &EngineJob) -> Result<bool> {
        let mut jobs = self.jobs.lock().unwrap();
        match jobs.iter_mut().find(|j| j.id == job.id) {
            Some(held) => {
                *held = job.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn open(&self) -> Result<Vec<EngineJob>> {
        Ok(self.open_where(|_| true))
    }

    async fn open_by_class(&self, class: JobClass) -> Result<Vec<EngineJob>> {
        Ok(self.open_where(|j| j.class == class))
    }

    async fn open_for_deliverable(&self, deliverable_id: &str) -> Result<Option<EngineJob>> {
        Ok(self
            .open_where(|j| j.deliverable_id.as_deref() == Some(deliverable_id))
            .into_iter()
            .next())
    }

    async fn open_for_capture(&self, capture_id: &str) -> Result<Option<EngineJob>> {
        Ok(self
            .open_where(|j| {
                matches!(j.kind, JobKind::Capture | JobKind::Ledger) && j.subject == capture_id
            })
            .into_iter()
            .next())
    }

    async fn open_of_kind(&self, profile_id: &str, kind: JobKind) -> Result<Vec<EngineJob>> {
        Ok(self.open_where(|j| j.profile_id.as_deref() == Some(profile_id) && j.kind == kind))
    }

    async fn count_open(&self, profile_id: &str, class: JobClass) -> Result<usize> {
        Ok(self
            .open_where(|j| j.profile_id.as_deref() == Some(profile_id) && j.class == class)
            .len())
    }

    async fn recent(&self, profile_id: &str, limit: usize) -> Result<Vec<EngineJob>> {
        let mut found: Vec<EngineJob> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|j| j.profile_id.as_deref() == Some(profile_id))
            .cloned()
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        found.truncate(limit);
        Ok(found)
    }

    async fn last_finished(
        &self,
        profile_id: &str,
        kind: JobKind,
    ) -> Result<Option<DateTime<Utc>>> {
        let jobs = self.jobs.lock().unwrap();
        Ok(jobs
            .iter()
            .filter(|j| j.profile_id.as_deref() == Some(profile_id) && j.kind == kind)
            .filter_map(|j| j.finished_at)
            .max())
    }

    async fn prune_finished(&self, before: DateTime<Utc>) -> Result<u64> {
        let mut jobs = self.jobs.lock().unwrap();
        let held = jobs.len();
        jobs.retain(|j| !j.finished_at.is_some_and(|at| at < before));
        Ok((held - jobs.len()) as u64)
    }
}
