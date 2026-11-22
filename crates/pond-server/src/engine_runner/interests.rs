//! A member's interests as the tick reads them: an interest marked avoided stops the anchor made
//! for it, and a guess seen often enough is offered as a question. A confirmed interest gets no
//! anchor of its own: a standing topic gave only vague weekly digests.

use chrono::{DateTime, Duration, Utc};

use pond_core::shared::domain::long_task::LongTaskState;
use pond_core::shared::domain::todo::zone;
use pond_core::user_data::domain::anchor::{Anchor, AnchorOrigin, AnchorState};
use pond_core::user_data::domain::engine_job::{EngineJob, JobState};
use pond_core::user_data::domain::interest::Interest;
use pond_core::user_data::domain::question::{interest_question, QuestionPurpose};

use super::{deliverable, EngineDeps};

/// One batch of interest questions a member gets in this long at most.
const ONE_BATCH_EVERY: Duration = Duration::days(1);

/// What one look at a member's interests did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Followed {
    pub stopped: usize,
}

fn is_own(anchor: &Anchor, member: &str) -> bool {
    anchor.profile_id == member
}

/// Stops the anchor of an interest now marked avoided. `anchors` is the member's in every state.
pub(super) async fn follow(
    deps: &EngineDeps,
    member: &str,
    anchors: &mut [Anchor],
    open_jobs: &[EngineJob],
    now: DateTime<Utc>,
) -> anyhow::Result<Followed> {
    let mut followed = Followed::default();
    for interest in deps.interests.list(member).await? {
        if interest.avoid {
            let held = anchors.iter_mut().find(|a| {
                is_own(a, member)
                    && a.origin == AnchorOrigin::Interest
                    && a.origin_ref.as_deref() == Some(interest.id.as_str())
                    && a.state == AnchorState::Active
            });
            if let Some(anchor) = held {
                stop(deps, anchor, open_jobs, now).await?;
                followed.stopped += 1;
            }
        }
    }
    Ok(followed)
}

/// Stops an anchor the member no longer follows and calls off the jobs working for it.
async fn stop(
    deps: &EngineDeps,
    anchor: &mut Anchor,
    open_jobs: &[EngineJob],
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    anchor.stop(now)?;
    deps.anchors.save(anchor).await?;
    for job in open_jobs
        .iter()
        .filter(|job| job.anchor_id.as_deref() == Some(anchor.id.as_str()))
    {
        let Some(mut job) = deps.jobs.get(&job.id).await? else {
            continue;
        };
        if job
            .settle(JobState::Cancelled, "Stopped: no longer followed.", now)
            .is_err()
        {
            continue;
        }
        deps.jobs.save(&job).await?;
        if let Some(task_id) = job.task_id.as_deref() {
            let _ = deps
                .tasks
                .set_task_state(task_id, LongTaskState::Cancelled, now)
                .await;
        }
        deliverable::rest(deps, &job, now).await;
    }
    Ok(())
}

/// The one wording an interest is asked about in, so the store's key asks it once.
fn ask_text(interest: &Interest) -> String {
    format!(
        "You keep coming back to {}. Should I follow it for you?",
        interest.topic
    )
}

/// Asks the member about one guess worth confirming that was never asked about, at most one a
/// day; returns how many were asked.
pub(super) async fn offer(
    deps: &EngineDeps,
    member: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let last = deps
        .questions
        .last_batch_at(member, QuestionPurpose::Interest)
        .await?;
    if last.is_some_and(|at| now - at < ONE_BATCH_EVERY) {
        return Ok(0);
    }
    let (timezone, _) = deps.clock_and_place().await;
    let ready = deps
        .interests
        .ready_to_offer(member, zone(&timezone))
        .await?;
    let batch_id = uuid::Uuid::new_v4().to_string();
    let mut asked = Vec::new();
    for interest in &ready {
        let Some(mut question) = interest_question(member, &batch_id, &ask_text(interest), now)
        else {
            continue;
        };
        question.subject_ref = Some(interest.id.clone());
        // The store refuses one asked before; the next guess is tried in its place.
        asked = deps.questions.add_batch(&[question]).await?;
        if !asked.is_empty() {
            break;
        }
    }
    if asked.is_empty() {
        return Ok(0);
    }
    // Not pushed: the question waits in My Goose and on Home until the member looks.
    deps.questions.mark_asked(member, &batch_id, now).await?;
    Ok(asked.len())
}
