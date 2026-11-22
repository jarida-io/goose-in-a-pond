//! In-memory [`DontListRepository`] with the SQLite adapter's rule: one rule per key, a second
//! saying merged into the first.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::dont_list::{sort_rules, DontRule, Strength};
use crate::user_data::ports::dont_list::DontListRepository;

#[derive(Default)]
pub struct InMemoryDontListRepository {
    rows: Mutex<Vec<DontRule>>,
}

impl InMemoryDontListRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DontListRepository for InMemoryDontListRepository {
    async fn list(&self, profile_id: &str, strength: Option<Strength>) -> Result<Vec<DontRule>> {
        let mut rules: Vec<DontRule> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.profile_id == profile_id && strength.is_none_or(|s| r.strength == s))
            .cloned()
            .collect();
        sort_rules(&mut rules);
        Ok(rules)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<DontRule>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|r| r.profile_id == profile_id && r.id == id)
            .cloned())
    }

    async fn add(&self, rule: &DontRule) -> Result<DontRule> {
        let mut rows = self.rows.lock().unwrap();
        let key = rule.key();
        match rows
            .iter_mut()
            .find(|r| r.profile_id == rule.profile_id && r.key() == key)
        {
            Some(held) => {
                *held = held.clone().merged_with(rule);
                Ok(held.clone())
            }
            None => {
                rows.push(rule.clone());
                Ok(rule.clone())
            }
        }
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|r| !(r.profile_id == profile_id && r.id == id));
        Ok(rows.len() < before)
    }
}
