//! Who may administer the pond right now: a member's device that presented the admin card or its
//! own key, for [`ADMIN_IDLE`] after its last admin request. Held in memory, so a restart locks
//! everyone.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::network::domain::claim::RetryAfter;
use crate::network::services::setup_guess_throttle::SetupGuessThrottle;
use crate::security::domain::admin_card::AdminPassword;
use crate::security::ports::admin_card::{AdminCardStore, AdminDeviceKeys, CardCheck};

/// How long an unlocked device stays unlocked without an admin request.
pub const ADMIN_IDLE: Duration = Duration::from_secs(15 * 60);

/// Why an unlock was refused. `Malformed` and `CardMissing` spend no guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockRefusal {
    Malformed,
    Rejected,
    /// The key is not this device's live one: revoked, replaced, or the card was rotated.
    KeyRejected,
    CardMissing,
    Cooling(RetryAfter),
    /// The card could not be read; nobody is unlocked while that lasts.
    Unavailable(String),
}

impl UnlockRefusal {
    /// The wire `error`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed => "admin_password_malformed",
            Self::Rejected => "admin_password_rejected",
            Self::KeyRejected => "admin_key_rejected",
            Self::CardMissing => "admin_card_missing",
            Self::Cooling(_) => "admin_cooling",
            Self::Unavailable(_) => "admin_card_unreadable",
        }
    }
}

/// Why an admin request was refused; the reason is for the log, the wire says only "locked".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locked {
    NotUnlocked,
    Expired,
    /// The card was rotated or removed since this device unlocked.
    CardChanged,
    Unavailable(String),
}

impl Locked {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotUnlocked => "not_unlocked",
            Self::Expired => "expired",
            Self::CardChanged => "card_changed",
            Self::Unavailable(_) => "card_unreadable",
        }
    }
}

/// What `GET /admin/status` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStatus {
    pub card_present: bool,
    /// Whether this device holds a live key of its own.
    pub key_held: bool,
    /// Time left before this device locks, if it is unlocked.
    pub unlocked_for: Option<Duration>,
    pub retry_after: Option<RetryAfter>,
}

#[derive(Debug, Clone)]
struct Grant {
    /// The card in force when the device unlocked; `None` on a pond that has no card yet.
    card_id: Option<String>,
    last_used: Instant,
}

#[derive(Default)]
struct Ledger {
    throttle: SetupGuessThrottle,
    grants: HashMap<String, Grant>,
}

pub struct AdminElevation {
    cards: Arc<dyn AdminCardStore>,
    keys: Arc<dyn AdminDeviceKeys>,
    idle: Duration,
    // Held across the card check, so concurrent guesses are judged one at a time.
    ledger: Mutex<Ledger>,
}

impl AdminElevation {
    pub fn new(cards: Arc<dyn AdminCardStore>, keys: Arc<dyn AdminDeviceKeys>) -> Self {
        Self::with_idle(cards, keys, ADMIN_IDLE)
    }

    pub fn with_idle(
        cards: Arc<dyn AdminCardStore>,
        keys: Arc<dyn AdminDeviceKeys>,
        idle: Duration,
    ) -> Self {
        Self {
            cards,
            keys,
            idle,
            ledger: Mutex::new(Ledger::default()),
        }
    }

    pub fn idle(&self) -> Duration {
        self.idle
    }

    /// Unlock `device` with what the member typed. Returns how long it stays unlocked.
    pub async fn unlock(
        &self,
        device: &str,
        typed: &str,
        now: Instant,
    ) -> Result<Duration, UnlockRefusal> {
        let password = AdminPassword::parse(typed).map_err(|_| UnlockRefusal::Malformed)?;
        let mut ledger = self.ledger.lock().await;
        ledger.throttle.check(now).map_err(UnlockRefusal::Cooling)?;
        let check = self
            .cards
            .check(&password)
            .await
            .map_err(|e| UnlockRefusal::Unavailable(e.to_string()))?;
        match check {
            CardCheck::Missing => Err(UnlockRefusal::CardMissing),
            CardCheck::Mismatch => {
                ledger.throttle.record_failure(now);
                Err(UnlockRefusal::Rejected)
            }
            CardCheck::Matches { card_id } => {
                ledger.throttle.record_success();
                ledger.grants.insert(
                    device.to_string(),
                    Grant {
                        card_id: Some(card_id),
                        last_used: now,
                    },
                );
                Ok(self.idle)
            }
        }
    }

