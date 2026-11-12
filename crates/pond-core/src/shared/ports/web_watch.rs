//! Driven ports for web watches: where they are kept, how a watched page is read, and how its owner
//! is told.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared::domain::web_watch::{WatchChange, WatchFilters, WatchItem};

/// A page a member asked the pond to keep checking.
#[derive(Debug, Clone, PartialEq)]
pub struct WebWatch {
    pub id: String,
    /// The member who set it up; only they are told, and only they see it.
    pub owner: String,
    pub address: String,
    pub title: String,
    /// What they said they are looking for, in their words.
    pub looking_for: String,
    pub filters: WatchFilters,
    pub interval_hours: u32,
    pub paused: bool,
    /// The items the last good check found; `None` before the first.
    pub items: Option<Vec<WatchItem>>,
    /// Failed checks in a row.
    pub failures: u32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// The conversation it was set up in, if any, which a tapped notice opens.
    pub session_id: Option<String>,
}

/// A change a check found, as kept.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredChange {
    pub id: i64,
    pub watch_id: String,
    pub found_at: DateTime<Utc>,
    pub change: WatchChange,
    /// Whether it went into a daily digest; a matched change is pushed when found instead.
    pub digested: bool,
}

/// What a member may change on a watch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WatchEdit {
    pub looking_for: Option<String>,
    pub filters: Option<WatchFilters>,
    pub interval_hours: Option<u32>,
    pub paused: Option<bool>,
}

/// One check's result, written at once.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    pub items: Option<Vec<WatchItem>>,
    pub failures: u32,
    pub checked_at: DateTime<Utc>,
    pub next_check_at: DateTime<Utc>,
    pub changes: Vec<WatchChange>,
    /// The changes wait for the model to say which the owner asked for: none is pushed or
    /// digested until then.
    pub held: bool,
}

#[async_trait]
pub trait WebWatchRepository: Send + Sync {
    async fn create(&self, watch: &WebWatch) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<WebWatch>>;
    /// The owner's watches, newest first.
    async fn list_for(&self, owner: &str) -> Result<Vec<WebWatch>>;
    /// Watches not paused whose next check is at or before `now`, soonest first.
    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<WebWatch>>;
    async fn record_check(&self, id: &str, outcome: &CheckOutcome) -> Result<()>;
    /// Applies `edit` to the owner's watch; `false` when they have none by that id.
    async fn edit(&self, owner: &str, id: &str, edit: &WatchEdit) -> Result<bool>;
    /// Deletes the owner's watch and its changes; `false` when they have none by that id.
    async fn delete(&self, owner: &str, id: &str) -> Result<bool>;
    /// A watch's changes, newest first.
    async fn changes(&self, watch_id: &str, limit: usize) -> Result<Vec<StoredChange>>;
    /// Unmatched changes not yet in a digest, across the owner's watches, oldest first.
    async fn undigested(&self, owner: &str) -> Result<Vec<StoredChange>>;
    async fn owners_with_undigested(&self) -> Result<Vec<String>>;
    async fn mark_digested(&self, ids: &[i64]) -> Result<()>;
    /// Deletes changes found before `before`; returns how many.
    async fn prune_changes(&self, before: DateTime<Utc>) -> Result<u64>;
    /// Changes waiting to be judged, oldest first.
    async fn held(&self, limit: usize) -> Result<Vec<StoredChange>>;
    /// Releases held changes: `told` were what the owner asked for and count as matches; the
    /// rest of `judged` stay in the watch's history and reach no digest.
    async fn judged(&self, judged: &[i64], told: &[i64]) -> Result<()>;
}

/// Why a check could not read its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchReadError {
    /// The site would not let the pond read it (401 or 403).
    Refused,
    /// The site asked the pond to wait (429).
    RateLimited,
    /// No page is at the address any more (404 or 410).
    Gone,
    Other(String),
}

#[async_trait]
pub trait WatchPageReader: Send + Sync {
    /// The items the page lists now.
    async fn read(&self, address: &str) -> std::result::Result<Vec<WatchItem>, WatchReadError>;
}

/// What a watch notice is about: only a match may also be said aloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// Something the member asked for appeared, or dropped in price.
    Match,
    /// The page could not be checked several times in a row.
    Trouble,
    Digest,
}

/// A notice for a watch's owner.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchNotice {
    pub owner: String,
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
    /// The watch a tap opens; `None` for a digest across several.
    pub watch_id: Option<String>,
}

#[async_trait]
pub trait WatchNotifier: Send + Sync {
    async fn notify(&self, notice: WatchNotice) -> Result<()>;
}
