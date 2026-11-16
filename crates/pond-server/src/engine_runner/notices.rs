//! What the engine tells a member: a notice on their own phones and, for something they will
//! want to find again, a line in their Goose thread.

use chrono::{DateTime, Utc};

use pond_core::mcp::domain::notice_delivery::{marked, SPEAK_IF_UNFETCHED_KEY};
use pond_core::mcp::ports::notification::{MemberDelivery, Notification, OpenScreen};
use pond_core::user_data::domain::anchor::{Anchor, Deliverable};
use pond_core::user_data::domain::message_note::{MessageNote, SourceKind};
use pond_core::user_data::domain::question::Question;
use pond_core::user_data::services::engine::settle::ready_notice;

use super::EngineDeps;

/// Tells one member on their own devices. With `thread`, the notice's words are also kept as
/// a line in their Goose thread under that note.
pub async fn tell_member(
    deps: &EngineDeps,
    profile_id: &str,
    notice: Notification,
    thread: Option<MessageNote>,
) -> MemberDelivery {
    if let (Some(posts), Some(note)) = (&deps.thread, thread) {
        let line = format!("{}. {}", notice.title, notice.body);
        if let Err(e) = posts.post(Some(profile_id), &line, note).await {
            tracing::warn!(error = %e, "the engine could not post to the Goose thread");
        }
    }
    let (id, category) = (notice.id.clone(), notice.category.clone());
    let delivery = deps.notifier.notify_member(profile_id, notice).await;
    tracing::info!(
        target: "giap::trace",
        kind = "engine_notice_sent",
        notice = %id,
        member = %profile_id,
        category = %category,
        reached = matches!(delivery, MemberDelivery::Reached(_)),
        "the engine told a member"
    );
    delivery
}

/// The notice that a deliverable is shown to its anchor's owner, opening it; said in the room
/// when no phone fetches it.
pub(super) async fn deliverable_ready(
    deps: &EngineDeps,
    anchor: &Anchor,
    deliverable: &Deliverable,
    now: DateTime<Utc>,
) {
    let (title, body) = ready_notice(anchor, deliverable);
    let open = OpenScreen::Deliverable {
        deliverable_id: deliverable.id.clone(),
    };
    let notice = Notification {
        id: format!("deliverable-{}-{}", deliverable.id, deliverable.version),
        target: String::new(),
        category: "info".to_string(),
        title,
        body,
        timestamp: now.to_rfc3339(),
        data: marked(Some(open.data()), SPEAK_IF_UNFETCHED_KEY),
    };
    let note = MessageNote::from_source(
        SourceKind::Deliverable,
        deliverable.title.clone(),
        Some(deliverable.id.clone()),
    );
    tell_member(deps, &anchor.profile_id, notice, Some(note)).await;
}

/// The one notice a stored batch of questions gets, opening the questions.
pub(super) async fn questions_asked(
    deps: &EngineDeps,
    anchor: &Anchor,
    batch: &[Question],
    now: DateTime<Utc>,
) {
    let Some(first) = batch.first() else {
        return;
    };
    let body = match batch.len() - 1 {
        0 => first.text.clone(),
        1 => format!("{} And one more.", first.text),
        more => format!("{} And {more} more.", first.text),
    };
    let mut fields = serde_json::Map::new();
    fields.insert("anchor_id".into(), anchor.id.as_str().into());
    fields.insert("batch_id".into(), first.batch_id.as_str().into());
    let notice = Notification {
        id: format!("questions-{}", first.batch_id),
        target: String::new(),
        category: "action_required".to_string(),
        title: format!("Questions about {}", anchor.title),
        body,
        timestamp: now.to_rfc3339(),
        data: Some(OpenScreen::Questions.merged_into(fields)),
    };
    tell_member(deps, &anchor.profile_id, notice, None).await;
}
