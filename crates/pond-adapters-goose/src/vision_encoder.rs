//! Vision-encoder (mmproj) resolution: the engine enables images only for a registry entry with
//! `mmproj_path`, which goose's featured lookup never sets for our bare stems, so we stamp it.
//! The encoder only arrives when the household asks for it, through the pond's download pipeline;
//! this module verifies what is on disk and attaches it, and never downloads.

use crate::registry_rows::{self, GooseRegistry, RegistryRows, RowSnapshot};
use goose::providers::local_inference::local_model_registry::LocalModelEntry;
use pond_core::models::domain::device_budget::{self, VisionDeclaration};
use pond_core::models::domain::litert;
use pond_core::models::domain::vision_encoder::{
    self as domain, EncoderSidecar, EncoderSpec, EncoderState, FailReason, OnDisk, RetryBackoff,
    RowChange, RowView, StampDecision,
};
use pond_core::shared::services::egress;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Max wait for the boot warm-up before hashing anyway: filling page cache under the cold GGUF
/// load causes NvMap error 12 / silent CPU fallback on the Orin.
const WARMUP_WAIT_CAP: Duration = Duration::from_secs(10 * 60);

// ── Paths ───────────────────────────────────────────────────────────────────
pub use domain::encoder_file;

/// The chat model's GGUF, symlinks resolved, so this and `apply_jetson_settings` read one file.
pub fn chat_gguf_path(data_dir: &Path, chat_model: &str) -> PathBuf {
    let gguf_dir = data_dir.join("models").join("gguf");
    let filename = crate::goose_agent::resolve_gguf_filename(chat_model, &gguf_dir);
    registry_rows::resolve(&gguf_dir.join(filename))
}

// ── Declaration ─────────────────────────────────────────────────────────────
/// Whether `chat_model` reads pictures on this device, as declared, not downloaded: it feeds
/// the KV-cached `<vision>` prompt section, so it may change only with the model or its file.
/// The file is read only when the pairing needs its header or the fit needs its weights.
pub fn declaration(data_dir: Option<&Path>, chat_model: &str) -> VisionDeclaration {
    if chat_model.trim().is_empty() || litert::is_litert_model(chat_model) {
        return VisionDeclaration::NotDeclared;
    }
    let gguf = data_dir.map(|dd| chat_gguf_path(dd, chat_model));
    device_budget::vision_declaration(gguf.as_deref(), chat_model)
}

/// [`declaration`] for a GGUF already in hand (resolved).
pub fn declaration_at(gguf: Option<&Path>, chat_model: &str) -> VisionDeclaration {
    device_budget::vision_declaration(gguf, chat_model)
}

/// A file's state from disk alone; a wrong file reads as `Absent`, since the fix is a download.
pub fn disk_state(file: &Path, spec: &EncoderSpec) -> EncoderState {
    match domain::encoder_on_disk(file, spec) {
        OnDisk::Verified { bytes } => EncoderState::Ready { bytes: Some(bytes) },
        OnDisk::Unverified { .. } => EncoderState::Verifying,
        OnDisk::Missing | OnDisk::Invalid(_) => EncoderState::Absent,
    }
}

/// Unix time in milliseconds, the unit `EncoderState::Failed` carries.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// ── The warm-up gate ────────────────────────────────────────────────────────
/// Lets a budgeted device's companion worker wait out the model load. Waits for a warm-up to
/// have finished, not merely none running: at boot the worker spawns before the warm-up starts.
#[derive(Default)]
pub struct WarmupGate {
    finished_once: AtomicBool,
    running: AtomicUsize,
    notify: tokio::sync::Notify,
}

/// Held for the length of one warm-up; dropping it ends it on every exit path.
pub struct WarmupRun<'a>(&'a WarmupGate);

impl Drop for WarmupRun<'_> {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
        self.0.finished_once.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }
}

impl WarmupGate {
    pub fn begin(&self) -> WarmupRun<'_> {
        self.running.fetch_add(1, Ordering::SeqCst);
        WarmupRun(self)
    }

    fn clear(&self) -> bool {
        self.finished_once.load(Ordering::SeqCst) && self.running.load(Ordering::SeqCst) == 0
    }

    /// Wait for the warm-up, at most [`WARMUP_WAIT_CAP`] (a pond may never warm up).
    pub async fn wait(&self) {
        let deadline = tokio::time::Instant::now() + WARMUP_WAIT_CAP;
        while !self.clear() && tokio::time::Instant::now() < deadline {
            let _ = tokio::time::timeout(Duration::from_secs(1), self.notify.notified()).await;
        }
    }
}

// ── Picture support ─────────────────────────────────────────────────────────

/// What is known about one encoder beyond what its file says.
#[derive(Default)]
struct DirState {
    /// Set while an ensure works on it or after the engine refused it; `None` defers to the disk.
    live: Option<EncoderState>,
    in_flight: bool,
    backoff: RetryBackoff,
}

/// Status map, in-flight set and backoff ladder for every encoder, shared process-wide.
pub struct PictureSupport {
    rows: Arc<dyn RegistryRows>,
    /// Set only by the serve process. Everything that hashes or renames checks it.
    provisioning: AtomicBool,
    dirs: Mutex<HashMap<&'static str, DirState>>,
    /// Held across each stamp decision and its application, so a stale "unverified" read can't
    /// clear a stamp an ensure just wrote.
    stamp_lock: Mutex<()>,
    warmup: Arc<WarmupGate>,
    /// Whether each pinned encoder hears, by sha256: a verified file's header never changes.
    hears: Mutex<HashMap<&'static str, bool>>,
}

impl Default for PictureSupport {
    fn default() -> Self {
        Self::with_rows(Arc::new(GooseRegistry))
    }
}

