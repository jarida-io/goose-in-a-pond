//! What Goose did for a member without being asked, kept so they can read it, be told of it and
//! take it back. Nothing here acts: an entry is written after the thing is done.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Characters of an entry's one line.
pub const SUMMARY_CHARS: usize = 180;
/// Characters kept of an errand's answer.
pub const DETAIL_CHARS: usize = 2400;
/// How long an entry is kept.
pub const ACTIVITY_KEPT: Duration = Duration::days(30);
/// Lines one notification carries; the rest are on the page it opens.
const NOTICE_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// A reminder put on the member's list.
    Reminder,
    /// A follow-up Goose wrote for itself.
    Note,
    /// A follow-up of its own it closed.
    NoteClosed,
    /// A web watch that kept failing, paused.
    WatchPaused,
    /// Something it asked the member to approve.
    Suggestion,
    /// Something it looked up and answers here.
    Errand,
    /// A paired phone that stopped connecting, removed.
    PhoneRemoved,
}

impl ActivityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reminder => "reminder",
            Self::Note => "note",
            Self::NoteClosed => "note_closed",
            Self::WatchPaused => "watch_paused",
            Self::Suggestion => "suggestion",
            Self::Errand => "errand",
            Self::PhoneRemoved => "phone_removed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "reminder" => Self::Reminder,
            "note" => Self::Note,
            "note_closed" => Self::NoteClosed,
            "watch_paused" => Self::WatchPaused,
            "suggestion" => Self::Suggestion,
            "errand" => Self::Errand,
            "phone_removed" => Self::PhoneRemoved,
            _ => return None,
        })
    }

    /// Whether the member is told at once. Goose's own notes wait to be read, and a suggestion
    /// has a notification of its own.
    pub fn tells(self) -> bool {
        matches!(self, Self::Reminder | Self::WatchPaused | Self::Errand)
    }

    /// Whether the member can take it back with a tap.
    pub fn can_undo(self) -> bool {
        matches!(self, Self::Reminder | Self::Note | Self::WatchPaused)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GooseActivity {
    pub id: String,
    pub profile_id: String,
    pub kind: ActivityKind,
    /// One line, as the member reads it.
    pub summary: String,
    /// An errand's answer.
    pub detail: Option<String>,
    /// The to-do, note, watch or proposal it made or changed.
    pub ref_id: Option<String>,
    pub at: DateTime<Utc>,
    pub undone_at: Option<DateTime<Utc>>,
}

fn clipped(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let cut: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

impl GooseActivity {
    pub fn new(
        member: &str,
        kind: ActivityKind,
        summary: &str,
        ref_id: Option<&str>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: member.to_string(),
            kind,
            summary: clipped(summary, SUMMARY_CHARS),
            detail: None,
            ref_id: ref_id.map(str::to_string),
            at,
            undone_at: None,
        }
    }

    /// A reminder set for `when`, already in the member's own words for a time.
    pub fn reminder(
        member: &str,
        text: &str,
        when: &str,
        todo_id: &str,
        at: DateTime<Utc>,
    ) -> Self {
        Self::new(
            member,
            ActivityKind::Reminder,
            &format!("Set a reminder for {when}: {text}"),
            Some(todo_id),
            at,
        )
    }

    pub fn note(member: &str, text: &str, note_id: &str, at: DateTime<Utc>) -> Self {
        Self::new(
            member,
            ActivityKind::Note,
            &format!("Wrote itself a note: {text}"),
            Some(note_id),
            at,
        )
    }

    pub fn note_closed(member: &str, text: &str, at: DateTime<Utc>) -> Self {
        Self::new(
            member,
            ActivityKind::NoteClosed,
            &format!("Closed its note: {text}"),
            None,
            at,
        )
    }

    pub fn watch_paused(
        member: &str,
        title: &str,
        failures: u32,
        watch_id: &str,
        at: DateTime<Utc>,
    ) -> Self {
        Self::new(
            member,
            ActivityKind::WatchPaused,
            &format!("Paused the watch on {title}: its last {failures} checks failed"),
            Some(watch_id),
            at,
        )
    }

    /// A phone removed for not connecting in `days` days. Pairing it again is the way back.
    pub fn phone_removed(member: &str, phone: &str, days: u32, at: DateTime<Utc>) -> Self {
        Self::new(
            member,
            ActivityKind::PhoneRemoved,
            &format!("Removed the phone {phone}: it had not connected for {days} days"),
            None,
            at,
        )
    }

    pub fn suggestion(member: &str, words: &str, proposal_id: &str, at: DateTime<Utc>) -> Self {
        Self::new(
            member,
            ActivityKind::Suggestion,
            &format!("Asked you: {words}"),
            Some(proposal_id),
            at,
        )
    }

    /// Something looked up, with what was found.
    pub fn errand(member: &str, asked: &str, answer: &str, at: DateTime<Utc>) -> Self {
        let mut item = Self::new(
            member,
            ActivityKind::Errand,
            &format!("Looked into: {asked}"),
            None,
            at,
        );
        let answer = answer.trim();
        item.detail = (!answer.is_empty()).then(|| {
            let kept: String = answer.chars().take(DETAIL_CHARS).collect();
            kept.trim().to_string()
        });
        item
    }

    pub fn can_undo(&self) -> bool {
        self.kind.can_undo() && self.undone_at.is_none() && self.ref_id.is_some()
    }
}

/// The one notification for what a run did: its title and body, or `None` when nothing it did
/// is worth a buzz.
pub fn notice(items: &[GooseActivity]) -> Option<(String, String)> {
    let told: Vec<&GooseActivity> = items.iter().filter(|item| item.kind.tells()).collect();
    let first = told.first()?;
    if let ([only], Some(answer)) = (told.as_slice(), first.detail.as_deref()) {
        return Some((only.summary.clone(), clipped(answer, SUMMARY_CHARS)));
    }
    let title = match told.len() {
        1 => "Goose did something for you".to_string(),
        n => format!("Goose did {n} things for you"),
    };
    let mut lines: Vec<String> = told
        .iter()
        .take(NOTICE_LINES)
        .map(|item| item.summary.clone())
        .collect();
    if told.len() > NOTICE_LINES {
        lines.push(format!("and {} more", told.len() - NOTICE_LINES));
    }
    Some((title, lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> DateTime<Utc> {
        "2026-11-12T19:00:00Z".parse().unwrap()
    }

    #[test]
    fn every_kind_round_trips_and_only_what_reaches_the_member_buzzes() {
        for kind in [
            ActivityKind::Reminder,
            ActivityKind::Note,
            ActivityKind::NoteClosed,
            ActivityKind::WatchPaused,
            ActivityKind::Suggestion,
            ActivityKind::Errand,
        ] {
            assert_eq!(ActivityKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ActivityKind::parse("device"), None);
        assert!(!ActivityKind::Note.tells());
        assert!(!ActivityKind::Suggestion.tells(), "it has its own notice");
        assert!(ActivityKind::Errand.tells());
        assert!(!ActivityKind::Errand.can_undo());
    }

    #[test]
    fn a_run_is_told_in_one_notification() {
        let note = GooseActivity::note("m", "Ask how the interview went", "n1", at());
        assert_eq!(notice(std::slice::from_ref(&note)), None);

        let reminder = GooseActivity::reminder("m", "Call the dentist", "Fri 08:00", "t1", at());
        assert_eq!(
            notice(&[note.clone(), reminder.clone()]),
            Some((
                "Goose did something for you".to_string(),
                "Set a reminder for Fri 08:00: Call the dentist".to_string()
            ))
        );
        let paused = GooseActivity::watch_paused("m", "AliExpress", 5, "w1", at());
        let (title, body) = notice(&[reminder, paused]).unwrap();
        assert_eq!(title, "Goose did 2 things for you");
        assert_eq!(body.lines().count(), 2);

        // An errand alone is told as its question and its answer.
        let errand = GooseActivity::errand("m", "when the pool opens", "It opens at 6am.", at());
        assert_eq!(
            notice(&[errand]),
            Some((
                "Looked into: when the pool opens".to_string(),
                "It opens at 6am.".to_string()
            ))
        );
    }

    #[test]
    fn an_entry_is_one_bounded_line_and_is_taken_back_once() {
        let long = "word ".repeat(100);
        let mut item = GooseActivity::note("m", &long, "n1", at());
        assert!(item.summary.chars().count() <= SUMMARY_CHARS);
        assert!(item.summary.ends_with('…'));
        assert!(item.can_undo());
        item.undone_at = Some(at());
        assert!(!item.can_undo());
        assert!(!GooseActivity::note_closed("m", "x", at()).can_undo());
        assert_eq!(GooseActivity::errand("m", "q", "  ", at()).detail, None);
    }
}
