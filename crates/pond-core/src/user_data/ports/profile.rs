//! Profile port — driven port for household member profile persistence.

use crate::user_data::domain::profile::{CreateProfileRequest, Profile, Role};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

#[async_trait]
pub trait ProfileRepository: Send + Sync {
    async fn create(&self, request: CreateProfileRequest) -> Result<Profile>;
    async fn get(&self, profile_id: &str) -> Result<Option<Profile>>;
    async fn list(&self) -> Result<Vec<Profile>>;
    async fn update_preferences(
        &self,
        profile_id: &str,
        prefs: HashMap<String, String>,
    ) -> Result<Profile>;
    /// `guest_until` is only persisted when `role` is `Guest`; ignored (stored `NULL`) otherwise.
    async fn set_role(
        &self,
        profile_id: &str,
        role: Role,
        guest_until: Option<DateTime<Utc>>,
    ) -> Result<Profile>;
    /// Makes `profile_id` the only owner, every other owner a member, in one step: the owner is
    /// whoever holds the pairing lock's main member seat. Errs, changing nothing, for no such profile.
    async fn hand_ownership_to(&self, profile_id: &str) -> Result<()>;
    async fn delete(&self, profile_id: &str) -> Result<()>;
}