impl PictureSupport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Picture support over a given registry (tests pass an in-memory one).
    pub fn with_rows(rows: Arc<dyn RegistryRows>) -> Self {
        Self {
            rows,
            provisioning: AtomicBool::new(false),
            dirs: Mutex::new(HashMap::new()),
            stamp_lock: Mutex::new(()),
            warmup: Arc::new(WarmupGate::default()),
            hears: Mutex::new(HashMap::new()),
        }
    }

    /// Opt this process in to hashing and quarantining. The serve process only.
    pub fn enable_provisioning(&self) {
        if !self.provisioning.swap(true, Ordering::SeqCst) {
            tracing::info!(
                target: "giap::vision",
                "this process verifies and attaches picture add-ons on disk; it fetches none"
            );
        }
    }

    pub fn provisioning_enabled(&self) -> bool {
        self.provisioning.load(Ordering::SeqCst)
    }

    /// The warm-up gate, which the adapter's prewarm holds open while it loads.
    pub fn warmup(&self) -> &Arc<WarmupGate> {
        &self.warmup
    }

    fn with_dir<R>(&self, dir: &'static str, f: impl FnOnce(&mut DirState) -> R) -> R {
        let mut dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        f(dirs.entry(dir).or_default())
    }

    fn set_live(&self, dir: &'static str, state: Option<EncoderState>) {
        self.with_dir(dir, |d| d.live = state);
    }

    /// The live state, if it still applies; an expired, unretried failure defers to the disk.
    fn live_state(&self, dir: &'static str) -> Option<EncoderState> {
        let dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let d = dirs.get(dir)?;
        let live = d.live.clone()?;
        if let EncoderState::Failed {
            retry_at_unix_ms, ..
        } = &live
        {
            if !d.in_flight && now_ms() >= *retry_at_unix_ms {
                return None;
            }
        }
        Some(live)
    }

    /// Where picture support stands for `chat_model`; a pure read, cheap per model-list row.
    pub fn state(&self, data_dir: &Path, chat_model: &str) -> EncoderState {
        let declared = declaration(Some(data_dir), chat_model);
        if let Some(undeclared) = declared.undeclared_state() {
            return undeclared;
        }
        match declared.spec() {
            Some(spec) => self.state_of(data_dir, spec),
            None => EncoderState::NotDeclared,
        }
    }

    /// Where voice notes stand for `chat_model`: the picture encoder's state while it is not
    /// ready, then whether its header declares an audio tower the engine runs.
    pub fn audio_state(&self, data_dir: &Path, chat_model: &str) -> EncoderState {
        let declared = declaration(Some(data_dir), chat_model);
        if let Some(undeclared) = declared.undeclared_state() {
            return undeclared;
        }
        let Some(spec) = declared.spec() else {
            return EncoderState::NotDeclared;
        };
        let state = self.state_of(data_dir, spec);
        if !state.is_ready() || self.encoder_hears(&encoder_file(data_dir, spec), spec) {
            state
        } else {
            EncoderState::NotDeclared
        }
    }

    /// The encoder file that hears `chat_model`'s recordings, once it is ready.
    pub fn hearing_file(&self, data_dir: &Path, chat_model: &str) -> Option<PathBuf> {
        let declared = declaration(Some(data_dir), chat_model);
        let spec = declared.spec()?;
        self.audio_state(data_dir, chat_model)
            .is_ready()
            .then(|| encoder_file(data_dir, spec))
    }

    /// Reads the header once per pinned file (~85 KB); an unreadable one is retried next time.
    fn encoder_hears(&self, file: &Path, spec: &EncoderSpec) -> bool {
        if let Some(known) = self
            .hears
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(spec.sha256)
        {
            return *known;
        }
        let Some(layout) = pond_core::models::domain::gguf::parse_gguf_layout_file(file) else {
            return false;
        };
        let hears = pond_core::models::domain::audio_input::encoder_hears(&layout);
        self.hears
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec.sha256, hears);
        hears
    }

    /// [`Self::state`] for a declared encoder.
    pub fn state_of(&self, data_dir: &Path, spec: &EncoderSpec) -> EncoderState {
        if let Some(live) = self.live_state(spec.dir) {
            return live;
        }
        disk_state(&encoder_file(data_dir, spec), spec)
    }

    /// Provider-build step: stamp every row naming `gguf` only if its encoder is declared and
    /// verified (a stamp loads it eagerly), else clear. Returns the encoder if it is declared and
    /// not yet verified, for [`Self::spawn_settle`].
    pub fn settle_stamp(
        &self,
        data_dir: &Path,
        chat_model: &str,
        gguf: &Path,
    ) -> Option<EncoderSpec> {
        let _serial = self.stamp_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = registry_rows::resolve(gguf);
        let declared = declaration_at(Some(&target), chat_model);
        let (stamp, wants_work) = match declared {
            VisionDeclaration::Declared(spec) => {
                let file = encoder_file(data_dir, &spec);
                match domain::encoder_on_disk(&file, &spec) {
                    OnDisk::Verified { bytes } => (Some((file, bytes)), None),
                    _ => (None, Some(spec)),
                }
            }
            VisionDeclaration::NotDeclared | VisionDeclaration::NotOnThisDevice(_) => (None, None),
        };
        let rows = self.rows.snapshot();
        let views = views(&rows);
        let decision = match &stamp {
            Some((path, bytes)) => StampDecision::Stamp {
                mmproj_path: path,
                size_bytes: *bytes,
            },
            None => StampDecision::Clear,
        };
        let plan = domain::stamp_plan(&views, &target, decision);
        let changed = self.apply(&plan, declared.spec());
        if changed > 0 {
            tracing::info!(
                target: "giap::vision",
                model = %chat_model,
                gguf = %target.display(),
                declaration = ?declared_kind(&declared),
                stamped = stamp.is_some(),
                rows = changed,
                "picture support {} on every row naming this model's file",
                if stamp.is_some() { "attached" } else { "cleared" }
            );
        }
        wants_work
    }

    /// Multimodal init failed for `spec`: report could-not-start for one backoff step.
    pub fn mark_could_not_start(&self, spec: &EncoderSpec) {
        let mode = egress::network_mode();
        let wait = self.with_dir(spec.dir, |d| {
            let wait = d.backoff.on_failure(mode.as_str());
            d.live = Some(EncoderState::Failed {
                reason: FailReason::CouldNotStart,
                retry_at_unix_ms: now_ms().saturating_add(wait.as_millis() as u64),
            });
            wait
        });
        tracing::warn!(
            target: "giap::vision",
            encoder = spec.dir,
            retry_in_s = wait.as_secs(),
            "the engine could not start picture support for this model; images are refused until \
             the retry time"
        );
    }

    /// Check `chat_model`'s add-on on disk in the background: hash an unverified file once, set a
    /// wrong one aside, attach a good one. Never downloads; a no-op without a Tokio runtime.
    pub fn spawn_settle(self: &Arc<Self>, data_dir: &Path, chat_model: &str) {
        if !self.provisioning_enabled() {
            return;
        }
        let VisionDeclaration::Declared(spec) = declaration(Some(data_dir), chat_model) else {
            return;
        };
        if self.state_of(data_dir, &spec).is_ready() {
            // Verified on disk: attach it now, in case it arrived after the last provider build.
            self.stamp_declared(&spec, &encoder_file(data_dir, &spec));
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let claimed = self.with_dir(spec.dir, |d| !std::mem::replace(&mut d.in_flight, true));
        if !claimed {
            return;
        }
        handle.spawn(self.clone().settle(data_dir.to_path_buf(), spec));
    }

    /// One settle, holding the in-flight claim until it ends.
    async fn settle(self: Arc<Self>, data_dir: PathBuf, spec: EncoderSpec) {
        struct Flight<'a>(&'a PictureSupport, &'static str);
        impl Drop for Flight<'_> {
            fn drop(&mut self) {
                self.0.with_dir(self.1, |d| d.in_flight = false);
            }
        }
        let _flight = Flight(&self, spec.dir);

        if device_budget::budgeted_device() {
            self.warmup.wait().await;
        }
        match self.settle_once(&data_dir, &spec).await {
            Ok(Some(bytes)) => {
                self.with_dir(spec.dir, |d| {
                    d.live = None;
                    d.backoff.on_success();
                });
                tracing::info!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    bytes,
                    "picture support is ready"
                );
            }
            Ok(None) => self.set_live(spec.dir, None),
            Err(e) => {
                self.set_live(spec.dir, None);
                tracing::warn!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    error = %format!("{e:#}"),
                    "could not check the picture support on disk"
                );
            }
        }
    }

    /// One pass: validate, repair, stamp. `Ok(None)` when nothing usable is on disk. Validation
    /// sends nothing, so an offline pond with a good encoder is never shown as blocked.
    async fn settle_once(
        &self,
        data_dir: &Path,
        spec: &EncoderSpec,
    ) -> anyhow::Result<Option<u64>> {
        let file = encoder_file(data_dir, spec);
        match domain::encoder_on_disk(&file, spec) {
            OnDisk::Verified { bytes } => {
                self.stamp_declared(spec, &file);
                Ok(Some(bytes))
            }
            OnDisk::Unverified { bytes } => {
                self.set_live(spec.dir, Some(EncoderState::Verifying));
                tracing::info!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    file = %file.display(),
                    "checking the encoder on disk against its pinned sha256 before its first use"
                );
                let sha = sha256_file(&file).await?;
                if sha.eq_ignore_ascii_case(spec.sha256) {
                    write_sidecar(&file, &sha)?;
                    self.stamp_declared(spec, &file);
                    return Ok(Some(bytes));
                }
                tracing::warn!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    found = %sha,
                    expected = spec.sha256,
                    "the encoder on disk has the right shape but not the pinned bytes"
                );
                self.quarantine(data_dir, &file)?;
                Ok(None)
            }
            OnDisk::Invalid(why) => {
                tracing::warn!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    file = %file.display(),
                    reason = %why,
                    "the file at the encoder's path is not the pinned encoder"
                );
                self.quarantine(data_dir, &file)?;
                Ok(None)
            }
            OnDisk::Missing => Ok(None),
        }
    }

    /// Set a bad encoder aside (renamed, never deleted) and clear the rows naming it. A cache
    /// link has its blob renamed, or the refetch fast path would return the same bad blob.
    fn quarantine(&self, data_dir: &Path, file: &Path) -> anyhow::Result<()> {
        let hub = pond_hf_cache::HfCache::new(data_dir).hub_dir();
        let moved = set_aside(&hub, file)?;
        let cleared = {
            let _serial = self.stamp_lock.lock().unwrap_or_else(|e| e.into_inner());
            let rows = self.rows.snapshot();
            let plan = domain::unstamp_plan(&views(&rows), file);
            self.apply(&plan, None)
        };
        tracing::warn!(
            target: "giap::vision",
            file = %file.display(),
            moved_to = %moved.as_deref().map(|p| p.display().to_string()).unwrap_or_default(),
            rows_cleared = cleared,
            "set aside an encoder that is not the pinned file"
        );
        Ok(())
    }

    /// Stamp every row whose GGUF declares `spec` on this device, now that `file` verifies.
    fn stamp_declared(&self, spec: &EncoderSpec, file: &Path) -> usize {
        let _serial = self.stamp_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Re-read under the lock: the verdict stamped must be the one that holds now.
        let OnDisk::Verified { bytes } = domain::encoder_on_disk(file, spec) else {
            return 0;
        };
        let rows = self.rows.snapshot();
        let views = views(&rows);
        let targets: BTreeSet<&Path> = rows
            .iter()
            .filter(|r| row_declares(r, spec))
            .map(|r| r.resolved_path.as_path())
            .collect();
        let plan: Vec<RowChange> = targets
            .into_iter()
            .flat_map(|target| {
                domain::stamp_plan(
                    &views,
                    target,
                    StampDecision::Stamp {
                        mmproj_path: file,
                        size_bytes: bytes,
                    },
                )
            })
            .collect();
        let changed = self.apply(&plan, Some(spec));
        if changed > 0 {
            tracing::info!(
                target: "giap::vision",
                encoder = spec.dir,
                rows = changed,
                "picture support attached to every row whose model reads pictures here"
            );
        }
        changed
    }

    /// Apply a plan to the registry: one pass, one save.
    fn apply(&self, plan: &[RowChange], spec: Option<&EncoderSpec>) -> usize {
        if plan.is_empty() {
            return 0;
        }
        let by_id: HashMap<&str, &RowChange> = plan
            .iter()
            .map(|c| match c {
                RowChange::Stamp { id, .. } | RowChange::Clear { id } => (id.as_str(), c),
            })
            .collect();
        self.rows
            .edit(&mut |entry| match by_id.get(entry.id.as_str()) {
                Some(RowChange::Stamp {
                    mmproj_path,
                    size_bytes,
                    ..
                }) => stamp_entry(entry, mmproj_path, *size_bytes, spec),
                Some(RowChange::Clear { .. }) => clear_entry(entry),
                None => false,
            })
    }
}

