use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::security::domain::admin_card::AdminPassword;
use crate::security::ports::admin_card::{AdminCardStore, CardCheck};

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
