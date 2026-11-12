//! In-memory mock implementation of `ProfileRepository` for tests.

use crate::user_data::domain::profile::{CreateProfileRequest, Profile, Role};
use crate::user_data::ports::profile::ProfileRepository;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct MockProfileRepository {
    store: Arc<RwLock<HashMap<String, Profile>>>,
}

impl MockProfileRepository {
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockProfileRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProfileRepository for MockProfileRepository {
    async fn create(&self, request: CreateProfileRequest) -> Result<Profile> {
        let now = Utc::now();
        let mut store = self.store.write().await;
        // Mirrors `SqliteProfileRepository::create`: the first profile in an otherwise-empty
        // household is unambiguously its owner.
        let role = if store.is_empty() {
            Role::Owner
        } else {
            Role::Member
        };
        let profile = Profile {
            id: Uuid::new_v4().to_string(),
            display_name: request.display_name,
            avatar_emoji: request.avatar_emoji,
            preferences: HashMap::new(),
            role,
            guest_until: None,
            created_at: now,
            updated_at: now,
        };
        store.insert(profile.id.clone(), profile.clone());
        Ok(profile)
    }

    async fn get(&self, profile_id: &str) -> Result<Option<Profile>> {
        Ok(self.store.read().await.get(profile_id).cloned())
    }

    async fn list(&self) -> Result<Vec<Profile>> {
        let mut profiles: Vec<Profile> = self.store.read().await.values().cloned().collect();
        profiles.sort_by_key(|p| p.created_at);
        Ok(profiles)
    }

    async fn update_preferences(
        &self,
        profile_id: &str,
        prefs: HashMap<String, String>,
    ) -> Result<Profile> {
        let mut store = self.store.write().await;
        let profile = store
            .get_mut(profile_id)
            .ok_or_else(|| anyhow!("profile not found: {}", profile_id))?;
        profile.preferences = prefs;
        profile.updated_at = Utc::now();
        Ok(profile.clone())
    }

    async fn set_role(
        &self,
        profile_id: &str,
        role: Role,
        guest_until: Option<DateTime<Utc>>,
    ) -> Result<Profile> {
        let mut store = self.store.write().await;
        let profile = store
            .get_mut(profile_id)
            .ok_or_else(|| anyhow!("profile not found: {}", profile_id))?;
        profile.role = role;
        profile.guest_until = if role == Role::Guest { guest_until } else { None };
        profile.updated_at = Utc::now();
        Ok(profile.clone())
    }

    async fn delete(&self, profile_id: &str) -> Result<()> {
        self.store.write().await.remove(profile_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_get() {
        let repo = MockProfileRepository::new();
        let req = CreateProfileRequest {
            display_name: "Jerry".to_string(),
            avatar_emoji: "\u{1F986}".to_string(),
        };
        let created = repo.create(req).await.unwrap();
        assert_eq!(created.display_name, "Jerry");

        let fetched = repo.get(&created.id).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().display_name, "Jerry");
    }

    #[tokio::test]
    async fn list_returns_all() {
        let repo = MockProfileRepository::new();
        repo.create(CreateProfileRequest {
            display_name: "A".to_string(),
            avatar_emoji: "A".to_string(),
        })
        .await
        .unwrap();
        repo.create(CreateProfileRequest {
            display_name: "B".to_string(),
            avatar_emoji: "B".to_string(),
        })
        .await
        .unwrap();
        assert_eq!(repo.list().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn update_preferences() {
        let repo = MockProfileRepository::new();
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Jerry".to_string(),
                avatar_emoji: "X".to_string(),
            })
            .await
            .unwrap();
        let mut prefs = HashMap::new();
        prefs.insert("language".to_string(), "en".to_string());
        let updated = repo.update_preferences(&profile.id, prefs).await.unwrap();
        assert_eq!(
            updated.preferences.get("language").map(|s| s.as_str()),
            Some("en")
        );
    }

    #[tokio::test]
    async fn delete_removes_profile() {
        let repo = MockProfileRepository::new();
        let profile = repo
            .create(CreateProfileRequest {
                display_name: "Temp".to_string(),
                avatar_emoji: "T".to_string(),
            })
            .await
            .unwrap();
        repo.delete(&profile.id).await.unwrap();
        assert!(repo.get(&profile.id).await.unwrap().is_none());
    }
}