    /// Unlock `device` with the key the pond issued it. A wrong key is a wrong guess.
    pub async fn unlock_with_key(
        &self,
        device: &str,
        key: &str,
        now: Instant,
    ) -> Result<Duration, UnlockRefusal> {
        let mut ledger = self.ledger.lock().await;
        ledger.throttle.check(now).map_err(UnlockRefusal::Cooling)?;
        let live = self
            .keys
            .check(device, key)
            .await
            .map_err(|e| UnlockRefusal::Unavailable(e.to_string()))?;
        if !live {
            ledger.throttle.record_failure(now);
            return Err(UnlockRefusal::KeyRejected);
        }
        let card_id = self
            .cards
            .current()
            .await
            .map_err(|e| UnlockRefusal::Unavailable(e.to_string()))?;
        ledger.throttle.record_success();
        ledger.grants.insert(
            device.to_string(),
            Grant {
                card_id,
                last_used: now,
            },
        );
        Ok(self.idle)
    }

    /// A key of `device`'s own, for an unlocked device; it replaces any older one.
    pub async fn enroll(&self, device: &str, now: Instant) -> Result<String, Locked> {
        self.admit(device, now).await?;
        self.keys
            .issue(device)
            .await
            .map_err(|e| Locked::Unavailable(e.to_string()))
    }

    /// The first activation user's key, issued without an unlock: the caller has proved `device`
    /// claimed the pond and has just completed its setup.
    pub async fn issue_first_key(&self, device: &str) -> anyhow::Result<String> {
        self.keys.issue(device).await
    }

    /// End `device`'s unlock and revoke its key: it forgot My Pond, or was removed from the pond.
    pub async fn revoke_device(&self, device: &str) -> anyhow::Result<bool> {
        self.lock(device).await;
        self.keys.revoke(device).await
    }

    /// Admit an admin request from `device`, renewing its idle window.
    pub async fn admit(&self, device: &str, now: Instant) -> Result<Duration, Locked> {
        let current = self
            .cards
            .current()
            .await
            .map_err(|e| Locked::Unavailable(e.to_string()))?;
        let mut ledger = self.ledger.lock().await;
        let Some(grant) = ledger.grants.get_mut(device) else {
            return Err(Locked::NotUnlocked);
        };
        if current != grant.card_id {
            ledger.grants.remove(device);
            return Err(Locked::CardChanged);
        }
        if now.saturating_duration_since(grant.last_used) >= self.idle {
            ledger.grants.remove(device);
            return Err(Locked::Expired);
        }
        grant.last_used = now;
        Ok(self.idle)
    }

    /// End `device`'s unlock now; `true` if it was unlocked.
    pub async fn lock(&self, device: &str) -> bool {
        self.ledger.lock().await.grants.remove(device).is_some()
    }

