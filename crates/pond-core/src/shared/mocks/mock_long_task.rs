//! In-memory [`LongTaskRepository`] with `sqlite_long_task`'s semantics: compare-and-set moves,
//! all-or-nothing replans, cascading deletes and insertion order as every tiebreak.

use crate::shared::domain::long_task::{
    AnswerShape, Artifact, ArtifactSummary, LongTask, LongTaskState, PlanStep, StepStateRefused,
    ValidatedPlan,
};
use crate::shared::ports::long_task::{LongTaskOwnerFilter, LongTaskRepository};
use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Mutex;

#[derive(Default)]
pub struct MockLongTaskRepository {
    ledger: Mutex<Ledger>,
}

/// Each `Vec` is in insertion order, standing in for SQLite's rowid.
#[derive(Default)]
struct Ledger {
    tasks: Vec<LongTask>,
    steps: Vec<PlanStep>,
    artifacts: Vec<Artifact>,
}

impl MockLongTaskRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every step held, for any task, in insertion order.
    pub fn all_steps(&self) -> Vec<PlanStep> {
        self.ledger.lock().unwrap().steps.clone()
    }

    /// How many artifacts are held, for any task.
    pub fn artifact_count(&self) -> usize {
        self.ledger.lock().unwrap().artifacts.len()
    }
}

/// The SQL adapter's owner predicate, restated; the two must agree.
fn owned_by(task: &LongTask, owner: LongTaskOwnerFilter<'_>) -> bool {
    match owner {
        LongTaskOwnerFilter::Everyone => true,
        LongTaskOwnerFilter::Member(id) => task.owner_profile_id() == Some(id),
        LongTaskOwnerFilter::Unowned => task.owner_profile_id().is_none(),
    }
}

#[async_trait]
impl LongTaskRepository for MockLongTaskRepository {
    async fn create_task(&self, task: &LongTask) -> Result<()> {
        let mut ledger = self.ledger.lock().unwrap();
        if ledger.tasks.iter().any(|held| held.id() == task.id()) {
            bail!("long task {} already exists", task.id());
        }
        ledger.tasks.push(task.clone());
        Ok(())
    }

    async fn get_task(&self, id: &str) -> Result<Option<LongTask>> {
        let ledger = self.ledger.lock().unwrap();
        Ok(ledger.tasks.iter().find(|task| task.id() == id).cloned())
    }

    async fn list_tasks(
        &self,
        owner: LongTaskOwnerFilter<'_>,
        include_finished: bool,
    ) -> Result<Vec<LongTask>> {
        let ledger = self.ledger.lock().unwrap();
        // Reversed first, so the stable sort leaves same-instant tasks newest-inserted first.
        let mut tasks: Vec<LongTask> = ledger
            .tasks
            .iter()
            .rev()
            .filter(|task| owned_by(task, owner))
            .filter(|task| include_finished || !task.state().is_terminal())
            .cloned()
            .collect();
        tasks.sort_by(|a, b| b.created_at().cmp(&a.created_at()));
        Ok(tasks)
    }

    async fn set_task_state(
        &self,
        id: &str,
        state: LongTaskState,
        at: DateTime<Utc>,
    ) -> Result<Option<LongTask>> {
        let mut ledger = self.ledger.lock().unwrap();
        let Some(task) = ledger.tasks.iter_mut().find(|task| task.id() == id) else {
            return Ok(None);
        };
        task.transition_to(state, at)?;
        Ok(Some(task.clone()))
    }

    async fn decide_task_answer(&self, id: &str, answer: AnswerShape) -> Result<bool> {
        let mut ledger = self.ledger.lock().unwrap();
        Ok(ledger
            .tasks
            .iter_mut()
            .find(|task| task.id() == id)
            .is_some_and(|task| task.decide_answer(answer)))
    }

    async fn replace_open_steps(&self, plan: &ValidatedPlan) -> Result<()> {
        let mut guard = self.ledger.lock().unwrap();
        let ledger = &mut *guard;
        let task_id = plan.task_id();
        let Some(task) = ledger.tasks.iter().find(|task| task.id() == task_id) else {
            bail!("there is no long task {task_id} to plan");
        };
        if task.state().is_terminal() {
            bail!(
                "long task {task_id} is {} and takes no new plan",
                task.state()
            );
        }
        plan.check_current(
            ledger
                .steps
                .iter()
                .filter(|step| step.task_id() == task_id)
                .map(|step| (step.key().as_str(), step.state())),
        )?;
        ledger
            .steps
            .retain(|step| step.task_id() != task_id || step.state().survives_replan());
        ledger.steps.extend(plan.steps().iter().cloned());
        Ok(())
    }

    async fn list_steps(&self, task_id: &str) -> Result<Vec<PlanStep>> {
        let ledger = self.ledger.lock().unwrap();
        let mut steps: Vec<PlanStep> = ledger
            .steps
            .iter()
            .filter(|step| step.task_id() == task_id)
            .cloned()
            .collect();
        steps.sort_by_key(PlanStep::ordinal);
        Ok(steps)
    }

