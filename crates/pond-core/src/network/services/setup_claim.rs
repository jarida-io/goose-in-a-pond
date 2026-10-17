//! Which secret a pond accepts when, and how fast its setup code may be guessed.

use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify};

use crate::network::domain::claim::ClaimRefusal;
use crate::network::domain::setup_code::SetupCode;
use crate::network::domain::setup_record::{ClaimState, SetupRecord};
use crate::network::ports::setup_code_store::SetupCodeStore;
use crate::network::services::setup_guess_throttle::SetupGuessThrottle;

/// Guards the printed setup code. The claim latch is read from the store on every call and never
/// cached: a cached flag would make factory reset a one-way door (PAI-2 section 3.7).
pub struct SetupClaimService {
    store: Arc<dyn SetupCodeStore>,
    // Held across the store calls so admission, and a claim's check-then-mark, are each one step.
    throttle: Mutex<SetupGuessThrottle>,
    // `notify_one` keeps a permit, so a claim landing while nobody waits is still seen.
    claimed: Notify,
}

impl SetupClaimService {
    pub fn new(store: Arc<dyn SetupCodeStore>) -> Self {
        Self {
            store,
            throttle: Mutex::new(SetupGuessThrottle::new()),
            claimed: Notify::new(),
        }
    }

    /// Charged as a failure up front and refunded by [`Self::record_claim`], as `verify_pake`
    /// spends its budget before checking a tag, so attempts started together cannot all slip in.
    pub async fn admit_setup_attempt(&self, now: Instant) -> Result<SetupCode, ClaimRefusal> {
        let mut throttle = self.throttle.lock().await;
        let record = match self.store.load().await {
            Ok(Some(record)) => record,
            Ok(None) => return Err(ClaimRefusal::SetupCodeUnavailable),
            Err(error) => {
                tracing::warn!(%error, "setup code store unreadable; refusing the setup code");
                return Err(ClaimRefusal::SetupCodeUnavailable);
            }
        };
        if record.claim_state().is_claimed() {
            return Err(ClaimRefusal::SetupCodeUnavailable);
        }
        throttle
            .check(now)
            .map_err(|retry_after| ClaimRefusal::SetupCodeCooling { retry_after })?;
        throttle.record_failure(now);
        Ok(record.code)
    }

    /// Call BEFORE the first device's tokens are returned, so a crash leaves a claimed pond, never
    /// a paired phone beside a live card. `device` is the one those tokens will be issued to.
    /// `SetupCodeUnavailable`: another device won the claim.
    pub async fn record_claim(
        &self,
        at: DateTime<Utc>,
        device: &str,
    ) -> anyhow::Result<SetupRecord> {
        let mut throttle = self.throttle.lock().await;
        match self.store.load().await? {
            Some(record) if !record.claim_state().is_claimed() => {}
            _ => return Err(ClaimRefusal::SetupCodeUnavailable.into()),
        }
        let record = self.store.mark_claimed(at, Some(device)).await?;
        throttle.record_success();
        self.claimed.notify_one();
        tracing::info!(claimed_at = %at, "pond claimed with its setup code");
        Ok(record)
    }

    /// Any first pairing claims the pond, so a phone paired with a pairing code closes the card
    /// too. `device` is the one the pairing issues its tokens to. `Ok(false)` when there was
    /// nothing to latch: no record, or already claimed.
    pub async fn latch_claim(&self, at: DateTime<Utc>, device: &str) -> anyhow::Result<bool> {
        let _throttle = self.throttle.lock().await;
        match self.store.load().await? {
            Some(record) if !record.claim_state().is_claimed() => {}
            _ => return Ok(false),
        }
        self.store.mark_claimed(at, Some(device)).await?;
        self.claimed.notify_one();
        tracing::info!(claimed_at = %at, "pond claimed by a pairing");
        Ok(true)
    }

    /// Resolves once a claim this service latched lands, for whatever advertises the claim state.
    /// A re-arm from another process is not signalled; readers poll for that.
    pub async fn claim_latched(&self) {
        self.claimed.notified().await;
    }

    /// Unclaimed when no code was ever made: nothing has latched yet.
    pub async fn claim_state(&self) -> anyhow::Result<ClaimState> {
        Ok(self
            .store
            .load()
            .await?
            .map_or(ClaimState::Unclaimed, |record| record.claim_state()))
    }

    /// The device whose pairing latched the claim; `None` when nothing latched or nobody was
    /// recorded (a record written before `claimed_by`, a pond created claimed).
    pub async fn claimed_by(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .store
            .load()
            .await?
            .filter(|record| record.claim_state().is_claimed())
            .and_then(|record| record.claimed_by))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::TimeZone;

