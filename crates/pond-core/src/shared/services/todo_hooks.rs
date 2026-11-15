//! One pass of the to-do hook engine: due times and repeats, due dates standing in for a time,
//! follow-ups after a not-yet, and queued events matched to place, event and connector hooks.
//! Each fires at most one push per to-do per pass, to the member's own devices only.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde_json::json;

use crate::mcp::domain::notice_delivery::{marked, QUIET_PASS_KEY, SPEAK_IF_UNFETCHED_KEY};
use crate::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification, OpenScreen};
use crate::shared::domain::todo::{
    question_for, rfc3339, EventKey, PushKind, Todo, MAX_ASKS, STALE_EVENT_MINUTES,
};
use crate::shared::ports::todo::{HookEventQueue, TodoRepository};
use crate::shared::services::goose_thread;
use crate::user_data::domain::message_note::{MessageNote, SourceKind};

/// Events read per pass; a backlog drains over the next passes.
pub const EVENT_BATCH: usize = 200;
/// Handled events are kept this long, for a look at what fired.
pub const HANDLED_EVENTS_KEPT_DAYS: i64 = 7;
/// A push's title is one line on a lock screen.
const TITLE_CHARS: usize = 120;
const BODY_CHARS: usize = 240;

/// What one pass did, for the runner's log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PassReport {
    pub pushed: usize,
    pub events: usize,
}

pub struct TodoHookEngine {
    todos: Arc<dyn TodoRepository>,
    events: Arc<dyn HookEventQueue>,
    notifier: Arc<dyn MemberNotifier>,
}

impl TodoHookEngine {
    pub fn new(
        todos: Arc<dyn TodoRepository>,
        events: Arc<dyn HookEventQueue>,
        notifier: Arc<dyn MemberNotifier>,
    ) -> Self {
        Self {
            todos,
            events,
            notifier,
        }
    }

    /// Everything due at `now`, with repeats and follow-ups counted in `tz`.
    pub async fn pass(&self, now: DateTime<Utc>, tz: Tz) -> Result<PassReport> {
        let mut report = PassReport::default();
        let mut pushed: HashSet<String> = HashSet::new();

        for todo in self.todos.due_clock_hooks(now).await? {
            let mut why = None;
            for hook in todo.hooks.iter().filter(|h| {
                h.spec.listens_for().is_none() && h.next_fire_at.is_some_and(|t| t <= now)
            }) {
                let next = hook.spec.next_after_firing(now, tz);
                self.todos.hook_fired(&hook.id, now, next).await?;
                why.get_or_insert(hook.label.clone());
            }
            if pushed.insert(todo.id.clone()) && self.push(&todo, why.as_deref(), now).await? {
                report.pushed += 1;
            }
        }

        for todo in self.todos.due_dates(now).await? {
            if todo.due_without_hooks(now)
                && pushed.insert(todo.id.clone())
                && self.push(&todo, None, now).await?
            {
                report.pushed += 1;
            }
        }

        for todo in self.todos.follow_ups_due(now).await? {
            if pushed.insert(todo.id.clone()) && self.push(&todo, None, now).await? {
                report.pushed += 1;
            }
        }

        let events = self.events.unhandled(EVENT_BATCH).await?;
        if events.is_empty() {
            return Ok(report);
        }
        let mut listening = self.todos.listening().await?;
        let stale = Duration::minutes(STALE_EVENT_MINUTES);
        for event in &events {
            if now - event.at > stale {
                continue;
            }
            for todo in listening.iter_mut() {
                let mut why = None;
                for hook in todo.hooks.iter_mut() {
                    if hook.spec.matches(event, &todo.owner) && hook.cooled_down(now) {
                        hook.last_fired_at = Some(now);
                        self.todos.hook_fired(&hook.id, now, None).await?;
                        why.get_or_insert(hook.label.clone());
                    }
                }
                if why.is_some()
                    && pushed.insert(todo.id.clone())
                    && self.push(todo, why.as_deref(), now).await?
                {
                    report.pushed += 1;
                }
            }
        }
        let ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
        self.events.mark_handled(&ids, now).await?;
        report.events = ids.len();
        Ok(report)
    }

