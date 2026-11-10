//! Reminders: the dated half of an utterance, kept apart because a date in a memory goes false.

use chrono::{DateTime, Utc};

/// Something with a date in it, on its way to the proposal queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderCandidate {
    /// What it is about, with no date in it.
    pub about: String,
    /// The words the conversation used about the timing. Never parsed.
    pub when_said: String,
    pub session_id: String,
    /// The source window's last message id, the value the extraction cursor advances to.
    pub window_id: String,
    /// Who the window was about, by name; the proposal says whose reminder it is.
    pub subject: String,
    /// The owning household member, when the window's subject was one.
    pub profile_id: Option<String>,
    /// The window's last message time, not the wall clock, so a backfilled chat never looks fresh.
    pub said_at: DateTime<Utc>,
}

/// A stored reminder row; [`ReminderCandidate`] is what the window produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedReminder {
    pub id: String,
    /// What it is about, with no date in it.
    pub about: String,
    /// The words the conversation used about the timing. Never parsed.
    pub when_said: String,
    pub session_id: String,
    /// The stretch of that conversation it came out of.
    pub window_id: String,
    /// Whose reminder this is, by name.
    pub subject: String,
    /// Which household member owns it, when the window's subject was one.
    pub profile_id: Option<String>,
    /// When the conversation happened.
    pub said_at: DateTime<Utc>,
    /// When the pond lifted it out of that conversation.
    pub captured_at: DateTime<Utc>,
    pub disposition: ReminderDisposition,
}

impl CapturedReminder {
    pub fn from_candidate(
        candidate: ReminderCandidate,
        id: impl Into<String>,
        captured_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            about: candidate.about,
            when_said: candidate.when_said,
            session_id: candidate.session_id,
            window_id: candidate.window_id,
            subject: candidate.subject,
            profile_id: candidate.profile_id,
            said_at: candidate.said_at,
            captured_at,
            disposition: ReminderDisposition::Pending,
        }
    }

    /// The half of the storage key that is not the window.
    pub fn dedup_key(&self) -> String {
        reminder_dedup_key(&self.about)
    }
}

/// Whether anything has been done about a stored reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderDisposition {
    /// Held, and nothing has acted on it.
    Pending,
    /// Turned into a proposal. Terminal here: it stops a second proposal from the same row.
    Proposed,
    /// Offered to its member as a to-do; the phone's Add or No settles it.
    Offered,
    /// The member added it to their to-dos.
    Accepted,
    /// Somebody said no.
    Dismissed,
    /// Its member was deleted. Written only by the `BEFORE DELETE ON profiles` trigger; nothing
    /// expires a reminder for age, so an old one stays `pending`.
    Expired,
}

impl ReminderDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Proposed => "proposed",
            Self::Offered => "offered",
            Self::Accepted => "accepted",
            Self::Dismissed => "dismissed",
            Self::Expired => "expired",
        }
    }

    /// Parse a stored value; `None` for an unknown word, since `Pending` would re-show the row.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "proposed" => Some(Self::Proposed),
            "offered" => Some(Self::Offered),
            "accepted" => Some(Self::Accepted),
            "dismissed" => Some(Self::Dismissed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// Dedup key: `about` as lowercased alphanumeric words, single-spaced, never merging meanings.
/// Half of a UNIQUE constraint (migration 0057), so changing it splits old rows from new.
pub fn reminder_dedup_key(about: &str) -> String {
    about
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Max conversation age, in days, for a reminder; older windows are mined for memories only.
pub const REMINDER_MAX_AGE_DAYS: i64 = 7;

/// Whether a window is recent enough to be asked for reminders.
pub fn window_is_fresh_enough(last_message_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    (now - last_message_at).num_days() < REMINDER_MAX_AGE_DAYS
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn a_conversation_from_last_march_is_never_asked_for_a_reminder() {
        let now = Utc::now();
        assert!(window_is_fresh_enough(now - Duration::hours(6), now));
        assert!(window_is_fresh_enough(now - Duration::days(6), now));
        assert!(!window_is_fresh_enough(now - Duration::days(8), now));
        assert!(!window_is_fresh_enough(now - Duration::days(200), now));
    }

    #[test]
    fn the_dedup_key_absorbs_the_drift_a_re_walk_produces() {
        let key = reminder_dedup_key("The dentist");
        assert_eq!(key, "the dentist");
        assert_eq!(reminder_dedup_key("  the   Dentist. "), key);
        assert_eq!(reminder_dedup_key("The dentist!"), key);
    }

    #[test]
    fn two_different_reminders_keep_two_keys() {
        assert_ne!(
            reminder_dedup_key("the dentist"),
            reminder_dedup_key("the school run")
        );
    }

    #[test]
    fn the_timing_words_are_not_part_of_the_key() {
        let a = ReminderCandidate {
            about: "the dentist".into(),
            when_said: "Tuesday".into(),
            session_id: "s".into(),
            window_id: "w".into(),
            subject: "Jerry".into(),
            profile_id: None,
            said_at: Utc::now(),
        };
        let mut b = a.clone();
        b.when_said = "next Tuesday".into();

        let now = Utc::now();
        assert_eq!(
            CapturedReminder::from_candidate(a, "id-a", now).dedup_key(),
            CapturedReminder::from_candidate(b, "id-b", now).dedup_key()
        );
    }

    #[test]
    fn an_unreadable_disposition_is_not_read_as_pending() {
        assert_eq!(
            ReminderDisposition::parse("dismissed"),
            Some(ReminderDisposition::Dismissed)
        );
        assert_eq!(ReminderDisposition::parse("acted-upon"), None);
    }

    /// Clock skew: a negative age must count as fresh, not panic.
    #[test]
    fn a_window_from_the_future_is_still_fresh() {
        let now = Utc::now();
        assert!(window_is_fresh_enough(now + Duration::hours(2), now));
    }
}
