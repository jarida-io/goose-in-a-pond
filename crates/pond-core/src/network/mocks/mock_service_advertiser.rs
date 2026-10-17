//! Records what the pond published, standing in for the host's mDNS responder.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::network::domain::pond_advert::{AdvertiseError, PondAdvert};
use crate::network::ports::service_advertiser::ServiceAdvertiser;

#[derive(Default)]
pub struct MockServiceAdvertiser {
    published: Mutex<Vec<PondAdvert>>,
    failing: AtomicBool,
    attempts: AtomicUsize,
    withdrawals: AtomicUsize,
}

impl MockServiceAdvertiser {
    pub const RESPONDER: &'static str = "mock";

    pub fn new() -> Self {
        Self::default()
    }

    /// Every advert accepted, oldest first.
    pub fn published(&self) -> Vec<PondAdvert> {
        self.published.lock().unwrap().clone()
    }

    pub fn last(&self) -> Option<PondAdvert> {
        self.published.lock().unwrap().last().cloned()
    }

    /// Calls to `publish`, accepted or not.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    pub fn withdrawals(&self) -> usize {
        self.withdrawals.load(Ordering::SeqCst)
    }

    /// Makes `publish` fail as a responder that is not running would.
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }
}

#[async_trait]
impl ServiceAdvertiser for MockServiceAdvertiser {
    async fn publish(&self, advert: &PondAdvert) -> Result<(), AdvertiseError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            return Err(AdvertiseError::Unavailable(
                "the mock responder is down".into(),
            ));
        }
        self.published.lock().unwrap().push(advert.clone());
        Ok(())
    }

    async fn withdraw(&self) {
        self.withdrawals.fetch_add(1, Ordering::SeqCst);
    }

    fn responder(&self) -> &'static str {
        Self::RESPONDER
    }
}
