//! In-memory [`ReminderRepository`], and a broken one.
//! Dedup must match migration 0057's UNIQUE constraint on window and `reminder_dedup_key`.

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use crate::user_data::ports::reminder_repository::ReminderRepository;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Mutex;

#[derive(Default)]
pub struct MockReminderRepository {
    rows: Mutex<Vec<CapturedReminder>>,
}

impl MockReminderRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every row held, in write order.
    pub fn rows(&self) -> Vec<CapturedReminder> {
        self.rows.lock().unwrap().clone()
    }
}

/// The SQL adapter's scope predicate, restated for in-memory rows; the two must agree.
fn in_scope(scope: &ProfileScope, row_owner: Option<&str>) -> bool {
    match scope {
        ProfileScope::Owner(id) => row_owner.is_none() || row_owner == Some(id.as_str()),
        ProfileScope::Household => true,
        ProfileScope::Guest => false,
    }
}

#[async_trait]
impl ReminderRepository for MockReminderRepository {
    async fn capture(&self, reminder: &CapturedReminder) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let already = rows.iter().any(|row| {
            row.window_id == reminder.window_id && row.dedup_key() == reminder.dedup_key()
        });
        if already {
            return Ok(false);
        }
        rows.push(reminder.clone());
        Ok(true)
    }

    async fn list_pending(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<CapturedReminder>> {
        let rows = self.rows.lock().unwrap();
        let mut pending: Vec<CapturedReminder> = rows
            .iter()
            .filter(|row| row.disposition == ReminderDisposition::Pending)
            .filter(|row| in_scope(scope, row.profile_id.as_deref()))
            .cloned()
            .collect();
        pending.sort_by(|a, b| b.said_at.cmp(&a.said_at));
        pending.truncate(limit);
        Ok(pending)
    }

    /// Only a pending row moves, matching `sqlite_reminder`'s `AND disposition = 'pending'`.
    async fn set_disposition(
        &self,
        id: &str,
        scope: &ProfileScope,
        disposition: ReminderDisposition,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows.iter_mut().find(|row| {
            row.id == id
                && row.disposition == ReminderDisposition::Pending
                && in_scope(scope, row.profile_id.as_deref())
        }) {
            Some(row) => {
                row.disposition = disposition;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn offer(&self, id: &str, _at: DateTime<Utc>) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|row| row.id == id && row.disposition == ReminderDisposition::Pending)
        {
            Some(row) => {
                row.disposition = ReminderDisposition::Offered;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn offered_to(&self, profile_id: &str) -> Result<Vec<CapturedReminder>> {
        let mut offered: Vec<CapturedReminder> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| {
                row.disposition == ReminderDisposition::Offered
                    && row.profile_id.as_deref() == Some(profile_id)
            })
            .cloned()
            .collect();
        offered.sort_by(|a, b| b.said_at.cmp(&a.said_at));
        Ok(offered)
    }

    async fn settle_offer(
        &self,
        profile_id: &str,
        id: &str,
        accepted: bool,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows.iter_mut().find(|row| {
            row.id == id
                && row.disposition == ReminderDisposition::Offered
                && row.profile_id.as_deref() == Some(profile_id)
        }) {
            Some(row) => {
                row.disposition = if accepted {
                    ReminderDisposition::Accepted
                } else {
                    ReminderDisposition::Dismissed
                };
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// A reminder store that cannot write; the engine must count the failure and say so.
#[derive(Default)]
pub struct FailingReminderRepository;

#[async_trait]
impl ReminderRepository for FailingReminderRepository {
    async fn capture(&self, _reminder: &CapturedReminder) -> Result<bool> {
        anyhow::bail!("the reminder store is unwritable")
    }
}
