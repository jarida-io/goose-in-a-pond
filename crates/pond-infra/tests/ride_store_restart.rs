//! A pond restart, for real: rides written to `pond_system.db` by one `RideBooking` are taken back
//! by a fresh one opened over the same database file, as `pond-server` does at startup.

use std::sync::Arc;

use chrono::Utc;
use pond_core::rides::booking::{BookingError, RideBooking};
use pond_core::rides::domain::{BookingState, Place};
use pond_core::rides::mocks::MockRideProvider;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_infra::db::Database;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_ride_store::SqliteRideStore;

fn place(name: &str, latitude: f64) -> Place {
    Place {
        name: name.to_string(),
        latitude,
        longitude: 36.8,
    }
}

/// A booking service over the database in `dir`, opened afresh as a restarted pond would.
async fn pond(dir: &std::path::Path, provider: &Arc<MockRideProvider>) -> RideBooking {
    let db = Database::init(dir).await.unwrap();
    let booking = RideBooking::new(provider.clone())
        .with_store(Arc::new(SqliteRideStore::new(db.system.clone())));
    booking.restore(Utc::now()).await;
    booking
}

#[tokio::test]
async fn a_fare_quoted_before_a_restart_is_confirmed_after_it_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let liz = {
        let db = Database::init(dir.path()).await.unwrap();
        SqliteProfileRepository::new(db.system.clone())
            .create(CreateProfileRequest {
                display_name: "Liz".into(),
                avatar_emoji: "*".into(),
            })
            .await
            .unwrap()
            .id
    };
    let provider = Arc::new(MockRideProvider::new());

    let quoted = {
        let before = pond(dir.path(), &provider).await;
        before
            .quote(&liz, place("Home", -1.27), place("JKIA", -1.32), Utc::now())
            .await
            .unwrap()
    };

    let after = pond(dir.path(), &provider).await;
    let state = after.confirm(&quoted.id, &liz, Utc::now()).await.unwrap();
    assert!(matches!(state, BookingState::Requested { .. }), "{state:?}");
    assert_eq!(provider.requests(), 1);

    // A second restart remembers it was booked, so it is never requested again.
    let again = pond(dir.path(), &provider).await;
    assert!(matches!(
        again.confirm(&quoted.id, &liz, Utc::now()).await,
        Err(BookingError::AlreadyDecided(_))
    ));
    assert_eq!(provider.requests(), 1);
    assert!(matches!(
        again.get(&quoted.id, &liz).unwrap().state,
        BookingState::Requested { .. }
    ));
}
