//! What follows a job's steps once they are all settled, and the steps the engine writes itself.

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::critic::Verdict;
use super::{ROLE_CRITIC, ROLE_MAKE, ROLE_REVISE};
use crate::shared::domain::long_task::{
    validate_appended_step, validate_plan, ArtifactSummary, LongTask, PlanError, PlanStep,
    StepDraft, StepState, ValidatedPlan,
};
use crate::shared::services::long_task_roles::VERIFIER_ROLE;
use crate::shared::services::long_task_runner::RESULT_TITLE;
use crate::user_data::domain::engine_job::{EngineJob, JobState, MAX_ROUNDS};

/// The key of an inline job's one maker step.
pub const MAKE_KEY: &str = "make";
const CRITIC_KEY: &str = "critic";
const REVISE_KEY: &str = "revise";

const CRITIC_STEP: &str = "Check the draft against its brief, its sources, what is known about \
     the member and their don't-list.";
const REVISE_STEP: &str = "Correct the draft where the check objected, and change nothing else.";

/// What the runner does with a job whose steps are all settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    AddCritic,
    AddRevise,
    Settle(JobState),
}

fn is_a_check(step: &PlanStep) -> bool {
    step.role() == ROLE_CRITIC || step.role() == VERIFIER_ROLE
}

fn completed(step: &PlanStep) -> bool {
    step.state() == StepState::Completed
}

/// The completed step whose output is the job's draft as it stands: the last that is not a
/// check.
pub fn latest_draft_step(steps: &[PlanStep]) -> Option<&PlanStep> {
    steps
        .iter()
        .filter(|step| completed(step) && !is_a_check(step))
        .max_by_key(|step| step.ordinal())
}

fn critics_after<'a>(
    steps: &'a [PlanStep],
    draft: &'a PlanStep,
) -> impl Iterator<Item = &'a PlanStep> {
    steps
        .iter()
        .filter(move |step| step.role() == ROLE_CRITIC && step.ordinal() > draft.ordinal())
}

/// The completed critic step that ruled on the draft as it stands.
pub fn latest_critic_step(steps: &[PlanStep]) -> Option<&PlanStep> {
    let draft = latest_draft_step(steps)?;
    critics_after(steps, draft)
        .filter(|step| completed(step))
        .max_by_key(|step| step.ordinal())
}

/// What comes after a job's steps; ask only once every step is settled. `verdict` is the
/// ruling of [`latest_critic_step`], and a ruling that is missing counts as a fail.
pub fn next_after_steps(
    job: &EngineJob,
    steps: &[PlanStep],
    verdict: Option<&Verdict>,
    now: DateTime<Utc>,
) -> Next {
    let Some(draft) = latest_draft_step(steps) else {
        return Next::Settle(JobState::Failed);
    };
    let without_a_pass = Next::Settle(job.kind.settles_without_a_pass_as());
    if latest_critic_step(steps).is_none() {
        // A check that was added and never completed is not added again.
        return match critics_after(steps, draft).next() {
            None => Next::AddCritic,
            Some(_) => without_a_pass,
        };
    }
    if verdict.is_some_and(|verdict| verdict.pass) {
        return Next::Settle(JobState::Passed);
    }
    // The steps are the count: the caller may not have counted this round on the job yet.
    let checked = steps
        .iter()
        .filter(|step| step.role() == ROLE_CRITIC && completed(step))
        .count();
    let rounds = job.rounds.max(u32::try_from(checked).unwrap_or(u32::MAX));
    let revisions = steps
        .iter()
        .filter(|step| step.role() == ROLE_REVISE)
        .count();
    let may_revise =
        rounds < MAX_ROUNDS && revisions + 1 < MAX_ROUNDS as usize && !job.is_out_of_time(now);
    if may_revise {
        Next::AddRevise
    } else {
        without_a_pass
    }
}

/// Why the engine could not add a step of its own.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StepRefused {
    #[error("the job has no finished draft to check")]
    NoDraft,
    #[error("the job's draft has no check to answer")]
    NoCheck,
    #[error(transparent)]
    Plan(#[from] PlanError),
}

