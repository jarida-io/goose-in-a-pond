//! Port for the reminders a dated utterance becomes.
//!
//! The extraction gate refuses any memory whose note carries a one-off calendar
//! date, on the understanding that the date goes somewhere else instead. This is
//! that somewhere: the trait the batch engine writes through, backed by the
//! `reminders` table (migration 0057).
//!
//! # Dedup belongs to the adapter, not the caller
//!
//! [`capture`](ReminderRepository::capture) is idempotent on
//! `(window_id, reminder_dedup_key(about))`, and it is the STORE that has to
//! make it so. The engine re-walks — a watermark naming a deleted message clears
//! the cursor and the conversation is read again from its first message — and a
//! caller-side "have I seen this?" read would be a race and a second mechanism
//! besides. The window-level guard the engine already has
//! (`BatchExtractionService::already_mined`) does not help here and cannot: it
//! only fires for a window that wrote a memory, and the windows that matter most
//! to this table are exactly the ones whose only yield was a reminder the date
//! rule refused to store as a memory.
//!
//! `capture` therefore answers with whether the row was NEW, rather than with
//! `()`. The caller needs the distinction to report itself honestly: a duplicate
//! is not a loss, a failed write is.
//!
//! # Why two of the three methods are defaulted
//!
//! Writing is the obligation — a reminder that is not stored is the defect this
//! port exists to close, so [`capture`](ReminderRepository::capture) is
//! required. The read and the disposition move are defaulted so an adapter that
//! only needs to be a write target (a test double, a future peer that forwards
//! rather than stores) compiles without asserting it can answer questions it
//! cannot, in the same spirit as the defaults that let the extraction cursor be
//! added to `SessionStorage` without touching every implementation.

use crate::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait ReminderRepository: Send + Sync {
    /// Store a captured reminder, unless this window already produced it.
    ///
    /// `Ok(true)` when a row landed, `Ok(false)` when one was already there.
    /// `Err` is a real failure and must not be swallowed by the caller: it is
    /// the case where the date is gone.
    async fn capture(&self, reminder: &CapturedReminder) -> Result<bool>;

    /// Reminders nothing has acted on yet, most recently said first.
    ///
    /// Ordered by when the CONVERSATION happened rather than by when the pond
    /// read it, because a backlog walk reads a year of history in one night and
    /// capture order says nothing about which reminder is still worth asking
    /// about.
    async fn list_pending(&self, _limit: usize) -> Result<Vec<CapturedReminder>> {
        Ok(vec![])
    }

    /// Record that something was done about one reminder.
    ///
    /// The move off `Pending` is one-way in practice: `Proposed` hands the
    /// decision to the proposal, and the other two are terminal. It is
    /// therefore a move **out of** `Pending` and nothing else: a row that has
    /// already been proposed, dismissed or expired is not moved again, so a
    /// second caller arriving late cannot resurrect a decision or overwrite one.
    ///
    /// `Ok(true)` when a pending row moved, `Ok(false)` when there was none to
    /// move -- an unknown id, or one something else disposed of first. The
    /// distinction is not bookkeeping: a route that cannot tell them apart
    /// answers "dismissed" for an id that does not exist, which is the pond
    /// asserting something it does not know.
    async fn set_disposition(
        &self,
        _id: &str,
        _disposition: ReminderDisposition,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(false)
    }
}