    use super::*;
    use crate::network::domain::claim::RetryAfter;
    use crate::network::mocks::mock_setup_code_store::MockSetupCodeStore;
    use crate::network::services::setup_guess_throttle::{FIRST_SETUP_DELAY, FREE_SETUP_FAILURES};

    fn service(store: &Arc<MockSetupCodeStore>) -> SetupClaimService {
        SetupClaimService::new(store.clone())
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, hour, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn an_unclaimed_pond_admits_its_setup_code() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        let code = service.admit_setup_attempt(Instant::now()).await.unwrap();
        assert_eq!(code, MockSetupCodeStore::fixed_record().code);
        assert_eq!(service.claim_state().await.unwrap(), ClaimState::Unclaimed);
    }

    #[tokio::test]
    async fn a_claimed_pond_refuses_its_setup_code_as_unavailable() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        store.set_claimed_at(Some(at(10)));
        let refusal = service(&store)
            .admit_setup_attempt(Instant::now())
            .await
            .unwrap_err();
        assert_eq!(refusal, ClaimRefusal::SetupCodeUnavailable);
        assert_eq!(refusal.code(), "setup_code_unavailable");
    }

    #[tokio::test]
    async fn a_pond_with_no_code_refuses_and_reads_as_unclaimed() {
        let store = Arc::new(MockSetupCodeStore::new());
        let service = service(&store);
        assert_eq!(
            service.admit_setup_attempt(Instant::now()).await,
            Err(ClaimRefusal::SetupCodeUnavailable)
        );
        assert_eq!(service.claim_state().await.unwrap(), ClaimState::Unclaimed);
    }

    #[tokio::test]
    async fn an_unreadable_store_refuses_rather_than_admits() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        store.set_failing(true);
        let service = service(&store);
        assert_eq!(
            service.admit_setup_attempt(Instant::now()).await,
            Err(ClaimRefusal::SetupCodeUnavailable)
        );
        assert!(service.claim_state().await.is_err());
        assert!(service.record_claim(at(10), "phone").await.is_err());
        assert!(service.claimed_by().await.is_err());
    }

    #[tokio::test]
    async fn the_latch_is_read_live_on_every_attempt() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        let now = Instant::now();
        assert!(service.admit_setup_attempt(now).await.is_ok());

        store.set_claimed_at(Some(at(10)));
        assert_eq!(
            service.admit_setup_attempt(now).await,
            Err(ClaimRefusal::SetupCodeUnavailable)
        );
        assert_eq!(
            service.claim_state().await.unwrap(),
            ClaimState::Claimed { at: at(10) }
        );

        store.set_claimed_at(None);
        assert!(
            service.admit_setup_attempt(now).await.is_ok(),
            "a cleared latch must admit again on the very next attempt"
        );
    }

    #[tokio::test]
    async fn a_claim_latches_and_a_rearm_readmits_the_same_code() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        let now = Instant::now();
        let code = service.admit_setup_attempt(now).await.unwrap();

        let claimed = service.record_claim(at(10), "card-phone").await.unwrap();
        assert_eq!(claimed.claimed_at, Some(at(10)));
        assert_eq!(
            service.claimed_by().await.unwrap().as_deref(),
            Some("card-phone")
        );
        assert_eq!(
            service.admit_setup_attempt(now).await,
            Err(ClaimRefusal::SetupCodeUnavailable)
        );

        store.rearm().await.unwrap();
        assert_eq!(service.admit_setup_attempt(now).await.unwrap(), code);
        assert_eq!(service.claimed_by().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_second_claim_is_refused_and_keeps_the_first_timestamp() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        let now = Instant::now();
        service.admit_setup_attempt(now).await.unwrap();
        service.admit_setup_attempt(now).await.unwrap();

        service.record_claim(at(10), "first-phone").await.unwrap();
        let err = service
            .record_claim(at(11), "second-phone")
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<ClaimRefusal>(),
            Some(&ClaimRefusal::SetupCodeUnavailable)
        );
        let record = store.record().unwrap();
        assert_eq!(record.claimed_at, Some(at(10)));
        assert_eq!(record.claimed_by.as_deref(), Some("first-phone"));
    }

    #[tokio::test]
    async fn a_claim_on_a_pond_with_no_code_is_refused() {
        let store = Arc::new(MockSetupCodeStore::new());
        let err = service(&store)
            .record_claim(at(10), "phone")
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<ClaimRefusal>(),
            Some(&ClaimRefusal::SetupCodeUnavailable)
        );
    }

    #[tokio::test]
    async fn the_sixth_attempt_cools_with_a_retry_after() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        let t0 = Instant::now();
        for attempt in 1..=FREE_SETUP_FAILURES {
            assert!(
                service.admit_setup_attempt(t0).await.is_ok(),
                "attempt {attempt} is free"
            );
        }

        let refusal = service
            .admit_setup_attempt(t0 + Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(refusal.code(), "setup_code_cooling");
        assert_eq!(
            refusal,
            ClaimRefusal::SetupCodeCooling {
                retry_after: RetryAfter(FIRST_SETUP_DELAY - Duration::from_secs(5))
            }
        );

        assert!(
            service
                .admit_setup_attempt(t0 + FIRST_SETUP_DELAY)
                .await
                .is_ok(),
            "a refusal while cooling must not have extended the wait"
        );
        assert_eq!(
            service
                .admit_setup_attempt(t0 + FIRST_SETUP_DELAY)
                .await
                .unwrap_err()
                .retry_after(),
            Some(RetryAfter(FIRST_SETUP_DELAY * 2)),
            "the admitted sixth attempt was charged, so the next wait doubled"
        );
    }

    #[tokio::test]
    async fn an_unavailable_refusal_charges_nothing() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        store.set_claimed_at(Some(at(10)));
        let service = service(&store);
        let now = Instant::now();
        for _ in 0..FREE_SETUP_FAILURES * 3 {
            assert!(service.admit_setup_attempt(now).await.is_err());
        }
        store.set_claimed_at(None);
        for attempt in 1..=FREE_SETUP_FAILURES {
            assert!(
                service.admit_setup_attempt(now).await.is_ok(),
                "attempt {attempt} should still be free"
            );
        }
    }

    #[tokio::test]
    async fn a_pairing_latches_the_claim_once_and_keeps_the_first_timestamp() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        assert!(service.latch_claim(at(10), "desktop").await.unwrap());
        assert!(!service.latch_claim(at(11), "late-phone").await.unwrap());
        let record = store.record().unwrap();
        assert_eq!(record.claimed_at, Some(at(10)));
        assert_eq!(record.claimed_by.as_deref(), Some("desktop"));
        assert_eq!(
            service.admit_setup_attempt(Instant::now()).await,
            Err(ClaimRefusal::SetupCodeUnavailable)
        );
    }

    #[tokio::test]
    async fn nobody_is_the_claimant_until_a_claim_latches_or_when_none_was_recorded() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        assert_eq!(service.claimed_by().await.unwrap(), None);

        // Created claimed, or written before `claimed_by` existed.
        store.set_claimed_at(Some(at(10)));
        assert_eq!(service.claimed_by().await.unwrap(), None);

        // A stale name on an unclaimed record names nobody.
        let mut record = MockSetupCodeStore::fixed_record();
        record.claimed_by = Some("ghost".into());
        store.set_record(Some(record));
        assert_eq!(service.claimed_by().await.unwrap(), None);

        let recordless = SetupClaimService::new(Arc::new(MockSetupCodeStore::new()));
        assert_eq!(recordless.claimed_by().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_pairing_on_a_pond_with_no_record_has_nothing_to_latch() {
        let store = Arc::new(MockSetupCodeStore::new());
        assert!(!service(&store).latch_claim(at(10), "phone").await.unwrap());
        assert_eq!(store.record(), None);
    }

    #[tokio::test]
    async fn a_latch_against_an_unreadable_store_is_an_error_not_a_silent_skip() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        store.set_failing(true);
        assert!(service(&store).latch_claim(at(10), "phone").await.is_err());
    }

    #[tokio::test]
    async fn a_claim_is_signalled_even_when_nobody_was_waiting_yet() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        service.latch_claim(at(10), "phone").await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), service.claim_latched())
            .await
            .expect("the latch must leave a permit for a later waiter");
    }

    #[tokio::test]
    async fn a_claim_refunds_the_throttle() {
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let service = service(&store);
        let now = Instant::now();
        for _ in 0..FREE_SETUP_FAILURES {
            service.admit_setup_attempt(now).await.unwrap();
        }
        assert!(service.admit_setup_attempt(now).await.is_err());

        service.record_claim(at(10), "phone").await.unwrap();
        store.rearm().await.unwrap();
        assert!(
            service.admit_setup_attempt(now).await.is_ok(),
            "the claim should have reset the wait"
        );
    }
}