    /// What the place, event and connector hooks of open to-dos wait for, so a producer can
    /// skip raising what nobody listens to.
    pub async fn listening_keys(&self) -> Result<HashSet<EventKey>> {
        Ok(self
            .todos
            .listening()
            .await?
            .iter()
            .flat_map(|t| t.hooks.iter().filter_map(|h| h.spec.listens_for()))
            .collect())
    }

    /// Deletes events handled more than [`HANDLED_EVENTS_KEPT_DAYS`] ago.
    pub async fn prune(&self, now: DateTime<Utc>) -> Result<u64> {
        self.events
            .prune_handled(now - Duration::days(HANDLED_EVENTS_KEPT_DAYS))
            .await
    }

    /// Pushes what the to-do calls for now; `false` when it calls for nothing.
    async fn push(&self, todo: &Todo, why: Option<&str>, now: DateTime<Utc>) -> Result<bool> {
        let Some(kind) = todo.push_kind() else {
            return Ok(false);
        };
        let notice = notice_for(todo, kind, why, now);
        if kind == PushKind::Reminder {
            let text = match notice.body.trim() {
                "" => notice.title.clone(),
                body => format!("{}\n{body}", notice.title),
            };
            goose_thread::post(
                Some(&todo.owner),
                &text,
                MessageNote::from_source(
                    SourceKind::Reminder,
                    todo.text.clone(),
                    Some(todo.id.clone()),
                ),
            )
            .await;
        }
        let delivery = self.notifier.notify_member(&todo.owner, notice).await;
        let outcome = match &delivery {
            MemberDelivery::Reached(devices) => format!("queued for {}", devices.len()),
            MemberDelivery::NoPhone => "no_phone".to_string(),
            MemberDelivery::Failed(why) => format!("failed: {why}"),
        };
        tracing::info!(
            target: "giap::trace",
            kind = "todo_push",
            push = ?kind,
            todo_id = %todo.id,
            outcome = %outcome,
        );
        // Recorded however it went, so an undeliverable push is not retried every pass.
        let asked = todo.asked + u32::from(kind == PushKind::CheckIn);
        self.todos.pushed(&todo.id, now, asked).await?;
        Ok(true)
    }
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

/// The push for a to-do: a reminder says what it is; a check-in asks in Goose's voice and is
/// answered Done or Not yet on the phone.
pub fn notice_for(
    todo: &Todo,
    kind: PushKind,
    why: Option<&str>,
    now: DateTime<Utc>,
) -> Notification {
    let open = OpenScreen::Todos {
        todo_id: Some(todo.id.clone()),
    };
    let (category, title, body, data) = match kind {
        PushKind::Reminder => (
            "info",
            clip(&format!("Reminder: {}", todo.text), TITLE_CHARS),
            todo.notes
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .or(why)
                .map(|b| clip(b, BODY_CHARS))
                .unwrap_or_default(),
            // A reminder comes due when the person set it to, so quiet hours do not hold it, and
            // it is said in the room if no phone picks it up.
            marked(
                marked(Some(open.data()), QUIET_PASS_KEY),
                SPEAK_IF_UNFETCHED_KEY,
            )
            .unwrap_or_else(|| open.data()),
        ),
        PushKind::CheckIn => {
            let last = todo.asked + 1 >= MAX_ASKS;
            let mut fields = serde_json::Map::new();
            fields.insert("action".into(), json!("todo_checkin"));
            fields.insert("todo_id".into(), json!(todo.id));
            fields.insert("text".into(), json!(todo.text));
            fields.insert("label".into(), json!("Done"));
            (
                "action_required",
                clip(&question_for(&todo.text), TITLE_CHARS),
                if last {
                    "Tap Done or Not yet. This is the last time I will ask.".to_string()
                } else {
                    "Tap Done, or Not yet and I will ask again later.".to_string()
                },
                open.merged_into(fields),
            )
        }
    };
    Notification {
        id: format!("todo-{}-{}", todo.id, now.timestamp_millis()),
        target: String::new(),
        category: category.to_string(),
        title,
        body,
        timestamp: rfc3339(now),
        data: Some(data),
    }
}

#[cfg(test)]
mod tests;
