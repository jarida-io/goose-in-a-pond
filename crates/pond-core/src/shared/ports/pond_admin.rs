//! Driven port: the host a pond runs on, as My Pond administers it. Paths reach it already
//! judged by [`classify`](crate::shared::domain::pond_admin::classify); an adapter still keeps
//! every path inside the data directory and never follows a symlink.

use async_trait::async_trait;

use crate::shared::domain::pond_admin::{
    AdminError, CacheId, CacheUsage, DataEntry, DataPath, DatabaseUsage, DiskSpace, HostHealth,
    PowerModeSet, PowerState, RecordCounts,
};

/// The data directory's size, per area root of `area_roots()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReport {
    pub disk: DiskSpace,
    pub data_dir_bytes: u64,
    /// Bytes under each root of `area_roots()`, in that order; a missing root is 0.
    pub area_root_bytes: Vec<u64>,
    pub databases: Vec<DatabaseUsage>,
    pub counts: RecordCounts,
}

#[async_trait]
pub trait PondAdministrator: Send + Sync {
    async fn storage(&self) -> Result<StorageReport, AdminError>;

    /// A directory's entries. `NotFound` for nothing there, `NotADirectory` for a file, and
    /// `InvalidPath` when a segment on the way is a symlink.
    async fn list_dir(&self, path: &DataPath) -> Result<Vec<DataEntry>, AdminError>;

    /// Delete a file, a symlink (not its target) or a directory's whole tree. Returns the bytes
    /// freed.
    async fn delete(&self, path: &DataPath) -> Result<u64, AdminError>;

    async fn caches(&self) -> Result<Vec<CacheUsage>, AdminError>;

    /// Returns the bytes freed.
    async fn clear_cache(&self, cache: CacheId) -> Result<u64, AdminError>;

    async fn health(&self) -> Result<HostHealth, AdminError>;

    async fn power(&self) -> Result<PowerState, AdminError>;

    /// Switch to a mode `power()` listed. `PowerControlUnavailable` when the host cannot.
    async fn set_power_mode(&self, id: u32) -> Result<PowerModeSet, AdminError>;

    /// Restart the computer the pond runs on.
    async fn restart(&self) -> Result<(), AdminError>;
}
