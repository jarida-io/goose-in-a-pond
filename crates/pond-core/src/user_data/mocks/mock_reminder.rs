//! In-memory [`ReminderRepository`], and a broken one.
//!
//! The dedup rule here is a deliberate twin of the UNIQUE constraint in
//! migration 0057: same window, same `reminder_dedup_key`, no second row. These
//! two must agree or every mock-backed test is testing a fiction — the same
//! contract `mock_memory`'s `scope_matches` holds against `sqlite_memory`'s
//! `scope_sql`.
//!
//! [`FailingReminderRepository`] exists because a store that cannot be made to
//! fail cannot show that a failure is counted, and "the date is lost" is the one
//! outcome here that has to be visible rather than merely handled.

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

    /// Everything the store is holding, in the order it was written.
    pub fn rows(&self) -> Vec<CapturedReminder> {
        self.rows.lock().unwrap().clone()
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

    async fn list_pending(&self, limit: usize) -> Result<Vec<CapturedReminder>> {
        let rows = self.rows.lock().unwrap();
        let mut pending: Vec<CapturedReminder> = rows
            .iter()
            .filter(|row| row.disposition == ReminderDisposition::Pending)
            .cloned()
            .collect();
        pending.sort_by(|a, b| b.said_at.cmp(&a.said_at));
        pending.truncate(limit);
        Ok(pending)
    }

    /// Only a PENDING row moves, matching the `AND disposition = 'pending'`
    /// clause in `sqlite_reminder`. A mock that moved any row would let a test
    /// prove a double-dismiss is harmless when against the real store it is not.
    async fn set_disposition(
        &self,
        id: &str,
        disposition: ReminderDisposition,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|row| row.id == id && row.disposition == ReminderDisposition::Pending)
        {
            Some(row) => {
                row.disposition = disposition;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// A reminder store that cannot write.
///
/// A full disk, a locked database, a table that never migrated. Whatever the
/// cause, the engine's response has to be the same: count it and say so.
#[derive(Default)]
pub struct FailingReminderRepository;

#[async_trait]
impl ReminderRepository for FailingReminderRepository {
    async fn capture(&self, _reminder: &CapturedReminder) -> Result<bool> {
        anyhow::bail!("the reminder store is unwritable")
    }
}
