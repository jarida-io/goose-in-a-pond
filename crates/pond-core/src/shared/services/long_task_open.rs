//! Storing a new long task. A short answer the member chose is stored with its one step and starts
//! Running, so no planner runs for it; anything else starts Planning, as before.
use crate::shared::domain::long_task::{
    AnswerShape, Artifact, ArtifactKind, LongTask, LongTaskState,
};
use crate::shared::ports::long_task::LongTaskRepository;
use crate::shared::services::long_task_runner::{short_answer_plan, BRIEF_TITLE};
use anyhow::Result;
use chrono::Utc;
use std::sync::{Arc, OnceLock};
use tokio::sync::Notify;

static RUNNER_BELL: OnceLock<Arc<Notify>> = OnceLock::new();

/// The long-task runner's bell, rung by every [`open_task`]: a route that opens a task has no other
/// way to reach the runner, which would otherwise find it on its next poll.
pub fn install_runner_bell(bell: Arc<Notify>) {
    let _ = RUNNER_BELL.set(bell);
}

fn ring_runner() {
    if let Some(bell) = RUNNER_BELL.get() {
        bell.notify_one();
    }
}

/// What the conversation said that the goal leaves out, saved as the task's brief.
pub struct OpeningBrief<'a> {
    pub summary: &'a str,
    pub body: String,
}

/// Store `task` with its brief, and for a chosen short answer its one step. Returns the task as
/// stored.
pub async fn open_task(
    repo: &dyn LongTaskRepository,
    mut task: LongTask,
    brief: Option<OpeningBrief<'_>>,
) -> Result<LongTask> {
    let quick = task.answer() == AnswerShape::Short;
    // Paused until its step is stored, so the runner never plans a short answer.
    if quick {
        task.transition_to(LongTaskState::Paused, Utc::now())?;
    }
    repo.create_task(&task).await?;
    if let Some(brief) = brief {
        let artifact = Artifact::new(
            task.id(),
            None,
            ArtifactKind::Note,
            BRIEF_TITLE,
            brief.summary,
            brief.body,
            Utc::now(),
        )?;
        repo.insert_artifact(&artifact).await?;
    }
    if !quick {
        ring_runner();
        return Ok(task);
    }
    let started = async {
        repo.replace_open_steps(&short_answer_plan(&task)?).await?;
        repo.set_task_state(task.id(), LongTaskState::Running, Utc::now())
            .await?
            .ok_or_else(|| anyhow::anyhow!("long task {} vanished while it opened", task.id()))
    }
    .await;
    match started {
        Ok(running) => {
            ring_runner();
            Ok(running)
        }
        Err(e) => {
            let _ = repo
                .set_task_state(task.id(), LongTaskState::Cancelled, Utc::now())
                .await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::LongTaskBudget;
    use crate::shared::mocks::mock_long_task::MockLongTaskRepository;
    use crate::shared::services::long_task_runner::{next_run, NextRun, SHORT_ANSWER_KEY};
    use crate::user_data::domain::profile::ProfileScope;

    fn task(answer: AnswerShape) -> LongTask {
        LongTask::start(
            ProfileScope::Owner("liz".into()),
            None,
            "who rang the bell this morning",
            Some("s-1".into()),
            LongTaskBudget::new(6, None).unwrap(),
            Utc::now(),
        )
        .unwrap()
        .with_answer(answer)
    }

    #[tokio::test]
    async fn a_chosen_short_answer_runs_its_one_step_with_no_planner() {
        let repo = MockLongTaskRepository::default();
        let brief = OpeningBrief {
            summary: "before the ask",
            body: "We have a doorbell camera.".into(),
        };
        let opened = open_task(&repo, task(AnswerShape::Short), Some(brief))
            .await
            .unwrap();
        assert_eq!(opened.state(), LongTaskState::Running);
        let steps = repo.list_steps(opened.id()).await.unwrap();
        assert_eq!(steps.len(), 1);
        match next_run(&opened, &steps, 0, Utc::now()) {
            NextRun::Do(step) => assert_eq!(step.key().as_str(), SHORT_ANSWER_KEY),
            other => panic!("a short answer should run its step, not {other:?}"),
        }
        assert_eq!(repo.list_artifacts(opened.id()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn any_other_task_is_left_to_its_planner() {
        for answer in [AnswerShape::Auto, AnswerShape::Report] {
            let repo = MockLongTaskRepository::default();
            let opened = open_task(&repo, task(answer), None).await.unwrap();
            assert_eq!(opened.state(), LongTaskState::Planning);
            assert!(repo.list_steps(opened.id()).await.unwrap().is_empty());
        }
    }
}
