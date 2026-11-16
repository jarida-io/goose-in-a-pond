//! In-memory [`FactSheetRepository`] with the SQLite adapter's rules: one line per key, a
//! removed key never written again, the sheet held to its limit.

use std::sync::Mutex;

use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::fact_sheet::{rank, rebuilt, with_line, FactLine, FactState};
use crate::user_data::ports::fact_sheet::FactSheetRepository;

#[derive(Default)]
pub struct InMemoryFactSheetRepository {
    rows: Mutex<Vec<FactLine>>,
}

impl InMemoryFactSheetRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every member's rows, for assertions.
    pub fn all(&self) -> Vec<FactLine> {
        self.rows.lock().unwrap().clone()
    }
}

/// Takes one member's rows out of `rows`.
fn take_member(rows: &mut Vec<FactLine>, profile_id: &str) -> Vec<FactLine> {
    let (mine, others) = std::mem::take(rows)
        .into_iter()
        .partition(|r| r.profile_id == profile_id);
    *rows = others;
    mine
}

#[async_trait]
impl FactSheetRepository for InMemoryFactSheetRepository {
    async fn list(&self, profile_id: &str, state: FactState) -> Result<Vec<FactLine>> {
        let mut lines: Vec<FactLine> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.profile_id == profile_id && r.state == state)
            .cloned()
            .collect();
        rank(&mut lines);
        Ok(lines)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<FactLine>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|r| r.profile_id == profile_id && r.id == id)
            .cloned())
    }

    async fn by_key(&self, profile_id: &str, fact_key: &str) -> Result<Option<FactLine>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|r| r.profile_id == profile_id && r.key() == fact_key)
            .cloned())
    }

    async fn write(&self, line: &FactLine) -> Result<Option<FactLine>> {
        let mut rows = self.rows.lock().unwrap();
        let mine = take_member(&mut rows, &line.profile_id);
        match with_line(mine.clone(), line.clone(), line.updated_at) {
            Some((after, stored)) => {
                rows.extend(after);
                Ok(Some(stored))
            }
            None => {
                rows.extend(mine);
                Ok(None)
            }
        }
    }

    async fn replace(
        &self,
        profile_id: &str,
        incoming: &[FactLine],
        now: DateTime<Utc>,
    ) -> Result<Vec<FactLine>> {
        let incoming = incoming
            .iter()
            .filter(|line| line.profile_id == profile_id)
            .cloned()
            .collect();
        {
            let mut rows = self.rows.lock().unwrap();
            let mine = take_member(&mut rows, profile_id);
            rows.extend(rebuilt(mine, incoming, now));
        }
        self.list(profile_id, FactState::Active).await
    }

    async fn save(&self, line: &FactLine) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let key = line.key();
        if rows
            .iter()
            .any(|r| r.profile_id == line.profile_id && r.id != line.id && r.key() == key)
        {
            bail!("the member already has a fact line under that key");
        }
        match rows
            .iter_mut()
            .find(|r| r.profile_id == line.profile_id && r.id == line.id)
        {
            Some(row) => {
                *row = line.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn remove(&self, profile_id: &str, id: &str, now: DateTime<Utc>) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|r| r.profile_id == profile_id && r.id == id && r.state != FactState::Removed)
        {
            Some(row) => {
                row.state = FactState::Removed;
                row.pinned = false;
                row.updated_at = now;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn count_used(&self, profile_id: &str, ids: &[String]) -> Result<()> {
        let mut rows = self.rows.lock().unwrap();
        for row in rows
            .iter_mut()
            .filter(|r| r.profile_id == profile_id && ids.contains(&r.id))
        {
            row.used += 1;
        }
        Ok(())
    }
}
