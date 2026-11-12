//! Notes from a member's phone screenshots, for their ideas. Off unless the member turns it on.
//!
//! The Pond looks at a screenshot once and keeps one line about what the member seems
//! interested in. The picture waits in memory until it is read and is never written to disk;
//! a screen that shows money, logins, a private conversation or a document is skipped whole.

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::models::domain::message::ImageAttachment;
use crate::user_data::services::model_json::{object_in, text_of};

/// The member preference that turns it on; absent or anything but `"true"` is off.
pub const PREFERENCE: &str = "ideas_from_screenshots";
/// Screenshots one member may have waiting to be read.
pub const MAX_PENDING: usize = 6;
/// Largest picture taken, as base64 characters (about 1.5 MB of picture).
pub const MAX_IMAGE_CHARS: usize = 2_000_000;
/// Picture types the model reads.
pub const MIME_TYPES: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];
/// Longest note kept, in characters.
pub const NOTE_CHARS: usize = 140;
/// Notes kept for one member; the oldest go first.
pub const NOTES_KEPT: usize = 40;
/// How long a note may seed an idea.
pub const SEED_FOR: Duration = Duration::days(14);
/// Times a screenshot is tried before it is let go unread.
const MAX_ATTEMPTS: u8 = 2;

pub fn is_on(preferences: &HashMap<String, String>) -> bool {
    preferences.get(PREFERENCE).is_some_and(|v| v == "true")
}

/// One line about a screenshot the member took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotNote {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub note: String,
    /// When the phone says the screenshot was taken.
    pub taken_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// When an idea was built on it.
    pub used_at: Option<DateTime<Utc>>,
}

/// A screenshot waiting to be read. It lives only here.
pub struct Shot {
    pub member: String,
    pub image: ImageAttachment,
    pub taken_at: Option<DateTime<Utc>>,
    attempts: u8,
}

impl Shot {
    pub fn new(member: String, image: ImageAttachment, taken_at: Option<DateTime<Utc>>) -> Self {
        Self {
            member,
            image,
            taken_at,
            attempts: 0,
        }
    }
}

/// Why a screenshot was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotTaken {
    /// The member already has [`MAX_PENDING`] waiting.
    QueueFull,
    TooLarge,
    /// Not a picture type the model reads.
    WrongType,
}

impl NotTaken {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueueFull => "enough screenshots are waiting to be read; send more later",
            Self::TooLarge => "the picture is too large; send it smaller",
            Self::WrongType => "the picture must be a JPEG, a PNG or a WebP",
        }
    }
}

/// The screenshots waiting to be read, in memory only: a restart forgets them.
#[derive(Default)]
pub struct PendingShots {
    shots: Mutex<VecDeque<Shot>>,
}

impl PendingShots {
    /// Queue a screenshot; returns how many the member now has waiting.
    pub fn push(&self, shot: Shot) -> Result<usize, NotTaken> {
        if !MIME_TYPES.contains(&shot.image.mime_type.as_str()) {
            return Err(NotTaken::WrongType);
        }
        if shot.image.data.len() > MAX_IMAGE_CHARS {
            return Err(NotTaken::TooLarge);
        }
        let mut shots = self.shots.lock().unwrap_or_else(|e| e.into_inner());
        let waiting = shots.iter().filter(|s| s.member == shot.member).count();
        if waiting >= MAX_PENDING {
            return Err(NotTaken::QueueFull);
        }
        shots.push_back(shot);
        Ok(waiting + 1)
    }

