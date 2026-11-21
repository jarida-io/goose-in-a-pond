use anyhow::Result;
use async_trait::async_trait;

use crate::network::domain::pond_status::{CoordinatorView, StatusReport};

/// Delivers the Pond's status reports to the remote-access coordinator, signed with the
/// household's key, and returns the coordinator's answer. See
/// [`pond_status`](crate::network::domain::pond_status) for what is sent and when.
#[async_trait]
pub trait PondStatusReporter: Send + Sync {
    /// Sends one report. An error means it did not arrive, or was refused; either way the
    /// caller tries again later with a new sequence number.
    async fn report(&self, report: &StatusReport) -> Result<CoordinatorView>;
}
