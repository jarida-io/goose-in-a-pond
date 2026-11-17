//! Where a member's fact sheet is kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::fact_sheet::{FactLine, FactState};

/// Every read and write is one member's: no method reaches another member's lines.
#[async_trait]
pub trait FactSheetRepository: Send + Sync {
    /// The member's lines in `state`: pinned first, then their own, then by score.
    async fn list(&self, profile_id: &str, state: FactState) -> Result<Vec<FactLine>>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<FactLine>>;

    /// The line under this key in any state, a removed one included.
    async fn by_key(&self, profile_id: &str, fact_key: &str) -> Result<Option<FactLine>>;

    /// Writes one line by its key and holds the sheet to its limit; `None` when the member
    /// removed that key.
    async fn write(&self, line: &FactLine) -> Result<Option<FactLine>>;

    /// A rebuild: `incoming` written over the member's sheet by `fact_sheet::rebuilt`'s rules.
    /// Returns the lines now active.
    async fn replace(
        &self,
        profile_id: &str,
        incoming: &[FactLine],
        now: DateTime<Utc>,
    ) -> Result<Vec<FactLine>>;

    /// Writes every field of a line the member already has; `false` when they have none by
    /// that id. Fails when the new wording is another line's key.
    async fn save(&self, line: &FactLine) -> Result<bool>;

    /// Marks the line removed and keeps its key, so no rebuild writes it again; `false` when
    /// the member has no such line left.
    async fn remove(&self, profile_id: &str, id: &str, now: DateTime<Utc>) -> Result<bool>;

    /// Counts one use of each of these lines.
    async fn count_used(&self, profile_id: &str, ids: &[String]) -> Result<()>;
}
