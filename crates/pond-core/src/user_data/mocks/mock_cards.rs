//! In-memory [`CardRepository`] with the SQLite adapter's ordering and fading.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::card::{Card, CardState};
use crate::user_data::ports::cards::CardRepository;

#[derive(Default)]
pub struct InMemoryCardRepository {
    rows: Mutex<Vec<Card>>,
}

impl InMemoryCardRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every stored card, in any state.
    pub fn all(&self) -> Vec<Card> {
        self.rows.lock().unwrap().clone()
    }

    fn newest_first(&self, keep: impl Fn(&Card) -> bool) -> Vec<Card> {
        let mut found: Vec<Card> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|card| keep(card))
            .cloned()
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.rank.cmp(&b.rank)));
        found
    }
}

#[async_trait]
impl CardRepository for InMemoryCardRepository {
    async fn add_set(&self, profile_id: &str, cards: &[Card], now: DateTime<Utc>) -> Result<()> {
        let mut rows = self.rows.lock().unwrap();
        for card in rows.iter_mut().filter(|card| {
            card.profile_id == profile_id && card.state == CardState::New && card.fades_at > now
        }) {
            card.fades_at = now;
        }
        rows.extend(cards.iter().cloned());
        Ok(())
    }

    async fn add_one(&self, card: &Card) -> Result<()> {
        self.rows.lock().unwrap().push(card.clone());
        Ok(())
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Card>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|card| card.profile_id == profile_id && card.id == id)
            .cloned())
    }

    async fn shown(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Card>> {
        Ok(self.newest_first(|card| card.profile_id == profile_id && card.is_shown(now)))
    }

    async fn since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<Card>> {
        Ok(self.newest_first(|card| card.profile_id == profile_id && card.created_at >= since))
    }

    async fn save(&self, card: &Card) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let Some(held) = rows
            .iter_mut()
            .find(|held| held.profile_id == card.profile_id && held.id == card.id)
        else {
            return Ok(false);
        };
        held.state = card.state;
        Ok(true)
    }

    async fn purge_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let mut rows = self.rows.lock().unwrap();
        let held = rows.len();
        rows.retain(|card| card.state == CardState::Saved || card.created_at >= before);
        Ok((held - rows.len()) as u64)
    }
}
