//! In-memory mock implementation of `PromptExtraRepository`.

use crate::user_data::domain::prompt_extra::PromptExtra;
use crate::user_data::ports::prompt_extra::PromptExtraRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct MockPromptExtraRepository {
    extras: Arc<RwLock<Vec<PromptExtra>>>,
}

impl MockPromptExtraRepository {
    pub fn new() -> Self {
        Self {
            extras: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for MockPromptExtraRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn in_order(mut v: Vec<PromptExtra>) -> Vec<PromptExtra> {
    v.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.key.cmp(&b.key)));
    v
}

#[async_trait]
impl PromptExtraRepository for MockPromptExtraRepository {
    async fn list_active(&self) -> Result<Vec<PromptExtra>> {
        let store = self.extras.read().await;
        Ok(in_order(
            store
                .iter()
                .filter(|e| e.active && e.profile_id.is_none())
                .cloned()
                .collect(),
        ))
    }

    async fn list_all(&self) -> Result<Vec<PromptExtra>> {
        let store = self.extras.read().await;
        Ok(in_order(
            store
                .iter()
                .filter(|e| e.profile_id.is_none())
                .cloned()
                .collect(),
        ))
    }

    async fn list_for_profile(&self, profile_id: &str) -> Result<Vec<PromptExtra>> {
        let store = self.extras.read().await;
        Ok(in_order(
            store
                .iter()
                .filter(|e| e.profile_id.as_deref() == Some(profile_id))
                .cloned()
                .collect(),
        ))
    }

    async fn list_all_members(&self) -> Result<Vec<PromptExtra>> {
        let store = self.extras.read().await;
        Ok(in_order(
            store
                .iter()
                .filter(|e| e.profile_id.is_some())
                .cloned()
                .collect(),
        ))
    }

    async fn upsert(&self, extra: &PromptExtra) -> Result<()> {
        self.upsert_all(std::slice::from_ref(extra)).await
    }

    async fn upsert_all(&self, extras: &[PromptExtra]) -> Result<()> {
        let mut store = self.extras.write().await;
        for extra in extras {
            match store
                .iter_mut()
                .find(|e| e.key == extra.key && e.profile_id == extra.profile_id)
            {
                Some(existing) => *existing = extra.clone(),
                None => store.push(extra.clone()),
            }
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.extras
            .write()
            .await
            .retain(|e| !(e.key == key && e.profile_id.is_none()));
        Ok(())
    }

    async fn delete_for_profile(&self, profile_id: &str, key: &str) -> Result<()> {
        self.extras
            .write()
            .await
            .retain(|e| !(e.key == key && e.profile_id.as_deref() == Some(profile_id)));
        Ok(())
    }

    async fn delete_all_for_profile(&self, profile_id: &str) -> Result<u64> {
        let mut store = self.extras.write().await;
        let before = store.len();
        store.retain(|e| e.profile_id.as_deref() != Some(profile_id));
        Ok((before - store.len()) as u64)
    }
}
