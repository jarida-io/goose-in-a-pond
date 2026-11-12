//! Driven port: erase every household data store a factory reset must clear.
//!
//! The actual erase touches raw SQLite pools and files (`pond_system.db`, `secrets.json`, saved
//! Wi-Fi networks) that pond-core has no business reaching directly — this port is the seam an
//! adapter in pond-api/pond-infra implements, so [`crate::network::services::factory_reset::
//! FactoryResetService`] can stay pure orchestration over ports like everything else it calls.

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait PersonalDataWipe: Send + Sync {
    /// Erase every household-personal store: profiles, sessions, memories, devices, rooms,
    /// secrets, saved Wi-Fi networks, mesh credit/trust state. Leaves infrastructure (models,
    /// the setup card, TLS identity) untouched — a factory reset undoes setup, not installation.
    async fn wipe(&self) -> Result<()>;
}
