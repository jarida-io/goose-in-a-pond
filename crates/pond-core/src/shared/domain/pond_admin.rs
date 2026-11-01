//! What My Pond shows and changes: the data directory, its caches, the host's health and its
//! power. The rules for what may be seen or deleted live here, so every adapter obeys them.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Why an admin request was refused. No variant carries a path, so an error is safe to log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminError {
    #[error("not a path inside the data directory")]
    InvalidPath,
    #[error("no such entry")]
    NotFound,
    #[error("not a directory")]
    NotADirectory,
    #[error("protected: {}", .0.as_str())]
    Protected(Protection),
    #[error("no such cache")]
    UnknownCache,
    #[error("the task has not finished")]
    TaskActive,
    #[error("no such power mode")]
    UnknownPowerMode,
    #[error("this pond cannot change its power")]
    PowerControlUnavailable,
    #[error("a long task is running")]
    LongTaskRunning,
    #[error("{0}")]
    Internal(String),
}

impl AdminError {
    /// The wire `error`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::NotFound => "not_found",
            Self::NotADirectory => "not_a_directory",
            Self::Protected(_) => "protected_path",
            Self::UnknownCache => "unknown_cache",
            Self::TaskActive => "task_active",
            Self::UnknownPowerMode => "unknown_power_mode",
            Self::PowerControlUnavailable => "power_control_unavailable",
            Self::LongTaskRunning => "long_task_running",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<anyhow::Error> for AdminError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

/// A path inside the data directory as a caller named it: relative, `/`-separated, with no
/// `.`, `..` or empty segment. `""` is the data directory itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DataPath(String);

impl DataPath {
    pub fn root() -> Self {
        Self(String::new())
    }

    /// Leading and trailing slashes are forgiven; anything that could leave the directory is not.
    pub fn parse(raw: &str) -> Result<Self, AdminError> {
        let trimmed = raw.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(Self::root());
        }
        if trimmed.contains('\\') || trimmed.contains('\0') {
            return Err(AdminError::InvalidPath);
        }
        if trimmed
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(AdminError::InvalidPath);
        }
        Ok(Self(trimmed.to_string()))
    }

    /// One more segment, for naming a directory's entries.
    pub fn child(&self, name: &str) -> Result<Self, AdminError> {
        if name.is_empty() || name.contains('/') {
            return Err(AdminError::InvalidPath);
        }
        Self::parse(&if self.is_root() {
            name.to_string()
        } else {
            format!("{}/{name}", self.0)
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|s| !s.is_empty())
    }

    fn starts_with(&self, prefix: &[&str]) -> bool {
        let mut segments = self.segments();
        prefix.iter().all(|want| segments.next() == Some(want))
    }

    fn is(&self, exact: &[&str]) -> bool {
        self.segments().eq(exact.iter().copied())
    }
}

/// Credentials and the pond's identity: never listed, never deleted, never even confirmed to exist.
const HIDDEN_TOP_LEVEL: &[&str] = &[
    "setup",
    "tls",
    "secrets",
    "secrets.json",
    "embedded-network",
    "lightning",
];

/// What the running pond reads or writes in place; deleting any of it breaks a feature.
const IN_USE_TREES: &[&[&str]] = &[
    &["lib"],
    &["bin"],
    &["matter-server"],
    &["prompts"],
    &["memory"],
    &["schedules.json"],
    &["schedule_runs.json"],
    &["engine", "config"],
    &["engine", "data", "sessions"],
    // A suspended long task's saved state, which it resumes from.
    &["engine", "data", "prompt-cache", "suspended"],
];

/// The live databases.
const DATABASES: &[&str] = &["pond_system.db", "pond_logs.db", "pond_vectors.db"];

/// The live databases with SQLite's journal files.
const DATABASE_FILES: &[&str] = &[
    "pond_system.db",
    "pond_system.db-wal",
    "pond_system.db-shm",
    "pond_logs.db",
    "pond_logs.db-wal",
    "pond_logs.db-shm",
    "pond_vectors.db",
    "pond_vectors.db-wal",
    "pond_vectors.db-shm",
];

/// Removed through Models, which also updates the registry that points at them.
const MODEL_TREES: &[&[&str]] = &[&["models"], &["hf_cache"], &["engine", "data", "models"]];

/// Directories the pond expects to exist: their contents may go, the directory may not.
const CONTAINERS: &[&[&str]] = &[
    &[],
    &["logs"],
    &["attachments"],
    &["snapshots"],
    &["audio"],
    &["cache"],
    &["engine"],
    &["engine", "data"],
    &["engine", "data", "prompt-cache"],
    &["engine", "data", "litert-lm"],
];