    /// The next screenshot to read, oldest first.
    pub fn next(&self) -> Option<Shot> {
        self.shots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Give back a screenshot the model never got to; `false` once it has been tried enough
    /// and is let go.
    pub fn put_back(&self, mut shot: Shot) -> bool {
        shot.attempts += 1;
        if shot.attempts >= MAX_ATTEMPTS {
            return false;
        }
        self.shots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_front(shot);
        true
    }

    /// Forget every screenshot a member has waiting; returns how many.
    pub fn drop_for(&self, member: &str) -> usize {
        let mut shots = self.shots.lock().unwrap_or_else(|e| e.into_inner());
        let before = shots.len();
        shots.retain(|s| s.member != member);
        before - shots.len()
    }

    pub fn count_for(&self, member: &str) -> usize {
        self.shots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|s| s.member == member)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.shots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

static PENDING: LazyLock<PendingShots> = LazyLock::new(PendingShots::default);

/// The process's one queue: the route fills it and the lane job empties it.
pub fn pending() -> &'static PendingShots {
    &PENDING
}

const SYSTEM: &str = "\
You are shown one screenshot from {name}'s phone. Say in one line what it \
shows {name} is interested in, so the Pond can suggest ideas later. The \
picture is not kept; only your line is.

Reply with JSON only, one of these two shapes:
{\"note\":\"...\"}
{\"skip\":\"...\"}

- note: one plain line of at most 20 words about the subject on the screen, \
by its name: the place, product, dish, event, show, article or topic. Start \
with \"Looking at\", \"Reading about\" or \"Saved\". Never copy a message, a \
private person's name, a number, an address, an email or a code.
- skip: use it instead, with one word for why, when the screenshot shows any \
of these: banking, payments or balances; passwords, codes or logins; a \
private conversation or an email; an identity document; medical or legal \
records; someone's personal details; or nothing a person could act on, such \
as a settings page, an error or a blank screen.
When unsure, skip.";

pub fn render_system(name: &str) -> String {
    SYSTEM.replace("{name}", name)
}

/// What goes beside the picture in the model call.
pub const USER_MESSAGE: &str = "The screenshot is attached.\n\nJSON:";

/// What the model made of a screenshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Note(String),
    /// Skipped, with why: the model's word, or `private` when the guard here refused its line.
    Skipped(String),
    /// No JSON, or neither field.
    Unusable,
}

/// Words a note about something to do never needs and a private screen often has.
const PRIVATE_WORDS: [&str; 24] = [
    "password",
    "passcode",
    "one-time",
    "otp",
    "verification code",
    "cvv",
    "balance",
    "account number",
    "iban",
    "bank statement",
    "salary",
    "payslip",
    "log in",
    "login",
    "sign-in",
    "message from",
    "conversation with",
    "chat with",
    "texted",
    "diagnos",
    "prescription",
    "passport",
    "id card",
    "licence number",
];

/// Whether a line carries something the Pond must not keep, whatever the model judged: an
/// address for mail, a long number, or the vocabulary of a private screen.
pub fn keeps_private(note: &str) -> bool {
    let lower = note.to_lowercase();
    if lower.contains('@') || PRIVATE_WORDS.iter().any(|word| lower.contains(word)) {
        return true;
    }
    let mut run = 0;
    for c in lower.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= 5 {
                return true;
            }
        } else if !matches!(c, ' ' | '-' | '.' | ',') {
            run = 0;
        }
    }
    false
}

/// One line, at most [`NOTE_CHARS`], or `None` when too little is left to be a note.
fn tidy(note: &str) -> Option<String> {
    let line = note
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let line = if line.chars().count() > NOTE_CHARS {
        let cut: String = line.chars().take(NOTE_CHARS).collect();
        let end = cut.rfind(' ').unwrap_or(cut.len());
        cut[..end].to_string()
    } else {
        line
    };
    let line = line.trim_end_matches([',', ';', ':']).trim().to_string();
    (line.split_whitespace().count() >= 3).then_some(line)
}

