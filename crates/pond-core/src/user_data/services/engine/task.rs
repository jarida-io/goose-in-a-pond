//! The long task an engine job runs as.

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use super::steps::inline_plan;
use super::{DELIVERABLE_MINUTES, INLINE_MINUTES};
use crate::shared::domain::long_task::{
    AnswerShape, LongTask, LongTaskBudget, LongTaskError, LongTaskKind, LongTaskParts,
    LongTaskState, LongTaskStateRefused, PlanError, ValidatedPlan,
};
use crate::shared::services::deep_research::{GOAL_CHARS, RESEARCH_STEPS};
use crate::user_data::domain::engine_job::{new_engine_task_id, EngineJob, JobKind, MAX_ROUNDS};
use crate::user_data::domain::profile::ProfileScope;

/// Steps the critic loop adds at most: a check each round and a revision between rounds.
pub const LOOP_STEPS: u32 = 2 * MAX_ROUNDS - 1;
/// A deliverable's research, with room left for its checks whatever the planner used.
pub const DELIVERABLE_STEPS: u32 = RESEARCH_STEPS + LOOP_STEPS;
/// An inline job's maker step and its checks.
pub const INLINE_STEPS: u32 = 1 + LOOP_STEPS;

/// Why a job got no long task.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskRefused {
    #[error("a {0} job keeps its own slot on the inference lane and runs as no long task")]
    NotATask(&'static str),
    #[error(transparent)]
    Task(#[from] LongTaskError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    State(#[from] LongTaskStateRefused),
}

fn with_engine_id(task: LongTask) -> Result<LongTask, LongTaskError> {
    LongTask::from_parts(LongTaskParts {
        id: new_engine_task_id(),
        owner_profile_id: task.owner_profile_id().map(str::to_string),
        scope: task.scope().clone(),
        goal: task.goal().to_string(),
        origin_session_id: task.origin_session_id().map(str::to_string),
        state: task.state(),
        max_steps: task.budget().max_steps(),
        deadline: task.budget().deadline(),
        kind: task.kind(),
        answer: task.answer(),
        created_at: task.created_at(),
        updated_at: task.updated_at(),
        finished_at: task.finished_at(),
    })
}

/// The long task for `job`, owned by `owner`. A deliverable is a research task left to its
/// planner. Any other job comes back Paused with its one-step plan: store the task, store the
/// plan, then move it to Running, so the runner never meets it without its step.
pub fn task_for_job(
    job: &EngineJob,
    goal: &str,
    owner: &str,
    now: DateTime<Utc>,
) -> Result<(LongTask, Option<ValidatedPlan>), TaskRefused> {
    let (kind, answer, steps, minutes) = match job.kind {
        JobKind::Memory | JobKind::Consolidation => {
            return Err(TaskRefused::NotATask(job.kind.as_str()))
        }
        JobKind::Deliverable => (
            LongTaskKind::Research,
            AnswerShape::Report,
            DELIVERABLE_STEPS,
            DELIVERABLE_MINUTES,
        ),
        _ => (
            LongTaskKind::General,
            AnswerShape::Auto,
            INLINE_STEPS,
            INLINE_MINUTES,
        ),
    };
    let goal: String = goal.trim().chars().take(GOAL_CHARS).collect();
    let budget = LongTaskBudget::new(steps, Some(now + Duration::minutes(minutes)))?;
    let started = LongTask::start(
        ProfileScope::Owner(owner.to_string()),
        None,
        &goal,
        None,
        budget,
        now,
    )?
    .with_kind(kind)
    .with_answer(answer);
    let mut task = with_engine_id(started)?;
    if job.kind == JobKind::Deliverable {
        return Ok((task, None));
    }
    task.transition_to(LongTaskState::Paused, now)?;
    let plan = inline_plan(&task, &goal)?;
    Ok((task, Some(plan)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::{
        PlanStep, PlanStepParts, StepState, MAX_LONG_TASK_STEPS,
    };
    use crate::user_data::domain::engine_job::{is_engine_task, JobClass};
    use crate::user_data::services::engine::steps::{critic_step, revise_step, MAKE_KEY};
    use crate::user_data::services::engine::{ROLE_CRITIC, ROLE_MAKE, ROLE_REVISE};

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    fn job(kind: JobKind) -> EngineJob {
        EngineJob::new(kind, JobClass::Deadline, Some("jerry"), "a job", now())
    }

    #[test]
    fn a_deliverable_job_is_a_research_task_left_to_its_planner() {
        let (task, plan) = task_for_job(
            &job(JobKind::Deliverable),
            "  Write a dossier for the summit.  ",
            "jerry",
            now(),
        )
        .unwrap();
        assert!(plan.is_none());
        assert!(is_engine_task(task.id()));
        assert_eq!(task.kind(), LongTaskKind::Research);
        assert_eq!(task.answer(), AnswerShape::Report);
        assert_eq!(task.state(), LongTaskState::Planning);
        assert_eq!(task.owner_profile_id(), Some("jerry"));
        assert_eq!(task.goal(), "Write a dossier for the summit.");
        assert_eq!(task.origin_session_id(), None);
        assert_eq!(
            task.budget().deadline(),
            Some(now() + Duration::minutes(DELIVERABLE_MINUTES))
        );
        assert_eq!(task.budget().max_steps(), RESEARCH_STEPS + LOOP_STEPS);
        assert!(DELIVERABLE_STEPS <= MAX_LONG_TASK_STEPS);
    }

    #[test]
    fn any_other_job_is_one_maker_step_and_waits_paused_for_it_to_be_stored() {
        for kind in [
            JobKind::FactSheet,
            JobKind::Heartbeat,
            JobKind::Capture,
            JobKind::Ledger,
            JobKind::Rule,
            JobKind::Questions,
        ] {
            let (task, plan) =
                task_for_job(&job(kind), "Write the fact sheet.", "jerry", now()).unwrap();
            let plan = plan.unwrap_or_else(|| panic!("{kind:?} has no plan"));
            assert!(is_engine_task(task.id()), "{kind:?}");
            assert_eq!(task.state(), LongTaskState::Paused, "{kind:?}");
            assert_eq!(task.kind(), LongTaskKind::General);
            assert_eq!(
                task.budget().deadline(),
                Some(now() + Duration::minutes(INLINE_MINUTES))
            );
            assert_eq!(plan.task_id(), task.id());
            assert_eq!(plan.steps().len(), 1);
            let make = &plan.steps()[0];
            assert_eq!((make.key().as_str(), make.role()), (MAKE_KEY, ROLE_MAKE));
            assert_eq!(make.instructions(), "Write the fact sheet.");
        }
    }

    #[test]
    fn every_task_gets_its_own_id() {
        let job = job(JobKind::Rule);
        let (first, _) = task_for_job(&job, "Write the rule.", "jerry", now()).unwrap();
        let (second, _) = task_for_job(&job, "Write the rule.", "jerry", now()).unwrap();
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn a_pass_that_keeps_its_own_lane_slot_and_a_job_with_no_goal_get_no_task() {
        for kind in [JobKind::Memory, JobKind::Consolidation] {
            assert_eq!(
                task_for_job(&job(kind), "Extract memories.", "jerry", now()),
                Err(TaskRefused::NotATask(kind.as_str()))
            );
        }
        assert_eq!(
            task_for_job(&job(JobKind::Rule), "   ", "jerry", now()),
            Err(TaskRefused::Task(LongTaskError::MissingGoal))
        );
        let long = "g".repeat(GOAL_CHARS + 100);
        let (task, _) = task_for_job(&job(JobKind::Deliverable), &long, "jerry", now()).unwrap();
        assert_eq!(task.goal().chars().count(), GOAL_CHARS);
    }

    #[test]
    fn an_inline_task_has_room_for_every_round_of_the_critic_loop_and_no_more() {
        let (mut task, plan) =
            task_for_job(&job(JobKind::Rule), "Write the rule.", "jerry", now()).unwrap();
        task.transition_to(LongTaskState::Running, now()).unwrap();
        let done = |key: &str, ordinal: u32, role: &str| {
            PlanStep::from_parts(PlanStepParts {
                task_id: task.id().to_string(),
                key: key.to_string(),
                ordinal,
                role: role.to_string(),
                instructions: "done".to_string(),
                inputs: Vec::new(),
                depends_on: Vec::new(),
                acceptance: None,
                state: StepState::Completed,
                attempts: 1,
                summary: None,
                failure: None,
                started_at: None,
                finished_at: None,
                agent: None,
            })
            .unwrap()
        };
        let mut steps = vec![done(MAKE_KEY, 1, ROLE_MAKE)];
        assert_eq!(plan.unwrap().steps()[0].ordinal(), 1);
        for round in 1..=MAX_ROUNDS {
            let added = critic_step(&task, &steps, &[]).unwrap();
            let critic = added.steps().last().unwrap();
            assert_eq!(critic.key().as_str(), format!("critic-{round}"));
            steps.push(done(critic.key().as_str(), critic.ordinal(), ROLE_CRITIC));
            if round < MAX_ROUNDS {
                let added = revise_step(&task, &steps, &[]).unwrap();
                let revise = added.steps().last().unwrap();
                assert_eq!(revise.key().as_str(), format!("revise-{round}"));
                steps.push(done(revise.key().as_str(), revise.ordinal(), ROLE_REVISE));
            }
        }
        assert_eq!(steps.len(), INLINE_STEPS as usize);
        assert!(
            revise_step(&task, &steps, &[]).is_err(),
            "the cap is the loop"
        );
    }
}
