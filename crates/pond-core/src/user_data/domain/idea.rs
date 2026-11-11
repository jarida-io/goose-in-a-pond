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
    /// `interest`, `memory` or `conversation`.
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
    pub title: String,
    pub pitch: String,
    pub why: String,
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
