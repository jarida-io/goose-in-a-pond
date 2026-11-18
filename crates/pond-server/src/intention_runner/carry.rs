//! What each act of a met intention does: tell the member, refresh what was made for a plan,
//! look something up, put a plan's open questions to them, or offer a to-do.

use chrono::{DateTime, Duration, Utc};

use pond_core::mcp::ports::notification::{MemberDelivery, Notification, OpenScreen};
use pond_core::shared::domain::goose_activity::{ActivityKind, GooseActivity};
use pond_core::shared::services::goose_thread::thread_id;
use pond_core::shared::services::todo_offers::{offer_due, offer_notice};
use pond_core::user_data::domain::anchor::{Anchor, AnchorState, DeliverableState};
use pond_core::user_data::domain::engine_job::JobClass;
use pond_core::user_data::domain::intention::{text_key, IntentAct, Intention};
use pond_core::user_data::domain::message_note::{MessageNote, SourceKind};
use pond_core::user_data::domain::question::QuestionState;
use pond_core::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use pond_core::user_data::services::engine::refresh::request_refresh;
use pond_core::user_data::services::proactive_review::MAX_ERRANDS_PER_DAY;

use super::{IntentionDeps, Looked};
use crate::engine_runner::{questions_notice, tell_member};
use crate::system_notices::goose_activity_notification;

const HEADS_UP: &str = "A heads-up";
/// What an offered to-do's row holds where a mention's words about its time would be. It names
/// no time, so the member picks one when they add it.
pub(super) const NO_TIME_SAID: &str = "no time yet";
const PICK_A_TIME: &str = "Add it and pick when I should remind you.";

/// What carrying an intention out came to.
pub(super) struct Carried {
    /// One plain sentence for the member.
    pub outcome: String,
    /// What a look-up came back with; empty when it ran and found nothing.
    pub found: Option<String>,
}

impl Carried {
    pub(super) fn said(outcome: impl Into<String>) -> Self {
        Self {
            outcome: outcome.into(),
            found: None,
        }
    }
}

fn counted(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("One {one}"),
        count => format!("{count} {many}"),
    }
}

/// The anchor an intention serves, whoever owns it.
async fn anchor_of(deps: &IntentionDeps, job: &Intention) -> anyhow::Result<Option<Anchor>> {
    match job.anchor_id.as_deref() {
        Some(id) => deps.engine.anchors.find(id).await,
        None => Ok(None),
    }
}

pub(super) async fn out(
    deps: &IntentionDeps,
    job: &Intention,
    now: DateTime<Utc>,
) -> anyhow::Result<Carried> {
    match job.act {
        IntentAct::Push => push(deps, job, now).await,
        IntentAct::Refresh => refresh(deps, job, now).await,
        IntentAct::LookInto => look_into(deps, job, now).await,
        IntentAct::Ask => ask(deps, job, now).await,
        IntentAct::ProposeTodo => propose_todo(deps, job, now).await,
    }
}

async fn push(
    deps: &IntentionDeps,
    job: &Intention,
    now: DateTime<Utc>,
) -> anyhow::Result<Carried> {
    let member = job.profile_id.as_str();
    let anchor = anchor_of(deps, job).await?;
    let (title, open) = match &anchor {
        Some(anchor) => (
            anchor.title.clone(),
            OpenScreen::Anchor {
                anchor_id: anchor.id.clone(),
            },
        ),
        None => (
            HEADS_UP.to_string(),
            OpenScreen::chat(Some(&thread_id(Some(member)))),
        ),
    };
    let notice = Notification {
        id: format!("intention-{}", job.id),
        target: String::new(),
        category: "alert".to_string(),
        title,
        body: job.text.clone(),
        timestamp: now.to_rfc3339(),
        data: Some(open.data()),
    };
    let note = MessageNote::from_source(
        SourceKind::Heartbeat,
        job.condition_label.clone(),
        Some(job.id.clone()),
    );
    let said = match tell_member(&deps.engine, member, notice, Some(note)).await {
        MemberDelivery::Reached(_) => "Sent to your phone.",
        MemberDelivery::NoPhone => "You have no phone paired, so it is only in your Goose thread.",
        MemberDelivery::Failed(_) => {
            "Your phone could not be reached, so it is only in your Goose thread."
        }
    };
    Ok(Carried::said(said))
}

async fn refresh(
    deps: &IntentionDeps,
    job: &Intention,
    now: DateTime<Utc>,
) -> anyhow::Result<Carried> {
    let Some(anchor) = anchor_of(deps, job).await? else {
        return Ok(Carried::said(
            "Its plan is gone, so nothing was made again.",
        ));
    };
    if anchor.state != AnchorState::Active {
        return Ok(Carried::said(format!(
            "{} is over, so nothing was made again.",
            anchor.title
        )));
    }
    let shown: Vec<_> = deps
        .engine
        .anchors
        .deliverables_of(&anchor.id)
        .await?
        .into_iter()
        .filter(|made| {
            matches!(
                made.state,
                DeliverableState::Ready | DeliverableState::Unsure
            )
        })
        .collect();
    if shown.is_empty() {
        return Ok(Carried::said(format!(
            "Nothing made for {} was ready to make again.",
            anchor.title
        )));
    }
    for made in &shown {
        request_refresh(
            deps.engine.jobs.as_ref(),
            &anchor,
            made,
            JobClass::Deadline,
            now,
        )
        .await?;
    }
    let titles: Vec<&str> = shown.iter().map(|made| made.title.as_str()).collect();
    Ok(Carried::said(format!(
        "Making again: {}.",
        titles.join(", ")
    )))
}

