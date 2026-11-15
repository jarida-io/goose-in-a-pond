//! What is kept under a stored message besides its words: where a message the pond wrote by
//! itself came from, what a turn's tools really set, and a suggestion's reason and offer.
//! Screens draw these from the stored note, never from the words of the reply.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageNote {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<NoteSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<SuggestionOffer>,
}

impl MessageNote {
    pub fn is_empty(&self) -> bool {
        self.from.is_none() && self.receipts.is_empty() && self.suggestion.is_none()
    }

    pub fn from_source(kind: SourceKind, label: impl Into<String>, ref_id: Option<String>) -> Self {
        Self {
            from: Some(NoteSource {
                kind,
                label: label.into(),
                ref_id,
            }),
            ..Self::default()
        }
    }

    /// The stored form; `None` for a note with nothing in it, so the column stays NULL.
    pub fn to_stored(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        serde_json::to_string(self).ok()
    }

    /// An unreadable note reads as none: the message's words still stand.
    pub fn from_stored(stored: Option<&str>) -> Option<Self> {
        let note: Self = serde_json::from_str(stored?.trim()).ok()?;
        (!note.is_empty()).then_some(note)
    }

    /// The receipts of things that were asked for and not set.
    pub fn not_set(&self) -> impl Iterator<Item = &Receipt> {
        self.receipts
            .iter()
            .filter(|r| r.outcome == ReceiptOutcome::NotSet)
    }
}

/// What kind of background work wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Schedule,
    Reminder,
    Task,
    Lookup,
    Watch,
    Heartbeat,
    Suggestion,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Schedule => "schedule",
            SourceKind::Reminder => "reminder",
            SourceKind::Task => "task",
            SourceKind::Lookup => "lookup",
            SourceKind::Watch => "watch",
            SourceKind::Heartbeat => "heartbeat",
            SourceKind::Suggestion => "suggestion",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSource {
    pub kind: SourceKind,
    /// The thing's own name: a schedule's label, a task's title, a watch's title.
    pub label: String,
    /// Its id, so a screen can open it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    Set,
    NotSet,
}

/// What became of one request to set something for later, as the tool itself reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// What kind of thing: `reminder`, `timer`, `schedule`, `watch`, `task`, `standing`.
    pub kind: String,
    pub outcome: ReceiptOutcome,
    /// What was set, or what was asked for when it was not: "Get superglue, today at 17:00".
    pub what: String,
    /// The thing's id, to open or undo it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Why it was not set, in a sentence a person can act on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Receipt {
    pub fn set(kind: &str, what: impl Into<String>, id: Option<String>) -> Self {
        Self {
            kind: kind.to_string(),
            outcome: ReceiptOutcome::Set,
            what: what.into(),
            id,
            reason: None,
        }
    }

    pub fn not_set(kind: &str, what: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            outcome: ReceiptOutcome::NotSet,
            what: what.into(),
            id: None,
            reason: Some(reason.into()),
        }
    }

    /// The fixed line added to a reply, and spoken, when this was not set.
    pub fn not_set_line(&self) -> Option<String> {
        if self.outcome != ReceiptOutcome::NotSet {
            return None;
        }
        let what = self.what.trim().trim_end_matches('.');
        let reason = self
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        Some(match (what.is_empty(), reason) {
            (true, Some(reason)) => format!("That was not set: {reason}"),
            (true, None) => "That was not set.".to_string(),
            (false, Some(reason)) => format!("Not set: {what}. {reason}"),
            (false, None) => format!("Not set: {what}."),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferState {
    Open,
    Done,
    Dismissed,
}

/// A suggestion as it sits in the thread: why, what the pond would do, and whether it was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionOffer {
    /// One line on what the pond noticed.
    pub noticed: String,
    /// The offer's words, as a button: what the pond would do.
    pub action: String,
    /// What is sent as the person's turn when they take the offer.
    pub prompt: String,
    pub state: OfferState,
    /// The suggestion-queue row this came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_note_is_not_stored_and_a_full_one_round_trips() {
        assert_eq!(MessageNote::default().to_stored(), None);
        assert_eq!(MessageNote::from_stored(None), None);
        assert_eq!(MessageNote::from_stored(Some("{}")), None);
        assert_eq!(MessageNote::from_stored(Some("not json")), None);

        let mut note = MessageNote::from_source(SourceKind::Schedule, "Daily Briefing", None);
        note.receipts.push(Receipt::set(
            "reminder",
            "Get superglue, today at 17:00",
            Some("t1".into()),
        ));
        let stored = note.to_stored().expect("a note with content is stored");
        assert_eq!(MessageNote::from_stored(Some(&stored)), Some(note));
    }

    #[test]
    fn only_a_failed_set_yields_a_line_and_it_says_what_and_why() {
        assert_eq!(
            Receipt::set("timer", "10 minutes", None).not_set_line(),
            None
        );
        assert_eq!(
            Receipt::not_set(
                "timer",
                "A 41 day timer.",
                "A timer needs a length of time."
            )
            .not_set_line()
            .as_deref(),
            Some("Not set: A 41 day timer. A timer needs a length of time.")
        );
        assert_eq!(
            Receipt::not_set("watch", "", "That page lists nothing to compare.")
                .not_set_line()
                .as_deref(),
            Some("That was not set: That page lists nothing to compare.")
        );
    }
}