    /// Replaces the whole step, where SQL writes only the run columns; they differ only when the
    /// caller holds a step from before a replan.
    async fn update_step(&self, step: &PlanStep) -> Result<()> {
        let mut ledger = self.ledger.lock().unwrap();
        let Some(stored) = ledger
            .steps
            .iter_mut()
            .find(|held| held.task_id() == step.task_id() && held.key() == step.key())
        else {
            bail!(
                "there is no step `{}` in long task {}",
                step.key(),
                step.task_id()
            );
        };
        if !stored.state().can_become(step.state()) {
            return Err(StepStateRefused {
                step: step.key().to_string(),
                from: stored.state(),
                to: step.state(),
            }
            .into());
        }
        *stored = step.clone();
        Ok(())
    }

    async fn insert_artifact(&self, artifact: &Artifact) -> Result<()> {
        let mut ledger = self.ledger.lock().unwrap();
        if !ledger
            .tasks
            .iter()
            .any(|task| task.id() == artifact.task_id())
        {
            bail!(
                "there is no long task {} for this artifact",
                artifact.task_id()
            );
        }
        if ledger
            .artifacts
            .iter()
            .any(|held| held.id() == artifact.id())
        {
            bail!("artifact {} already exists", artifact.id());
        }
        ledger.artifacts.push(artifact.clone());
        Ok(())
    }

    async fn get_artifact(&self, task_id: &str, artifact_id: &str) -> Result<Option<Artifact>> {
        let ledger = self.ledger.lock().unwrap();
        Ok(ledger
            .artifacts
            .iter()
            .find(|artifact| artifact.id() == artifact_id && artifact.task_id() == task_id)
            .cloned())
    }

    async fn list_artifacts(&self, task_id: &str) -> Result<Vec<ArtifactSummary>> {
        let ledger = self.ledger.lock().unwrap();
        let mut artifacts: Vec<&Artifact> = ledger
            .artifacts
            .iter()
            .filter(|artifact| artifact.task_id() == task_id)
            .collect();
        artifacts.sort_by_key(|artifact| artifact.created_at());
        Ok(artifacts.into_iter().map(Artifact::summary_view).collect())
    }

    async fn delete_task(&self, id: &str) -> Result<bool> {
        let mut ledger = self.ledger.lock().unwrap();
        let before = ledger.tasks.len();
        ledger.tasks.retain(|task| task.id() != id);
        if ledger.tasks.len() == before {
            return Ok(false);
        }
        ledger.steps.retain(|step| step.task_id() != id);
        ledger.artifacts.retain(|artifact| artifact.task_id() != id);
        Ok(true)
    }

