//! Switching on ride booking: each member's Uber sign-in (through Jarida's credentials service),
//! the booking rules, the routes the phone uses, the `book_ride` tool, and the ride tracker.

use std::sync::Arc;
use std::time::Duration;

use pond_adapters_uber::accounts::{SignInRelay, UberAccounts};
use pond_adapters_uber::{UberConfig, UberRides};
use pond_core::mcp::ports::notification::MemberNotifier;
use pond_core::rides::booking::RideBooking;
use pond_core::rides::ports::RideStore;
use pond_core::security::ports::secret::SecretRepository;

const POLL_ENV: &str = "GIAP_RIDE_POLL_SECS";
const DEFAULT_POLL: Duration = Duration::from_secs(15);
/// Faster than this and a pond with a ride under way calls Uber more than any update needs.
const MIN_POLL: Duration = Duration::from_secs(5);

/// How often booked rides are read, from `GIAP_RIDE_POLL_SECS`.
pub fn poll_interval(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .map_or(DEFAULT_POLL, |d| d.max(MIN_POLL))
}

/// Members' Uber connections, installed for `book_ride`. Off unless the household switched travel
/// on (`ext_travel_enabled`, off by default); needs the secret store (member sign-ins) and the
/// credentials service (Uber's client secret) too. Every process that runs giap-travel calls
/// this; only `serve` goes on to [`start`].
pub fn install_accounts(
    travel_enabled: bool,
    secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
    relay_url: Option<String>,
) -> Option<Arc<UberAccounts>> {
    let accounts = accounts(travel_enabled, secrets, relay_url)?;
    pond_mcp_server::travel::init_ride_accounts(accounts.clone());
    Some(accounts)
}

fn accounts(
    travel_enabled: bool,
    secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
    relay_url: Option<String>,
) -> Option<Arc<UberAccounts>> {
    if !travel_enabled {
        tracing::info!("ride booking is off: travel is switched off (ext_travel_enabled)");
        return None;
    }
    let (Some(secrets), Some(relay_url)) = (secrets, relay_url) else {
        tracing::info!(
            "ride booking is off: it needs the secret store and Jarida's credentials service"
        );
        return None;
    };
    Some(Arc::new(UberAccounts::new(
        secrets,
        Some(SignInRelay::new(reqwest::Client::new(), &relay_url)),
    )))
}

/// Turn ride booking on, or say why it stays off: the accounts as [`install_accounts`], the
/// booking rules, the phone's ride routes and the tracker. Without them `book_ride` and the
/// phone's ride routes answer that booking is not set up. With `store`, rides kept before a
/// restart are taken back before the phone's routes can be asked about them.
pub async fn start(
    travel_enabled: bool,
    secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
    relay_url: Option<String>,
    notifier: Arc<dyn MemberNotifier>,
    store: Option<Arc<dyn RideStore>>,
) -> Option<Arc<RideBooking>> {
    let accounts = accounts(travel_enabled, secrets, relay_url)?;
    let config = match UberConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "ride booking is off");
            return None;
        }
    };
    pond_mcp_server::travel::init_ride_accounts(accounts.clone());
    let uber = UberRides::new(reqwest::Client::new(), config, accounts.clone());
    let mut booking = RideBooking::new(Arc::new(uber));
    if let Some(store) = store {
        booking = booking.with_store(store);
    }
    let booking = Arc::new(booking);
    let restored = booking.restore(chrono::Utc::now()).await;
    if restored > 0 {
        tracing::info!(
            rides = restored,
            "rides: took back the rides kept before the restart"
        );
    }

    pond_api::rides::install(booking.clone());

    // A trip booked some other way (or lost by a pond with no store) is followed from the first
    // pass; one already taken back above is recognised and not followed twice.
    let interval = poll_interval(std::env::var(POLL_ENV).ok().as_deref());
    let tracked = booking.clone();
    tokio::spawn(async move {
        take_over_rides_under_way(&tracked, &accounts).await;
        loop {
            tokio::time::sleep(interval).await;
            pond_core::rides::tracking::track_once(&tracked, notifier.as_ref()).await;
        }
    });
    tracing::info!(poll_secs = interval.as_secs(), "ride booking is on (Uber)");
    Some(booking)
}

async fn take_over_rides_under_way(booking: &RideBooking, accounts: &UberAccounts) {
    let members = match accounts.connected_members().await {
        Ok(members) => members,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "rides: could not list members with Uber");
            return;
        }
    };
    for member in members {
        match booking.take_over_current(&member, chrono::Utc::now()).await {
            Ok(Some(ride)) => tracing::info!(ride = %ride, "rides: following a trip under way"),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "rides: could not read a member's trip under way"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poll_interval_defaults_and_cannot_be_set_too_fast() {
        assert_eq!(poll_interval(None), DEFAULT_POLL);
        assert_eq!(poll_interval(Some("not a number")), DEFAULT_POLL);
        assert_eq!(poll_interval(Some("30")), Duration::from_secs(30));
        assert_eq!(poll_interval(Some("1")), MIN_POLL);
    }

    #[tokio::test]
    async fn booking_stays_off_without_sign_in_support() {
        let notifier: Arc<dyn MemberNotifier> =
            Arc::new(pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier::new());
        assert!(start(
            true,
            None,
            Some("https://credentials.example".into()),
            notifier.clone(),
            None,
        )
        .await
        .is_none());
    }

    /// Everything booking needs is here; only the household's switch is off.
    #[tokio::test]
    async fn booking_stays_off_while_travel_is_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretRepository + Send + Sync> = Arc::new(
            pond_infra::file_secret_repository::FileSecretRepository::new(dir.path()).unwrap(),
        );
        let notifier: Arc<dyn MemberNotifier> =
            Arc::new(pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier::new());
        let relay = Some("https://credentials.example".to_string());
        assert!(
            start(false, Some(secrets.clone()), relay.clone(), notifier, None)
                .await
                .is_none()
        );
        assert!(install_accounts(false, Some(secrets), relay).is_none());
    }
}
