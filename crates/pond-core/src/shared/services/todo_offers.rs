//! Dated mentions lifted from chats ("call the garage on Friday"), offered to their member as
//! to-dos: one push each, answered Add or No on the phone, under the same daily cap as the
//! reviewer's proposals. A mention with no member to address stays pending.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::mcp::ports::notification::{Notification, OpenScreen};
use crate::shared::domain::todo::{
    clock_label, rfc3339, Hook, HookSpec, Todo, TodoKind, TodoSource, TodoState,
};
use crate::shared::domain::when_said::due_from_words;
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::reminder::CapturedReminder;
use crate::user_data::ports::proposal::ProposalRepository;
use crate::user_data::ports::reminder_repository::ReminderRepository;
use crate::user_data::services::proactive_review::MAX_PROPOSALS_PER_DAY;

/// The longest to-do text an offer quotes in its question.
const QUOTED_CHARS: usize = 80;

/// What one offering run came to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OfferReport {
    /// Pending reminders looked at.
    pub considered: usize,
    /// Reminders now offered, for the caller to push to each member.
    pub offered: Vec<CapturedReminder>,
    /// Reminders with no member to address, left pending; expected, not a fault.
    pub unaddressable: usize,
    /// Reminders held back by the daily cap, left pending for a later day.
    pub capped: usize,
    /// Store errors; the reminder stays pending and is tried again.
    pub failed: usize,
}

/// Offer what can be offered; the rest stays pending.
pub async fn offer_pending_reminders(
    reminders: &dyn ReminderRepository,
    proposals: &dyn ProposalRepository,
    limit: usize,
    now: DateTime<Utc>,
) -> Result<OfferReport> {
    let pending = reminders
        .list_pending(&ProfileScope::Household, limit)
        .await?;
    let mut report = OfferReport {
        considered: pending.len(),
        ..Default::default()
    };
    // Member -> what Goose put in front of them in the last day, this run's offers included.
    let mut made_today: BTreeMap<String, usize> = BTreeMap::new();
    for reminder in pending {
        let Some(member) = reminder.profile_id.clone() else {
            report.unaddressable += 1;
            continue;
        };
        let made = match made_today.get(&member) {
            Some(made) => *made,
            None => match proposals
                .count_made_since(&member, now - Duration::days(1))
                .await
            {
                Ok(made) => made,
                // Fails closed: an unreadable count must not flood the member.
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(reminder_id = %reminder.id, error = %e, "[reminders] could not read the member's daily count, so the mention stays pending");
                    continue;
                }
            },
        };
        if made >= MAX_PROPOSALS_PER_DAY {
            report.capped += 1;
            made_today.insert(member, made);
            continue;
        }
        match reminders.offer(&reminder.id, now).await {
            Ok(true) => {
                made_today.insert(member, made + 1);
                report.offered.push(reminder);
            }
            Ok(false) => {}
            Err(e) => {
                report.failed += 1;
                tracing::warn!(reminder_id = %reminder.id, error = %e, "[reminders] the mention could not be marked offered, so it stays pending");
            }
        }
    }
    Ok(report)
}

/// The due time an offer proposes: the mention's words read from when they were said, in the
/// member's timezone; `None` when the words name no one time or that time has passed.
pub fn offer_due(reminder: &CapturedReminder, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    due_from_words(&reminder.when_said, reminder.said_at, tz).filter(|due| *due > now)
}

/// One offer as `GET /me/todos/offers` lists it.
pub fn offer_view(reminder: &CapturedReminder, due: Option<DateTime<Utc>>) -> Value {
    json!({
        "reminder_id": reminder.id,
        "text": reminder.about,
        "when_said": reminder.when_said,
        "due_at": due.map(rfc3339),
        "said_at": rfc3339(reminder.said_at),
    })
}

fn quoted(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= QUOTED_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(QUOTED_CHARS - 3).collect();
    format!("{}...", cut.trim_end())
}

/// "today", "tonight", "tomorrow", "Friday", or "Tuesday 24 November" further off.
fn day_phrase(due: DateTime<Utc>, now: DateTime<Utc>, tz: Tz) -> String {
    let due = due.with_timezone(&tz);
    let days = (due.date_naive() - now.with_timezone(&tz).date_naive()).num_days();
    match days {
        0 if due.hour() >= 19 => "tonight".to_string(),
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        2..=6 => due.format("%A").to_string(),
        _ => format!("{} {} {}", due.format("%A"), due.day(), due.format("%B")),
    }
}