fn declared_kind(d: &VisionDeclaration) -> &'static str {
    match d {
        VisionDeclaration::NotDeclared => "not_declared",
        VisionDeclaration::NotOnThisDevice(_) => "not_on_this_device",
        VisionDeclaration::Declared(_) => "declared",
    }
}

/// Borrowed views of owned snapshots, for the pond-core planners.
fn views(rows: &[RowSnapshot]) -> Vec<RowView<'_>> {
    rows.iter()
        .map(|r| RowView {
            id: &r.id,
            resolved_path: &r.resolved_path,
            mmproj_path: r.mmproj_path.as_deref(),
            mmproj_size_bytes: r.mmproj_size_bytes,
        })
        .collect()
}

/// Whether `row`'s model declares `spec` here, by its id or by its file's own name.
fn row_declares(row: &RowSnapshot, spec: &EncoderSpec) -> bool {
    let file_name = row
        .resolved_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    [row.id.as_str(), file_name].into_iter().any(|name| {
        matches!(
            declaration_at(Some(&row.resolved_path), name),
            VisionDeclaration::Declared(s) if s.dir == spec.dir
        )
    })
}

fn stamp_entry(
    entry: &mut LocalModelEntry,
    path: &Path,
    size: u64,
    spec: Option<&EncoderSpec>,
) -> bool {
    let before = (
        entry.mmproj_path.clone(),
        entry.mmproj_size_bytes,
        entry.settings.mmproj_size_bytes,
        entry.settings.vision_capable,
    );
    entry.mmproj_path = Some(path.to_path_buf());
    entry.mmproj_size_bytes = size;
    entry.mmproj_checked = true;
    entry.settings.vision_capable = true;
    // The engine subtracts this from its context-memory budget before sizing the KV cache.
    entry.settings.mmproj_size_bytes = size;
    if let Some(spec) = spec {
        entry.mmproj_source_url = Some(format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            spec.repo, spec.revision, spec.filename
        ));
    }
    before
        != (
            entry.mmproj_path.clone(),
            entry.mmproj_size_bytes,
            entry.settings.mmproj_size_bytes,
            entry.settings.vision_capable,
        )
}