/// Why an entry is shown but may not be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    InUse,
    ManagedByModels,
}

impl Protection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InUse => "in_use",
            Self::ManagedByModels => "managed_by_models",
        }
    }
}

/// Whether a path may be listed, and whether it may be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Hidden,
    Shown(Option<Protection>),
}

pub fn classify(path: &DataPath) -> Visibility {
    if let Some(top) = path.segments().next() {
        if HIDDEN_TOP_LEVEL.contains(&top) || top.starts_with(".runtime_") {
            return Visibility::Hidden;
        }
    }
    if MODEL_TREES.iter().any(|tree| path.starts_with(tree)) {
        return Visibility::Shown(Some(Protection::ManagedByModels));
    }
    if DATABASE_FILES.iter().any(|file| path.is(&[file]))
        || IN_USE_TREES.iter().any(|tree| path.starts_with(tree))
        || CONTAINERS.iter().any(|dir| path.is(dir))
    {
        return Visibility::Shown(Some(Protection::InUse));
    }
    Visibility::Shown(None)
}

/// What the file browser receives from the host for one directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataEntry {
    pub name: String,
    pub kind: EntryKind,
    /// A directory's whole tree, symlinks not followed; a symlink is 0.
    pub bytes: u64,
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

/// One entry as My Pond shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub bytes: u64,
    pub modified_at: Option<DateTime<Utc>>,
    pub deletable: bool,
    pub reason: Option<&'static str>,
}

/// The groups the overview totals the data directory into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AreaId {
    Models,
    Conversations,
    KvSnapshots,
    EngineCaches,
    Attachments,
    Camera,
    Logs,
    Other,
}

/// Where each area lives. The roots never overlap, so `Other` is the data directory's total less
/// their sum.
pub fn area_roots() -> Vec<(AreaId, Vec<&'static str>)> {
    let mut roots = vec![
        (AreaId::Models, vec!["models"]),
        (AreaId::Models, vec!["hf_cache"]),
        (AreaId::Models, vec!["engine", "data", "models"]),
        (AreaId::KvSnapshots, vec!["engine", "data", "prompt-cache"]),
        (
            AreaId::KvSnapshots,
            vec!["engine", "data", "litert-lm", "kv-snapshots"],
        ),
        (AreaId::EngineCaches, vec!["cache"]),
        (
            AreaId::EngineCaches,
            vec!["engine", "data", "litert-lm", "cache"],
        ),
        (AreaId::Attachments, vec!["attachments"]),
        (AreaId::Camera, vec!["snapshots"]),
        (AreaId::Logs, vec!["logs"]),
    ];
    for file in DATABASE_FILES {
        roots.push((AreaId::Conversations, vec![*file]));
    }
    roots
}

