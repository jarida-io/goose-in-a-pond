//! Removes a paired phone that has not connected for the household's chosen number of days, and
//! writes down that it did. A phone in a drawer keeps a live pairing nobody is watching.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::security::ports::handshake::Handshake;
use crate::shared::domain::goose_activity::GooseActivity;
use crate::shared::ports::goose_activity::GooseActivityRepository;
use crate::user_data::ports::device_registry::DeviceRegistry;
use crate::user_data::ports::idle_phones::{IdlePhone, IdlePhones};
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::settings::SettingsRepository;

pub struct PhoneRetirement {
    idle: Arc<dyn IdlePhones>,
    handshake: Arc<dyn Handshake>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    activity: Arc<dyn GooseActivityRepository>,
    profiles: Arc<dyn ProfileRepository + Send + Sync>,
    settings: Arc<dyn SettingsRepository>,
}

impl PhoneRetirement {
    pub fn new(
        idle: Arc<dyn IdlePhones>,
        handshake: Arc<dyn Handshake>,
        registry: Arc<dyn DeviceRegistry + Send + Sync>,
        activity: Arc<dyn GooseActivityRepository>,
        profiles: Arc<dyn ProfileRepository + Send + Sync>,
        settings: Arc<dyn SettingsRepository>,
    ) -> Self {
        Self {
            idle,
            handshake,
            registry,
            activity,
            profiles,
            settings,
        }
    }

    /// Removes every phone past the limit; returns how many went.
    pub async fn pass(&self, now: DateTime<Utc>) -> usize {
        let days = match self.settings.get().await {
            Ok(settings) => settings.unseen_phone_remove_days,
            Err(error) => {
                tracing::warn!(%error, "settings unreadable; no phone is removed this pass");
                return 0;
            }
        };
        if days == 0 {
            return 0;
        }
        let idle = match self
            .idle
            .unseen_since(now - Duration::days(i64::from(days)))
            .await
        {
            Ok(idle) => idle,
            Err(error) => {
                tracing::warn!(%error, "could not list the phones that stopped connecting");
                return 0;
            }
        };
        let mut removed = 0;
        for phone in &idle {
            if self.remove(phone, days, now).await {
                removed += 1;
            }
        }
        removed
    }

    async fn remove(&self, phone: &IdlePhone, days: u32, now: DateTime<Utc>) -> bool {
        // Access first: a row gone with its sessions alive would be a phone nobody can see.
        if let Err(error) = self.handshake.revoke_device(&phone.device_id).await {
            tracing::warn!(%error, device = %phone.device_id, "could not revoke an idle phone; it stays");
            return false;
        }
        if let Err(error) = self.registry.unregister(&phone.device_id).await {
            tracing::warn!(%error, device = %phone.device_id, "an idle phone's access is revoked but its row stayed");
            return false;
        }
        tracing::info!(
            target: "giap::trace",
            kind = "phone_retired",
            device = %phone.device_id,
            unseen_days = (now - phone.last_seen).num_days(),
            limit_days = days,
            "a phone that stopped connecting was removed"
        );
        for member in self.told(phone).await {
            let entry = GooseActivity::phone_removed(&member, &phone.name, days, now);
            if let Err(error) = self.activity.add(&entry).await {
                tracing::warn!(%error, "could not write down that a phone was removed");
            }
        }
        true
    }

    /// The phone's own member, or the whole household for a phone bound to nobody.
    async fn told(&self, phone: &IdlePhone) -> Vec<String> {
        if let Some(member) = &phone.profile_id {
            return vec![member.clone()];
        }
        match self.profiles.list().await {
            Ok(members) => members.into_iter().map(|m| m.id).collect(),
            Err(error) => {
                tracing::warn!(%error, "could not list members to tell of a removed phone");
                Vec::new()
            }
        }
    }
}