fn clear_entry(entry: &mut LocalModelEntry) -> bool {
    let had = entry.mmproj_path.is_some()
        || entry.mmproj_size_bytes != 0
        || entry.settings.mmproj_size_bytes != 0
        || entry.settings.vision_capable;
    entry.mmproj_path = None;
    entry.mmproj_source_url = None;
    entry.mmproj_size_bytes = 0;
    entry.settings.mmproj_size_bytes = 0;
    entry.settings.vision_capable = false;
    had
}

// ── Files ───────────────────────────────────────────────────────────────────

/// A file's sha256, hashed off the async executor.
pub async fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|e| anyhow::anyhow!("the hash did not finish: {e}"))?
}

/// Record that `file` (as it is now: size and mtime) hashed to `sha`.
fn write_sidecar(file: &Path, sha: &str) -> anyhow::Result<()> {
    let meta = std::fs::metadata(file)?;
    let mtime = domain::mtime_unix(&meta)
        .ok_or_else(|| anyhow::anyhow!("{} has no readable mtime", file.display()))?;
    let sidecar = EncoderSidecar::new(sha, meta.len(), mtime);
    let path = domain::sidecar_path(file);
    let staged = path.with_extension("verified.tmp");
    std::fs::write(&staged, sidecar.to_json())?;
    std::fs::rename(&staged, &path)?;
    Ok(())
}

/// Move `file`'s bytes aside and return where they went. A cache link's blob is renamed; a link
/// elsewhere is only removed (`None`). The `.verified` sidecar is removed either way.
fn set_aside(hub: &Path, file: &Path) -> anyhow::Result<Option<PathBuf>> {
    let meta = std::fs::symlink_metadata(file)?;
    let moved = if meta.file_type().is_symlink() {
        let blob = std::fs::canonicalize(file)
            .ok()
            .filter(|target| is_hf_blob(hub, target));
        let moved = match blob {
            Some(blob) => Some(move_aside(&blob)?),
            None => None,
        };
        // The link is a pointer drawn by the cache or by this module; the bytes moved above.
        std::fs::remove_file(file)?;
        moved
    } else {
        Some(move_aside(file)?)
    };
    let _ = std::fs::remove_file(domain::sidecar_path(file));
    Ok(moved)
}

