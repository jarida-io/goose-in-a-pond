//! An in-memory host: a tree of files with sizes, three caches and a Jetson's power modes.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::shared::domain::pond_admin::{
    area_roots, AdminError, CacheId, CacheUsage, DataEntry, DataPath, DiskSpace, EntryKind,
    HostHealth, HostInfo, MemoryUsage, PowerControl, PowerMode, PowerModeSet, PowerState,
    ProcessUsage, RecordCounts,
};
use crate::shared::ports::pond_admin::{PondAdministrator, StorageReport};

pub struct MockPondAdministrator {
    state: Mutex<State>,
}

struct State {
    /// Files by path; a directory is any proper prefix of one.
    files: BTreeMap<String, u64>,
    caches: BTreeMap<&'static str, u64>,
    mode: u32,
    can_change: bool,
    restarts: u32,
    deleted: Vec<String>,
}

impl Default for MockPondAdministrator {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPondAdministrator {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                files: BTreeMap::new(),
                caches: CacheId::ALL.iter().map(|id| (id.as_str(), 0)).collect(),
                mode: 1,
                can_change: true,
                restarts: 0,
                deleted: Vec::new(),
            }),
        }
    }

    pub fn with_file(self, path: &str, bytes: u64) -> Self {
        self.state.lock().unwrap().files.insert(path.into(), bytes);
        self
    }

    pub fn with_cache(self, id: CacheId, bytes: u64) -> Self {
        self.state.lock().unwrap().caches.insert(id.as_str(), bytes);
        self
    }

    pub fn without_power_control(self) -> Self {
        self.state.lock().unwrap().can_change = false;
        self
    }

    /// Every path handed to `delete`, in order.
    pub fn deleted(&self) -> Vec<String> {
        self.state.lock().unwrap().deleted.clone()
    }

    pub fn restarts(&self) -> u32 {
        self.state.lock().unwrap().restarts
    }

    fn modes() -> Vec<PowerMode> {
        [(0, "15W"), (1, "25W"), (2, "MAXN_SUPER"), (3, "7W")]
            .into_iter()
            .map(|(id, name)| PowerMode {
                id,
                name: name.into(),
            })
            .collect()
    }
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[async_trait]
impl PondAdministrator for MockPondAdministrator {
    async fn storage(&self) -> Result<StorageReport, AdminError> {
        let state = self.state.lock().unwrap();
        let total: u64 = state.files.values().sum();
        let area_root_bytes = area_roots()
            .iter()
            .map(|(_, root)| {
                let root = root.join("/");
                state
                    .files
                    .iter()
                    .filter(|(path, _)| **path == root || under(path, &root))
                    .map(|(_, bytes)| bytes)
                    .sum()
            })
            .collect();
        Ok(StorageReport {
            disk: DiskSpace {
                total_bytes: 1_000_000,
                free_bytes: 1_000_000 - total.min(1_000_000),
            },
            data_dir_bytes: total,
            area_root_bytes,
            databases: Vec::new(),
            counts: RecordCounts::default(),
        })
    }

    async fn list_dir(&self, path: &DataPath) -> Result<Vec<DataEntry>, AdminError> {
        let state = self.state.lock().unwrap();
        let dir = path.as_str();
        if state.files.contains_key(dir) {
            return Err(AdminError::NotADirectory);
        }
        let mut entries: BTreeMap<String, DataEntry> = BTreeMap::new();
        for (file, bytes) in state.files.iter().filter(|(file, _)| under(file, dir)) {
            let rest = if dir.is_empty() {
                file.as_str()
            } else {
                &file[dir.len() + 1..]
            };
            let (name, kind) = match rest.split_once('/') {
                Some((name, _)) => (name, EntryKind::Directory),
                None => (rest, EntryKind::File),
            };
            let entry = entries.entry(name.to_string()).or_insert(DataEntry {
                name: name.to_string(),
                kind,
                bytes: 0,
                modified_at: None,
            });
            entry.bytes += bytes;
        }
        if entries.is_empty() && !dir.is_empty() {
            return Err(AdminError::NotFound);
        }
        Ok(entries.into_values().collect())
    }

    async fn delete(&self, path: &DataPath) -> Result<u64, AdminError> {
        let mut state = self.state.lock().unwrap();
        let target = path.as_str().to_string();
        let doomed: Vec<String> = state
            .files
            .keys()
            .filter(|file| **file == target || under(file, &target))
            .cloned()
            .collect();
        if doomed.is_empty() {
            return Err(AdminError::NotFound);
        }
        let freed = doomed.iter().filter_map(|f| state.files.remove(f)).sum();
        state.deleted.push(target);
        Ok(freed)
    }

    async fn caches(&self) -> Result<Vec<CacheUsage>, AdminError> {
        let state = self.state.lock().unwrap();
        Ok(CacheId::ALL
            .iter()
            .map(|id| {
                let bytes = state.caches[id.as_str()];
                CacheUsage {
                    id: *id,
                    bytes,
                    files: u64::from(bytes > 0),
                }
            })
            .collect())
    }

    async fn clear_cache(&self, cache: CacheId) -> Result<u64, AdminError> {
        let mut state = self.state.lock().unwrap();
        Ok(std::mem::take(
            state.caches.get_mut(cache.as_str()).unwrap(),
        ))
    }

    async fn health(&self) -> Result<HostHealth, AdminError> {
        Ok(HostHealth {
            host: HostInfo {
                hostname: "pond".into(),
                os: "linux".into(),
                arch: "aarch64".into(),
                cpus: 6,
                uptime_secs: 3_600,
                load: [0.5, 0.4, 0.3],
            },
            memory: MemoryUsage {
                total_bytes: 8_000,
                available_bytes: 4_000,
                swap_total_bytes: 0,
                swap_free_bytes: 0,
            },
            disk: DiskSpace {
                total_bytes: 100,
                free_bytes: 50,
            },
            temperatures: Vec::new(),
            process: ProcessUsage { rss_bytes: 1_000 },
        })
    }

    async fn power(&self) -> Result<PowerState, AdminError> {
        let state = self.state.lock().unwrap();
        let modes = Self::modes();
        Ok(PowerState {
            control: PowerControl::Nvpmodel,
            mode: modes.iter().find(|m| m.id == state.mode).cloned(),
            default_mode_id: Some(1),
            modes,
            can_change: state.can_change,
            supply: None,
            battery: None,
        })
    }

    async fn set_power_mode(&self, id: u32) -> Result<PowerModeSet, AdminError> {
        let mut state = self.state.lock().unwrap();
        if !state.can_change {
            return Err(AdminError::PowerControlUnavailable);
        }
        let mode = Self::modes()
            .into_iter()
            .find(|m| m.id == id)
            .ok_or(AdminError::UnknownPowerMode)?;
        state.mode = id;
        Ok(PowerModeSet {
            mode,
            reboot_required: false,
        })
    }

    async fn restart(&self) -> Result<(), AdminError> {
        let mut state = self.state.lock().unwrap();
        if !state.can_change {
            return Err(AdminError::PowerControlUnavailable);
        }
        state.restarts += 1;
        Ok(())
    }
}
