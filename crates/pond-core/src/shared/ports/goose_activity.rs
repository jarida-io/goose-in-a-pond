//! GooseActivityRepository port: the record of what Goose did for a member by itself.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared::domain::goose_activity::{ActivityKind, GooseActivity};

#[async_trait]
pub trait GooseActivityRepository: Send + Sync {
    async fn add(&self, item: &GooseActivity) -> Result<()>;

    /// One member's entries, newest first.
    async fn list_for(&self, member: &str, limit: usize) -> Result<Vec<GooseActivity>>;

    /// One entry, only if it is this member's.
    async fn get(&self, member: &str, id: &str) -> Result<Option<GooseActivity>>;

    /// Mark an entry taken back. `false` when it was not theirs or already was.
    async fn mark_undone(&self, member: &str, id: &str, at: DateTime<Utc>) -> Result<bool>;

    /// How many entries of `kind` a member has since `since`: what a day's cap counts.
    async fn count_since(
        &self,
        member: &str,
        kind: ActivityKind,
        since: DateTime<Utc>,
    ) -> Result<usize>;

    /// Forget entries older than `before`.
    async fn prune(&self, before: DateTime<Utc>) -> Result<u64>;
}