async fn look_into(
    deps: &IntentionDeps,
    job: &Intention,
    now: DateTime<Utc>,
) -> anyhow::Result<Carried> {
    let member = job.profile_id.as_str();
    // Unreadable counts as spent, so a store that cannot be read looks nothing up.
    let looked_today = deps
        .activity
        .count_since(member, ActivityKind::Errand, now - Duration::days(1))
        .await
        .unwrap_or(MAX_ERRANDS_PER_DAY);
    if looked_today >= MAX_ERRANDS_PER_DAY {
        return Ok(Carried::said(
            "Not looked up: enough had been looked up for you in one day.",
        ));
    }
    let zone = deps.zone().await;
    let answer = match deps
        .errands
        .look_into(member, &job.text, &job.why, now, zone)
        .await
    {
        Looked::Found(answer) => answer,
        Looked::Nothing => {
            return Ok(Carried {
                outcome: "It could not be found out.".to_string(),
                found: Some(String::new()),
            })
        }
        Looked::NotRun(why) => return Ok(Carried::said(why)),
    };
    let mut kept = "The answer is in your Goose activity.";
    if let Some(thread) = &deps.engine.thread {
        let note =
            MessageNote::from_source(SourceKind::Lookup, job.text.clone(), Some(job.id.clone()));
        match thread.post(member, &answer, note).await {
            Ok(_) => kept = "The answer is in your Goose thread.",
            Err(e) => tracing::warn!(error = %e, "a look-up's answer could not be posted"),
        }
    }
    // The notice an errand's answer has always had: what was asked, then what was found.
    let asked = GooseActivity::errand(member, &job.text, &answer, now);
    if let Some(notice) = goose_activity_notification(std::slice::from_ref(&asked)) {
        tell_member(&deps.engine, member, notice, None).await;
    }
    Ok(Carried {
        outcome: kept.to_string(),
        found: Some(answer),
    })
}

async fn ask(deps: &IntentionDeps, job: &Intention, now: DateTime<Utc>) -> anyhow::Result<Carried> {
    let Some(anchor) = anchor_of(deps, job).await? else {
        return Ok(Carried::said("Its plan is gone, so nothing was asked."));
    };
    let questions = &deps.engine.questions;
    let open: Vec<_> = questions
        .for_anchor(&anchor.profile_id, &anchor.id)
        .await?
        .into_iter()
        .filter(|question| question.state == QuestionState::Open)
        .collect();
    let Some(notice) = questions_notice(&anchor, &open, now) else {
        return Ok(Carried::said(format!(
            "There was no open question about {}.",
            anchor.title
        )));
    };
    tell_member(&deps.engine, &job.profile_id, notice, None).await;
    let mut batches: Vec<&str> = open.iter().map(|q| q.batch_id.as_str()).collect();
    batches.sort_unstable();
    batches.dedup();
    for batch in batches {
        questions.mark_asked(&anchor.profile_id, batch, now).await?;
    }
    let waiting = match open.len() {
        1 => "is waiting",
        _ => "are waiting",
    };
    Ok(Carried::said(format!(
        "{} about {} {waiting} for your answer.",
        counted(open.len(), "question", "questions"),
        anchor.title
    )))
}

async fn propose_todo(
    deps: &IntentionDeps,
    job: &Intention,
    now: DateTime<Utc>,
) -> anyhow::Result<Carried> {
    let member = job.profile_id.as_str();
    let key = job.key();
    if deps.todos.open_with_key(member, &key).await?.is_some() {
        return Ok(Carried::said(
            "It is already on your to-dos, so it was not offered.",
        ));
    }
    let scheduled = deps.schedules.words_for(member).await?;
    if scheduled.iter().any(|words| text_key(words) == key) {
        return Ok(Carried::said(
            "A schedule already does it, so it was not offered.",
        ));
    }
    let offered = deps.reminders.open_to(member).await?;
    if offered.iter().any(|offer| text_key(&offer.about) == key) {
        return Ok(Carried::said(
            "That offer was already waiting for your answer.",
        ));
    }
    let subject = match deps.engine.profiles.get(member).await {
        Ok(Some(profile)) => profile.display_name,
        _ => String::new(),
    };
    let offer = CapturedReminder {
        id: uuid::Uuid::new_v4().to_string(),
        about: job.text.clone(),
        when_said: NO_TIME_SAID.to_string(),
        session_id: thread_id(Some(member)),
        window_id: format!("intention-{}", job.id),
        subject,
        profile_id: Some(member.to_string()),
        said_at: now,
        captured_at: now,
        disposition: ReminderDisposition::Pending,
    };
    anyhow::ensure!(
        deps.reminders.capture(&offer).await? && deps.reminders.offer(&offer.id, now).await?,
        "the offer could not be stored"
    );
    let zone = deps.zone().await;
    let mut notice = offer_notice(&offer, offer_due(&offer, now, zone), now, zone);
    notice.body = match job.why.trim() {
        "" => PICK_A_TIME.to_string(),
        why if why.ends_with(['.', '!', '?']) => format!("{why} {PICK_A_TIME}"),
        why => format!("{why}. {PICK_A_TIME}"),
    };
    tell_member(&deps.engine, member, notice, None).await;
    Ok(Carried::said("Asked whether to add it to your to-dos."))
}
