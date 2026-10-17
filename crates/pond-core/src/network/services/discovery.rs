//! Keeps the pond's `_pond._tcp` advertisement in step with its claim, so `setup=1` goes away once
//! a phone pairs and comes back after a factory reset (`docs/architecture/pond-networking.md`).

use std::sync::Arc;
use std::time::Duration;

use crate::network::domain::pond_advert::{AdvertiseError, PondAdvert, SetupAdvert};
use crate::network::domain::setup_record::SetupRecord;
use crate::network::ports::service_advertiser::ServiceAdvertiser;
use crate::network::ports::setup_code_store::SetupCodeStore;
use crate::network::services::setup_claim::SetupClaimService;

/// How often the advert re-reads the claim, to see a `setup-code --rearm` made by another process
/// and to retry a publish the responder refused; a claim made here is seen at once.
pub const ADVERT_RECHECK_INTERVAL: Duration = Duration::from_secs(60);

pub struct DiscoveryService {
    advertiser: Arc<dyn ServiceAdvertiser>,
    store: Arc<dyn SetupCodeStore>,
    claims: Arc<SetupClaimService>,
    base: PondAdvert,
}

impl DiscoveryService {
    /// `base` is the pond's advert without its setup state, which is read from `store`.
    pub fn new(
        advertiser: Arc<dyn ServiceAdvertiser>,
        store: Arc<dyn SetupCodeStore>,
        claims: Arc<SetupClaimService>,
        base: PondAdvert,
    ) -> Self {
        Self {
            advertiser,
            store,
            claims,
            base: base.with_setup(None),
        }
    }

    pub fn advertiser(&self) -> Arc<dyn ServiceAdvertiser> {
        self.advertiser.clone()
    }

    fn advert_for(&self, record: Option<&SetupRecord>) -> PondAdvert {
        self.base
            .clone()
            .with_setup(record.map(SetupAdvert::from_record))
    }

