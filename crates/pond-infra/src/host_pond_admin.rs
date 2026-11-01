//! The [`PondAdministrator`] for the machine the pond runs on: its data directory, `/proc` and
//! `/sys` through sysinfo, and on a Jetson `nvpmodel` and the INA3221 supply monitor.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::shared::domain::pond_admin::{
    area_roots, database_names, AdminError, Battery, CacheId, CacheUsage, DataEntry, DataPath,
    DatabaseUsage, DiskSpace, EntryKind, HostHealth, HostInfo, MemoryUsage, PowerControl,
    PowerMode, PowerModeSet, PowerState, ProcessUsage, RecordCounts, Supply, Temperature,
};
use pond_core::shared::ports::pond_admin::{PondAdministrator, StorageReport};
use sqlx::SqlitePool;

use crate::hf_cleanup;

/// The engines' saved prompt states; `suspended/` beneath it is a long task's, and stays.
const PROMPT_CACHE: &[&str] = &["engine", "data", "prompt-cache"];
const SUSPENDED: &str = "suspended";
const LITERT_KV: &[&str] = &["engine", "data", "litert-lm", "kv-snapshots"];
const ACCELERATOR_CACHES: &[&[&str]] = &[
    &["cache", "onnx-trt"],
    &["engine", "data", "litert-lm", "cache"],
];

/// Where the host keeps what the power page reads, so tests can point it at fixtures.
#[derive(Debug, Clone)]
pub struct PowerHost {
    pub nvpmodel_conf: PathBuf,
    pub nvpmodel: Option<PathBuf>,
    pub sudo: PathBuf,
    pub systemctl: PathBuf,
    pub hwmon: PathBuf,
    pub power_supply: PathBuf,
}

impl PowerHost {
    pub fn system() -> Self {
        Self {
            nvpmodel_conf: "/etc/nvpmodel.conf".into(),
            nvpmodel: ["/usr/sbin/nvpmodel", "/usr/bin/nvpmodel"]
                .into_iter()
                .map(PathBuf::from)
                .find(|p| p.is_file()),
            sudo: "/usr/bin/sudo".into(),
            systemctl: "/usr/bin/systemctl".into(),
            hwmon: "/sys/class/hwmon".into(),
            power_supply: "/sys/class/power_supply".into(),
        }
    }
}

pub struct HostPondAdministrator {
    data_dir: PathBuf,
    pool: SqlitePool,
    model_repo: Option<Arc<dyn ModelRepository + Send + Sync>>,
    power: PowerHost,
    started: std::time::Instant,
}

impl HostPondAdministrator {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        pool: SqlitePool,
        model_repo: Option<Arc<dyn ModelRepository + Send + Sync>>,
        power: PowerHost,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            pool,
            model_repo,
            power,
            started: std::time::Instant::now(),
        }
    }

    /// How long this pond process has run, for the health page.
    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(PathBuf) -> Result<T, AdminError> + Send + 'static,
    ) -> Result<T, AdminError> {
        let dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || work(dir))
            .await
            .map_err(|e| AdminError::Internal(e.to_string()))?
    }

    async fn protected_names(&self) -> HashSet<String> {
        match &self.model_repo {
            Some(repo) => hf_cleanup::protected_model_names(repo.as_ref()).await,
            None => HashSet::new(),
        }
    }

    async fn count(&self, table: &str) -> u64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        match sqlx::query_scalar::<_, i64>(&sql)
            .fetch_one(&self.pool)
            .await
        {
            Ok(n) => u64::try_from(n).unwrap_or(0),
            Err(e) => {
                tracing::warn!(table, error = %e, "pond admin: could not count rows");
                0
            }
        }
    }

    async fn counts(&self) -> RecordCounts {
        RecordCounts {
            sessions: self.count("sessions").await,
            messages: self.count("session_messages").await,
            memories: self.count("memory_fragments").await,
            long_tasks: self.count("long_tasks").await,
            artifacts: self.count("long_task_artifacts").await,
            models: self.count("models").await,
        }
    }
}

fn io(e: std::io::Error) -> AdminError {
    match e.kind() {
        std::io::ErrorKind::NotFound => AdminError::NotFound,
        _ => AdminError::Internal(e.to_string()),
    }
}

