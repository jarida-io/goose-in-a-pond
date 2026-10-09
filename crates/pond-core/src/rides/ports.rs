//! Driven port: a ride-hailing provider, acting for one member with that member's own account.

use anyhow::Result;
use async_trait::async_trait;

use super::domain::{FareQuote, Place, RequestFailure, Ride, SavedRide};

#[async_trait]
pub trait RideProvider: Send + Sync {
    /// Stable, lower-case provider name (`uber`), for logs and the member's notification.
    fn name(&self) -> &'static str;

    /// An upfront fare from `pickup` to `dropoff`. Requests nothing.
    async fn quote(&self, profile_id: &str, pickup: &Place, dropoff: &Place) -> Result<FareQuote>;

    /// Request the ride at `quote`. Call only after the member confirmed it. A failure says
    /// whether the provider may hold the ride all the same.
    async fn request(
        &self,
        profile_id: &str,
        pickup: &Place,
        dropoff: &Place,
        quote: &FareQuote,
    ) -> std::result::Result<Ride, RequestFailure>;

    /// The ride as the provider holds it now.
    async fn ride(&self, profile_id: &str, request_id: &str) -> Result<Ride>;

    /// The member's trip under way with the provider, if they have one.
    async fn current(&self, profile_id: &str) -> Result<Option<Ride>>;

    async fn cancel(&self, profile_id: &str, request_id: &str) -> Result<()>;
}

/// Which members can book with a provider: they connected their own account on this pond.
#[async_trait]
pub trait RideAccounts: Send + Sync {
    async fn is_connected(&self, profile_id: &str) -> Result<bool>;
}

/// Driven port: where rides are kept, so a restart loses neither a quote nor a booked ride.
#[async_trait]
pub trait RideStore: Send + Sync {
    /// Keep this ride as it now stands, replacing what was kept for it.
    async fn save(&self, ride: &SavedRide) -> Result<()>;

    async fn remove(&self, id: &str) -> Result<()>;

    /// Every kept ride. One that can no longer be read is skipped, never an error for the rest.
    async fn load(&self) -> Result<Vec<SavedRide>>;
}