    /// The advert as the store reads now. `None` when it is unreadable and something is already
    /// published: keep that, since the store refuses the card anyway while it cannot be read.
    async fn current(&self, published: Option<&PondAdvert>) -> Option<PondAdvert> {
        match self.store.load().await {
            Ok(record) => Some(self.advert_for(record.as_ref())),
            Err(error) if published.is_some() => {
                tracing::debug!(%error, "setup record unreadable; mDNS setup state unchanged");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "setup record unreadable; advertising without its id");
                Some(self.base.clone())
            }
        }
    }

    /// Publishes the advert the store reads now. An error leaves LAN discovery off until
    /// [`Self::run`] lands a later publish.
    pub async fn publish_current(&self) -> Result<PondAdvert, AdvertiseError> {
        let advert = self
            .current(None)
            .await
            .unwrap_or_else(|| self.base.clone());
        self.advertiser.publish(&advert).await?;
        tracing::info!(
            instance = %advert.instance(),
            port = advert.port,
            setup_open = advert.setup_open(),
            responder = self.advertiser.responder(),
            "_pond._tcp.local. registered; phones on the same LAN can now discover this hub"
        );
        Ok(advert)
    }

    /// Never returns. Re-publishes on every claim this process latches and every `interval`, and
    /// only when the advert changed or the last publish failed.
    pub async fn run(self, mut published: Option<PondAdvert>, interval: Duration) {
        let mut last_error: Option<AdvertiseError> = None;
        loop {
            tokio::select! {
                _ = self.claims.claim_latched() => {}
                _ = tokio::time::sleep(interval) => {}
            }
            let Some(current) = self.current(published.as_ref()).await else {
                continue;
            };
            if published.as_ref() == Some(&current) {
                continue;
            }
            match self.advertiser.publish(&current).await {
                Ok(()) => {
                    tracing::info!(
                        setup_open = current.setup_open(),
                        responder = self.advertiser.responder(),
                        recovered = last_error.take().is_some(),
                        "mDNS: published _pond._tcp with the current claim state"
                    );
                    published = Some(current);
                }
                // Once per distinct error: a responder that stays down would warn every interval.
                Err(error) if last_error.as_ref() == Some(&error) => {
                    tracing::debug!(%error, "mDNS: publish still failing");
                }
                Err(error) => {
                    tracing::warn!(%error, "mDNS: could not publish _pond._tcp; retrying");
                    last_error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::network::mocks::mock_service_advertiser::MockServiceAdvertiser;
    use crate::network::mocks::mock_setup_code_store::MockSetupCodeStore;

    const INTERVAL: Duration = ADVERT_RECHECK_INTERVAL;

    struct Fixture {
        advertiser: Arc<MockServiceAdvertiser>,
        store: Arc<MockSetupCodeStore>,
        claims: Arc<SetupClaimService>,
        service: DiscoveryService,
    }

    fn fixture(store: MockSetupCodeStore) -> Fixture {
        let advertiser = Arc::new(MockServiceAdvertiser::new());
        let store = Arc::new(store);
        let claims = Arc::new(SetupClaimService::new(store.clone()));
        let service = DiscoveryService::new(
            advertiser.clone(),
            store.clone(),
            claims.clone(),
            PondAdvert::new("J3RRA4", 4543, "0.1.0", "sha256/x"),
        );
        Fixture {
            advertiser,
            store,
            claims,
            service,
        }
    }

    /// Lets the spawned loop run up to `by` of paused time.
    async fn advance(by: Duration) {
        tokio::time::sleep(by).await;
    }

    #[tokio::test]
    async fn an_unclaimed_pond_publishes_its_id_and_opens_setup() {
        let f = fixture(MockSetupCodeStore::unclaimed());
        let advert = f.service.publish_current().await.unwrap();

        assert_eq!(f.advertiser.last(), Some(advert.clone()));
        assert!(advert.setup_open());
        assert_eq!(advert.setup.as_ref().unwrap().id, "7f3a");
        assert_eq!(advert.instance(), "Pond Hub @ J3RRA4");
    }

    #[tokio::test]
    async fn a_pond_with_no_card_publishes_no_id() {
        let f = fixture(MockSetupCodeStore::new());
        let advert = f.service.publish_current().await.unwrap();
        assert_eq!(advert.setup, None);
    }

    #[tokio::test]
    async fn an_unreadable_store_at_startup_still_advertises_the_pond() {
        let store = MockSetupCodeStore::unclaimed();
        store.set_failing(true);
        let f = fixture(store);
        let advert = f.service.publish_current().await.unwrap();
        assert_eq!(advert.setup, None, "no id it could not read");
    }

    #[tokio::test(start_paused = true)]
    async fn a_claim_closes_setup_at_once_under_the_same_name() {
        let f = fixture(MockSetupCodeStore::unclaimed());
        let opened = f.service.publish_current().await.unwrap();
        tokio::spawn(f.service.run(Some(opened.clone()), INTERVAL));

        f.claims.record_claim(Utc::now(), "phone-a").await.unwrap();
        advance(Duration::from_millis(1)).await;

        let closed = f.advertiser.last().unwrap();
        assert!(
            !closed.setup_open(),
            "a claimed pond must not invite a card"
        );
        assert_eq!(closed.setup.as_ref().unwrap().id, "7f3a", "the id stays");
        assert_eq!(
            (closed.instance(), closed.port),
            (opened.instance(), opened.port)
        );
        assert_eq!(f.advertiser.attempts(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_rearm_by_another_process_reopens_setup_after_the_interval() {
        let store = MockSetupCodeStore::unclaimed();
        store.set_claimed_at(Some(Utc.with_ymd_and_hms(2026, 10, 16, 10, 0, 0).unwrap()));
        let f = fixture(store);
        let closed = f.service.publish_current().await.unwrap();
        assert!(!closed.setup_open());
        tokio::spawn(f.service.run(Some(closed), INTERVAL));

        f.store.set_claimed_at(None);
        advance(INTERVAL / 2).await;
        assert_eq!(f.advertiser.attempts(), 1, "not before the interval");

        advance(INTERVAL).await;
        assert!(f.advertiser.last().unwrap().setup_open());
        assert_eq!(f.advertiser.attempts(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchanged_claim_is_not_republished() {
        let f = fixture(MockSetupCodeStore::unclaimed());
        let opened = f.service.publish_current().await.unwrap();
        tokio::spawn(f.service.run(Some(opened), INTERVAL));

        advance(INTERVAL * 5).await;
        assert_eq!(f.advertiser.attempts(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_publish_is_retried_until_the_responder_answers() {
        let f = fixture(MockSetupCodeStore::unclaimed());
        f.advertiser.set_failing(true);
        assert!(f.service.publish_current().await.is_err());
        tokio::spawn(f.service.run(None, INTERVAL));

        advance(INTERVAL * 2 + Duration::from_secs(1)).await;
        assert_eq!(f.advertiser.attempts(), 3, "retried every interval");
        assert!(f.advertiser.published().is_empty());

        f.advertiser.set_failing(false);
        advance(INTERVAL).await;
        assert!(f.advertiser.last().unwrap().setup_open());

        advance(INTERVAL * 3).await;
        assert_eq!(f.advertiser.attempts(), 4, "and only until it lands");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_store_keeps_what_is_published() {
        let f = fixture(MockSetupCodeStore::unclaimed());
        let opened = f.service.publish_current().await.unwrap();
        tokio::spawn(f.service.run(Some(opened), INTERVAL));

        f.store.set_failing(true);
        advance(INTERVAL * 3).await;
        assert_eq!(f.advertiser.attempts(), 1);
        assert!(f.advertiser.last().unwrap().setup_open());
    }
}