fn join(base: &Path, segments: &[&str]) -> PathBuf {
    segments.iter().fold(base.to_path_buf(), |p, s| p.join(s))
}

/// `path` under the data directory, refusing a symlink anywhere before its last segment, so no
/// request ever reaches outside the directory however the tree is linked.
fn resolve(data_dir: &Path, path: &DataPath) -> Result<PathBuf, AdminError> {
    let segments: Vec<&str> = path.segments().collect();
    let mut at = data_dir.to_path_buf();
    for (i, segment) in segments.iter().enumerate() {
        at.push(segment);
        if i + 1 == segments.len() {
            break;
        }
        let meta = fs::symlink_metadata(&at).map_err(io)?;
        if meta.file_type().is_symlink() {
            return Err(AdminError::InvalidPath);
        }
        if !meta.is_dir() {
            return Err(AdminError::NotFound);
        }
    }
    Ok(at)
}

/// Bytes and regular files under `path`, never following a symlink; a missing path is empty.
fn tree_usage(path: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(meta) = fs::symlink_metadata(&next) else {
            continue;
        };
        if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
            files += 1;
        } else if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&next) {
                pending.extend(entries.filter_map(|e| e.ok()).map(|e| e.path()));
            }
        }
    }
    (bytes, files)
}

/// Remove a file, a symlink (never its target) or a whole tree; returns the bytes freed.
fn remove(path: &Path) -> Result<u64, AdminError> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if meta.is_dir() {
        let (bytes, _) = tree_usage(path);
        // `remove_dir_all` unlinks symlinks inside the tree rather than following them.
        fs::remove_dir_all(path).map_err(io)?;
        Ok(bytes)
    } else {
        let bytes = if meta.is_file() { meta.len() } else { 0 };
        fs::remove_file(path).map_err(io)?;
        Ok(bytes)
    }
}

/// The entries of `dir` a cache clear takes, skipping any named in `keep`.
fn cache_entries(dir: &Path, keep: &[&str]) -> Vec<PathBuf> {
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return Vec::new();
    };
    if !meta.is_dir() {
        return Vec::new();
    }
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| !keep.iter().any(|k| e.file_name() == *k))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

fn cache_paths(data_dir: &Path, cache: CacheId) -> Vec<PathBuf> {
    match cache {
        CacheId::KvSnapshots => {
            let mut paths = cache_entries(&join(data_dir, PROMPT_CACHE), &[SUSPENDED]);
            paths.extend(cache_entries(&join(data_dir, LITERT_KV), &[]));
            paths
        }
        CacheId::AcceleratorCaches => ACCELERATOR_CACHES
            .iter()
            .flat_map(|dir| cache_entries(&join(data_dir, dir), &[]))
            .collect(),
        CacheId::DownloadLeftovers => Vec::new(),
    }
}

fn entry_of(path: &Path, name: String) -> Option<DataEntry> {
    let meta = fs::symlink_metadata(path).ok()?;
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let bytes = match kind {
        EntryKind::Symlink => 0,
        EntryKind::File => meta.len(),
        EntryKind::Directory => tree_usage(path).0,
    };
    Some(DataEntry {
        name,
        kind,
        bytes,
        modified_at: meta.modified().ok().map(DateTime::<Utc>::from),
    })
}

fn disk_space(path: &Path) -> DiskSpace {
    DiskSpace {
        total_bytes: fs2::total_space(path).unwrap_or(0),
        free_bytes: fs2::available_space(path).unwrap_or(0),
    }
}

/// `< POWER_MODEL ID=0 NAME=15W >` lines, and the `< PM_CONFIG DEFAULT=1 >` line.
pub fn parse_nvpmodel_conf(text: &str) -> (Vec<PowerMode>, Option<u32>) {
    let field = |line: &str, key: &str| -> Option<String> {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .map(|v| v.trim_end_matches('>').to_string())
    };
    let mut modes = Vec::new();
    let mut default = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("< POWER_MODEL") {
            if let (Some(Ok(id)), Some(name)) = (
                field(line, "ID=").map(|v| v.parse::<u32>()),
                field(line, "NAME="),
            ) {
                modes.push(PowerMode { id, name });
            }
        } else if line.starts_with("< PM_CONFIG") {
            default = field(line, "DEFAULT=").and_then(|v| v.parse().ok());
        }
    }
    (modes, default)
}

