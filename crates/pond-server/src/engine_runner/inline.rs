//! An inline step: one model call whose prompt the code builds and whose reply is saved as an
//! artifact of the job's task. Each case is one function in its job kind's module.

use chrono::{DateTime, Utc};

use pond_core::models::domain::message::ImageAttachment;
use pond_core::shared::domain::long_task::{
    Artifact, ArtifactKind, ArtifactSummary, LongTask, OutcomeKind, PlanStep, StepOutcome,
};
use pond_core::shared::services::long_task_runner::brief_artifact;
use pond_core::user_data::domain::engine_job::{EngineJob, JobKind};
use pond_core::user_data::services::engine::steps::{draft_artifact, verdict_artifact};
use pond_core::user_data::services::engine::{ROLE_CRITIC, ROLE_MAKE, ROLE_REVISE};

use super::{
    capture, cards, deliverable, fact_sheet, ledger, questions, rule, EngineDeps, ModelStop,
};

/// Why a step of a kind with no case here fails.
pub const CANNOT_RUN: &str = "this pond cannot run this job yet";

/// What a model that answered with nothing is recorded as having said.
const NO_REPLY: &str = "(the model gave no reply)";

/// Whether this pond has a case for every inline step of a job of `kind`.
pub fn runs(kind: JobKind) -> bool {
    matches!(
        kind,
        JobKind::Deliverable
            | JobKind::FactSheet
            | JobKind::Questions
            | JobKind::Capture
            | JobKind::Ledger
            | JobKind::Rule
            | JobKind::Cards
    )
}

/// What a finished step saved, and the line its plan keeps about it.
pub(super) struct Saved {
    pub summary: String,
    pub artifacts: Vec<String>,
}

/// One inline step as its case sees it.
pub(super) struct StepCtx<'a> {
    pub deps: &'a EngineDeps,
    pub task: &'a LongTask,
    pub job: &'a EngineJob,
    pub step: &'a PlanStep,
    pub steps: &'a [PlanStep],
    pub artifacts: &'a [ArtifactSummary],
    pub now: DateTime<Utc>,
}

pub(super) fn failed(reason: impl std::fmt::Display) -> ModelStop {
    ModelStop::Failed(reason.to_string())
}

impl StepCtx<'_> {
    /// The member the job serves.
    pub fn member(&self) -> Result<&str, ModelStop> {
        self.job
            .profile_id
            .as_deref()
            .ok_or_else(|| failed("the job names no member"))
    }

    pub async fn ask(&self, system: &str, user: String) -> Result<String, ModelStop> {
        self.deps.model.complete(system, user).await
    }

    pub async fn ask_seeing(
        &self,
        system: &str,
        user: String,
        picture: ImageAttachment,
    ) -> Result<String, ModelStop> {
        self.deps.model.complete_seeing(system, user, picture).await
    }

    pub async fn body_of(&self, artifact: &ArtifactSummary) -> Result<String, ModelStop> {
        body_of(self.deps, self.task, artifact)
            .await
            .map_err(failed)
    }

    /// The task's opening brief; empty for a job opened without one.
    pub async fn brief(&self) -> Result<String, ModelStop> {
        match brief_artifact(self.artifacts) {
            Some(found) => self.body_of(found).await,
            None => Ok(String::new()),
        }
    }

    /// The draft as it stands, with the artifact that holds it.
    pub async fn draft(&self) -> Result<(&ArtifactSummary, String), ModelStop> {
        let found = draft_artifact(self.steps, self.artifacts)
            .ok_or_else(|| failed("the job has no draft to work on"))?;
        Ok((found, self.body_of(found).await?))
    }

    /// The reply of the check that ruled on the draft as it stands.
    pub async fn verdict(&self) -> Result<String, ModelStop> {
        let found = verdict_artifact(self.steps, self.artifacts)
            .ok_or_else(|| failed("the draft has no check to answer"))?;
        self.body_of(found).await
    }

    /// Saves `body` as an artifact of this step and returns its id.
    pub async fn save(
        &self,
        kind: ArtifactKind,
        title: &str,
        summary: &str,
        body: String,
    ) -> Result<String, ModelStop> {
        let body = if body.trim().is_empty() {
            NO_REPLY.to_string()
        } else {
            body
        };
        let artifact = Artifact::new(
            self.task.id(),
            Some(self.step.key().clone()),
            kind,
            title,
            summary,
            body,
            Utc::now(),
        )
        .map_err(failed)?;
        self.deps
            .tasks
            .insert_artifact(&artifact)
            .await
            .map_err(failed)?;
        Ok(artifact.id().to_string())
    }
}

