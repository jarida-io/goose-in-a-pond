//! An idea card: one short, sourced thing Goose offers a member, with one action. See
//! docs/architecture/idea-cards.md.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::anchor::IllegalMove;
use super::question::{QuestionDraft, QuestionOption};

/// How long a card stays on show after its set is made, unless it is saved.
pub const FADE_HOURS: i64 = 36;
pub const MAX_TITLE_CHARS: usize = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    Do,
    NextStep,
    Reminder,
    Ask,
}

impl CardKind {
    pub const ALL: [Self; 4] = [Self::Do, Self::NextStep, Self::Reminder, Self::Ask];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Do => "do",
            Self::NextStep => "next_step",
            Self::Reminder => "reminder",
            Self::Ask => "ask",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardSourceKind {
    Memory,
    Fact,
    Plan,
    Interest,
    Todo,
    Web,
}

impl CardSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Fact => "fact",
            Self::Plan => "plan",
            Self::Interest => "interest",
            Self::Todo => "todo",
            Self::Web => "web",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Todo,
    Reminder,
    Chat,
    Watch,
    GoDeeper,
}

impl ActionKind {
    pub const ALL: [Self; 5] = [
        Self::Todo,
        Self::Reminder,
        Self::Chat,
        Self::Watch,
        Self::GoDeeper,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Reminder => "reminder",
            Self::Chat => "chat",
            Self::Watch => "watch",
            Self::GoDeeper => "go_deeper",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    New,
    Saved,
    Done,
    Dismissed,
}

impl CardState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Saved => "saved",
            Self::Done => "done",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "new" => Some(Self::New),
            "saved" => Some(Self::Saved),
            "done" => Some(Self::Done),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhySource {
    pub kind: CardSourceKind,
    pub label: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Why {
    pub text: String,
    pub source: WhySource,
}

/// The one structured question an ask card puts to the member. The option they pick is sent to
/// Goose as their message; "Other" opens the composer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardQuestion {
    pub header: String,
    pub text: String,
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<usize>,
    #[serde(default)]
    pub multi: bool,
}

impl From<QuestionDraft> for CardQuestion {
    fn from(draft: QuestionDraft) -> Self {
        Self {
            header: draft.header,
            text: draft.text,
            options: draft.options,
            recommended: draft.recommended,
            multi: draft.multi,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardAction {
    pub kind: ActionKind,
    pub label: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// Member-local `YYYY-MM-DD HH:MM`, or RFC 3339.
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// An ask card's question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<CardQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSource {
    pub kind: CardSourceKind,
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardNotes {
    #[serde(default)]
    pub found: String,
    #[serde(default)]
    pub next: String,
    #[serde(default)]
    pub caveats: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardAnchor {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub kind: CardKind,
    pub title: String,
    pub why: Why,
    pub when: Option<String>,
    #[serde(rename = "where")]
    pub where_: Option<String>,
    pub cost: Option<String>,
    pub effort: Option<String>,
    pub action: Option<CardAction>,
    pub sources: Vec<CardSource>,
    pub notes: CardNotes,
    pub anchor: Option<CardAnchor>,
    pub state: CardState,
    pub batch_id: String,
    /// Its place in its set, best first.
    #[serde(skip)]
    pub rank: u32,
    pub created_at: DateTime<Utc>,
    pub fades_at: DateTime<Utc>,
}

impl Card {
    pub fn fade_time(made_at: DateTime<Utc>) -> DateTime<Utc> {
        made_at + Duration::hours(FADE_HOURS)
    }

    /// Whether the member still sees it: saved, or new and not yet faded.
    pub fn is_shown(&self, now: DateTime<Utc>) -> bool {
        match self.state {
            CardState::Saved => true,
            CardState::New => self.fades_at > now,
            CardState::Done | CardState::Dismissed => false,
        }
    }

    fn illegal(&self, to: CardState) -> IllegalMove {
        IllegalMove {
            what: "a card",
            from: self.state.as_str(),
            to: to.as_str(),
        }
    }

    /// Moves a shown card to `to`; a done or dismissed card stays as it is.
    pub fn set_state(&mut self, to: CardState) -> Result<(), IllegalMove> {
        if matches!(self.state, CardState::Done | CardState::Dismissed) && self.state != to {
            return Err(self.illegal(to));
        }
        self.state = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_read_as_a_small_model_writes_them() {
        assert_eq!(CardKind::parse("Next step"), Some(CardKind::NextStep));
        assert_eq!(CardKind::parse("idea"), None);
        assert_eq!(ActionKind::parse("go-deeper"), Some(ActionKind::GoDeeper));
        assert_eq!(ActionKind::parse("call"), None);
    }
}