/// `nvpmodel -q` prints the mode's name, then its id on the last line.
pub fn parse_nvpmodel_query(text: &str) -> Option<u32> {
    text.lines().rev().find_map(|line| line.trim().parse().ok())
}

/// The INA3221 rail that feeds the whole module.
pub fn read_supply(hwmon: &Path) -> Option<Supply> {
    for entry in fs::read_dir(hwmon).ok()?.filter_map(|e| e.ok()) {
        let dir = entry.path();
        let name = fs::read_to_string(dir.join("name")).unwrap_or_default();
        if name.trim() != "ina3221" {
            continue;
        }
        for channel in 1..=4 {
            let label =
                fs::read_to_string(dir.join(format!("in{channel}_label"))).unwrap_or_default();
            if label.trim() != "VDD_IN" {
                continue;
            }
            let read = |file: String| -> Option<u64> {
                fs::read_to_string(dir.join(file)).ok()?.trim().parse().ok()
            };
            let millivolts = read(format!("in{channel}_input"))?;
            let milliamps = read(format!("curr{channel}_input"))?;
            return Some(Supply {
                millivolts,
                milliamps,
                milliwatts: millivolts * milliamps / 1000,
            });
        }
    }
    None
}

pub fn read_battery(power_supply: &Path) -> Option<Battery> {
    for entry in fs::read_dir(power_supply).ok()?.filter_map(|e| e.ok()) {
        let dir = entry.path();
        let kind = fs::read_to_string(dir.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let percent: u8 = fs::read_to_string(dir.join("capacity"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        return Some(Battery {
            percent: percent.min(100),
            charging: matches!(status.trim(), "Charging" | "Full"),
        });
    }
    None
}

/// Linux's thermal zones; a zone that cannot be read right now is left out.
fn thermal_zones() -> Vec<Temperature> {
    let Ok(entries) = fs::read_dir("/sys/class/thermal") else {
        return Vec::new();
    };
    let mut zones: Vec<Temperature> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
        .filter_map(|e| {
            let zone = fs::read_to_string(e.path().join("type")).ok()?;
            let milli: f64 = fs::read_to_string(e.path().join("temp"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            Some(Temperature {
                zone: zone.trim().to_string(),
                celsius: milli / 1000.0,
            })
        })
        .collect();
    zones.sort_by(|a, b| a.zone.cmp(&b.zone));
    zones
}

fn temperatures() -> Vec<Temperature> {
    let zones = thermal_zones();
    if !zones.is_empty() {
        return zones;
    }
    sysinfo::Components::new_with_refreshed_list()
        .iter()
        .filter_map(|c| {
            Some(Temperature {
                zone: c.label().to_string(),
                celsius: f64::from(c.temperature()?),
            })
        })
        .collect()
}

impl PowerHost {
    /// Whether `sudo` lets this account run the command without a password.
    fn may_run(&self, command: &Path, args: &[&str]) -> bool {
        Command::new(&self.sudo)
            .args(["-n", "-l"])
            .arg(command)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[async_trait]
impl PondAdministrator for HostPondAdministrator {
    async fn storage(&self) -> Result<StorageReport, AdminError> {
        let counts = self.counts().await;
        self.blocking(move |dir| {
            let area_root_bytes = area_roots()
                .iter()
                .map(|(_, root)| tree_usage(&join(&dir, root)).0)
                .collect();
            let databases = database_names()
                .iter()
                .map(|name| DatabaseUsage {
                    name: name.to_string(),
                    bytes: ["", "-wal", "-shm"]
                        .iter()
                        .map(|suffix| tree_usage(&dir.join(format!("{name}{suffix}"))).0)
                        .sum(),
                })
                .collect();
            Ok(StorageReport {
                disk: disk_space(&dir),
                data_dir_bytes: tree_usage(&dir).0,
                area_root_bytes,
                databases,
                counts,
            })
        })
        .await
    }

    async fn list_dir(&self, path: &DataPath) -> Result<Vec<DataEntry>, AdminError> {
        let path = path.clone();
        self.blocking(move |dir| {
            let target = resolve(&dir, &path)?;
            let meta = fs::symlink_metadata(&target).map_err(io)?;
            if meta.file_type().is_symlink() {
                return Err(AdminError::InvalidPath);
            }
            if !meta.is_dir() {
                return Err(AdminError::NotADirectory);
            }
            Ok(fs::read_dir(&target)
                .map_err(io)?
                .filter_map(|e| e.ok())
                // A name that is not UTF-8 cannot be named back in a request, so it is not offered.
                .filter_map(|e| entry_of(&e.path(), e.file_name().into_string().ok()?))
                .collect())
        })
        .await
    }

    async fn delete(&self, path: &DataPath) -> Result<u64, AdminError> {
        if path.is_root() {
            return Err(AdminError::InvalidPath);
        }
        let path = path.clone();
        self.blocking(move |dir| remove(&resolve(&dir, &path)?))
            .await
    }

    async fn caches(&self) -> Result<Vec<CacheUsage>, AdminError> {
        let protected = self.protected_names().await;
        let leftovers = hf_cleanup::preview_cleanup(&self.data_dir, &protected).await?;
        self.blocking(move |dir| {
            Ok(CacheId::ALL
                .iter()
                .map(|&id| {
                    let (bytes, files) = if id == CacheId::DownloadLeftovers {
                        (leftovers.reclaimed_bytes, leftovers.removed.len() as u64)
                    } else {
                        cache_paths(&dir, id)
                            .iter()
                            .map(|p| tree_usage(p))
                            .fold((0, 0), |(b, f), (pb, pf)| (b + pb, f + pf))
                    };
                    CacheUsage { id, bytes, files }
                })
                .collect())
        })
        .await
    }

    async fn clear_cache(&self, cache: CacheId) -> Result<u64, AdminError> {
        if cache == CacheId::DownloadLeftovers {
            let protected = self.protected_names().await;
            let report = hf_cleanup::run_cleanup(&self.data_dir, &protected).await?;
            return Ok(report.reclaimed_bytes);
        }
        self.blocking(move |dir| {
            let mut freed = 0u64;
            for path in cache_paths(&dir, cache) {
                match remove(&path) {
                    Ok(bytes) => freed += bytes,
                    // Gone already, or written again mid-clear: either way nothing is lost.
                    Err(e) => tracing::warn!(path = %path.display(), error = %e, "pond admin: cache entry kept"),
                }
            }
            Ok(freed)
        })
        .await
    }

    async fn health(&self) -> Result<HostHealth, AdminError> {
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut system = sysinfo::System::new();
            system.refresh_memory();
            let pid = sysinfo::get_current_pid().ok();
            if let Some(pid) = pid {
                system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
            }
            let load = sysinfo::System::load_average();
            HostHealth {
                host: HostInfo {
                    hostname: sysinfo::System::host_name().unwrap_or_default(),
                    os: sysinfo::System::long_os_version()
                        .unwrap_or_else(|| std::env::consts::OS.to_string()),
                    arch: sysinfo::System::cpu_arch(),
                    cpus: std::thread::available_parallelism()
                        .map(|n| n.get() as u32)
                        .unwrap_or(1),
                    uptime_secs: sysinfo::System::uptime(),
                    load: [load.one, load.five, load.fifteen],
                },
                memory: MemoryUsage {
                    total_bytes: system.total_memory(),
                    available_bytes: system.available_memory(),
                    swap_total_bytes: system.total_swap(),
                    swap_free_bytes: system.free_swap(),
                },
                disk: disk_space(&data_dir),
                temperatures: temperatures(),
                process: ProcessUsage {
                    rss_bytes: pid
                        .and_then(|pid| system.process(pid))
                        .map(|p| p.memory())
                        .unwrap_or(0),
                },
            }
        })
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))
    }

    async fn power(&self) -> Result<PowerState, AdminError> {
        let power = self.power.clone();
        tokio::task::spawn_blocking(move || {
            let supply = read_supply(&power.hwmon);
            let battery = read_battery(&power.power_supply);
            let conf = fs::read_to_string(&power.nvpmodel_conf).ok();
            let (Some(nvpmodel), Some(conf)) = (power.nvpmodel.as_ref(), conf) else {
                return PowerState {
                    control: PowerControl::None,
                    mode: None,
                    default_mode_id: None,
                    modes: Vec::new(),
                    can_change: false,
                    supply,
                    battery,
                };
            };
            let (modes, default_mode_id) = parse_nvpmodel_conf(&conf);
            let current = Command::new(nvpmodel)
                .arg("-q")
                .stdin(Stdio::null())
                .output()
                .ok()
                .and_then(|out| parse_nvpmodel_query(&String::from_utf8_lossy(&out.stdout)));
            PowerState {
                control: PowerControl::Nvpmodel,
                mode: current.and_then(|id| modes.iter().find(|m| m.id == id).cloned()),
                default_mode_id,
                can_change: power.may_run(nvpmodel, &["-m", "0"]),
                modes,
                supply,
                battery,
            }
        })
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))
    }

    async fn set_power_mode(&self, id: u32) -> Result<PowerModeSet, AdminError> {
        let state = self.power().await?;
        let mode = state
            .modes
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or(AdminError::UnknownPowerMode)?;
        let Some(nvpmodel) = self.power.nvpmodel.clone().filter(|_| state.can_change) else {
            return Err(AdminError::PowerControlUnavailable);
        };
        let sudo = self.power.sudo.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut child = Command::new(&sudo)
                .arg("-n")
                .arg(&nvpmodel)
                .args(["-m", &id.to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| AdminError::Internal(e.to_string()))?;
            // nvpmodel asks before rebooting into a mode that needs it; the phone asks instead.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"NO\n");
            }
            let out = child
                .wait_with_output()
                .map_err(|e| AdminError::Internal(e.to_string()))?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let reboot_required = text.to_ascii_lowercase().contains("reboot");
            if !out.status.success() && !reboot_required {
                return Err(AdminError::Internal(format!(
                    "nvpmodel -m {id} failed: {}",
                    text.trim()
                )));
            }
            Ok(PowerModeSet {
                mode,
                reboot_required,
            })
        })
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))?
    }

    async fn restart(&self) -> Result<(), AdminError> {
        let power = self.power.clone();
        tokio::task::spawn_blocking(move || {
            if !power.may_run(&power.systemctl, &["reboot"]) {
                return Err(AdminError::PowerControlUnavailable);
            }
            Command::new(&power.sudo)
                .arg("-n")
                .arg(&power.systemctl)
                .arg("reboot")
                .stdin(Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|e| AdminError::Internal(e.to_string()))
        })
        .await
        .map_err(|e| AdminError::Internal(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn host(dir: &Path) -> HostPondAdministrator {
        let db = crate::db::Database::init(dir).await.unwrap();
        let fixtures = dir.join("no-such-sys");
        HostPondAdministrator::new(
            dir,
            db.system.clone(),
            None,
            PowerHost {
                nvpmodel_conf: fixtures.join("nvpmodel.conf"),
                nvpmodel: None,
                sudo: "/usr/bin/false".into(),
                systemctl: "/usr/bin/false".into(),
                hwmon: fixtures.join("hwmon"),
                power_supply: fixtures.join("power_supply"),
            },
        )
    }

    fn write(dir: &Path, rel: &str, bytes: usize) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn path(raw: &str) -> DataPath {
        DataPath::parse(raw).unwrap()
    }

    #[tokio::test]
    async fn a_directory_lists_with_tree_sizes_and_never_follows_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        write(tmp.path(), "logs/a.log", 10);
        write(tmp.path(), "logs/old/b.log", 5);
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            write(outside.path(), "big", 1_000);
            std::os::unix::fs::symlink(outside.path(), tmp.path().join("logs/link")).unwrap();
        }
        let mut entries = admin.list_dir(&path("logs")).await.unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let shape: Vec<(&str, EntryKind, u64)> = entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind, e.bytes))
            .collect();
        #[cfg(unix)]
        assert_eq!(
            shape,
            [
                ("a.log", EntryKind::File, 10),
                ("link", EntryKind::Symlink, 0),
                ("old", EntryKind::Directory, 5),
            ]
        );
        assert_eq!(
            admin.list_dir(&path("logs/a.log")).await,
            Err(AdminError::NotADirectory)
        );
        assert_eq!(
            admin.list_dir(&path("nothing")).await,
            Err(AdminError::NotFound)
        );
        #[cfg(unix)]
        assert_eq!(
            admin.list_dir(&path("logs/link")).await,
            Err(AdminError::InvalidPath),
            "a symlinked directory is not entered"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deleting_through_or_at_a_link_never_touches_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "precious", 7);
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("attachments")).unwrap();

        assert_eq!(
            admin.delete(&path("attachments/precious")).await,
            Err(AdminError::InvalidPath)
        );
        assert_eq!(admin.delete(&path("attachments")).await, Ok(0));
        assert!(outside.path().join("precious").exists());
        assert!(!tmp.path().join("attachments").exists());
    }

    #[tokio::test]
    async fn deleting_returns_the_bytes_freed() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        write(tmp.path(), "snapshots/a.jpg", 3);
        write(tmp.path(), "snapshots/b.jpg", 4);
        write(tmp.path(), "old.bak", 9);
        assert_eq!(admin.delete(&path("old.bak")).await, Ok(9));
        assert_eq!(admin.delete(&path("snapshots")).await, Ok(7));
        assert_eq!(
            admin.delete(&path("snapshots")).await,
            Err(AdminError::NotFound)
        );
        assert_eq!(
            admin.delete(&DataPath::root()).await,
            Err(AdminError::InvalidPath)
        );
    }

    #[tokio::test]
    async fn clearing_kv_snapshots_keeps_a_suspended_tasks_state() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        write(tmp.path(), "engine/data/prompt-cache/v1-a.kv", 100);
        write(tmp.path(), "engine/data/prompt-cache/v1-a.index.json", 2);
        write(tmp.path(), "engine/data/prompt-cache/suspended/task.kv", 50);
        write(tmp.path(), "engine/data/litert-lm/kv-snapshots/s1", 30);
        write(tmp.path(), "engine/data/litert-lm/cache/model.bin", 70);
        write(tmp.path(), "cache/onnx-trt/sm87/engine", 20);

        let caches = admin.caches().await.unwrap();
        let usage = |id| caches.iter().find(|c| c.id == id).unwrap().clone();
        assert_eq!(
            (
                usage(CacheId::KvSnapshots).bytes,
                usage(CacheId::KvSnapshots).files
            ),
            (132, 3)
        );
        assert_eq!(usage(CacheId::AcceleratorCaches).bytes, 90);
        assert_eq!(usage(CacheId::DownloadLeftovers).bytes, 0);

        assert_eq!(admin.clear_cache(CacheId::KvSnapshots).await, Ok(132));
        assert!(tmp
            .path()
            .join("engine/data/prompt-cache/suspended/task.kv")
            .exists());
        assert!(tmp.path().join("engine/data/prompt-cache").is_dir());
        assert!(tmp
            .path()
            .join("engine/data/litert-lm/kv-snapshots")
            .is_dir());
        assert!(tmp
            .path()
            .join("engine/data/litert-lm/cache/model.bin")
            .exists());

        assert_eq!(admin.clear_cache(CacheId::AcceleratorCaches).await, Ok(90));
        assert!(tmp.path().join("cache/onnx-trt").is_dir());
        assert_eq!(admin.clear_cache(CacheId::AcceleratorCaches).await, Ok(0));
    }

    #[tokio::test]
    async fn storage_sizes_each_area_root_and_counts_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        write(tmp.path(), "logs/a.log", 10);
        write(tmp.path(), "hf_cache/hub/blob", 40);
        let report = admin.storage().await.unwrap();
        let roots = area_roots();
        let bytes_of = |root: &[&str]| {
            roots
                .iter()
                .position(|(_, r)| r.as_slice() == root)
                .map(|i| report.area_root_bytes[i])
        };
        assert_eq!(bytes_of(&["logs"]), Some(10));
        assert_eq!(bytes_of(&["hf_cache"]), Some(40));
        assert!(report.data_dir_bytes >= 50);
        assert!(report
            .databases
            .iter()
            .any(|d| d.name == "pond_system.db" && d.bytes > 0));
        assert_eq!(
            report.counts,
            RecordCounts::default(),
            "a fresh pond has no rows"
        );
        assert!(report.disk.total_bytes > 0);
    }

    #[tokio::test]
    async fn a_host_without_nvpmodel_reports_no_power_control() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        let power = admin.power().await.unwrap();
        assert_eq!(power.control, PowerControl::None);
        assert!(!power.can_change);
        assert_eq!(
            admin.set_power_mode(0).await,
            Err(AdminError::UnknownPowerMode)
        );
        assert_eq!(
            admin.restart().await,
            Err(AdminError::PowerControlUnavailable)
        );
    }

    #[test]
    fn the_orin_nanos_nvpmodel_conf_parses() {
        let conf = "# < POWER_MODEL ID=id_num NAME=mode_name >\n\
                    < POWER_MODEL ID=0 NAME=15W >\n\
                    CPU_ONLINE CORE_0 1\n\
                    < POWER_MODEL ID=1 NAME=25W >\n\
                    < POWER_MODEL ID=2 NAME=MAXN_SUPER >\n\
                    < POWER_MODEL ID=3 NAME=7W >\n\
                    # < PM_CONFIG DEFAULT=default_mode >\n\
                    < PM_CONFIG DEFAULT=1 >\n";
        let (modes, default) = parse_nvpmodel_conf(conf);
        let names: Vec<(u32, &str)> = modes.iter().map(|m| (m.id, m.name.as_str())).collect();
        assert_eq!(
            names,
            [(0, "15W"), (1, "25W"), (2, "MAXN_SUPER"), (3, "7W")]
        );
        assert_eq!(default, Some(1));
        assert_eq!(
            parse_nvpmodel_query("NV Power Mode: MAXN_SUPER\n2\n"),
            Some(2)
        );
        assert_eq!(parse_nvpmodel_query("NVPM ERROR: failed\n"), None);
    }

    #[test]
    fn the_supply_rail_and_a_battery_are_read_from_sysfs() {
        let tmp = tempfile::tempdir().unwrap();
        let fan = tmp.path().join("hwmon/hwmon0");
        let ina = tmp.path().join("hwmon/hwmon1");
        fs::create_dir_all(&fan).unwrap();
        fs::create_dir_all(&ina).unwrap();
        fs::write(fan.join("name"), "pwmfan\n").unwrap();
        fs::write(ina.join("name"), "ina3221\n").unwrap();
        fs::write(ina.join("in1_label"), "VDD_IN\n").unwrap();
        fs::write(ina.join("in1_input"), "4896\n").unwrap();
        fs::write(ina.join("curr1_input"), "3992\n").unwrap();
        fs::write(ina.join("in2_label"), "VDD_CPU_GPU_CV\n").unwrap();
        assert_eq!(
            read_supply(&tmp.path().join("hwmon")),
            Some(Supply {
                millivolts: 4896,
                milliamps: 3992,
                milliwatts: 19_544,
            })
        );
        assert_eq!(read_battery(&tmp.path().join("power_supply")), None);

        let bat = tmp.path().join("power_supply/BAT0");
        fs::create_dir_all(&bat).unwrap();
        fs::write(bat.join("type"), "Battery\n").unwrap();
        fs::write(bat.join("capacity"), "87\n").unwrap();
        fs::write(bat.join("status"), "Discharging\n").unwrap();
        assert_eq!(
            read_battery(&tmp.path().join("power_supply")),
            Some(Battery {
                percent: 87,
                charging: false
            })
        );
    }

    #[tokio::test]
    async fn health_reads_this_machine() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = host(tmp.path()).await;
        let health = admin.health().await.unwrap();
        assert!(health.memory.total_bytes > 0);
        assert!(health.host.cpus >= 1);
        assert!(health.process.rss_bytes > 0);
    }
}
