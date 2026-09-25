//! What a dated utterance becomes, once the date has been taken out of the
//! memory it would otherwise have poisoned.
//!
//! # Why this is not a memory, and not a calendar event either
//!
//! A memory is read back months later with the conversation gone. "The dentist
//! is on Tuesday" is then not merely useless, it is false — that Tuesday was in
//! March. So a dated thing is never stored as a memory: the date is stripped
//! out and what is left, if anything is left, is stored as the fact it always
//! was ("the user sees a dentist in Kisumu").
//!
//! The date half has to go somewhere, and it now does: a row in `reminders`
//! (migration 0057). There is **no calendar write** in this pond — its absence
//! is enforced by a test in `pond-adapters-caldav` that reads its own source and
//! fails the build if `PUT`, `POST`, `DELETE` or `MKCALENDAR` appears — and
//! there are no sticky notes: `StickyNote.tsx` is one string in `localStorage`
//! imported by nothing.
//!
//! The eventual destination is a proposal the household approves or declines,
//! and the table is what makes that reachable rather than what replaces it.
//! `ProposalAudience::from_scope` refuses Household and Guest (PAI-7 invariant
//! 4) and `profile_id` is NULL on every live memory row, so a reminder on a real
//! pond today has no audience to be addressed to. Storing first and proposing
//! later is the only ordering under which the date survives that: the row lands
//! unconditionally, and [`ReminderDisposition::Proposed`] is where the phase
//! that builds the queue path records having made a proposal out of it.
//!
//! # `when_said` is words, never a date
//!
//! Deliberately. Nothing in this pipeline knows which Tuesday was meant, and a
//! wrong date in a reminder is worse than no reminder at all: it fires on the
//! wrong day, about something already done, and teaches the household that the
//! pond's reminders cannot be trusted. The subject's own words go through
//! unparsed, and whoever approves the proposal reads them and decides.

use chrono::{DateTime, Utc};

/// Something with a date in it, on its way to the proposal queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderCandidate {
    /// What it is about, with no date in it.
    pub about: String,
    /// The words the conversation used about the timing. Never parsed.
    pub when_said: String,
    /// The conversation it came out of.
    pub session_id: String,
    /// The window it came out of: the last message id the walk read. The same
    /// value the extraction cursor advances to, so a candidate can always be
    /// traced back to the exact stretch of conversation that produced it.
    pub window_id: String,
    /// Who the window was about, by name. The proposal says whose reminder this
    /// is, and on a multi-member pond that is the difference between a useful
    /// suggestion and one shown to the wrong person.
    pub subject: String,
    /// Which household member owns it, when the window's subject was one.
    /// `None` on a pond with no profile rows, which is every pond today.
    pub profile_id: Option<String>,
    /// When the conversation that produced it happened — the window's last
    /// message, not the wall clock.
    ///
    /// The staleness question is asked about the CONVERSATION, not about the
    /// moment the backlog happened to reach it. A first run over a year of
    /// history reads chats from last March at three in the morning tonight; a
    /// candidate stamped `now` would look fresh and propose a dentist
    /// appointment nine months late.
    pub said_at: DateTime<Utc>,
}

/// What a captured reminder becomes once it is stored.
///
/// [`ReminderCandidate`] is what the window produced; this is the row. The split
/// exists because the two carry different truths: a candidate is a thing the
/// model said, and a `CapturedReminder` is a thing the pond is holding, with an
/// id somebody can name, a stamp saying when it was taken, and a disposition
/// saying whether anything has been done about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedReminder {
    /// The pond's own id for the row.
    pub id: String,
    /// What it is about, with no date in it.
    pub about: String,
    /// The words the conversation used about the timing. Never parsed.
    pub when_said: String,
    /// The conversation it came out of.
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
    /// Whether anything has been done about it.
    pub disposition: ReminderDisposition,
}

impl CapturedReminder {
    /// Turn a candidate into the row it becomes.
    ///
    /// The id and the capture stamp are passed in rather than minted here so a
    /// test can name the row it wrote and assert on the time it was written.
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
    /// A proposal was made out of it. Terminal for THIS table: the proposal
    /// carries the decision from here on, and this value is what stops a second
    /// proposal being made out of the same row.
    Proposed,
    /// Somebody said no.
    Dismissed,
    /// The member it belonged to was deleted.
    ///
    /// The ONLY thing that writes this is the `BEFORE DELETE ON profiles`
    /// trigger in migration 0057. Nothing expires a reminder for being old:
    /// there is no clock anywhere in this engine, so a reminder about last
    /// Tuesday stays `pending` and keeps being listed. This doc said "time
    /// passed" before, and no such path has ever existed.
    Expired,
}

impl ReminderDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Proposed => "proposed",
            Self::Dismissed => "dismissed",
            Self::Expired => "expired",
        }
    }

    /// Read a stored value back.
    ///
    /// `None` rather than a default for an unknown word. A row whose disposition
    /// cannot be read is a row nobody can say the state of, and answering
    /// "pending" would put it back in front of the household — which is the one
    /// direction a failure here must not go.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "proposed" => Some(Self::Proposed),
            "dismissed" => Some(Self::Dismissed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// The key that decides whether two captured reminders are the same one.
///
/// Lowercased, stripped to its alphanumeric words, single-spaced. It absorbs the
/// only drift a re-walk actually produces — capitalisation, a trailing full
/// stop, a doubled space — without pretending to judge meaning. Two genuinely
/// different sentences stay different, which is the direction that matters: a
/// key that collapsed too much would silently discard the second of two
/// reminders one window filed, and nothing downstream would ever know.
///
/// It is half of a UNIQUE constraint in migration 0057. Changing what this
/// function returns changes what the database considers a duplicate, and rows
/// written under the old spelling will not collide with rows written under the
/// new one.
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

/// How old a conversation may be and still produce a reminder.
///
/// Seven days. A backfill walks the whole history, and almost every window in
/// it is months old: a reminder from one of those is about something that has
/// already happened or already been missed, and the design deliberately cannot
/// parse the date to tell which. Older windows are still mined for memories —
/// a habit does not go stale — and are simply never asked for reminders at all,
/// which also drops the reminders paragraph from their prompt.
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

    /// The direction that matters. Two reminders one window filed about
    /// different things must not collapse into one row.
    #[test]
    fn two_different_reminders_keep_two_keys() {
        assert_ne!(
            reminder_dedup_key("the dentist"),
            reminder_dedup_key("the school run")
        );
    }

    /// The timing words are not part of the key, so one appointment described
    /// twice is one row.
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

    /// An unreadable disposition is not quietly read as pending.
    #[test]
    fn an_unreadable_disposition_is_not_read_as_pending() {
        assert_eq!(
            ReminderDisposition::parse("dismissed"),
            Some(ReminderDisposition::Dismissed)
        );
        assert_eq!(ReminderDisposition::parse("acted-upon"), None);
    }

    /// A clock that has gone backwards must not make an old window look fresh
    /// in the other direction, and must not crash.
    #[test]
    fn a_window_from_the_future_is_still_fresh() {
        let now = Utc::now();
        assert!(window_is_fresh_enough(now + Duration::hours(2), now));
    }
}
