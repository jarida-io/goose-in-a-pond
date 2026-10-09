//! [`RideStore`] over the system database's `rides` table (migration 0060).

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use pond_core::rides::domain::SavedRide;
use pond_core::rides::ports::RideStore;
use sqlx::{Pool, Sqlite};

pub struct SqliteRideStore {
    pool: Pool<Sqlite>,
}

impl SqliteRideStore {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RideStore for SqliteRideStore {
    async fn save(&self, ride: &SavedRide) -> Result<()> {
        let saved = serde_json::to_string(ride).context("a ride could not be written as JSON")?;
        sqlx::query(
            "INSERT INTO rides (id, profile_id, saved, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET saved = excluded.saved, updated_at = excluded.updated_at",
        )
        .bind(&ride.ride.id)
        .bind(&ride.ride.profile_id)
        .bind(saved)
        .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
        .execute(&self.pool)
        .await
        .context("could not keep a ride")?;
        Ok(())
    }

    async fn remove(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM rides WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("could not forget a ride")?;
        Ok(())
    }

    async fn load(&self) -> Result<Vec<SavedRide>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, saved FROM rides")
            .fetch_all(&self.pool)
            .await
            .context("could not read the kept rides")?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, saved)| match serde_json::from_str::<SavedRide>(&saved) {
                Ok(ride) => Some(ride),
                Err(e) => {
                    tracing::warn!(ride = %id, error = %e, "a kept ride no longer reads; skipping it");
                    None
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_profile::SqliteProfileRepository;
    use chrono::Utc;
    use pond_core::rides::domain::{
        BookingState, FareQuote, PendingRide, Place, QuotedTrip, Reading,
    };
    use pond_core::user_data::domain::profile::CreateProfileRequest;
    use pond_core::user_data::ports::profile::ProfileRepository;

    async fn store() -> (
        SqliteRideStore,
        SqliteProfileRepository,
        String,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        let profiles = SqliteProfileRepository::new(db.system.clone());
        let liz = profiles
            .create(CreateProfileRequest {
                display_name: "Liz".into(),
                avatar_emoji: "*".into(),
            })
            .await
            .unwrap()
            .id;
        (SqliteRideStore::new(db.system.clone()), profiles, liz, dir)
    }

    fn ride(id: &str, profile_id: &str) -> SavedRide {
        let place = |name: &str| Place {
            name: name.into(),
            latitude: -1.3,
            longitude: 36.8,
        };
        SavedRide {
            ride: PendingRide {
                id: id.into(),
                profile_id: profile_id.into(),
                trip: Some(QuotedTrip {
                    pickup: place("Home"),
                    dropoff: place("JKIA"),
                    quote: FareQuote {
                        fare_id: "fare-1".into(),
                        display: "KES 1,250".into(),
                        currency_code: "KES".into(),
                        expires_at: Utc::now(),
                        pickup_eta_mins: Some(4),
                        product_id: None,
                    },
                }),
                created_at: Utc::now(),
                state: BookingState::AwaitingConfirmation,
            },
            announced: None,
            failed_reads: 2,
            reading: Reading::On,
        }
    }

    #[tokio::test]
    async fn a_ride_is_kept_replaced_and_forgotten() {
        let (store, _, liz, _dir) = store().await;
        let mut kept = ride("r-1", &liz);
        store.save(&kept).await.unwrap();
        kept.ride.state = BookingState::Declined;
        kept.failed_reads = 0;
        store.save(&kept).await.unwrap();

        let loaded = store.load().await.unwrap();
        assert_eq!(loaded, vec![kept]);

        store.remove("r-1").await.unwrap();
        assert!(store.load().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn removing_a_member_removes_their_rides() {
        let (store, profiles, liz, _dir) = store().await;
        store.save(&ride("r-1", &liz)).await.unwrap();
        profiles.delete(&liz).await.unwrap();
        assert!(
            store.load().await.unwrap().is_empty(),
            "a removed member's pickup and drop-off were kept"
        );
    }

    #[tokio::test]
    async fn a_ride_for_no_member_is_refused() {
        let (store, _, _, _dir) = store().await;
        assert!(store.save(&ride("r-1", "nobody")).await.is_err());
    }

    #[tokio::test]
    async fn a_row_that_no_longer_reads_is_skipped_not_fatal() {
        let (store, _, liz, _dir) = store().await;
        store.save(&ride("good", &liz)).await.unwrap();
        sqlx::query("INSERT INTO rides (id, profile_id, saved, updated_at) VALUES ('bad', ?, '{\"x\":1}', '')")
            .bind(&liz)
            .execute(&store.pool)
            .await
            .unwrap();
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].ride.id, "good");
    }
}
