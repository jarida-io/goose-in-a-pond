//! Where a member's ideas are kept, with when they last asked for more and the to-dos an idea
//! became.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::idea::{Idea, IdeaState};

/// When ideas were last asked for and thought of, for one member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdeaRun {
    pub requested_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl IdeaRun {
    /// Asked for, or started, after the last run finished.
    pub fn is_pending(&self) -> bool {
        let after_finish = |at: Option<DateTime<Utc>>| match (at, self.finished_at) {
            (Some(at), Some(done)) => at > done,
            (Some(_), None) => true,
            (None, _) => false,
        };
        after_finish(self.requested_at) || after_finish(self.started_at)
    }

    /// What the phone is told: thinking while a run is pending, but not past a couple of hours,
    /// so a Pond with no model to run it never shows a spinner for good.
    pub fn is_thinking(&self, now: DateTime<Utc>) -> bool {
        let recent =
            |at: Option<DateTime<Utc>>| at.is_some_and(|at| now - at < chrono::Duration::hours(2));
        self.is_pending() && (recent(self.requested_at) || recent(self.started_at))
    }
}

/// A topic from the member's topic map that the news does not cover: a seed for an idea.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicSeed {
    pub id: String,
    pub area: String,
    pub subject: String,
    /// The web-search query the topic map wrote for it.
    pub query: String,
    pub score: f64,
    pub why: String,
}

/// A to-do an idea's step became, as the to-do list keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaTodo {
    pub id: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait IdeaRepository: Send + Sync {
    /// The member's ideas that are not dismissed, newest first.
    async fn list(&self, profile_id: &str) -> Result<Vec<Idea>>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Idea>>;

    async fn add(&self, idea: &Idea) -> Result<()>;

    /// `false` when the member has no idea by that id.
    async fn set_state(
        &self,
        profile_id: &str,
        id: &str,
        state: IdeaState,
        task_id: Option<&str>,
    ) -> Result<bool>;

    /// Topics the member's ideas built on since `since`.
    async fn topics_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<String>>;

    /// Topics of the ideas the member turned down since `since`.
    async fn declined_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<String>>;

    async fn run(&self, profile_id: &str) -> Result<IdeaRun>;

    async fn mark_requested(&self, profile_id: &str, at: DateTime<Utc>) -> Result<()>;

    async fn mark_started(&self, profile_id: &str, at: DateTime<Utc>) -> Result<()>;

    async fn mark_finished(&self, profile_id: &str, at: DateTime<Utc>) -> Result<()>;

    /// The member's topics that are not newsworthy and were not used since `unused_since`, best
    /// first. Empty when the topic map has not been built.
    async fn research_seeds(
        &self,
        profile_id: &str,
        unused_since: DateTime<Utc>,
    ) -> Result<Vec<TopicSeed>>;

    async fn mark_seed_used(
        &self,
        profile_id: &str,
        seed_id: &str,
        at: DateTime<Utc>,
    ) -> Result<()>;

    /// One open to-do on the member's list, from an idea's step.
    async fn add_todo(&self, profile_id: &str, idea_id: &str, text: &str) -> Result<IdeaTodo>;
}