pub fn parse_reply(raw: &str) -> Seen {
    let Some(map) = object_in(raw) else {
        return Seen::Unusable;
    };
    if let Some(why) = text_of(&map, "skip") {
        let why: String = why.chars().take(40).collect();
        return Seen::Skipped(why.to_lowercase());
    }
    match text_of(&map, "note").as_deref().and_then(tidy) {
        Some(note) if keeps_private(&note) => Seen::Skipped("private".to_string()),
        Some(note) => Seen::Note(note),
        None => Seen::Unusable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(chars: usize, mime: &str) -> ImageAttachment {
        ImageAttachment {
            data: "A".repeat(chars),
            mime_type: mime.to_string(),
        }
    }

    fn shot(member: &str) -> Shot {
        Shot::new(member.to_string(), picture(8, "image/jpeg"), None)
    }

    #[test]
    fn it_is_off_until_the_member_turns_it_on() {
        let mut prefs = HashMap::new();
        assert!(!is_on(&prefs));
        prefs.insert(PREFERENCE.to_string(), "false".to_string());
        assert!(!is_on(&prefs));
        prefs.insert(PREFERENCE.to_string(), "true".to_string());
        assert!(is_on(&prefs));
    }

    #[test]
    fn a_member_has_a_bounded_queue_of_their_own() {
        let queue = PendingShots::default();
        for n in 1..=MAX_PENDING {
            assert_eq!(queue.push(shot("jerry")), Ok(n));
        }
        assert_eq!(queue.push(shot("jerry")).err(), Some(NotTaken::QueueFull));
        assert_eq!(
            queue.push(shot("amina")),
            Ok(1),
            "another member's is their own"
        );
        assert_eq!(queue.count_for("jerry"), MAX_PENDING);

        assert_eq!(queue.drop_for("jerry"), MAX_PENDING);
        assert_eq!(queue.count_for("jerry"), 0);
        assert_eq!(queue.next().map(|s| s.member).as_deref(), Some("amina"));
        assert!(queue.is_empty());
    }

    #[test]
    fn only_pictures_the_model_reads_and_of_a_sane_size_are_taken() {
        let queue = PendingShots::default();
        let gif = Shot::new("jerry".into(), picture(8, "image/gif"), None);
        assert_eq!(queue.push(gif).err(), Some(NotTaken::WrongType));
        let huge = Shot::new(
            "jerry".into(),
            picture(MAX_IMAGE_CHARS + 1, "image/png"),
            None,
        );
        assert_eq!(queue.push(huge).err(), Some(NotTaken::TooLarge));
        assert!(queue.is_empty());
    }

    #[test]
    fn a_screenshot_the_model_never_got_to_is_tried_once_more_then_let_go() {
        let queue = PendingShots::default();
        queue.push(shot("jerry")).unwrap();
        let first = queue.next().unwrap();
        assert!(queue.put_back(first), "tried again");
        let second = queue.next().unwrap();
        assert!(!queue.put_back(second), "then let go");
        assert!(queue.is_empty());
    }

    #[test]
    fn a_reply_is_a_note_a_skip_or_nothing() {
        assert_eq!(
            parse_reply(r#"{"note":"Looking at flights from Nairobi to Zanzibar in December"}"#),
            Seen::Note("Looking at flights from Nairobi to Zanzibar in December".into())
        );
        assert_eq!(
            parse_reply(r#"{"skip":"Banking"}"#),
            Seen::Skipped("banking".into())
        );
        // A skip wins over a note sent beside it.
        assert_eq!(
            parse_reply(r#"{"note":"Looking at a bank app","skip":"banking"}"#),
            Seen::Skipped("banking".into())
        );
        assert_eq!(parse_reply("I cannot see the image."), Seen::Unusable);
        assert_eq!(parse_reply(r#"{"note":"Shoes"}"#), Seen::Unusable);
    }

    #[test]
    fn a_line_that_carries_something_private_is_refused_whatever_the_model_judged() {
        for private in [
            "Looking at a transfer of 45000 shillings to an account",
            "Reading about a message from Amina about dinner",
            "Saved a login page for the bank",
            "Looking at an email to jerry@example.com",
            "Looking at a card ending 4521 9987",
            "Reading about a prescription for amoxicillin",
        ] {
            assert!(keeps_private(private), "{private}");
            assert_eq!(
                parse_reply(&format!(r#"{{"note":"{private}"}}"#)),
                Seen::Skipped("private".into()),
                "{private}"
            );
        }
        for fine in [
            "Looking at the Ngong Hills hiking trail, 11 km loop",
            "Saved a recipe for chicken biryani",
            "Looking at Nike Pegasus 41 running shoes",
            "Reading about the 2026 Safari Rally route",
        ] {
            assert!(!keeps_private(fine), "{fine}");
        }
    }

    #[test]
    fn a_note_is_one_line_of_bounded_length() {
        let long = format!("Looking at {}", "a very long product name ".repeat(12));
        let Seen::Note(note) = parse_reply(&format!(r#"{{"note":"{long}"}}"#)) else {
            panic!("expected a note");
        };
        assert!(note.chars().count() <= NOTE_CHARS);
        assert!(!note.ends_with(' '));
        let Seen::Note(note) =
            parse_reply("{\"note\":\"Saved a recipe for dal\\nIt has lentils\"}")
        else {
            panic!("expected a note");
        };
        assert!(!note.contains('\n'), "{note:?}");
    }

    #[test]
    fn the_prompt_names_what_is_skipped_and_what_is_never_copied() {
        let prompt = render_system("Jerry");
        for must in [
            "banking, payments or balances",
            "passwords, codes or logins",
            "a private conversation or an email",
            "an identity document",
            "medical or legal records",
            "When unsure, skip.",
            "Never copy a message",
        ] {
            assert!(prompt.contains(must), "{must}");
        }
        assert!(!prompt.contains("{name}"));
    }
}