/// The push for an offer, in Goose's voice: "Add 'call the garage' to your to-dos for Friday?".
pub fn offer_notice(
    reminder: &CapturedReminder,
    due: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    tz: Tz,
) -> Notification {
    let text = quoted(&reminder.about);
    let when_said = quoted(&reminder.when_said);
    let (title, body) = match due {
        Some(due) => (
            format!(
                "Add '{text}' to your to-dos for {}?",
                day_phrase(due, now, tz)
            ),
            format!(
                "You said \"{when_said}\". Add it and I will remind you at {}.",
                due.with_timezone(&tz).format("%H:%M")
            ),
        ),
        None => (
            format!("Add '{text}' to your to-dos?"),
            format!("You said \"{when_said}\". Add it and pick when I should remind you."),
        ),
    };
    let mut fields = serde_json::Map::new();
    fields.insert("action".into(), json!("todo_offer"));
    fields.insert("reminder_id".into(), json!(reminder.id));
    fields.insert("text".into(), json!(reminder.about));
    fields.insert("when_said".into(), json!(reminder.when_said));
    fields.insert("due_at".into(), json!(due.map(rfc3339)));
    fields.insert("label".into(), json!("Add"));
    Notification {
        // One per mention, so a second send replaces the first rather than stacking.
        id: format!("todo-offer-{}", reminder.id),
        target: String::new(),
        category: "action_required".to_string(),
        title,
        body,
        timestamp: rfc3339(now),
        data: Some(OpenScreen::Todos { todo_id: None }.merged_into(fields)),
    }
}