    async fn delete_artifact(&self, task_id: &str, artifact_id: &str) -> Result<bool> {
        let mut ledger = self.ledger.lock().unwrap();
        let before = ledger.artifacts.len();
        ledger
            .artifacts
            .retain(|artifact| !(artifact.task_id() == task_id && artifact.id() == artifact_id));
        Ok(ledger.artifacts.len() < before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::{
        validate_plan, ArtifactKind, LongTaskBudget, LongTaskStateRefused, OutcomeKind, StepDraft,
        StepOutcome, StepState,
    };
    use crate::user_data::domain::profile::ProfileScope;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn task_at(owner: Option<&str>, at: DateTime<Utc>) -> LongTask {
        let scope = match owner {
            Some(id) => ProfileScope::Owner(id.to_string()),
            None => ProfileScope::Household,
        };
        LongTask::start(
            scope,
            None,
            "sort the photos",
            None,
            LongTaskBudget::new(6, None).unwrap(),
            at,
        )
        .unwrap()
    }

    fn draft(key: &str, deps: &[&str]) -> StepDraft {
        StepDraft {
            key: key.to_string(),
            role: "researcher".to_string(),
            instructions: format!("do {key}"),
            depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
            ..StepDraft::default()
        }
    }

    async fn planned(repo: &MockLongTaskRepository, task: &LongTask, keys: &[&str]) {
        let drafts = keys.iter().map(|key| draft(key, &[])).collect();
        let plan = validate_plan(task, drafts, &repo.list_steps(task.id()).await.unwrap()).unwrap();
        repo.replace_open_steps(&plan).await.unwrap();
    }

    async fn step(repo: &MockLongTaskRepository, task: &LongTask, key: &str) -> PlanStep {
        repo.list_steps(task.id())
            .await
            .unwrap()
            .into_iter()
            .find(|step| step.key().as_str() == key)
            .unwrap()
    }

    #[tokio::test]
    async fn a_finished_task_refuses_to_move_and_is_left_as_it_was() {
        let repo = MockLongTaskRepository::new();
        let task = task_at(None, t(0));
        repo.create_task(&task).await.unwrap();

        let done = repo
            .set_task_state(task.id(), LongTaskState::Completed, t(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.finished_at(), Some(t(5)));

        let refused = repo
            .set_task_state(task.id(), LongTaskState::Running, t(6))
            .await
            .unwrap_err();
        assert!(
            refused.downcast_ref::<LongTaskStateRefused>().is_some(),
            "{refused}"
        );
        assert_eq!(repo.get_task(task.id()).await.unwrap().unwrap(), done);
        assert!(repo
            .set_task_state("nobody", LongTaskState::Running, t(6))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_replan_keeps_finished_and_running_steps_and_replaces_the_rest() {
        let repo = MockLongTaskRepository::new();
        let task = task_at(None, t(0));
        repo.create_task(&task).await.unwrap();
        planned(&repo, &task, &["done", "busy", "dropped", "redone"]).await;

        let mut done = step(&repo, &task, "done").await;
        done.start(t(1)).unwrap();
        repo.update_step(&done).await.unwrap();
        done.record(
            &StepOutcome::new(OutcomeKind::Completed, "found it", vec![], None),
            t(2),
        )
        .unwrap();
        repo.update_step(&done).await.unwrap();
        let mut busy = step(&repo, &task, "busy").await;
        busy.start(t(3)).unwrap();
        repo.update_step(&busy).await.unwrap();
        let mut redone = step(&repo, &task, "redone").await;
        redone.start(t(3)).unwrap();
        repo.update_step(&redone).await.unwrap();
        redone.fail("timed out", t(4)).unwrap();
        repo.update_step(&redone).await.unwrap();

        planned(&repo, &task, &["redone", "new"]).await;
        let steps = repo.list_steps(task.id()).await.unwrap();
        let keys: Vec<&str> = steps.iter().map(|step| step.key().as_str()).collect();
        assert_eq!(keys, vec!["done", "busy", "redone", "new"]);
        assert_eq!(steps[0], done, "a finished step was rewritten");
        assert_eq!(steps[1], busy, "a running step was rewritten");
        assert_eq!(steps[2].state(), StepState::Pending);
        assert_eq!(steps[2].attempts(), 0, "the replaced step kept its old run");
    }

    #[tokio::test]
    async fn a_stale_plan_writes_nothing() {
        let repo = MockLongTaskRepository::new();
        let task = task_at(None, t(0));
        repo.create_task(&task).await.unwrap();
        planned(&repo, &task, &["a"]).await;
        let plan = validate_plan(
            &task,
            vec![draft("a", &[])],
            &repo.list_steps(task.id()).await.unwrap(),
        )
        .unwrap();

        let mut a = step(&repo, &task, "a").await;
        a.start(t(1)).unwrap();
        repo.update_step(&a).await.unwrap();

        assert!(repo.replace_open_steps(&plan).await.is_err());
        assert_eq!(repo.list_steps(task.id()).await.unwrap(), vec![a]);
    }

    #[tokio::test]
    async fn an_illegal_step_move_is_refused() {
        let repo = MockLongTaskRepository::new();
        let task = task_at(None, t(0));
        repo.create_task(&task).await.unwrap();
        planned(&repo, &task, &["a"]).await;
        let mut a = step(&repo, &task, "a").await;
        a.start(t(1)).unwrap();
        a.fail("no network", t(2)).unwrap();
        let refused = repo.update_step(&a).await.unwrap_err();
        assert!(
            refused.downcast_ref::<StepStateRefused>().is_some(),
            "{refused}"
        );
        assert_eq!(step(&repo, &task, "a").await.state(), StepState::Pending);
    }

    #[tokio::test]
    async fn listings_break_ties_by_insertion_and_filter_by_owner() {
        let repo = MockLongTaskRepository::new();
        let first = task_at(Some("liz"), t(0));
        let second = task_at(None, t(0));
        let older = task_at(Some("liz"), t(-10));
        for task in [&first, &second, &older] {
            repo.create_task(task).await.unwrap();
        }
        let all: Vec<String> = repo
            .list_tasks(LongTaskOwnerFilter::Everyone, false)
            .await
            .unwrap()
            .iter()
            .map(|task| task.id().to_string())
            .collect();
        assert_eq!(all, vec![second.id(), first.id(), older.id()]);
        assert_eq!(
            repo.list_tasks(LongTaskOwnerFilter::Member("liz"), false)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            repo.list_tasks(LongTaskOwnerFilter::Unowned, false)
                .await
                .unwrap()
                .iter()
                .map(LongTask::id)
                .collect::<Vec<_>>(),
            vec![second.id()]
        );
    }

    #[tokio::test]
    async fn deleting_a_task_takes_its_steps_and_artifacts_and_nothing_else() {
        let repo = MockLongTaskRepository::new();
        let gone = task_at(None, t(0));
        let kept = task_at(None, t(1));
        for task in [&gone, &kept] {
            repo.create_task(task).await.unwrap();
            planned(&repo, task, &["a"]).await;
            let artifact = Artifact::new(
                task.id(),
                None,
                ArtifactKind::Note,
                "note",
                "a note",
                "body".to_string(),
                t(2),
            )
            .unwrap();
            repo.insert_artifact(&artifact).await.unwrap();
        }
        assert!(repo.delete_task(gone.id()).await.unwrap());
        assert!(!repo.delete_task(gone.id()).await.unwrap());
        assert!(repo
            .all_steps()
            .iter()
            .all(|step| step.task_id() == kept.id()));
        assert_eq!(repo.all_steps().len(), 1, "the other task's step went too");
        assert_eq!(repo.artifact_count(), 1);
    }
}