/// The whole plan of an inline job: one maker step, so no planner runs.
pub fn inline_plan(task: &LongTask, instructions: &str) -> Result<ValidatedPlan, PlanError> {
    validate_plan(
        task,
        vec![StepDraft {
            key: MAKE_KEY.to_string(),
            role: ROLE_MAKE.to_string(),
            instructions: instructions.to_string(),
            ..StepDraft::default()
        }],
        &[],
    )
}

fn saved_by<'a>(step: &PlanStep, artifacts: &'a [ArtifactSummary]) -> Vec<&'a ArtifactSummary> {
    artifacts
        .iter()
        .filter(|artifact| artifact.step_key.as_ref() == Some(step.key()))
        .collect()
}

/// The draft as it stands: what [`latest_draft_step`] saved as its Result, else what it saved
/// last.
pub fn draft_artifact<'a>(
    steps: &[PlanStep],
    artifacts: &'a [ArtifactSummary],
) -> Option<&'a ArtifactSummary> {
    let saved = saved_by(latest_draft_step(steps)?, artifacts);
    saved
        .iter()
        .rev()
        .find(|artifact| artifact.title.trim().eq_ignore_ascii_case(RESULT_TITLE))
        .or(saved.last())
        .copied()
}

/// The ruling on the draft as it stands: what [`latest_critic_step`] saved last.
pub fn verdict_artifact<'a>(
    steps: &[PlanStep],
    artifacts: &'a [ArtifactSummary],
) -> Option<&'a ArtifactSummary> {
    saved_by(latest_critic_step(steps)?, artifacts)
        .last()
        .copied()
}

/// `critic-1`, `revise-2`: numbered by how many steps of the role the task already has.
fn round_key(base: &str, role: &str, steps: &[PlanStep]) -> String {
    let taken = |key: &str| steps.iter().any(|step| step.key().as_str() == key);
    let mut round = steps.iter().filter(|step| step.role() == role).count() + 1;
    loop {
        let key = format!("{base}-{round}");
        if !taken(&key) {
            return key;
        }
        round += 1;
    }
}

/// The plan with a critic step added for the draft as it stands, given that draft as its input.
pub fn critic_step(
    task: &LongTask,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
) -> Result<ValidatedPlan, StepRefused> {
    let draft = latest_draft_step(steps).ok_or(StepRefused::NoDraft)?;
    let step = StepDraft {
        key: round_key(CRITIC_KEY, ROLE_CRITIC, steps),
        role: ROLE_CRITIC.to_string(),
        instructions: CRITIC_STEP.to_string(),
        inputs: draft_artifact(steps, artifacts)
            .map(|artifact| artifact.id.clone())
            .into_iter()
            .collect(),
        depends_on: vec![draft.key().to_string()],
        ..StepDraft::default()
    };
    Ok(validate_appended_step(task, step, steps)?)
}

