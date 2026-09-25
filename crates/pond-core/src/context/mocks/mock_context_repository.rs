//! In-memory [`ContextRepository`] for tests. Enforces the scope rule so scope guards can fail.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::context::domain::{ContextItem, ContextSource};
use crate::context::ports::ContextRepository;
use crate::context::retention::ContextRetention;
use crate::context::scope::{item_is_visible, source_is_visible};
use crate::user_data::domain::profile::ProfileScope;

#[derive(Default)]
pub struct MockContextRepository {
    sources: Mutex<Vec<ContextSource>>,
    items: Mutex<Vec<ContextItem>>,
    /// When set, every read of the source list fails with this message.
    unreadable_sources: Mutex<Option<String>>,
    /// When set, every item write fails with this message.
    unwritable_items: Mutex<Option<String>>,
}

impl MockContextRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every stored item, unscoped. For asserting what was written.
    pub fn all_items(&self) -> Vec<ContextItem> {
        self.items.lock().unwrap().clone()
    }

    /// Make [`ContextRepository::list_sources`] and
    /// [`ContextRepository::get_source`] fail.
    pub fn with_unreadable_sources(self, reason: &str) -> Self {
        *self.unreadable_sources.lock().unwrap() = Some(reason.to_string());
        self
    }

    /// Make [`ContextRepository::save_item`] fail.
    pub fn with_unwritable_items(self, reason: &str) -> Self {
        *self.unwritable_items.lock().unwrap() = Some(reason.to_string());
        self
    }

    fn source_read_failure(&self) -> Option<anyhow::Error> {
        self.unreadable_sources
            .lock()
            .unwrap()
            .as_ref()
            .map(|reason| anyhow::anyhow!(reason.clone()))
    }
}

#[async_trait]
impl ContextRepository for MockContextRepository {
    async fn upsert_source(&self, source: &ContextSource) -> Result<()> {
        let mut sources = self.sources.lock().unwrap();
        match sources.iter_mut().find(|s| s.id() == source.id()) {
            // Unlike the real adapter (migration 0044), this also overwrites kind and owner.
            Some(existing) => *existing = source.clone(),
            None => sources.push(source.clone()),
        }
        Ok(())
    }

    async fn get_source(&self, id: &str, scope: &ProfileScope) -> Result<Option<ContextSource>> {
        if let Some(e) = self.source_read_failure() {
            return Err(e);
        }
        Ok(self
            .sources
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id() == id && source_is_visible(s, scope))
            .cloned())
    }

    async fn list_sources(&self, scope: &ProfileScope) -> Result<Vec<ContextSource>> {
        if let Some(e) = self.source_read_failure() {
            return Err(e);
        }
        Ok(self
            .sources
            .lock()
            .unwrap()
            .iter()
            .filter(|s| source_is_visible(s, scope))
            .cloned()
            .collect())
    }

    async fn disconnect_source(&self, id: &str, scope: &ProfileScope) -> Result<u64> {
        let mut sources = self.sources.lock().unwrap();
        if !sources
            .iter()
            .any(|s| s.id() == id && source_is_visible(s, scope))
        {
            return Ok(0);
        }
        sources.retain(|s| s.id() != id);
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|i| i.source_id() != id);
        Ok((before - items.len()) as u64)
    }

    async fn save_item(&self, item: &ContextItem) -> Result<()> {
        if let Some(reason) = self.unwritable_items.lock().unwrap().as_ref() {
            return Err(anyhow::anyhow!(reason.clone()));
        }
        let mut items = self.items.lock().unwrap();
        match items
            .iter_mut()
            .find(|i| i.source_id() == item.source_id() && i.external_id() == item.external_id())
        {
            Some(existing) => *existing = item.clone(),
            None => items.push(item.clone()),
        }
        Ok(())
    }

    async fn recent_items(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<ContextItem>> {
        let items = self.items.lock().unwrap();
        let mut visible: Vec<ContextItem> = items
            .iter()
            .filter(|i| item_is_visible(i, scope))
            .cloned()
            .collect();
        visible.sort_by(|a, b| b.occurred_at().cmp(&a.occurred_at()));
        visible.truncate(limit);
        Ok(visible)
    }

    async fn search_items(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ContextItem>> {
        if keywords.is_empty() {
            return Ok(vec![]);
        }
        let items = self.items.lock().unwrap();
        let mut hits: Vec<ContextItem> = items
            .iter()
            .filter(|i| item_is_visible(i, scope))
            .filter(|i| {
                let haystack = format!("{} {}", i.title(), i.body()).to_lowercase();
                keywords
                    .iter()
                    .any(|k| haystack.contains(&k.to_lowercase()))
            })
            .cloned()
            .collect();
        hits.sort_by(|a, b| b.occurred_at().cmp(&a.occurred_at()));
        hits.truncate(limit);
        Ok(hits)
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<(ContextItem, f32)>> {
        let items = self.items.lock().unwrap();
        let mut scored: Vec<(ContextItem, f32)> = items
            .iter()
            .filter(|i| item_is_visible(i, scope))
            .filter_map(|i| {
                let embedding = i.embedding()?;
                Some((i.clone(), cosine(query_embedding, embedding)))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    async fn search_unembedded(&self, limit: usize) -> Result<Vec<ContextItem>> {
        Ok(self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.embedding().is_none())
            .take(limit)
            .cloned()
            .collect())
    }

    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id() == id) {
            *item = item.clone().with_embedding(embedding.to_vec());
        }
        Ok(())
    }

    async fn count_for_profile(&self, profile_id: &str) -> Result<u64> {
        Ok(self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.profile_id() == profile_id)
            .count() as u64)
    }

    async fn purge_expired(&self, retention: &ContextRetention, now: DateTime<Utc>) -> Result<u64> {
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|item| {
            match retention
                .window_for(item.source_kind(), item.sensitivity())
                .cutoff(now)
            {
                Some(cutoff) => item.occurred_at() >= cutoff,
                None => true,
            }
        });
        Ok((before - items.len()) as u64)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}
