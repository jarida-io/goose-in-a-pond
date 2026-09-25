//! The engine registry's rows, as the companion files (vision encoder, drafter) need to see and
//! edit them.
//!
//! One GGUF is registered under several ids -- the settings spelling, the canonical stem, and on
//! the Mac an owner/quant form -- and they share one engine slot, so whichever id loads first
//! decides what loads with it. Every decision about `mmproj_path` or `draft_model` is therefore
//! made for ALL the rows naming a file, and pond-core plans it over plain data
//! (`vision_encoder::stamp_plan`; `drafter::draft_reconcile_plan` too, while speculation was in
//! the engine). This module is the half that reads the rows out and writes the plan back.
//!
//! Two rules are structural here rather than remembered at each call site:
//!
//! * The registry is a std `Mutex` that is NOT reentrant, and `evict_model`,
//!   `ensure_drafter_registered` and the engine's `resolve_model_path` all take it themselves.
//!   Both methods take and drop the guard inside one synchronous call, so no caller can hold it
//!   across an `.await` or into one of those.
//! * An edit mutates through `list_models_mut` and saves ONCE. `update_model_settings` writes the
//!   whole `registry.json` under a file lock per call, which for a registry-wide clear is one
//!   full rewrite per row.

use goose::providers::local_inference::local_model_registry::{get_registry, LocalModelEntry};
use std::path::{Path, PathBuf};

/// What a plan needs to know about one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSnapshot {
    pub id: String,
    /// `local_path` resolved through symlinks, or as stored when it does not resolve, so two
    /// spellings of one file compare equal.
    pub resolved_path: PathBuf,
    pub mmproj_path: Option<PathBuf>,
    pub mmproj_size_bytes: u64,
    pub draft_model: Option<String>,
}

impl RowSnapshot {
    pub fn of(entry: &LocalModelEntry) -> Self {
        Self {
            id: entry.id.clone(),
            resolved_path: resolve(&entry.local_path),
            mmproj_path: entry.mmproj_path.clone(),
            mmproj_size_bytes: entry.mmproj_size_bytes,
            draft_model: entry.settings.draft_model.clone(),
        }
    }
}

/// A path resolved through symlinks, falling back to the path itself when it does not resolve
/// (a row for a file that is not there still has to be matched and planned for).
pub fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Where the rows live. The production store is goose's process-global registry; tests use an
/// in-memory one, because the global one saves to goose's data directory, which is only pinned
/// under the pond's own by `pond-server`.
pub trait RegistryRows: Send + Sync {
    /// Every row, read under the guard and returned without it.
    fn snapshot(&self) -> Vec<RowSnapshot>;

    /// Offer every row to `edit`, which returns whether it changed that row, and persist once
    /// when any did. Returns how many rows changed.
    fn edit(&self, edit: &mut dyn FnMut(&mut LocalModelEntry) -> bool) -> usize;
}

/// goose's registry, the one the engine resolves models through.
pub struct GooseRegistry;

impl RegistryRows for GooseRegistry {
    fn snapshot(&self) -> Vec<RowSnapshot> {
        // Copy the fields out, then drop the guard before resolving any path: canonicalize is a
        // syscall per row, and nothing needs the registry held for it.
        let rows: Vec<LocalModelEntry> = match get_registry().lock() {
            Ok(r) => r.list_models().to_vec(),
            Err(e) => {
                tracing::warn!("registry lock poisoned; reading no rows: {e}");
                return Vec::new();
            }
        };
        rows.iter().map(RowSnapshot::of).collect()
    }

    fn edit(&self, edit: &mut dyn FnMut(&mut LocalModelEntry) -> bool) -> usize {
        let mut registry = match get_registry().lock() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("registry lock poisoned; editing no rows: {e}");
                return 0;
            }
        };
        let mut changed = 0usize;
        for entry in registry.list_models_mut() {
            if edit(entry) {
                changed += 1;
            }
        }
        if changed > 0 {
            if let Err(e) = registry.save() {
                tracing::warn!(
                    "could not save the model registry after editing {changed} rows: {e}"
                );
            }
        }
        changed
    }
}

/// An in-memory registry for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryRows {
    pub rows: std::sync::Mutex<Vec<LocalModelEntry>>,
    /// How many times `edit` persisted, to pin "one save per plan".
    pub saves: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl MemoryRows {
    pub fn with(rows: Vec<LocalModelEntry>) -> Self {
        Self {
            rows: std::sync::Mutex::new(rows),
            saves: Default::default(),
        }
    }

    pub fn get(&self, id: &str) -> LocalModelEntry {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("no row {id}"))
    }
}

#[cfg(test)]
impl RegistryRows for MemoryRows {
    fn snapshot(&self) -> Vec<RowSnapshot> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .map(RowSnapshot::of)
            .collect()
    }

    fn edit(&self, edit: &mut dyn FnMut(&mut LocalModelEntry) -> bool) -> usize {
        let mut rows = self.rows.lock().unwrap();
        let mut changed = 0usize;
        for entry in rows.iter_mut() {
            if edit(entry) {
                changed += 1;
            }
        }
        if changed > 0 {
            self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        changed
    }
}

/// A registry row as `register_gguf_model` writes one, for tests.
#[cfg(test)]
pub fn test_entry(id: &str, local_path: &Path) -> LocalModelEntry {
    use goose::providers::local_inference::local_model_registry::{
        LocalModelStorage, ModelSettings,
    };
    LocalModelEntry {
        id: id.to_string(),
        repo_id: format!("local/{id}"),
        filename: local_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        quantization: String::new(),
        local_path: local_path.to_path_buf(),
        source_url: String::new(),
        backend_id: None,
        storage: LocalModelStorage::ManualPath,
        settings: ModelSettings::default(),
        size_bytes: 0,
        mmproj_path: None,
        mmproj_source_url: None,
        mmproj_size_bytes: 0,
        mmproj_checked: false,
        shard_files: vec![],
    }
}
