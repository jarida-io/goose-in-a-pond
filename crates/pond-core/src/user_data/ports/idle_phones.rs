//! Paired phones that have stopped connecting.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// A phone whose pairing was last used before a cutoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdlePhone {
    pub device_id: String,
    pub name: String,
    /// The member it is bound to, if any.
    pub profile_id: Option<String>,
    pub last_seen: DateTime<Utc>,
}

#[async_trait]
pub trait IdlePhones: Send + Sync {
    /// Phones with a live pairing none of whose sessions has been used since `cutoff`. A phone
    /// whose last use was never stamped is not one: nothing says it is idle.
    async fn unseen_since(&self, cutoff: DateTime<Utc>) -> Result<Vec<IdlePhone>>;
}
