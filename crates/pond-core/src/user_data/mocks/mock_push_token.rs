//! In-memory [`PushTokenRepository`] test double.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::push_token::PushToken;
use crate::user_data::ports::push_token::PushTokenRepository;

/// Thread-safe in-memory push-token store, keyed by device id.
#[derive(Default)]
pub struct MockPushTokenRepository {
    tokens: Mutex<HashMap<String, PushToken>>,
}

impl MockPushTokenRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PushTokenRepository for MockPushTokenRepository {
    async fn upsert(&self, token: PushToken) -> Result<()> {
        self.tokens
            .lock()
            .unwrap()
            .insert(token.device_id.clone(), token);
        Ok(())
    }

    async fn get(&self, device_id: &str) -> Result<Option<PushToken>> {
        Ok(self.tokens.lock().unwrap().get(device_id).cloned())
    }

    async fn list(&self) -> Result<Vec<PushToken>> {
        Ok(self.tokens.lock().unwrap().values().cloned().collect())
    }

    async fn delete(&self, device_id: &str) -> Result<()> {
        self.tokens.lock().unwrap().remove(device_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::push_token::PushPlatform;

    fn tok(device: &str) -> PushToken {
        PushToken {
            device_id: device.into(),
            token: "abc123".into(),
            platform: PushPlatform::Expo,
            updated_at: "2026-06-29T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn upsert_get_list_delete_roundtrip() {
        let repo = MockPushTokenRepository::new();
        assert!(repo.get("d1").await.unwrap().is_none());

        repo.upsert(tok("d1")).await.unwrap();
        assert_eq!(repo.get("d1").await.unwrap().unwrap().token, "abc123");

        // Upsert replaces (one token per device).
        let mut t2 = tok("d1");
        t2.token = "xyz789".into();
        repo.upsert(t2).await.unwrap();
        assert_eq!(repo.get("d1").await.unwrap().unwrap().token, "xyz789");
        assert_eq!(repo.list().await.unwrap().len(), 1);

        repo.delete("d1").await.unwrap();
        assert!(repo.get("d1").await.unwrap().is_none());
    }
}
