//! In-memory [`CaptureRepository`] with the SQLite adapter's rule: a member's picture is kept
//! once, by its content hash.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::capture::{Capture, CaptureState};
use crate::user_data::ports::captures::CaptureRepository;

#[derive(Default)]
pub struct InMemoryCaptureRepository {
    rows: Mutex<Vec<Capture>>,
}

impl InMemoryCaptureRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CaptureRepository for InMemoryCaptureRepository {
    async fn add(&self, capture: &Capture) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        if rows.iter().any(|c| {
            c.id == capture.id
                || (c.profile_id == capture.profile_id
                    && capture.content_hash.is_some()
                    && c.content_hash == capture.content_hash)
        }) {
            return Ok(false);
        }
        rows.push(capture.clone());
        Ok(true)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Capture>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|c| c.profile_id == profile_id && c.id == id)
            .cloned())
    }

    async fn by_hash(&self, profile_id: &str, content_hash: &str) -> Result<Option<Capture>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|c| c.profile_id == profile_id && c.content_hash.as_deref() == Some(content_hash))
            .cloned())
    }

    async fn list(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let mut found: Vec<Capture> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|c| c.profile_id == profile_id && !c.hot)
            .cloned()
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(found)
    }

    async fn hot(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let mut found: Vec<Capture> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.profile_id == profile_id && c.hot)
            .cloned()
            .collect();
        found.sort_by_key(|c| c.created_at);
        Ok(found)
    }

    async fn count_pending(&self, profile_id: &str) -> Result<usize> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|c| c.profile_id == profile_id && c.state == CaptureState::Pending)
            .count())
    }

    async fn save(&self, capture: &Capture) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|c| c.profile_id == capture.profile_id && c.id == capture.id)
        {
            Some(held) => {
                *held = capture.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<Option<Capture>> {
        let mut rows = self.rows.lock().unwrap();
        let at = rows
            .iter()
            .position(|c| c.profile_id == profile_id && c.id == id);
        Ok(at.map(|at| rows.remove(at)))
    }

    async fn delete_all(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let mut rows = self.rows.lock().unwrap();
        let (gone, kept) = std::mem::take(&mut *rows)
            .into_iter()
            .partition(|c| c.profile_id == profile_id);
        *rows = kept;
        Ok(gone)
    }

    async fn find(&self, id: &str) -> Result<Option<Capture>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.id == id)
            .cloned())
    }

    async fn pending(&self, limit: usize) -> Result<Vec<Capture>> {
        let mut found: Vec<Capture> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.state == CaptureState::Pending)
            .cloned()
            .collect();
        found.sort_by_key(|c| c.created_at);
        found.truncate(limit);
        Ok(found)
    }

    async fn pictures_due(&self, now: DateTime<Utc>) -> Result<Vec<Capture>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|c| c.picture_is_due(now))
            .cloned()
            .collect())
    }
}