    /// Where `device` stands, without renewing anything.
    pub async fn status(&self, device: &str, now: Instant) -> Result<AdminStatus, String> {
        let current = self.cards.current().await.map_err(|e| e.to_string())?;
        let key_held = self.keys.holds(device).await.map_err(|e| e.to_string())?;
        let ledger = self.ledger.lock().await;
        let unlocked_for = ledger.grants.get(device).and_then(|grant| {
            if current != grant.card_id {
                return None;
            }
            self.idle
                .checked_sub(now.saturating_duration_since(grant.last_used))
                .filter(|left| !left.is_zero())
        });
        Ok(AdminStatus {
            card_present: current.is_some(),
            key_held,
            unlocked_for,
            retry_after: ledger.throttle.check(now).err(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::services::setup_guess_throttle::FREE_SETUP_FAILURES;
    use crate::security::mocks::mock_admin_card::{MockAdminCardStore, MockAdminDeviceKeys};

    const CARD: &str = "0123-AJPV-ZYXW-9876";
    const WRONG: &str = "0123-AJPV-ZYXW-9877";

    fn pond_with_card() -> (Arc<MockAdminCardStore>, AdminElevation) {
        let cards = Arc::new(MockAdminCardStore::new());
        cards.issue("card-1", AdminPassword::parse(CARD).unwrap());
        let elevation = AdminElevation::new(cards.clone(), Arc::new(MockAdminDeviceKeys::new()));
        (cards, elevation)
    }

    #[tokio::test]
    async fn the_card_unlocks_only_the_device_that_presented_it() {
        let (_, admin) = pond_with_card();
        let t0 = Instant::now();
        assert_eq!(admin.unlock("phone-a", CARD, t0).await, Ok(ADMIN_IDLE));
        assert_eq!(admin.admit("phone-a", t0).await, Ok(ADMIN_IDLE));
        assert_eq!(
            admin.admit("phone-b", t0).await,
            Err(Locked::NotUnlocked),
            "an unlock belongs to one device, not to the household"
        );
    }

    #[tokio::test]
    async fn an_unlock_lapses_after_the_idle_window_and_each_request_renews_it() {
        let (_, admin) = pond_with_card();
        let t0 = Instant::now();
        admin.unlock("phone", CARD, t0).await.unwrap();
        let almost = t0 + ADMIN_IDLE - Duration::from_secs(1);
        assert!(admin.admit("phone", almost).await.is_ok());
        let later = almost + ADMIN_IDLE - Duration::from_secs(1);
        assert!(
            admin.admit("phone", later).await.is_ok(),
            "a request renews the window"
        );
        assert_eq!(
            admin.admit("phone", later + ADMIN_IDLE).await,
            Err(Locked::Expired)
        );
        assert_eq!(
            admin.admit("phone", later + ADMIN_IDLE).await,
            Err(Locked::NotUnlocked),
            "an expired unlock is gone, not merely refused once"
        );
    }

    #[tokio::test]
    async fn a_rotated_card_locks_every_device_unlocked_with_the_old_one() {
        let (cards, admin) = pond_with_card();
        let t0 = Instant::now();
        admin.unlock("phone", CARD, t0).await.unwrap();
        cards.issue("card-2", AdminPassword::parse(WRONG).unwrap());
        assert_eq!(admin.admit("phone", t0).await, Err(Locked::CardChanged));
        let status = admin.status("phone", t0).await.unwrap();
        assert_eq!(status.unlocked_for, None);
    }

    #[tokio::test]
    async fn an_unreadable_card_admits_nobody() {
        let (cards, admin) = pond_with_card();
        let t0 = Instant::now();
        admin.unlock("phone", CARD, t0).await.unwrap();
        cards.set_unreadable(true);
        assert!(matches!(
            admin.admit("phone", t0).await,
            Err(Locked::Unavailable(_))
        ));
        assert!(matches!(
            admin.unlock("phone", CARD, t0).await,
            Err(UnlockRefusal::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn wrong_passwords_are_slowed_like_the_setup_code() {
        let (_, admin) = pond_with_card();
        let t0 = Instant::now();
        for _ in 0..FREE_SETUP_FAILURES {
            assert_eq!(
                admin.unlock("phone", WRONG, t0).await,
                Err(UnlockRefusal::Rejected)
            );
        }
        let refused = admin.unlock("phone", CARD, t0).await;
        assert_eq!(
            refused,
            Err(UnlockRefusal::Cooling(RetryAfter(Duration::from_secs(30)))),
            "even the right card waits once guessing has been slowed"
        );
        assert_eq!(
            admin.unlock("other-phone", CARD, t0).await.map(|_| ()),
            Err(UnlockRefusal::Cooling(RetryAfter(Duration::from_secs(30)))),
            "the wait is the pond's, not one device's"
        );
        let after = t0 + Duration::from_secs(30);
        assert_eq!(admin.unlock("phone", CARD, after).await, Ok(ADMIN_IDLE));
        let status = admin.status("phone", after).await.unwrap();
        assert_eq!(status.retry_after, None, "a success resets the wait");
    }

    #[tokio::test]
    async fn a_typo_or_a_missing_card_spends_no_guess() {
        let cards = Arc::new(MockAdminCardStore::new());
        let admin = AdminElevation::new(cards.clone(), Arc::new(MockAdminDeviceKeys::new()));
        let t0 = Instant::now();
        for _ in 0..FREE_SETUP_FAILURES * 2 {
            assert_eq!(
                admin.unlock("phone", CARD, t0).await,
                Err(UnlockRefusal::CardMissing)
            );
            assert_eq!(
                admin.unlock("phone", "0123", t0).await,
                Err(UnlockRefusal::Malformed)
            );
        }
        assert_eq!(
            cards.checks(),
            FREE_SETUP_FAILURES * 2,
            "a malformed password never reaches the card"
        );
        cards.issue("card-1", AdminPassword::parse(CARD).unwrap());
        assert_eq!(admin.unlock("phone", CARD, t0).await, Ok(ADMIN_IDLE));
    }

    #[tokio::test]
    async fn locking_ends_the_unlock_and_status_never_renews_it() {
        let (_, admin) = pond_with_card();
        let t0 = Instant::now();
        admin.unlock("phone", CARD, t0).await.unwrap();
        let later = t0 + Duration::from_secs(60);
        let status = admin.status("phone", later).await.unwrap();
        assert!(status.card_present);
        assert_eq!(
            status.unlocked_for,
            Some(ADMIN_IDLE - Duration::from_secs(60))
        );
        assert_eq!(
            admin.status("phone", later).await.unwrap().unlocked_for,
            Some(ADMIN_IDLE - Duration::from_secs(60)),
            "reading the status is not an admin request"
        );
        assert!(admin.lock("phone").await);
        assert!(!admin.lock("phone").await);
        assert_eq!(admin.admit("phone", later).await, Err(Locked::NotUnlocked));
    }

    fn pond_with_keys() -> (
        Arc<MockAdminCardStore>,
        Arc<MockAdminDeviceKeys>,
        AdminElevation,
    ) {
        let cards = Arc::new(MockAdminCardStore::new());
        cards.issue("card-1", AdminPassword::parse(CARD).unwrap());
        let keys = Arc::new(MockAdminDeviceKeys::new());
        let elevation = AdminElevation::new(cards.clone(), keys.clone());
        (cards, keys, elevation)
    }

    #[tokio::test]
    async fn an_unlocked_phone_enrolls_and_then_opens_with_its_own_key() {
        let (_, _, admin) = pond_with_keys();
        let t0 = Instant::now();
        assert_eq!(admin.enroll("phone", t0).await, Err(Locked::NotUnlocked));
        admin.unlock("phone", CARD, t0).await.unwrap();
        let key = admin.enroll("phone", t0).await.unwrap();
        assert!(admin.status("phone", t0).await.unwrap().key_held);
        assert!(admin.lock("phone").await);
        assert_eq!(
            admin.unlock_with_key("phone", &key, t0).await,
            Ok(ADMIN_IDLE)
        );
        assert!(admin.admit("phone", t0).await.is_ok());
    }

    #[tokio::test]
    async fn a_key_opens_only_its_own_phone() {
        let (_, keys, admin) = pond_with_keys();
        let t0 = Instant::now();
        let key = keys.issue("phone-a").await.unwrap();
        assert_eq!(
            admin.unlock_with_key("phone-b", &key, t0).await,
            Err(UnlockRefusal::KeyRejected)
        );
        assert_eq!(admin.admit("phone-b", t0).await, Err(Locked::NotUnlocked));
    }

    #[tokio::test]
    async fn wrong_keys_are_slowed_like_wrong_cards() {
        let (_, keys, admin) = pond_with_keys();
        let t0 = Instant::now();
        let key = keys.issue("phone").await.unwrap();
        for _ in 0..FREE_SETUP_FAILURES {
            assert_eq!(
                admin.unlock_with_key("phone", "guess", t0).await,
                Err(UnlockRefusal::KeyRejected)
            );
        }
        assert!(matches!(
            admin.unlock_with_key("phone", &key, t0).await,
            Err(UnlockRefusal::Cooling(_))
        ));
        assert!(matches!(
            admin.unlock("other", CARD, t0).await,
            Err(UnlockRefusal::Cooling(_))
        ));
    }

    #[tokio::test]
    async fn a_revoked_or_replaced_key_no_longer_opens() {
        let (_, keys, admin) = pond_with_keys();
        let t0 = Instant::now();
        let first = keys.issue("phone").await.unwrap();
        let second = keys.issue("phone").await.unwrap();
        assert_eq!(
            admin.unlock_with_key("phone", &first, t0).await,
            Err(UnlockRefusal::KeyRejected),
            "a new key replaces the old one"
        );
        admin.unlock_with_key("phone", &second, t0).await.unwrap();
        assert!(admin.revoke_device("phone").await.unwrap());
        assert_eq!(admin.admit("phone", t0).await, Err(Locked::NotUnlocked));
        assert!(!admin.status("phone", t0).await.unwrap().key_held);
        assert_eq!(
            admin.unlock_with_key("phone", &second, t0).await,
            Err(UnlockRefusal::KeyRejected)
        );
    }

    #[tokio::test]
    async fn a_pond_without_a_card_opens_to_its_first_users_key() {
        let cards = Arc::new(MockAdminCardStore::new());
        let keys = Arc::new(MockAdminDeviceKeys::new());
        let admin = AdminElevation::new(cards.clone(), keys.clone());
        let t0 = Instant::now();
        let key = admin.issue_first_key("owner-phone").await.unwrap();
        assert_eq!(
            admin.unlock_with_key("owner-phone", &key, t0).await,
            Ok(ADMIN_IDLE)
        );
        assert!(admin.admit("owner-phone", t0).await.is_ok());
        let status = admin.status("owner-phone", t0).await.unwrap();
        assert!(!status.card_present && status.key_held && status.unlocked_for.is_some());

        cards.issue("card-1", AdminPassword::parse(CARD).unwrap());
        assert_eq!(
            admin.admit("owner-phone", t0).await,
            Err(Locked::CardChanged),
            "a card made later locks the session it did not exist for"
        );
        assert_eq!(
            admin.unlock_with_key("owner-phone", &key, t0).await,
            Ok(ADMIN_IDLE),
            "but keeps the key: making the first card is not a rotation"
        );
    }
}
