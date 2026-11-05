//! In-memory mock implementation of `LastActionRepository`.

use crate::user_data::ports::last_action::{LastActionRepository, LastDeviceAction};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct MockLastActionRepository {
    actions: RwLock<HashMap<String, LastDeviceAction>>,
}

#[async_trait]
impl LastActionRepository for MockLastActionRepository {
    async fn last_for(&self, profile_id: &str) -> Result<Option<LastDeviceAction>> {
        Ok(self.actions.read().await.get(profile_id).cloned())
    }

    async fn record(&self, profile_id: &str, action: LastDeviceAction) -> Result<()> {
        self.actions
            .write()
            .await
            .insert(profile_id.to_string(), action);
        Ok(())
    }

    async fn clear(&self, profile_id: &str) -> Result<()> {
        self.actions.write().await.remove(profile_id);
        Ok(())
    }
}
