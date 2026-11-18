//! In-memory [`LedgerRepository`], ordered as the SQLite adapter orders.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::NaiveDate;

use crate::user_data::domain::ledger::LedgerEntry;
use crate::user_data::ports::ledger::LedgerRepository;

#[derive(Default)]
pub struct InMemoryLedgerRepository {
    rows: Mutex<Vec<LedgerEntry>>,
}

impl InMemoryLedgerRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LedgerRepository for InMemoryLedgerRepository {
    async fn add(&self, entries: &[LedgerEntry]) -> Result<()> {
        let mut rows = self.rows.lock().unwrap();
        anyhow::ensure!(
            entries.iter().all(|e| rows.iter().all(|r| r.id != e.id)),
            "a ledger entry with that id is already stored"
        );
        rows.extend(entries.iter().cloned());
        Ok(())
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<LedgerEntry>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|e| e.profile_id == profile_id && e.id == id)
            .cloned())
    }

    async fn list(
        &self,
        profile_id: &str,
        from: NaiveDate,
        until: NaiveDate,
    ) -> Result<Vec<LedgerEntry>> {
        let mut found: Vec<LedgerEntry> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|e| {
                e.profile_id == profile_id && e.occurred_on >= from && e.occurred_on < until
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            b.occurred_on
                .cmp(&a.occurred_on)
                .then(b.created_at.cmp(&a.created_at))
        });
        Ok(found)
    }

    async fn save(&self, entry: &LedgerEntry) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|e| e.profile_id == entry.profile_id && e.id == entry.id)
        {
            Some(held) => {
                *held = entry.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let held = rows.len();
        rows.retain(|e| !(e.profile_id == profile_id && e.id == id));
        Ok(rows.len() < held)
    }
}