/// Whether `path` (resolved) is a blob of the cache whose hub directory is `hub`.
fn is_hf_blob(hub: &Path, path: &Path) -> bool {
    path.starts_with(registry_rows::resolve(hub))
        && path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "blobs")
}

/// Rename `path` to the first free `<name>.invalid[.N]`, never over an earlier quarantined copy.
fn move_aside(path: &Path) -> anyhow::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", path.display()))?;
    let mut n = 0u32;
    loop {
        let candidate = if n == 0 {
            path.with_file_name(format!("{name}.invalid"))
        } else {
            path.with_file_name(format!("{name}.invalid.{n}"))
        };
        if std::fs::symlink_metadata(&candidate).is_err() {
            std::fs::rename(path, &candidate)?;
            return Ok(candidate);
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry_rows::{test_entry, MemoryRows};

    /// A GGUF encoder header with one F32 tensor ending at `size`, kept sparse via `set_len`.
    fn write_encoder(path: &Path, projector: &str, projection_dim: u32, size: u64) {
        write_encoder_with(path, projector, projection_dim, size, None);
    }

    /// `audio`: the `clip.audio.projector_type` of a tower the file declares, if any.
    fn write_encoder_with(
        path: &Path,
        projector: &str,
        projection_dim: u32,
        size: u64,
        audio: Option<&str>,
    ) {
        fn raw_string(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        fn kv_str(kvs: &mut Vec<u8>, k: &str, v: &str) {
            raw_string(kvs, k);
            kvs.extend_from_slice(&8u32.to_le_bytes());
            raw_string(kvs, v);
        }
        let mut kvs = Vec::new();
        kv_str(&mut kvs, "general.architecture", "clip");
        kv_str(&mut kvs, "clip.vision.projector_type", projector);
        raw_string(&mut kvs, "clip.has_vision_encoder");
        kvs.extend_from_slice(&7u32.to_le_bytes());
        kvs.push(1);
        raw_string(&mut kvs, "clip.vision.projection_dim");
        kvs.extend_from_slice(&4u32.to_le_bytes());
        kvs.extend_from_slice(&projection_dim.to_le_bytes());
        let mut kv_count = 4u64;
        if let Some(audio) = audio {
            raw_string(&mut kvs, "clip.has_audio_encoder");
            kvs.extend_from_slice(&7u32.to_le_bytes());
            kvs.push(1);
            kv_str(&mut kvs, "clip.audio.projector_type", audio);
            kv_count += 2;
        }

        let header_len = |elements: u64| {
            let mut out = Vec::new();
            out.extend_from_slice(b"GGUF");
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&1u64.to_le_bytes());
            out.extend_from_slice(&kv_count.to_le_bytes());
            out.extend_from_slice(&kvs);
            raw_string(&mut out, "v.weight");
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&elements.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // F32
            out.extend_from_slice(&0u64.to_le_bytes()); // offset
            out
        };
        // The header's length does not depend on the element count's value, only its width.
        let data_start = (header_len(0).len() as u64).div_ceil(32) * 32;
        let elements = (size - data_start) / 4;
        let header = header_len(elements);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &header).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(size)
            .unwrap();
    }

    fn write_valid_encoder(data_dir: &Path, spec: &EncoderSpec) -> PathBuf {
        let path = domain::encoder_path(data_dir, spec);
        write_encoder(&path, spec.projector, spec.projection_dim, spec.size_bytes);
        assert_eq!(
            domain::validate_encoder_header(&path, spec),
            Ok(spec.size_bytes),
            "the fixture must pass the header check it stands in for"
        );
        path
    }

    /// A sidecar vouching for `file` unhashed; zero-filled test files never match the pinned sha.
    fn vouch(file: &Path, spec: &EncoderSpec) {
        let meta = std::fs::metadata(file).unwrap();
        let sidecar =
            EncoderSidecar::new(spec.sha256, meta.len(), domain::mtime_unix(&meta).unwrap());
        std::fs::write(domain::sidecar_path(file), sidecar.to_json()).unwrap();
    }

    fn spec(dir: &str) -> EncoderSpec {
        domain::encoder_by_dir(dir).unwrap()
    }

    /// Declarations are by name only off a budgeted device; under `scripts/jetson-emu.sh test`
    /// the Orin's measured list is empty, so tests assert that instead.
    fn budgeted() -> bool {
        device_budget::budgeted_device()
    }

    fn gguf(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join("models").join("gguf").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"GGUF weights stand-in").unwrap();
        path
    }

    #[test]
    fn a_text_only_model_reports_not_declared_with_no_file_io() {
        let tmp = tempfile::tempdir().unwrap();
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        for model in [
            "Llama-3.2-3B-Instruct-Q4_K_M",
            "mtp-gemma-4-E2B-it",
            "gemma-4-E1B-it",
        ] {
            assert_eq!(pictures.state(tmp.path(), model), EncoderState::NotDeclared);
        }
    }

    #[test]
    fn the_state_follows_the_file_absent_then_verifying_then_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        let model = "gemma-4-E2B-it-Q4_K_M";
        if budgeted() {
            assert_eq!(
                pictures.state(tmp.path(), model),
                EncoderState::NotOnThisDevice
            );
            return;
        }
        assert_eq!(pictures.state(tmp.path(), model), EncoderState::Absent);

        let e2b = spec("gemma-4-e2b-it");
        let file = write_valid_encoder(tmp.path(), &e2b);
        assert_eq!(
            pictures.state(tmp.path(), model),
            EncoderState::Verifying,
            "a header-valid file nobody has hashed is not ready: qat and non-qat pass the header alike"
        );
        vouch(&file, &e2b);
        assert_eq!(
            pictures.state(tmp.path(), model),
            EncoderState::Ready {
                bytes: Some(e2b.size_bytes)
            }
        );
    }

    #[test]
    fn a_voice_note_waits_for_the_encoder_then_needs_its_audio_tower() {
        let tmp = tempfile::tempdir().unwrap();
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        let model = "gemma-4-E2B-it-Q4_K_M";
        assert_eq!(
            pictures.audio_state(tmp.path(), "Llama-3.2-3B-Instruct-Q4_K_M"),
            EncoderState::NotDeclared
        );
        if budgeted() {
            assert_eq!(
                pictures.audio_state(tmp.path(), model),
                EncoderState::NotOnThisDevice
            );
            return;
        }
        assert_eq!(
            pictures.audio_state(tmp.path(), model),
            EncoderState::Absent
        );

        let e2b = spec("gemma-4-e2b-it");
        let file = domain::encoder_path(tmp.path(), &e2b);
        write_encoder_with(
            &file,
            e2b.projector,
            e2b.projection_dim,
            e2b.size_bytes,
            Some("gemma4a"),
        );
        assert_eq!(
            pictures.audio_state(tmp.path(), model),
            EncoderState::Verifying
        );
        vouch(&file, &e2b);
        assert_eq!(
            pictures.audio_state(tmp.path(), model),
            EncoderState::Ready {
                bytes: Some(e2b.size_bytes)
            }
        );
    }

    #[test]
    fn a_ready_encoder_with_no_audio_tower_never_hears() {
        let tmp = tempfile::tempdir().unwrap();
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        let model = "gemma-4-E2B-it-Q4_K_M";
        if budgeted() {
            return;
        }
        let e2b = spec("gemma-4-e2b-it");
        let file = write_valid_encoder(tmp.path(), &e2b);
        vouch(&file, &e2b);
        assert!(pictures.state(tmp.path(), model).is_ready());
        assert_eq!(
            pictures.audio_state(tmp.path(), model),
            EncoderState::NotDeclared,
            "pictures work, but the header declares nothing that hears"
        );
    }

    #[test]
    fn a_truncated_encoder_reads_as_absent_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        if budgeted() {
            return;
        }
        let e2b = spec("gemma-4-e2b-it");
        let path = domain::encoder_path(tmp.path(), &e2b);
        write_encoder(&path, e2b.projector, e2b.projection_dim, e2b.size_bytes);
        // The Orin's 64.5% E2B file: the right header, cut short.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(636_790_074)
            .unwrap();
        vouch(&path, &e2b);
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        assert_eq!(
            pictures.state(tmp.path(), "gemma-4-E2B-it-Q4_K_M"),
            EncoderState::Absent,
            "even a sidecar cannot vouch for a file shorter than its own tensor table"
        );
    }

    /// qat and non-qat encoders are different files of the same size.
    #[test]
    fn everything_is_keyed_by_the_encoder_dir_not_the_family() {
        let tmp = tempfile::tempdir().unwrap();
        if budgeted() {
            return;
        }
        let non_qat = spec("gemma-4-e2b-it");
        let file = write_valid_encoder(tmp.path(), &non_qat);
        vouch(&file, &non_qat);

        let qat_gguf = gguf(tmp.path(), "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf");
        let rows = Arc::new(MemoryRows::with(vec![
            test_entry("gemma-4-E2B-it-qat", &qat_gguf),
            test_entry("gemma-4-E2B-it-qat-UD-Q4_K_XL", &qat_gguf),
        ]));
        let pictures = PictureSupport::with_rows(rows.clone());

        assert_eq!(
            pictures.state(tmp.path(), "gemma-4-E2B-it-qat-UD-Q4_K_XL"),
            EncoderState::Absent
        );
        assert!(pictures
            .state(tmp.path(), "gemma-4-E2B-it-Q4_K_M")
            .is_ready());

        let wants = pictures.settle_stamp(tmp.path(), "gemma-4-E2B-it-qat-UD-Q4_K_XL", &qat_gguf);
        assert_eq!(wants.map(|s| s.dir), Some("gemma-4-e2b-it-qat"));
        for id in ["gemma-4-E2B-it-qat", "gemma-4-E2B-it-qat-UD-Q4_K_XL"] {
            assert_eq!(
                rows.get(id).mmproj_path,
                None,
                "{id} must not carry the non-qat projector"
            );
        }

        // A live download of one dir never reads as the other's progress.
        pictures.set_live(
            "gemma-4-e2b-it-qat",
            Some(EncoderState::Downloading { done: 5, total: 9 }),
        );
        assert!(pictures
            .state(tmp.path(), "gemma-4-E2B-it-Q4_K_M")
            .is_ready());
        assert_eq!(
            pictures.state(tmp.path(), "gemma-4-E2B-it-qat"),
            EncoderState::Downloading { done: 5, total: 9 }
        );
    }

    #[test]
    fn the_provider_build_stamps_and_clears_every_row_naming_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        if budgeted() {
            return;
        }
        let e2b = spec("gemma-4-e2b-it");
        let encoder = write_valid_encoder(tmp.path(), &e2b);
        vouch(&encoder, &e2b);

        let weights = gguf(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        let link = tmp.path().join("models").join("gguf").join("alias.gguf");
        std::os::unix::fs::symlink(&weights, &link).unwrap();
        let e4b = gguf(tmp.path(), "gemma-4-E4B-it-Q4_K_M.gguf");
        let rows = Arc::new(MemoryRows::with(vec![
            test_entry("gemma-4-E2B-it", &weights),
            test_entry("gemma-4-E2B-it-Q4_K_M", &weights),
            test_entry("unsloth/gemma-4-E2B-it-GGUF:Q4_K_M", &link),
            test_entry("gemma-4-E4B-it", &e4b),
        ]));
        let pictures = PictureSupport::with_rows(rows.clone());

        assert!(pictures
            .settle_stamp(tmp.path(), "gemma-4-E2B-it-Q4_K_M", &weights)
            .is_none());
        for id in [
            "gemma-4-E2B-it",
            "gemma-4-E2B-it-Q4_K_M",
            "unsloth/gemma-4-E2B-it-GGUF:Q4_K_M",
        ] {
            let row = rows.get(id);
            assert_eq!(row.mmproj_path.as_deref(), Some(encoder.as_path()), "{id}");
            assert_eq!(row.mmproj_size_bytes, e2b.size_bytes, "{id}");
            assert_eq!(
                row.settings.mmproj_size_bytes, e2b.size_bytes,
                "{id}: settings copy"
            );
            assert!(row.settings.vision_capable, "{id}");
        }
        assert_eq!(rows.get("gemma-4-E4B-it").mmproj_path, None);
        assert_eq!(rows.saves.load(Ordering::SeqCst), 1, "one plan, one save");

        // Settling again changes nothing and saves nothing.
        pictures.settle_stamp(tmp.path(), "gemma-4-E2B-it", &weights);
        assert_eq!(rows.saves.load(Ordering::SeqCst), 1);

        // Losing the sidecar (a file replaced in place) clears every row and asks for a re-verify.
        std::fs::remove_file(domain::sidecar_path(&encoder)).unwrap();
        let wants = pictures.settle_stamp(tmp.path(), "gemma-4-E2B-it-Q4_K_M", &weights);
        assert_eq!(wants.map(|s| s.dir), Some("gemma-4-e2b-it"));
        for id in [
            "gemma-4-E2B-it",
            "gemma-4-E2B-it-Q4_K_M",
            "unsloth/gemma-4-E2B-it-GGUF:Q4_K_M",
        ] {
            let row = rows.get(id);
            assert_eq!(row.mmproj_path, None, "{id}");
            assert_eq!(row.mmproj_size_bytes, 0, "{id}");
            assert_eq!(row.settings.mmproj_size_bytes, 0, "{id}");
            assert!(!row.settings.vision_capable, "{id}");
        }
        assert_eq!(rows.saves.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_model_that_declares_no_encoder_has_any_stale_stamp_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        let weights = gguf(tmp.path(), "Llama-3.2-3B-Instruct-Q4_K_M.gguf");
        let mut row = test_entry("Llama-3.2-3B-Instruct", &weights);
        row.mmproj_path = Some(tmp.path().join("stale.gguf"));
        row.mmproj_size_bytes = 7;
        let rows = Arc::new(MemoryRows::with(vec![row]));
        let pictures = PictureSupport::with_rows(rows.clone());
        assert!(pictures
            .settle_stamp(tmp.path(), "Llama-3.2-3B-Instruct", &weights)
            .is_none());
        assert_eq!(rows.get("Llama-3.2-3B-Instruct").mmproj_path, None);
    }

    #[test]
    fn a_mixed_case_encoder_dir_is_used_rather_than_shadowed() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        let mixed = tmp
            .path()
            .join("models")
            .join("mmproj")
            .join("gemma-4-E2B-it")
            .join(e2b.filename);
        std::fs::create_dir_all(mixed.parent().unwrap()).unwrap();
        std::fs::write(&mixed, b"x").unwrap();
        let found = encoder_file(tmp.path(), &e2b);
        // Case-insensitive or not, the lookup lands on the same file.
        assert_eq!(
            std::fs::canonicalize(&found).unwrap(),
            std::fs::canonicalize(&mixed).unwrap()
        );
    }

    #[test]
    fn quarantine_renames_a_regular_file_and_unstamps_the_rows_that_used_it() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        let encoder = domain::encoder_path(tmp.path(), &e2b);
        std::fs::create_dir_all(encoder.parent().unwrap()).unwrap();
        std::fs::write(&encoder, b"truncated").unwrap();
        std::fs::write(domain::sidecar_path(&encoder), b"{}").unwrap();

        let weights = gguf(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        let mut stamped = test_entry("gemma-4-E2B-it", &weights);
        stamped.mmproj_path = Some(encoder.clone());
        stamped.mmproj_size_bytes = 9;
        let other = test_entry("Llama-3.2-3B-Instruct", &gguf(tmp.path(), "llama.gguf"));
        let rows = Arc::new(MemoryRows::with(vec![stamped, other]));
        let pictures = PictureSupport::with_rows(rows.clone());

        pictures.quarantine(tmp.path(), &encoder).unwrap();
        assert!(!encoder.exists());
        assert_eq!(
            std::fs::read(encoder.with_file_name("mmproj-BF16.gguf.invalid")).unwrap(),
            b"truncated",
            "set aside, never deleted"
        );
        assert!(!domain::sidecar_path(&encoder).exists());
        assert_eq!(rows.get("gemma-4-E2B-it").mmproj_path, None);

        // A second bad copy does not overwrite the first one set aside.
        std::fs::write(&encoder, b"second").unwrap();
        pictures.quarantine(tmp.path(), &encoder).unwrap();
        assert_eq!(
            std::fs::read(encoder.with_file_name("mmproj-BF16.gguf.invalid")).unwrap(),
            b"truncated"
        );
        assert_eq!(
            std::fs::read(encoder.with_file_name("mmproj-BF16.gguf.invalid.1")).unwrap(),
            b"second"
        );
    }

    /// Renaming only the link would let the refetch's fast path return the same bad blob.
    #[test]
    fn quarantine_sets_aside_the_blob_behind_a_cache_link() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        // Built by hand under the tempdir so a developer's `HF_HOME` is never written.
        let hub = tmp.path().join("hf_cache").join("hub");
        let blob = hub
            .join("models--unsloth--gemma-4-E2B-it-GGUF")
            .join("blobs")
            .join(e2b.sha256);
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"bad bytes").unwrap();
        let encoder = domain::encoder_path(tmp.path(), &e2b);
        std::fs::create_dir_all(encoder.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&blob, &encoder).unwrap();

        let moved = set_aside(&hub, &encoder)
            .unwrap()
            .expect("the blob was moved");
        assert!(
            std::fs::symlink_metadata(&encoder).is_err(),
            "the link is gone"
        );
        assert!(!blob.exists(), "the blob no longer answers to its sha");
        assert_eq!(std::fs::read(&moved).unwrap(), b"bad bytes");
        assert_eq!(
            moved.file_name().unwrap().to_str().unwrap(),
            format!("{}.invalid", e2b.sha256)
        );

        // A link to something outside the cache: the link goes, the bytes are left alone.
        let elsewhere = tmp.path().join("someone-elses.gguf");
        std::fs::write(&elsewhere, b"theirs").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &encoder).unwrap();
        assert_eq!(set_aside(&hub, &encoder).unwrap(), None);
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"theirs");
    }

    /// A wrong file is set aside and nothing replaces it: settling never downloads.
    #[tokio::test]
    async fn an_unverified_file_with_the_wrong_bytes_is_hashed_and_set_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        let size = 4096u64;
        let file = domain::encoder_path(tmp.path(), &e2b);
        write_encoder(&file, e2b.projector, e2b.projection_dim, size);
        let other = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(b"the pinned encoder"))
        };
        let small = EncoderSpec {
            size_bytes: size,
            sha256: Box::leak(other.into_boxed_str()),
            ..e2b
        };
        assert_eq!(disk_state(&file, &small), EncoderState::Verifying);
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));

        assert_eq!(
            pictures.settle_once(tmp.path(), &small).await.unwrap(),
            None
        );
        assert!(!file.exists());
        assert!(file.with_file_name("mmproj-BF16.gguf.invalid").exists());
        assert_eq!(disk_state(&file, &small), EncoderState::Absent);
    }

    #[tokio::test]
    async fn a_file_that_is_not_an_encoder_is_set_aside_without_a_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        let file = domain::encoder_path(tmp.path(), &e2b);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"not really a gguf").unwrap();
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        assert_eq!(pictures.settle_once(tmp.path(), &e2b).await.unwrap(), None);
        assert!(file.with_file_name("mmproj-BF16.gguf.invalid").exists());
    }

    #[tokio::test]
    async fn hashing_a_file_that_matches_its_pin_writes_the_sidecar_and_reads_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        // Header-valid at a small size, with a pin made from its own bytes.
        let size = 4096u64;
        let file = domain::encoder_path(tmp.path(), &e2b);
        write_encoder(&file, e2b.projector, e2b.projection_dim, size);
        let sha = sha256_file(&file).await.unwrap();
        let small = EncoderSpec {
            size_bytes: size,
            sha256: Box::leak(sha.into_boxed_str()),
            ..e2b
        };
        assert_eq!(disk_state(&file, &small), EncoderState::Verifying);

        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        assert_eq!(
            pictures.settle_once(tmp.path(), &small).await.unwrap(),
            Some(size)
        );
        assert_eq!(
            disk_state(&file, &small),
            EncoderState::Ready { bytes: Some(size) }
        );
        let sidecar = std::fs::read_to_string(domain::sidecar_path(&file)).unwrap();
        assert!(EncoderSidecar::parse(&sidecar).is_some());
    }

    #[test]
    fn a_could_not_start_mark_holds_until_its_retry_time_then_gives_way() {
        let tmp = tempfile::tempdir().unwrap();
        if budgeted() {
            return;
        }
        let e2b = spec("gemma-4-e2b-it");
        let file = write_valid_encoder(tmp.path(), &e2b);
        vouch(&file, &e2b);
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default()));
        pictures.mark_could_not_start(&e2b);
        assert!(matches!(
            pictures.state(tmp.path(), "gemma-4-E2B-it-Q4_K_M"),
            EncoderState::Failed {
                reason: FailReason::CouldNotStart,
                ..
            }
        ));
        // Past its retry time, and nothing retrying: the disk decides again.
        pictures.with_dir(e2b.dir, |d| {
            d.live = Some(EncoderState::Failed {
                reason: FailReason::CouldNotStart,
                retry_at_unix_ms: 1,
            })
        });
        assert!(pictures
            .state(tmp.path(), "gemma-4-E2B-it-Q4_K_M")
            .is_ready());
    }

    #[test]
    fn provisioning_is_off_until_a_process_opts_in() {
        let pictures = Arc::new(PictureSupport::with_rows(Arc::new(MemoryRows::default())));
        assert!(!pictures.provisioning_enabled());
        let tmp = tempfile::tempdir().unwrap();
        // Off: no claim is taken, so nothing could be running.
        pictures.spawn_settle(tmp.path(), "gemma-4-E2B-it-Q4_K_M");
        assert!(!pictures.with_dir("gemma-4-e2b-it", |d| d.in_flight));
        pictures.enable_provisioning();
        assert!(pictures.provisioning_enabled());
    }

    #[tokio::test]
    async fn the_warmup_gate_waits_for_a_finished_warmup_not_merely_an_idle_one() {
        let gate = Arc::new(WarmupGate::default());
        assert!(
            !gate.clear(),
            "before any warm-up the load has not happened yet"
        );
        let run = gate.begin();
        assert!(!gate.clear());
        drop(run);
        assert!(gate.clear());
        tokio::time::timeout(Duration::from_secs(2), gate.wait())
            .await
            .expect("a finished warm-up releases the wait");
    }
}
