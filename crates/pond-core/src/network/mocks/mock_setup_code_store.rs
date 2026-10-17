//! In-memory [`SetupCodeStore`] with a fixed code, standing in for the file store.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::network::domain::discriminator::Discriminator;
use crate::network::domain::setup_code::{SetupCode, SETUP_CODE_RANDOM_DIGITS};
use crate::network::domain::setup_record::SetupRecord;
use crate::network::ports::setup_code_store::SetupCodeStore;

pub struct MockSetupCodeStore {
    record: Mutex<Option<SetupRecord>>,
    failing: AtomicBool,
}

impl MockSetupCodeStore {
    /// The classic Luhn vector, so a test can name the code it expects: `79927398713`.
    pub const DIGITS: [u8; SETUP_CODE_RANDOM_DIGITS] = [7, 9, 9, 2, 7, 3, 9, 8, 7, 1];
    pub const DISCRIMINATOR: Discriminator = Discriminator::new(0x7f3a);

    /// A store no code was ever made in.
    pub fn new() -> Self {
        Self {
            record: Mutex::new(None),
            failing: AtomicBool::new(false),
        }
    }

    /// A store holding [`Self::fixed_record`], unclaimed.
    pub fn unclaimed() -> Self {
        let store = Self::new();
        store.set_record(Some(Self::fixed_record()));
        store
    }

    /// What `create_if_missing` makes: [`Self::DIGITS`] and [`Self::DISCRIMINATOR`].
    pub fn fixed_record() -> SetupRecord {
        SetupRecord::new(
            SetupCode::from_digits(Self::DIGITS).unwrap(),
            Self::DISCRIMINATOR,
            Utc.with_ymd_and_hms(2026, 10, 16, 9, 0, 0).unwrap(),
        )
    }

    pub fn record(&self) -> Option<SetupRecord> {
        self.record.lock().unwrap().clone()
    }

    /// Replaces the record behind the service's back, as another process or a reset would.
    pub fn set_record(&self, record: Option<SetupRecord>) {
        *self.record.lock().unwrap() = record;
    }

    /// Sets `claimed_at` on the held record without the store's first-claim rule.
    pub fn set_claimed_at(&self, at: Option<DateTime<Utc>>) {
        if let Some(record) = self.record.lock().unwrap().as_mut() {
            record.claimed_at = at;
        }
    }

    /// Makes every call fail, as an unreadable file would.
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    fn check(&self) -> Result<()> {
        if self.failing.load(Ordering::SeqCst) {
            anyhow::bail!("the mock setup code store is failing");
        }
        Ok(())
    }
}

impl Default for MockSetupCodeStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SetupCodeStore for MockSetupCodeStore {
    async fn load(&self) -> Result<Option<SetupRecord>> {
        self.check()?;
        Ok(self.record())
    }

    async fn create_if_missing(&self) -> Result<SetupRecord> {
        self.check()?;
        let mut record = self.record.lock().unwrap();
        Ok(record.get_or_insert_with(Self::fixed_record).clone())
    }

    async fn mark_claimed(&self, at: DateTime<Utc>, by: Option<&str>) -> Result<SetupRecord> {
        self.check()?;
        let mut guard = self.record.lock().unwrap();
        let record = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no setup code to mark claimed"))?;
        if record.claimed_at.is_none() {
            record.claimed_at = Some(at);
            record.claimed_by = by.map(str::to_string);
        }
        Ok(record.clone())
    }

    async fn rearm(&self) -> Result<SetupRecord> {
        self.check()?;
        let mut guard = self.record.lock().unwrap();
        let record = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no setup code to re-arm"))?;
        record.claimed_at = None;
        record.claimed_by = None;
        Ok(record.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn it_keeps_the_store_contract() {
        let store = MockSetupCodeStore::new();
        assert_eq!(store.load().await.unwrap(), None);
        assert!(store.mark_claimed(Utc::now(), None).await.is_err());
        assert!(store.rearm().await.is_err());

        let made = store.create_if_missing().await.unwrap();
        assert_eq!(made.code.digits(), "79927398713");
        assert_eq!(store.create_if_missing().await.unwrap(), made);

        let first = Utc.with_ymd_and_hms(2026, 10, 16, 10, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 10, 16, 11, 0, 0).unwrap();
        let claimed = store.mark_claimed(first, Some("phone-a")).await.unwrap();
        assert_eq!(
            (claimed.claimed_at, claimed.claimed_by.as_deref()),
            (Some(first), Some("phone-a"))
        );
        let again = store.mark_claimed(later, Some("phone-b")).await.unwrap();
        assert_eq!(
            (again.claimed_at, again.claimed_by.as_deref()),
            (Some(first), Some("phone-a"))
        );

        let rearmed = store.rearm().await.unwrap();
        assert_eq!((rearmed.claimed_at, rearmed.claimed_by), (None, None));
        assert_eq!(rearmed.code, made.code);

        store.set_failing(true);
        assert!(store.load().await.is_err());
    }
}
