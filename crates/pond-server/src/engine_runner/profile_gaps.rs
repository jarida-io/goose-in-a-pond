//! The fact-sheet gap question, asked once the sheet is rebuilt: code picks the gap, one model
//! call words it, and the question passes code's checks before it is stored. Never pushed.

use chrono::{DateTime, Utc};

use pond_core::user_data::domain::fact_sheet::FactState;
use pond_core::user_data::domain::question::QuestionPurpose;
use pond_core::user_data::services::fact_sheet::sheet_for_brief;
use pond_core::user_data::services::profile_gaps::{
    build_profile_question_message, is_due, next_gap, parse_profile_question,
    PROFILE_QUESTION_SYSTEM,
};
use pond_core::user_data::services::question_desk::keep_new;

use super::{EngineDeps, ModelStop};

/// Every question the member was asked is compared, so a gap tried before is not asked again.
const ASKED_COMPARED: usize = 500;

/// Asks the member about one gap in their fact sheet, at most once a day; returns whether a
/// question was stored.
pub(super) async fn ask_profile_gap(
    deps: &EngineDeps,
    member: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let last = deps
        .questions
        .last_batch_at(member, QuestionPurpose::Profile)
        .await?;
    if !is_due(last, now) {
        return Ok(false);
    }
    let lines = deps.facts.list(member, FactState::Active).await?;
    let asked = deps.questions.recent(member, ASKED_COMPARED).await?;
    let Some(gap) = next_gap(&lines, &asked) else {
        return Ok(false);
    };
    let message = build_profile_question_message(gap, &sheet_for_brief(&lines));
    let reply = match deps.model.complete(PROFILE_QUESTION_SYSTEM, message).await {
        Ok(reply) => reply,
        Err(ModelStop::Cut) => return Ok(false),
        Err(ModelStop::Failed(why)) => anyhow::bail!("the gap question was not worded: {why}"),
    };
    let batch_id = uuid::Uuid::new_v4().to_string();
    let Some(question) = parse_profile_question(&reply, member, &batch_id, gap, now) else {
        tracing::info!(
            member,
            gap = gap.id,
            "the gap question failed code's checks"
        );
        return Ok(false);
    };
    let kept = keep_new(
        vec![question],
        member,
        deps.questions.as_ref(),
        deps.similar.as_deref(),
    )
    .await?;
    if deps.questions.add_batch(&kept).await?.is_empty() {
        return Ok(false);
    }
    deps.questions.mark_asked(member, &batch_id, now).await?;
    Ok(true)
}
