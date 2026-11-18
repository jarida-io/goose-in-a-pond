//! In-memory [`IntentionRepository`] with the SQLite adapter's rule: one waiting intention per
//! key and member.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::intention::{ConditionKind, Intention, IntentionState};
use crate::user_data::ports::intentions::IntentionRepository;

#[derive(Default)]
pub struct InMemoryIntentionRepository {
    rows: Mutex<Vec<Intention>>,
}

impl InMemoryIntentionRepository {
    pub fn new() -> Self {
        Self::default()
    }

    fn waiting_where(&self, keep: impl Fn(&Intention) -> bool) -> Vec<Intention> {
        let mut found: Vec<Intention> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.state == IntentionState::Waiting && keep(i))
            .cloned()
            .collect();
        found.sort_by_key(|i| i.until);
        found
    }
}

#[async_trait]
impl IntentionRepository for InMemoryIntentionRepository {
    async fn add(&self, intention: &Intention) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let key = intention.key();
        if rows.iter().any(|i| {
            i.id == intention.id
                || (i.profile_id == intention.profile_id
                    && i.state == IntentionState::Waiting
                    && i.key() == key)
        }) {
            return Ok(false);
        }
        rows.push(intention.clone());
        Ok(true)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Intention>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|i| i.profile_id == profile_id && i.id == id)
            .cloned())
    }

    async fn list(
        &self,
        profile_id: &str,
        state: Option<IntentionState>,
    ) -> Result<Vec<Intention>> {
        let mut found: Vec<Intention> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|i| i.profile_id == profile_id && state.is_none_or(|s| i.state == s))
            .cloned()
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(found)
    }

    async fn waiting(&self, kind: ConditionKind) -> Result<Vec<Intention>> {
        Ok(self.waiting_where(|i| i.condition.kind() == kind))
    }

    async fn waiting_past(&self, now: DateTime<Utc>) -> Result<Vec<Intention>> {
        Ok(self.waiting_where(|i| i.until < now))
    }

    async fn save(&self, intention: &Intention) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|i| i.profile_id == intention.profile_id && i.id == intention.id)
        {
            Some(held) => {
                held.state = intention.state;
                held.outcome = intention.outcome.clone();
                held.settled_at = intention.settled_at;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}
