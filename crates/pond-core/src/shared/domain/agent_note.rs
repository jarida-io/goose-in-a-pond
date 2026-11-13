//! What Goose writes down for itself: lessons about how to work, and follow-ups to come back
//! to. Never facts about people: those are the members' profiles (`member_profile`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lessons kept for one member, and for the household. Few on purpose: they ride every
/// conversation's first turn, and a list a small model wrote itself must not grow unread.
pub const LESSONS_KEPT: usize = 8;
/// Open follow-ups kept for one member, and for the household.
pub const NOTES_KEPT: usize = 8;
/// Longest note, in characters: one sentence.
pub const NOTE_CHARS: usize = 200;
/// A follow-up nobody came back to in this long is dropped.
pub const NOTE_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// How to work better, learnt from a mistake or a correction. Shown to every conversation.
    Lesson,
    /// Something to come back to. Shown to the heartbeat, which may act on it.
    Note,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lesson => "lesson",
            Self::Note => "note",
        }
    }

    /// The stored word, or a near word a model writes for it; anything else is a note.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "lesson" | "lessons" | "rule" | "correction" | "learned" | "learnt" => Self::Lesson,
            _ => Self::Note,
        }
    }

    /// How many of this kind one owner keeps; the oldest beyond it are dropped.
    pub fn kept(self) -> usize {
        match self {
            Self::Lesson => LESSONS_KEPT,
            Self::Note => NOTES_KEPT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNote {
    pub id: String,
    /// The member whose conversations it came from and returns to; `None` is the household's.
    pub profile_id: Option<String>,
    pub kind: NoteKind,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

/// Why a note was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteRefused {
    Empty,
}

/// A note as it is kept: one line, no longer than [`NOTE_CHARS`].
pub fn tidy(text: &str) -> Result<String, NoteRefused> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line: String = line
        .trim_start_matches(['-', '*', ' '])
        .chars()
        .take(NOTE_CHARS)
        .collect();
    let line = line.trim().to_string();
    if line.is_empty() {
        Err(NoteRefused::Empty)
    } else {
        Ok(line)
    }
}

/// Whether two notes say the same thing once case, spacing and the final stop are set aside.
pub fn same_note(a: &str, b: &str) -> bool {
    let plain = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(['.', '!'])
            .to_lowercase()
    };
    plain(a) == plain(b)
}

/// The notes that outlive a new one: of `existing` (one owner's, one kind, newest first), the
/// ids to drop so that with the new note the owner keeps at most `kind.kept()`.
pub fn dropped_for_room(kind: NoteKind, existing: &[AgentNote]) -> Vec<String> {
    existing
        .iter()
        .skip(kind.kept().saturating_sub(1))
        .map(|note| note.id.clone())
        .collect()
}

/// Whether a follow-up has gone unread too long to keep.
pub fn is_stale(note: &AgentNote, now: DateTime<Utc>) -> bool {
    note.kind == NoteKind::Note && now - note.created_at > chrono::Duration::days(NOTE_DAYS)
}

/// The lessons a conversation is shown, one a line, or nothing when there are none.
pub fn lessons_block(lessons: &[AgentNote]) -> String {
    if lessons.is_empty() {
        return String::new();
    }
    let mut out = String::from("<lessons>\n");
    for lesson in lessons.iter().take(LESSONS_KEPT) {
        out.push_str("- ");
        out.push_str(lesson.text.trim());
        out.push('\n');
    }
    out.push_str("</lessons>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, kind: NoteKind, text: &str, days_old: i64) -> AgentNote {
        AgentNote {
            id: id.to_string(),
            profile_id: None,
            kind,
            text: text.to_string(),
            created_at: Utc::now() - chrono::Duration::days(days_old),
        }
    }

    #[test]
    fn a_note_is_kept_as_one_line_of_bounded_length() {
        assert_eq!(
            tidy("  - Give prices\n in   shillings. "),
            Ok("Give prices in shillings.".to_string())
        );
        assert_eq!(tidy("  \n "), Err(NoteRefused::Empty));
        assert_eq!(tidy(&"a".repeat(500)).unwrap().chars().count(), NOTE_CHARS);
    }

    #[test]
    fn the_kind_is_read_from_the_word_and_anything_else_is_a_note() {
        assert_eq!(NoteKind::parse("Lesson"), NoteKind::Lesson);
        assert_eq!(NoteKind::parse("correction"), NoteKind::Lesson);
        assert_eq!(NoteKind::parse("note"), NoteKind::Note);
        assert_eq!(NoteKind::parse("follow-up"), NoteKind::Note);
        assert_eq!(NoteKind::parse(""), NoteKind::Note);
    }

    #[test]
    fn a_new_note_makes_room_by_dropping_the_oldest() {
        let existing: Vec<AgentNote> = (0..LESSONS_KEPT)
            .map(|i| note(&format!("l{i}"), NoteKind::Lesson, "x", i as i64))
            .collect();
        assert_eq!(
            dropped_for_room(NoteKind::Lesson, &existing),
            vec![format!("l{}", LESSONS_KEPT - 1)]
        );
        assert!(dropped_for_room(NoteKind::Lesson, &existing[..3]).is_empty());
    }

    #[test]
    fn only_a_follow_up_goes_stale() {
        let now = Utc::now();
        assert!(is_stale(
            &note("n", NoteKind::Note, "x", NOTE_DAYS + 1),
            now
        ));
        assert!(!is_stale(&note("n", NoteKind::Note, "x", 2), now));
        assert!(!is_stale(
            &note("l", NoteKind::Lesson, "x", NOTE_DAYS + 1),
            now
        ));
    }

    #[test]
    fn lessons_are_shown_one_a_line_or_not_at_all() {
        assert_eq!(lessons_block(&[]), "");
        assert_eq!(
            lessons_block(&[
                note("a", NoteKind::Lesson, "Give prices in shillings.", 0),
                note("b", NoteKind::Lesson, "Ask before booking anything.", 1),
            ]),
            "<lessons>\n- Give prices in shillings.\n- Ask before booking anything.\n</lessons>"
        );
    }
}