/// The plan with a revise step added, given the draft and the ruling that failed it.
pub fn revise_step(
    task: &LongTask,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
) -> Result<ValidatedPlan, StepRefused> {
    let critic = latest_critic_step(steps).ok_or(StepRefused::NoCheck)?;
    let step = StepDraft {
        key: round_key(REVISE_KEY, ROLE_REVISE, steps),
        role: ROLE_REVISE.to_string(),
        instructions: REVISE_STEP.to_string(),
        inputs: draft_artifact(steps, artifacts)
            .into_iter()
            .chain(verdict_artifact(steps, artifacts))
            .map(|artifact| artifact.id.clone())
            .collect(),
        depends_on: vec![critic.key().to_string()],
        ..StepDraft::default()
    };
    Ok(validate_appended_step(task, step, steps)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::{
        ArtifactKind, LongTaskBudget, LongTaskState, PlanStepParts, StepKey,
    };
    use crate::shared::services::long_task_roles::{COMPOSER_ROLE, WORKER_ROLE};
    use crate::user_data::domain::anchor::{Objection, ObjectionKind};
    use crate::user_data::domain::engine_job::{JobClass, JobKind};
    use crate::user_data::domain::profile::ProfileScope;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000, 0).unwrap()
    }

    fn task(max_steps: u32) -> LongTask {
        let mut task = LongTask::start(
            ProfileScope::Owner("jerry".into()),
            None,
            "a dossier for the summit",
            None,
            LongTaskBudget::new(max_steps, None).unwrap(),
            now(),
        )
        .unwrap();
        task.transition_to(LongTaskState::Running, now()).unwrap();
        task
    }

    fn step(task: &LongTask, key: &str, ordinal: u32, role: &str, state: StepState) -> PlanStep {
        PlanStep::from_parts(PlanStepParts {
            task_id: task.id().to_string(),
            key: key.to_string(),
            ordinal,
            role: role.to_string(),
            instructions: format!("do {key}"),
            inputs: Vec::new(),
            depends_on: Vec::new(),
            acceptance: None,
            state,
            attempts: 1,
            summary: None,
            failure: None,
            started_at: None,
            finished_at: None,
            agent: None,
        })
        .unwrap()
    }

    fn done(task: &LongTask, key: &str, ordinal: u32, role: &str) -> PlanStep {
        step(task, key, ordinal, role, StepState::Completed)
    }

    fn saved(task: &LongTask, id: &str, step_key: &str, title: &str) -> ArtifactSummary {
        ArtifactSummary {
            id: id.to_string(),
            task_id: task.id().to_string(),
            step_key: Some(StepKey::parse(step_key).unwrap()),
            kind: ArtifactKind::Draft,
            title: title.to_string(),
            summary: "what it holds".to_string(),
            tokens: 10,
            created_at: now(),
        }
    }

    fn job(kind: JobKind) -> EngineJob {
        let mut job = EngineJob::new(kind, JobClass::Deadline, Some("jerry"), "a job", now());
        job.start(Some("engine-1"), now()).unwrap();
        job
    }

    fn pass() -> Verdict {
        Verdict {
            pass: true,
            objections: Vec::new(),
        }
    }

    fn fail() -> Verdict {
        Verdict {
            pass: false,
            objections: vec![Objection::new(
                ObjectionKind::Vague,
                "the plan",
                "name days",
            )],
        }
    }

    #[test]
    fn a_draft_with_no_verdict_on_it_gets_a_critic() {
        let task = task(8);
        let inline = [done(&task, "make", 1, ROLE_MAKE)];
        assert_eq!(
            next_after_steps(&job(JobKind::Rule), &inline, None, now()),
            Next::AddCritic
        );

        let research = [
            done(&task, "gather", 1, WORKER_ROLE),
            done(&task, "write", 2, COMPOSER_ROLE),
        ];
        assert_eq!(
            next_after_steps(&job(JobKind::Deliverable), &research, None, now()),
            Next::AddCritic
        );
        assert_eq!(
            next_after_steps(&job(JobKind::Deliverable), &research, Some(&pass()), now()),
            Next::AddCritic,
            "a verdict from before the draft does not stand for it"
        );
    }

    #[test]
    fn a_pass_settles_the_job_as_passed() {
        let task = task(8);
        let steps = [
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
        ];
        assert_eq!(
            next_after_steps(&job(JobKind::FactSheet), &steps, Some(&pass()), now()),
            Next::Settle(JobState::Passed)
        );
    }

    #[test]
    fn a_fail_with_time_and_rounds_left_is_revised_and_the_revision_is_checked_again() {
        let task = task(8);
        let mut steps = vec![
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
        ];
        let job = job(JobKind::Deliverable);
        assert_eq!(
            next_after_steps(&job, &steps, Some(&fail()), now()),
            Next::AddRevise
        );
        assert_eq!(
            next_after_steps(&job, &steps, None, now()),
            Next::AddRevise,
            "a ruling nobody can find is a fail"
        );
        steps.push(done(&task, "revise-1", 3, ROLE_REVISE));
        assert_eq!(
            next_after_steps(&job, &steps, Some(&fail()), now()),
            Next::AddCritic
        );
    }

    #[test]
    fn a_fail_out_of_rounds_shows_a_deliverable_unsure_and_drops_anything_else() {
        let task = task(8);
        let steps = [
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
            done(&task, "revise-1", 3, ROLE_REVISE),
            done(&task, "critic-2", 4, ROLE_CRITIC),
            done(&task, "revise-2", 5, ROLE_REVISE),
            done(&task, "critic-3", 6, ROLE_CRITIC),
        ];
        assert_eq!(
            next_after_steps(&job(JobKind::Deliverable), &steps, Some(&fail()), now()),
            Next::Settle(JobState::Unsure)
        );
        for kind in [
            JobKind::FactSheet,
            JobKind::Heartbeat,
            JobKind::Capture,
            JobKind::Rule,
            JobKind::Questions,
        ] {
            assert_eq!(
                next_after_steps(&job(kind), &steps, Some(&fail()), now()),
                Next::Settle(JobState::Dropped),
                "{kind:?}"
            );
        }
        assert_eq!(
            next_after_steps(&job(JobKind::Deliverable), &steps, Some(&pass()), now()),
            Next::Settle(JobState::Passed),
            "the last round may still pass"
        );
    }

    #[test]
    fn a_fail_out_of_time_settles_without_a_revision() {
        let task = task(8);
        let steps = [
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
        ];
        let inline = job(JobKind::Questions);
        assert_eq!(
            next_after_steps(
                &inline,
                &steps,
                Some(&fail()),
                now() + Duration::minutes(14)
            ),
            Next::AddRevise
        );
        assert_eq!(
            next_after_steps(
                &inline,
                &steps,
                Some(&fail()),
                now() + Duration::minutes(15)
            ),
            Next::Settle(JobState::Dropped)
        );
        let deliverable = job(JobKind::Deliverable);
        assert_eq!(
            next_after_steps(
                &deliverable,
                &steps,
                Some(&fail()),
                now() + Duration::minutes(59)
            ),
            Next::AddRevise
        );
        assert_eq!(
            next_after_steps(
                &deliverable,
                &steps,
                Some(&fail()),
                now() + Duration::minutes(60)
            ),
            Next::Settle(JobState::Unsure)
        );
    }

    #[test]
    fn rounds_already_counted_on_the_job_are_not_counted_twice() {
        let task = task(8);
        let steps = [
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
        ];
        let mut counted = job(JobKind::Deliverable);
        counted.count_round(fail().objections);
        assert_eq!(
            next_after_steps(&counted, &steps, Some(&fail()), now()),
            Next::AddRevise
        );
        counted.rounds = MAX_ROUNDS;
        assert_eq!(
            next_after_steps(&counted, &steps, Some(&fail()), now()),
            Next::Settle(JobState::Unsure)
        );
    }

    #[test]
    fn a_job_with_no_draft_fails_and_a_check_or_revision_that_died_is_not_tried_for_ever() {
        let task = task(8);
        let nothing = [step(&task, "make", 1, ROLE_MAKE, StepState::Skipped)];
        assert_eq!(
            next_after_steps(&job(JobKind::Rule), &nothing, None, now()),
            Next::Settle(JobState::Failed)
        );
        assert_eq!(
            next_after_steps(&job(JobKind::Rule), &[], None, now()),
            Next::Settle(JobState::Failed)
        );

        let unchecked = [
            done(&task, "make", 1, ROLE_MAKE),
            step(&task, "critic-1", 2, ROLE_CRITIC, StepState::Skipped),
        ];
        assert_eq!(
            next_after_steps(&job(JobKind::Deliverable), &unchecked, None, now()),
            Next::Settle(JobState::Unsure)
        );

        let mut steps = vec![
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
            step(&task, "revise-1", 3, ROLE_REVISE, StepState::Skipped),
        ];
        let job = job(JobKind::Deliverable);
        assert_eq!(
            next_after_steps(&job, &steps, Some(&fail()), now()),
            Next::AddRevise
        );
        steps.push(step(&task, "revise-2", 4, ROLE_REVISE, StepState::Skipped));
        assert_eq!(
            next_after_steps(&job, &steps, Some(&fail()), now()),
            Next::Settle(JobState::Unsure)
        );
    }

    #[test]
    fn an_inline_job_is_planned_as_one_maker_step() {
        let task = task(8);
        let plan = inline_plan(&task, "Write the questions for the summit.").unwrap();
        assert_eq!(plan.steps().len(), 1);
        let make = &plan.steps()[0];
        assert_eq!(make.key().as_str(), MAKE_KEY);
        assert_eq!(make.role(), ROLE_MAKE);
        assert_eq!(make.ordinal(), 1);
        assert!(crate::user_data::services::engine::is_inline_role(
            make.role()
        ));
        assert!(inline_plan(&task, "  ").is_err());
    }

    #[test]
    fn critic_and_revise_steps_are_numbered_by_round_and_follow_the_steps_before_them() {
        let task = task(8);
        let mut steps = vec![done(&task, "make", 1, ROLE_MAKE)];
        let mut artifacts = vec![saved(&task, "draft-1", "make", "Draft")];

        let plan = critic_step(&task, &steps, &artifacts).unwrap();
        let critic = plan.steps().last().unwrap();
        assert_eq!(
            (critic.key().as_str(), critic.role(), critic.ordinal()),
            ("critic-1", ROLE_CRITIC, 2)
        );
        assert_eq!(critic.depends_on()[0].as_str(), "make");
        assert_eq!(critic.inputs(), ["draft-1"]);
        assert_eq!(critic.state(), StepState::Pending);
        assert_eq!(plan.kept_keys()[0].as_str(), "make");

        steps.push(done(&task, "critic-1", 2, ROLE_CRITIC));
        artifacts.push(saved(&task, "verdict-1", "critic-1", "Verdict"));
        let plan = revise_step(&task, &steps, &artifacts).unwrap();
        let revise = plan.steps().last().unwrap();
        assert_eq!((revise.key().as_str(), revise.ordinal()), ("revise-1", 3));
        assert_eq!(revise.role(), ROLE_REVISE);
        assert_eq!(revise.depends_on()[0].as_str(), "critic-1");
        assert_eq!(revise.inputs(), ["draft-1", "verdict-1"]);

        steps.push(done(&task, "revise-1", 3, ROLE_REVISE));
        artifacts.push(saved(&task, "draft-2", "revise-1", "Draft"));
        let plan = critic_step(&task, &steps, &artifacts).unwrap();
        let critic = plan.steps().last().unwrap();
        assert_eq!((critic.key().as_str(), critic.ordinal()), ("critic-2", 4));
        assert_eq!(critic.depends_on()[0].as_str(), "revise-1");
        assert_eq!(critic.inputs(), ["draft-2"]);
    }

    #[test]
    fn a_research_tasks_report_is_the_draft_its_critic_reads() {
        let task = task(8);
        let steps = [
            done(&task, "gather", 1, WORKER_ROLE),
            done(&task, "write", 2, COMPOSER_ROLE),
            done(&task, "verify", 3, VERIFIER_ROLE),
        ];
        let artifacts = [
            saved(&task, "notes", "gather", "Programme notes"),
            saved(&task, "report", "write", "Summit dossier"),
            saved(&task, "result", "write", "Result"),
            saved(&task, "sources", "write", "Sources"),
        ];
        assert_eq!(latest_draft_step(&steps).unwrap().key().as_str(), "write");
        assert_eq!(draft_artifact(&steps, &artifacts).unwrap().id, "result");
        assert_eq!(
            draft_artifact(&steps, &artifacts[..2]).unwrap().id,
            "report",
            "with no Result, what the step saved last"
        );
        let plan = critic_step(&task, &steps, &artifacts).unwrap();
        let critic = plan.steps().last().unwrap();
        assert_eq!((critic.key().as_str(), critic.ordinal()), ("critic-1", 4));
        assert_eq!(critic.depends_on()[0].as_str(), "write");
        assert_eq!(critic.inputs(), ["result"]);
        assert!(verdict_artifact(&steps, &artifacts).is_none());
    }

    #[test]
    fn a_step_the_engine_cannot_add_says_why() {
        let task = task(2);
        assert_eq!(critic_step(&task, &[], &[]), Err(StepRefused::NoDraft));
        let steps = [done(&task, "make", 1, ROLE_MAKE)];
        assert_eq!(revise_step(&task, &steps, &[]), Err(StepRefused::NoCheck));

        let full = [
            done(&task, "make", 1, ROLE_MAKE),
            done(&task, "critic-1", 2, ROLE_CRITIC),
        ];
        assert!(matches!(
            revise_step(&task, &full, &[]),
            Err(StepRefused::Plan(PlanError::TooManySteps { .. }))
        ));
    }
}
