use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::security::domain::admin_card::AdminPassword;
use crate::security::ports::admin_card::{AdminCardStore, AdminDeviceKeys, CardCheck};

/// An in-memory card: `None` until [`MockAdminCardStore::issue`], or unreadable on demand.
#[derive(Default)]
pub struct MockAdminCardStore {
    card: Mutex<Option<(String, AdminPassword)>>,
    unreadable: Mutex<bool>,
    checks: Mutex<u32>,
}

impl MockAdminCardStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a card in force, replacing any other, as a rotation does.
    pub fn issue(&self, card_id: &str, password: AdminPassword) {
        *self.card.lock().unwrap() = Some((card_id.to_string(), password));
    }

    pub fn set_unreadable(&self, unreadable: bool) {
        *self.unreadable.lock().unwrap() = unreadable;
    }

    /// How many passwords were checked against the card, so a test can see a guess was spared.
    pub fn checks(&self) -> u32 {
        *self.checks.lock().unwrap()
    }

    fn readable(&self) -> Result<()> {
        if *self.unreadable.lock().unwrap() {
            return Err(anyhow!("admin card unreadable"));
        }
        Ok(())
    }
}

#[async_trait]
impl AdminCardStore for MockAdminCardStore {
    async fn current(&self) -> Result<Option<String>> {
        self.readable()?;
        Ok(self.card.lock().unwrap().as_ref().map(|(id, _)| id.clone()))
    }

    async fn check(&self, password: &AdminPassword) -> Result<CardCheck> {
        self.readable()?;
        *self.checks.lock().unwrap() += 1;
        Ok(match self.card.lock().unwrap().as_ref() {
            None => CardCheck::Missing,
            Some((id, held)) if held == password => CardCheck::Matches {
                card_id: id.clone(),
            },
            Some(_) => CardCheck::Mismatch,
        })
    }
}

/// In-memory device keys: `key-1`, `key-2`, ... so a test can tell an old key from a new one.
#[derive(Default)]
pub struct MockAdminDeviceKeys {
    keys: Mutex<HashMap<String, String>>,
    issued: Mutex<u32>,
}

impl MockAdminDeviceKeys {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AdminDeviceKeys for MockAdminDeviceKeys {
    async fn issue(&self, device: &str) -> Result<String> {
        let mut issued = self.issued.lock().unwrap();
        *issued += 1;
        let key = format!("key-{issued}");
        self.keys
            .lock()
            .unwrap()
            .insert(device.to_string(), key.clone());
        Ok(key)
    }

    async fn check(&self, device: &str, key: &str) -> Result<bool> {
        Ok(self.keys.lock().unwrap().get(device).map(String::as_str) == Some(key))
    }

    async fn holds(&self, device: &str) -> Result<bool> {
        Ok(self.keys.lock().unwrap().contains_key(device))
    }

    async fn revoke(&self, device: &str) -> Result<bool> {
        Ok(self.keys.lock().unwrap().remove(device).is_some())
    }

    async fn revoke_all(&self) -> Result<u64> {
        let mut keys = self.keys.lock().unwrap();
        let n = keys.len() as u64;
        keys.clear();
        Ok(n)
    }
}