/// The live databases, for the overview's per-database sizes; journals count with their database.
pub fn database_names() -> &'static [&'static str] {
    DATABASES
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AreaUsage {
    pub id: AreaId,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatabaseUsage {
    pub name: String,
    /// The database with its journal files.
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RecordCounts {
    pub sessions: u64,
    pub messages: u64,
    pub memories: u64,
    pub long_tasks: u64,
    pub artifacts: u64,
    pub models: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PondOverview {
    pub disk: DiskSpace,
    pub data_dir_bytes: u64,
    pub areas: Vec<AreaUsage>,
    pub databases: Vec<DatabaseUsage>,
    pub counts: RecordCounts,
}

/// The caches My Pond may clear. Build output such as `target/` is never one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheId {
    /// The engines' saved prompt states; a suspended long task's state is not among them.
    KvSnapshots,
    /// Model downloads no installed model uses, and partial downloads left over a day.
    DownloadLeftovers,
    /// TensorRT and LiteRT build caches, rebuilt on the next load.
    AcceleratorCaches,
}

impl CacheId {
    pub const ALL: [CacheId; 3] = [
        CacheId::KvSnapshots,
        CacheId::DownloadLeftovers,
        CacheId::AcceleratorCaches,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::KvSnapshots => "kv_snapshots",
            Self::DownloadLeftovers => "download_leftovers",
            Self::AcceleratorCaches => "accelerator_caches",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheUsage {
    pub id: CacheId,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpus: u32,
    pub uptime_secs: u64,
    pub load: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MemoryUsage {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Temperature {
    pub zone: String,
    pub celsius: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProcessUsage {
    pub rss_bytes: u64,
}

/// What the host reports; the checks are judged from it in [`health_checks`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostHealth {
    pub host: HostInfo,
    pub memory: MemoryUsage,
    pub disk: DiskSpace,
    pub temperatures: Vec<Temperature>,
    pub process: ProcessUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthCheck {
    pub id: &'static str,
    pub status: CheckStatus,
}

/// Under these shares of free space the disk check warns, then turns critical.
pub const DISK_WARNING_FREE: f64 = 0.10;
pub const DISK_CRITICAL_FREE: f64 = 0.05;
pub const MEMORY_WARNING_AVAILABLE: f64 = 0.10;
/// The Orin throttles from about 85 °C; warn before it does.
pub const TEMPERATURE_WARNING_C: f64 = 80.0;
pub const TEMPERATURE_CRITICAL_C: f64 = 90.0;

pub fn health_checks(health: &HostHealth) -> Vec<HealthCheck> {
    let share = |part: u64, whole: u64| {
        if whole == 0 {
            1.0
        } else {
            part as f64 / whole as f64
        }
    };
    let free = share(health.disk.free_bytes, health.disk.total_bytes);
    let disk = if free < DISK_CRITICAL_FREE {
        CheckStatus::Critical
    } else if free < DISK_WARNING_FREE {
        CheckStatus::Warning
    } else {
        CheckStatus::Ok
    };
    let available = share(health.memory.available_bytes, health.memory.total_bytes);
    let memory = if available < MEMORY_WARNING_AVAILABLE {
        CheckStatus::Warning
    } else {
        CheckStatus::Ok
    };
    let hottest = health
        .temperatures
        .iter()
        .map(|t| t.celsius)
        .fold(f64::NEG_INFINITY, f64::max);
    let temperature = if hottest >= TEMPERATURE_CRITICAL_C {
        CheckStatus::Critical
    } else if hottest >= TEMPERATURE_WARNING_C {
        CheckStatus::Warning
    } else {
        CheckStatus::Ok
    };
    vec![
        HealthCheck {
            id: "disk_space",
            status: disk,
        },
        HealthCheck {
            id: "memory",
            status: memory,
        },
        HealthCheck {
            id: "temperature",
            status: temperature,
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerControl {
    Nvpmodel,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PowerMode {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Supply {
    pub millivolts: u64,
    pub milliamps: u64,
    pub milliwatts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PowerState {
    pub control: PowerControl,
    pub mode: Option<PowerMode>,
    pub default_mode_id: Option<u32>,
    pub modes: Vec<PowerMode>,
    /// Whether the service may run the mode switch without a password.
    pub can_change: bool,
    pub supply: Option<Supply>,
    pub battery: Option<Battery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PowerModeSet {
    pub mode: PowerMode,
    pub reboot_required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(raw: &str) -> DataPath {
        DataPath::parse(raw).unwrap()
    }

    #[test]
    fn a_path_cannot_leave_the_data_directory() {
        for raw in [
            "..",
            "../etc",
            "logs/../..",
            "logs/./x",
            "logs//x",
            "a\\..\\b",
            "logs/\0",
        ] {
            assert_eq!(
                DataPath::parse(raw),
                Err(AdminError::InvalidPath),
                "{raw:?}"
            );
        }
        assert_eq!(path("/logs/").as_str(), "logs");
        assert!(path("").is_root());
        assert!(path("/").is_root());
        assert_eq!(
            path("logs").child("pond.log").unwrap().as_str(),
            "logs/pond.log"
        );
        assert_eq!(DataPath::root().child("logs").unwrap().as_str(), "logs");
        assert_eq!(
            path("logs").child("../x"),
            Err(AdminError::InvalidPath),
            "a name from the disk is one segment"
        );
        assert_eq!(path("logs").child(".."), Err(AdminError::InvalidPath));
    }

    #[test]
    fn credentials_are_never_shown_at_any_depth() {
        for raw in [
            "setup",
            "setup/setup.json",
            "setup/admin.json",
            "tls/key.pem",
            "secrets",
            "secrets.json",
            "embedded-network/device",
            "lightning/seed",
            ".runtime_host_credential",
            ".runtime_api_port",
        ] {
            assert_eq!(classify(&path(raw)), Visibility::Hidden, "{raw}");
        }
        // Only the top level hides: a member's attachment called "tls" is still theirs to delete.
        assert_eq!(classify(&path("attachments/tls")), Visibility::Shown(None));
    }

    #[test]
    fn what_the_running_pond_needs_is_shown_but_kept() {
        for raw in [
            "pond_system.db",
            "pond_system.db-wal",
            "pond_logs.db-shm",
            "pond_vectors.db",
            "schedules.json",
            "lib/onnxruntime-gpu/libonnxruntime.so",
            "bin",
            "matter-server/storage-js",
            "engine/data/sessions/sessions.db",
            "engine/data/prompt-cache/suspended",
            "engine/data/prompt-cache/suspended/task.kv",
            "engine/config/prompts",
        ] {
            assert_eq!(
                classify(&path(raw)),
                Visibility::Shown(Some(Protection::InUse)),
                "{raw}"
            );
        }
        for raw in [
            "models",
            "models/gguf/x.gguf",
            "hf_cache/hub",
            "engine/data/models",
        ] {
            assert_eq!(
                classify(&path(raw)),
                Visibility::Shown(Some(Protection::ManagedByModels)),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_containers_contents_may_go_but_not_the_container() {
        for (container, inside) in [
            ("", "pond_system.db.before-0062-removal"),
            ("logs", "logs/pond.log.2026-07-12"),
            ("attachments", "attachments/abc/photo.jpg"),
            ("snapshots", "snapshots/camera-1.jpg"),
            (
                "engine/data/prompt-cache",
                "engine/data/prompt-cache/v1-a.kv",
            ),
            (
                "engine/data/litert-lm",
                "engine/data/litert-lm/kv-snapshots",
            ),
            ("cache", "cache/onnx-trt"),
        ] {
            assert_eq!(
                classify(&path(container)),
                Visibility::Shown(Some(Protection::InUse)),
                "{container:?}"
            );
            assert_eq!(classify(&path(inside)), Visibility::Shown(None), "{inside}");
        }
        // A database's name as a prefix is not the database.
        assert_eq!(
            classify(&path("pond_system.db.bak")),
            Visibility::Shown(None)
        );
    }

    #[test]
    fn area_roots_never_overlap() {
        let roots = area_roots();
        for (i, (_, a)) in roots.iter().enumerate() {
            for (_, b) in roots.iter().skip(i + 1) {
                let shorter = a.len().min(b.len());
                assert_ne!(a[..shorter], b[..shorter], "{a:?} and {b:?} overlap");
            }
        }
        assert_eq!(
            roots
                .iter()
                .filter(|(id, _)| *id == AreaId::Conversations)
                .count(),
            9
        );
    }

    fn health(free: u64, available: u64, hottest: f64) -> HostHealth {
        HostHealth {
            host: HostInfo {
                hostname: "pond".into(),
                os: "linux".into(),
                arch: "aarch64".into(),
                cpus: 6,
                uptime_secs: 60,
                load: [0.5, 0.4, 0.3],
            },
            memory: MemoryUsage {
                total_bytes: 100,
                available_bytes: available,
                swap_total_bytes: 0,
                swap_free_bytes: 0,
            },
            disk: DiskSpace {
                total_bytes: 100,
                free_bytes: free,
            },
            temperatures: vec![
                Temperature {
                    zone: "cpu".into(),
                    celsius: 40.0,
                },
                Temperature {
                    zone: "gpu".into(),
                    celsius: hottest,
                },
            ],
            process: ProcessUsage { rss_bytes: 1 },
        }
    }

    fn statuses(h: &HostHealth) -> Vec<CheckStatus> {
        health_checks(h).into_iter().map(|c| c.status).collect()
    }

    #[test]
    fn checks_judge_disk_memory_and_heat() {
        use CheckStatus::*;
        assert_eq!(statuses(&health(50, 50, 50.0)), [Ok, Ok, Ok]);
        assert_eq!(statuses(&health(9, 9, 80.0)), [Warning, Warning, Warning]);
        // The nano on 2026-10-29: 6 GB free of 116 GB.
        assert_eq!(statuses(&health(4, 50, 90.0)), [Critical, Ok, Critical]);
        let mut empty = health(0, 0, 0.0);
        empty.disk.total_bytes = 0;
        empty.memory.total_bytes = 0;
        empty.temperatures.clear();
        assert_eq!(
            statuses(&empty),
            [Ok, Ok, Ok],
            "an unknown reading is not an alarm"
        );
    }

    #[test]
    fn cache_ids_round_trip_and_nothing_else_parses() {
        for id in CacheId::ALL {
            assert_eq!(CacheId::parse(id.as_str()), Some(id));
        }
        assert_eq!(CacheId::parse("target"), None);
        assert_eq!(CacheId::parse("KV_SNAPSHOTS"), None);
    }
}
