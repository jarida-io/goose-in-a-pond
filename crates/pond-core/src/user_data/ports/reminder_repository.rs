//! Port for the reminders a dated utterance becomes. Dedup is the store's job, not the caller's:
//! the engine re-walks windows, and a caller-side check would race.

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait ReminderRepository: Send + Sync {
    /// Store a reminder unless this window already produced it: `Ok(true)` if a row landed,
    /// `Ok(false)` if it existed. Callers must not swallow `Err`; that is the date lost.
    async fn capture(&self, reminder: &CapturedReminder) -> Result<bool>;

    /// Pending reminders, by `said_at` newest first; capture order is meaningless after a backfill.
    async fn list_pending(
        &self,
        _scope: &ProfileScope,
        _limit: usize,
    ) -> Result<Vec<CapturedReminder>> {
        Ok(vec![])
    }

    /// Move one reminder out of `Pending`, never between other states, so a late caller can't
    /// undo a decision. `Ok(false)` if none moved; out-of-`scope` looks the same as unknown.
    async fn set_disposition(
        &self,
        _id: &str,
        _scope: &ProfileScope,
        _disposition: ReminderDisposition,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(false)
    }

    /// Move a pending reminder to `Offered`, stamping when the offer went out; `Ok(false)` if it
    /// was no longer pending, so a race's loser offers nothing twice.
    async fn offer(&self, _id: &str, _at: DateTime<Utc>) -> Result<bool> {
        Ok(false)
    }

    /// The member's offered reminders still waiting for an answer, newest said first.
    async fn offered_to(&self, _profile_id: &str) -> Result<Vec<CapturedReminder>> {
        Ok(vec![])
    }

    /// Settle the member's open offer as `Accepted` or `Dismissed`; `Ok(false)` when they have
    /// none open by that id.
    async fn settle_offer(
        &self,
        _profile_id: &str,
        _id: &str,
        _accepted: bool,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(false)
    }
}