/// The to-do an accepted offer becomes: a reminder of the mention, with a time hook at `due`.
pub fn todo_from_offer(
    reminder: &CapturedReminder,
    owner: &str,
    due: DateTime<Utc>,
    now: DateTime<Utc>,
    tz: Tz,
) -> Todo {
    let spec = HookSpec::Time { at: due };
    let label = clock_label(&spec, now, tz);
    Todo {
        id: uuid::Uuid::new_v4().to_string(),
        owner: owner.to_string(),
        kind: TodoKind::Reminder,
        text: reminder.about.trim().to_string(),
        notes: None,
        state: TodoState::Open,
        due_at: Some(due),
        ask: false,
        source: TodoSource::Chat,
        source_ref: Some(reminder.id.clone()),
        last_asked_at: None,
        asked: 0,
        snoozed_until: None,
        created_at: now,
        updated_at: now,
        done_at: None,
        hooks: vec![Hook::new(spec, label, now, tz)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::proposal::{Proposal, ProposalDecision};
    use crate::user_data::domain::reminder::ReminderDisposition;
    use crate::user_data::mocks::mock_reminder::MockReminderRepository;
    use std::sync::Mutex;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Counts only: what the reviewer already put in front of each member today.
    #[derive(Default)]
    struct MadeToday {
        made: Mutex<BTreeMap<String, usize>>,
        saved: Mutex<usize>,
    }

    #[async_trait::async_trait]
    impl ProposalRepository for MadeToday {
        async fn save(&self, _proposal: &Proposal) -> Result<()> {
            *self.saved.lock().unwrap() += 1;
            Ok(())
        }
        async fn list_live_for(&self, _p: &str, _now: DateTime<Utc>) -> Result<Vec<Proposal>> {
            Ok(vec![])
        }
        async fn get_live(&self, _id: &str, _now: DateTime<Utc>) -> Result<Option<Proposal>> {
            Ok(None)
        }
        async fn expire_due(&self, _now: DateTime<Utc>) -> Result<u64> {
            Ok(0)
        }
        async fn count_made_since(&self, profile_id: &str, _since: DateTime<Utc>) -> Result<usize> {
            Ok(*self.made.lock().unwrap().get(profile_id).unwrap_or(&0))
        }
        async fn decisions_since(
            &self,
            _profile_id: &str,
            _since: DateTime<Utc>,
        ) -> Result<Vec<ProposalDecision>> {
            Ok(vec![])
        }
    }

    fn mention(id: &str, about: &str, when_said: &str, owner: Option<&str>) -> CapturedReminder {
        CapturedReminder {
            id: id.into(),
            about: about.into(),
            when_said: when_said.into(),
            session_id: "s1".into(),
            window_id: format!("w-{id}"),
            subject: "Jerry".into(),
            profile_id: owner.map(str::to_string),
            said_at: utc("2026-11-09T07:00:00Z"),
            captured_at: utc("2026-11-09T07:05:00Z"),
            disposition: ReminderDisposition::Pending,
        }
    }

    #[tokio::test]
    async fn a_members_mentions_are_offered_up_to_the_shared_daily_cap_and_never_proposed() {
        let reminders = MockReminderRepository::new();
        for i in 0..4 {
            reminders
                .capture(&mention(
                    &format!("r{i}"),
                    "call the garage",
                    "Friday",
                    Some("jerry"),
                ))
                .await
                .unwrap();
        }
        reminders
            .capture(&mention("anon", "the dentist", "Tuesday", None))
            .await
            .unwrap();
        let proposals = MadeToday::default();
        proposals
            .made
            .lock()
            .unwrap()
            .insert("jerry".into(), MAX_PROPOSALS_PER_DAY - 2);

        let report =
            offer_pending_reminders(&reminders, &proposals, 50, utc("2026-11-09T08:00:00Z"))
                .await
                .unwrap();
        assert_eq!(report.considered, 5);
        assert_eq!(report.offered.len(), 2, "two left in today's budget");
        assert_eq!((report.capped, report.unaddressable), (2, 1));
        assert_eq!(
            *proposals.saved.lock().unwrap(),
            0,
            "no proposal beside the offer"
        );

        let offered: Vec<String> = report.offered.iter().map(|r| r.id.clone()).collect();
        assert_eq!(offered.len(), 2);
        assert_eq!(
            reminders.open_to("jerry").await.unwrap().len(),
            4,
            "the two the cap held back are still Jerry's to see and answer"
        );
        // The next day's budget: only the two still pending are offered.
        proposals.made.lock().unwrap().insert("jerry".into(), 0);
        let again =
            offer_pending_reminders(&reminders, &proposals, 50, utc("2026-11-10T08:00:00Z"))
                .await
                .unwrap();
        assert_eq!(again.offered.len(), 2);
        assert!(
            again.offered.iter().all(|r| !offered.contains(&r.id)),
            "an offered mention is not offered twice"
        );
    }

    #[tokio::test]
    async fn an_offer_is_settled_once_and_only_by_its_member() {
        let reminders = MockReminderRepository::new();
        reminders
            .capture(&mention("r1", "call the garage", "Friday", Some("jerry")))
            .await
            .unwrap();
        let now = utc("2026-11-09T08:00:00Z");
        assert!(reminders.offer("r1", now).await.unwrap());
        assert!(!reminders
            .settle_offer("emmanuel", "r1", true, now)
            .await
            .unwrap());
        assert!(reminders
            .settle_offer("jerry", "r1", false, now)
            .await
            .unwrap());
        assert!(!reminders
            .settle_offer("jerry", "r1", true, now)
            .await
            .unwrap());
        assert_eq!(
            reminders.rows()[0].disposition,
            ReminderDisposition::Dismissed
        );
    }

    #[test]
    fn the_offer_asks_in_gooses_voice_with_the_day_it_worked_out() {
        let now = utc("2026-11-09T08:00:00Z");
        let r = mention("r1", "call the garage", "on Friday", Some("jerry"));
        let due = offer_due(&r, now, NAIROBI);
        assert_eq!(due, Some(utc("2026-11-13T06:00:00Z")));
        let notice = offer_notice(&r, due, now, NAIROBI);
        assert_eq!(
            notice.title,
            "Add 'call the garage' to your to-dos for Friday?"
        );
        assert_eq!(
            notice.body,
            "You said \"on Friday\". Add it and I will remind you at 09:00."
        );
        assert_eq!(notice.category, "action_required");
        assert_eq!(
            notice.data,
            Some(json!({
                "action": "todo_offer",
                "reminder_id": "r1",
                "text": "call the garage",
                "when_said": "on Friday",
                "due_at": "2026-11-13T06:00:00Z",
                "label": "Add",
                "open": "todos",
            }))
        );

        let vague = mention(
            "r2",
            "service the Fielder",
            "after the rains",
            Some("jerry"),
        );
        assert_eq!(offer_due(&vague, now, NAIROBI), None);
        let notice = offer_notice(&vague, None, now, NAIROBI);
        assert_eq!(notice.title, "Add 'service the Fielder' to your to-dos?");
        assert!(notice.body.contains("pick when"));
        assert_eq!(notice.data.unwrap()["due_at"], Value::Null);
    }

    #[test]
    fn a_mention_whose_day_has_passed_proposes_no_time() {
        let r = mention("r1", "call the garage", "tomorrow", Some("jerry"));
        assert_eq!(offer_due(&r, utc("2026-11-12T08:00:00Z"), NAIROBI), None);
    }

    #[test]
    fn an_accepted_offer_is_a_reminder_with_a_time_hook() {
        let now = utc("2026-11-09T08:00:00Z");
        let due = utc("2026-11-13T06:00:00Z");
        let r = mention("r1", "call the garage", "on Friday", Some("jerry"));
        let todo = todo_from_offer(&r, "jerry", due, now, NAIROBI);
        assert_eq!(todo.kind, TodoKind::Reminder);
        assert_eq!(todo.source, TodoSource::Chat);
        assert_eq!(todo.source_ref.as_deref(), Some("r1"));
        assert_eq!(todo.due_at, Some(due));
        assert_eq!(todo.hooks.len(), 1);
        assert_eq!(todo.hooks[0].label, "On Friday 13 November at 09:00");
        assert_eq!(todo.hooks[0].next_fire_at, Some(due));
    }
}