pub(super) async fn body_of(
    deps: &EngineDeps,
    task: &LongTask,
    artifact: &ArtifactSummary,
) -> anyhow::Result<String> {
    deps.tasks
        .get_artifact(task.id(), &artifact.id)
        .await?
        .map(|artifact| artifact.body().to_string())
        .ok_or_else(|| anyhow::anyhow!("a saved artifact is gone"))
}

async fn run_case(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    match (ctx.job.kind, ctx.step.role().trim()) {
        (JobKind::Deliverable, ROLE_CRITIC) => deliverable::critic(ctx).await,
        (JobKind::Deliverable, ROLE_REVISE) => deliverable::revise(ctx).await,
        (JobKind::FactSheet, ROLE_MAKE) => fact_sheet::make(ctx).await,
        (JobKind::FactSheet, ROLE_CRITIC) => fact_sheet::critic(ctx).await,
        (JobKind::Questions, ROLE_MAKE) => questions::make(ctx).await,
        (JobKind::Questions, ROLE_CRITIC) => questions::critic(ctx).await,
        (JobKind::Capture, ROLE_MAKE) => capture::make(ctx).await,
        (JobKind::Capture, ROLE_CRITIC) => capture::critic(ctx).await,
        (JobKind::Ledger, ROLE_MAKE) => ledger::make(ctx).await,
        (JobKind::Ledger, ROLE_CRITIC) => ledger::critic(ctx).await,
        (JobKind::Rule, ROLE_MAKE) => rule::make(ctx).await,
        (JobKind::Rule, ROLE_CRITIC) => rule::critic(ctx).await,
        (JobKind::Rule, ROLE_REVISE) => rule::revise(ctx).await,
        (JobKind::Cards, ROLE_MAKE) => cards::make(ctx).await,
        (JobKind::Cards, ROLE_CRITIC) => cards::critic(ctx).await,
        _ => Err(failed(CANNOT_RUN)),
    }
}

/// Runs one step whose role is inline and settles it. A call a person's turn cut puts the
/// step back to pending without spending the attempt. `Ok(true)`: the task moved.
pub async fn run_inline_step(
    deps: &EngineDeps,
    task: &LongTask,
    step: &PlanStep,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let mut step = step.clone();
    step.start(now)?;
    deps.tasks.update_step(&step).await?;
    let steps = deps.tasks.list_steps(task.id()).await?;
    let artifacts = deps.tasks.list_artifacts(task.id()).await?;
    let end = match deps.jobs.by_task(task.id()).await? {
        Some(job) => {
            let ctx = StepCtx {
                deps,
                task,
                job: &job,
                step: &step,
                steps: &steps,
                artifacts: &artifacts,
                now,
            };
            run_case(&ctx).await
        }
        None => Err(failed("its job is gone")),
    };
    match &end {
        Ok(saved) => {
            let outcome = StepOutcome::new(
                OutcomeKind::Completed,
                &saved.summary,
                saved.artifacts.clone(),
                None,
            );
            step.record(&outcome, Utc::now())?;
        }
        Err(ModelStop::Cut) => step.preempt()?,
        Err(ModelStop::Failed(reason)) => step.fail(reason, Utc::now())?,
    }
    deps.tasks.update_step(&step).await?;
    tracing::info!(
        target: "giap::trace",
        kind = "engine_step_settled",
        task_id = %task.id(),
        step = %step.key(),
        role = step.role(),
        state = step.state().as_str(),
        attempt = step.attempts(),
        failure = step.failure().unwrap_or(""),
        "an inline step settled"
    );
    Ok(true)
}
