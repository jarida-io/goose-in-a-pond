//! The one-line notes an earlier reader kept of members' screenshots, which still seed their
//! ideas, and the guard on what a line about a picture may carry.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// The member preference that turned the notes on; absent or anything but `"true"` is off.
pub const PREFERENCE: &str = "ideas_from_screenshots";
/// Notes kept for one member; the oldest go first.
pub const NOTES_KEPT: usize = 40;
/// How long a note may seed an idea.
pub const SEED_FOR: Duration = Duration::days(14);

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
