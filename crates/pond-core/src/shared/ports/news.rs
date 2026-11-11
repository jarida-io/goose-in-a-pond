//! Driven ports for a member's news: where stories and digests are kept, and the web they are
//! found and read on.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use crate::shared::domain::news::{
    Digest, DigestEntry, InterestRow, NewsBook, NewsSettings, Portrait, Story, StoryKind, Topic,
    Vote,
};

/// A member's news choices, stories, digests and the bookkeeping between passes. Every call is
/// scoped to one member; nothing reads across members.
#[async_trait]
pub trait NewsStore: Send + Sync {
    /// The member's choices, or the defaults when they never chose.
    async fn settings(&self, member: &str) -> Result<NewsSettings>;
    async fn save_settings(&self, member: &str, settings: &NewsSettings) -> Result<()>;

    async fn book(&self, member: &str) -> Result<NewsBook>;
    async fn request_refresh(&self, member: &str, at: DateTime<Utc>) -> Result<()>;
    /// A pass starts for the member: marks it writing and takes any refresh it answers.
    async fn begin_writing(&self, member: &str, at: DateTime<Utc>) -> Result<()>;
    /// The pass ended; `stories_done` stamps a finished story pass.
    async fn end_writing(&self, member: &str, stories_done: Option<DateTime<Utc>>) -> Result<()>;
    async fn mark_spoken(&self, member: &str, day: NaiveDate) -> Result<()>;

    /// Every household member, whether or not they have interests.
    async fn members(&self) -> Result<Vec<String>>;
    async fn interests(&self, member: &str) -> Result<Vec<InterestRow>>;
    async fn portrait(&self, member: &str) -> Result<Option<Portrait>>;

    /// The member's topic map, highest score first.
    async fn topics(&self, member: &str) -> Result<Vec<Topic>>;
    /// Replaces the member's map. A topic whose query the old map had keeps that row's id and
    /// `used_at`, so a rebuild does not make the ideas search a seed again.
    async fn replace_topics(&self, member: &str, topics: &[Topic]) -> Result<()>;
    async fn mark_topics_used(&self, member: &str, ids: &[String], at: DateTime<Utc>)
        -> Result<()>;

    /// Keeps a story; `false` when the member already has one from the same page.
    async fn add_story(&self, story: &Story) -> Result<bool>;
    /// The member's stories written at or after `since`, newest first; none they called wrong.
    async fn stories_since(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>>;
    /// One interest's stories, newest first, at most `limit`; none they called wrong.
    async fn interest_stories(
        &self,
        member: &str,
        interest_id: &str,
        limit: usize,
    ) -> Result<Vec<Story>>;
    /// Stories the member read at or after `since`, the latest read first.
    async fn read_stories(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>>;
    /// Stories the member voted up or down at or after `since`.
    async fn voted_stories(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>>;
    async fn story(&self, member: &str, id: &str) -> Result<Option<Story>>;
    /// `false` when the member has no such story.
    async fn mark_read(&self, member: &str, id: &str, at: DateTime<Utc>) -> Result<bool>;
    /// `false` when the member has no such story.
    async fn set_vote(
        &self,
        member: &str,
        id: &str,
        vote: Vote,
        note: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<bool>;
    /// Whether the member has a story from the page whose [`crate::shared::services::news::dedupe::url_key`] this is.
    async fn has_page(&self, member: &str, url_key: &str) -> Result<bool>;
    /// How many of the member's stories of `kind` were written at or after `since`.
    async fn count_stories(
        &self,
        member: &str,
        kind: StoryKind,
        since: DateTime<Utc>,
    ) -> Result<u32>;
    /// Forgets stories written before `before`; digests keep their own copies.
    async fn prune_stories(&self, before: DateTime<Utc>) -> Result<u64>;

    async fn add_digest(&self, digest: &Digest) -> Result<()>;
    async fn digest(&self, member: &str, id: &str) -> Result<Option<Digest>>;
    async fn digest_on(&self, member: &str, day: NaiveDate) -> Result<Option<Digest>>;
    async fn latest_digest(&self, member: &str) -> Result<Option<Digest>>;
    /// Every digest, newest first.
    async fn digests(&self, member: &str) -> Result<Vec<DigestEntry>>;
}

/// How recent a search's pages must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Day,
    Week,
    /// Any age: an evergreen subject's best pages.
    Any,
}

/// A page a search found. Held only for the pass that asked: a search result's words are never
/// kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundPage {
    pub url: String,
    pub title: String,
}

/// A page as the pond read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadPage {
    /// Where the page ended up, after redirects.
    pub url: String,
    pub title: String,
    pub site: String,
    /// The page's text, one line per block.
    pub lines: Vec<String>,
    pub published_at: Option<DateTime<Utc>>,
}

/// Why the news cannot search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBlocked {
    /// No key is stored.
    NotConfigured,
    /// The search service refused the key.
    Refused,
    /// This month's allowance is spent.
    Quota,
}

/// What a search came to when it found nothing usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchFailure {
    /// Searching cannot work until a person acts; the pass stops.
    Blocked(SearchBlocked),
    /// This search failed; the next may not.
    Failed(String),
}

/// The web the news searches and reads, through the pond's own tracked clients.
#[async_trait]
pub trait NewsWeb: Send + Sync {
    /// Why searching cannot work now, if it cannot; checked before a pass spends anything.
    async fn blocked(&self) -> Option<SearchBlocked>;
    /// Recent pages for `query`, best first.
    async fn find(
        &self,
        query: &str,
        freshness: Freshness,
    ) -> std::result::Result<Vec<FoundPage>, SearchFailure>;
    /// The page's text, or `None` when it cannot be read.
    async fn read(&self, url: &str) -> Option<ReadPage>;
    /// A kept picture for the page, by its report-image key.
    async fn picture(&self, url: &str) -> Option<String>;
}
