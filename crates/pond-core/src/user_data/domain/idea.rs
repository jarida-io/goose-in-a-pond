//! An idea Goose researched for a member: new to them, built on something specific about them,
//! sourced, and something they or Goose can act on.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdeaState {
    #[default]
    New,
    Working,
    Done,
    Dismissed,
}

impl IdeaState {
    pub fn as_str(self) -> &'static str {
        match self {
            IdeaState::New => "new",
            IdeaState::Working => "working",
            IdeaState::Done => "done",
            IdeaState::Dismissed => "dismissed",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "working" => IdeaState::Working,
            "done" => IdeaState::Done,
            "dismissed" => IdeaState::Dismissed,
            _ => IdeaState::New,
        }
    }
}

/// What sort of thing an idea is. The phone picks the card's icon and label from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdeaKind {
    /// Somewhere to go outdoors.
    Outing,
    /// Something to do for fun.
    Activity,
    /// A date with their partner.
    Date,
    /// A date with themselves.
    SoloDate,
    /// A holiday or a weekend away.
    Trip,
    /// A dish to cook.
    Recipe,
    /// Something new about one of their interests.
    #[default]
    Interest,
    /// Something for a piece of work they have on.
    Project,
}

impl IdeaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Outing => "outing",
            Self::Activity => "activity",
            Self::Date => "date",
            Self::SoloDate => "solo_date",
            Self::Trip => "trip",
            Self::Recipe => "recipe",
            Self::Interest => "interest",
            Self::Project => "project",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(
            match raw.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
                "outing" => Self::Outing,
                "activity" => Self::Activity,
                "date" => Self::Date,
                "solo_date" => Self::SoloDate,
                "trip" => Self::Trip,
                "recipe" => Self::Recipe,
                "interest" => Self::Interest,
                "project" => Self::Project,
                _ => return None,
            },
        )
    }

    /// The card's small label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Outing => "Get outside",
            Self::Activity => "Something to do",
            Self::Date => "Date idea",
            Self::SoloDate => "Solo date",
            Self::Trip => "Holiday idea",
            Self::Recipe => "Recipe",
            Self::Interest => "From your interests",
            Self::Project => "For your projects",
        }
    }
}

/// The card's small label for an idea of `kind` built on `grounded_in`: one from a screenshot
/// says so, since it came from what they looked at and not from an interest they named.
pub fn card_label(kind: IdeaKind, grounded_in: &[Grounding]) -> &'static str {
    if grounded_in.iter().any(|g| g.kind == "screenshot") {
        "From a screenshot"
    } else {
        kind.label()
    }
}

/// One thing worth seeing at a glance on an idea's page: where, when, the cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaFact {
    pub label: String,
    pub value: String,
}

/// What Goose can do about an idea on the member's say-so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GooseKind {
    /// A deep-research long task.
    Research,
    /// A web watch on a listings page the idea names.
    Watch,
    /// A to-do on the member's list.
    Todo,
    /// A recurring prompt Goose runs for them.
    Schedule,
    /// Nothing on the Pond: the phone opens a chat about it.
    Chat,
}

impl GooseKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "research" => Some(GooseKind::Research),
            "watch" => Some(GooseKind::Watch),
            "todo" | "to-do" | "to_do" => Some(GooseKind::Todo),
            "schedule" => Some(GooseKind::Schedule),
            "chat" => Some(GooseKind::Chat),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GooseCan {
    pub label: String,
    pub kind: GooseKind,
}

/// What Goose's part needs and the phone never sees.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoosePlan {
    /// What Goose researches, or asks itself on each scheduled run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The listings page a watch checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grounding {
    /// `interest`, `memory`, `conversation` or `screenshot`.
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaSource {
    pub title: String,
    pub url: String,
    pub site: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeaProgress {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Idea {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    /// The interest or piece of work it builds on, so the next idea picks another.
    #[serde(skip)]
    pub topic: String,
    pub kind: IdeaKind,
    pub kind_label: String,
    pub title: String,
    /// The card's one line.
    pub hook: String,
    pub pitch: String,
    pub why: String,
    pub facts: Vec<IdeaFact>,
    /// What Goose kept to work from when the member asks about the idea: the details of the
    /// pages that did not fit the card.
    pub notes: String,
    pub grounded_in: Vec<Grounding>,
    pub sources: Vec<IdeaSource>,
    pub picture: Option<String>,
    pub goose_can: Option<GooseCan>,
    #[serde(skip)]
    pub goose_plan: GoosePlan,
    pub you_can: Vec<String>,
    pub state: IdeaState,
    pub progress: Option<IdeaProgress>,
    pub created_at: DateTime<Utc>,
}

impl Idea {
    /// The idea as a card sent to chat: what the member read, and what Goose works from.
    pub fn as_card(&self) -> crate::shared::domain::context_card::ContextCard {
        let mut content = format!("{}\n{}", self.hook, self.pitch);
        if !self.why.is_empty() {
            content.push_str(&format!("\nWhy it fits: {}", self.why));
        }
        for fact in &self.facts {
            content.push_str(&format!("\n{}: {}", fact.label, fact.value));
        }
        if !self.you_can.is_empty() {
            content.push_str("\nSteps:");
            for (i, step) in self.you_can.iter().enumerate() {
                content.push_str(&format!("\n{}. {step}", i + 1));
            }
        }

        let mut notes = String::new();
        if !self.notes.is_empty() {
            notes.push_str(&self.notes);
            notes.push('\n');
        }
        let built_on: Vec<&str> = self.grounded_in.iter().map(|g| g.label.as_str()).collect();
        if !built_on.is_empty() {
            notes.push_str(&format!("Built on: {}.\n", built_on.join(", ")));
        }
        for source in &self.sources {
            notes.push_str(&format!("Source: {} ({})\n", source.title, source.url));
        }
        if let Some(can) = &self.goose_can {
            notes.push_str(&format!("You offered to: {}.\n", can.label));
        }
        match self.state {
            IdeaState::Working => notes.push_str("You are already working on your part.\n"),
            IdeaState::Done => notes.push_str("Your part is done.\n"),
            _ => {}
        }
        crate::shared::domain::context_card::ContextCard::new(
            self.kind_label.clone(),
            self.title.clone(),
            content,
            notes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_reads_back_from_its_own_word() {
        for kind in [
            IdeaKind::Outing,
            IdeaKind::Activity,
            IdeaKind::Date,
            IdeaKind::SoloDate,
            IdeaKind::Trip,
            IdeaKind::Recipe,
            IdeaKind::Interest,
            IdeaKind::Project,
        ] {
            assert_eq!(IdeaKind::parse(kind.as_str()), Some(kind));
            assert!(!kind.label().is_empty());
        }
        assert_eq!(IdeaKind::parse("Solo date"), Some(IdeaKind::SoloDate));
        assert_eq!(IdeaKind::parse("chore"), None);
    }
}
