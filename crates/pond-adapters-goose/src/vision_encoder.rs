//! Picture support for the in-process engine: the adapter half of
//! `pond_core::models::domain::vision_encoder`.
//!
//! pond-core decides which encoder a chat model needs, whether a file is that encoder, whether
//! this device can carry it, and what the household reads while it is not ready. This module
//! applies those decisions to the filesystem and to the engine registry, in three separate
//! entry points with three separate rules:
//!
//! * [`PictureSupport::settle_stamp`], the provider-build step. Synchronous, and run before the
//!   provider is built and before anything loads, because the engine loads a stamped encoder
//!   eagerly at every model load. Declared on this device and verified stamps every row naming
//!   the chat model's GGUF; anything else clears them all.
//! * [`PictureSupport::state`], the pure read behind `vision_state` and the model list: the
//!   declaration first (so a model this device will not carry costs no file I/O), then header,
//!   size and the `.verified` sidecar. Never a hash, a rename or a fetch.
//! * [`PictureSupport::spawn_ensure`], the only path that hashes, quarantines and fetches, and
//!   only in a process that opted in with [`PictureSupport::enable_provisioning`]: the serve
//!   process. The voice child, `pond chat` and `pond agent` build adapters on the same data
//!   directory; they validate and stamp, and report what they see. The WHOLE sequence
//!   (validate, quarantine, fetch, hash, stamp) is single-flight per `EncoderSpec::dir`, not
//!   just the transfer, so a download-complete prepare and a provider-build ensure can never
//!   both quarantine and relink.
//!
//! Everything is keyed by `EncoderSpec::dir`, never by a spelling of the chat model: the qat and
//! non-qat encoders of one family are different files of identical size, and keying by family
//! let one report the other's progress. The fetch goes through `pond-hf-cache` with its
//! redirect-aware client (per-hop egress gating only holds with redirects disabled), pinned by
//! revision, size and sha256.
//!
//! Lifecycle lines log under the `giap::vision` target. `goose_local_inference` and
//! `llama-cpp-2` are carved to ERROR in the default filter, so a module-default target would be
//! invisible exactly where an operator is asking why the assistant cannot see.

use crate::registry_rows::{self, GooseRegistry, RegistryRows, RowSnapshot};
use goose::providers::local_inference::local_model_registry::LocalModelEntry;
use pond_core::models::domain::device_budget::{self, VisionDeclaration};
use pond_core::models::domain::vision_encoder::{
    self as domain, EncoderInvalid, EncoderSidecar, EncoderSpec, EncoderState, FailReason, OnDisk,
    RetryBackoff, RowChange, RowView, StampDecision,
};
use pond_core::shared::services::egress;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Set to `1` and a pond never downloads picture support, whatever else it does. For scratch
/// ponds (`scripts/live-test.sh`, `scripts/pai-bench.sh`), whose server is killed long before a
/// gigabyte arrives. Checked by the fetch path only: an encoder already on disk is still
/// validated, hashed and stamped.
pub const PROVISIONING_OPT_OUT_ENV: &str = "POND_DISABLE_MODEL_PROVISIONING";

/// Where every encoder comes from. The progress callback re-checks this host against the network
/// mode on every chunk; under both restrictive modes every hop of the chain (huggingface.co and
/// its CDNs) classifies the same way, so this one check stands for all of them.
const HF_HOST: &str = "huggingface.co";

/// How long a budgeted device's worker waits for the boot warm-up before moving bytes anyway.
/// The warm-up is a `-ngl 99` cold load of a 2.5-4 GB GGUF, and a gigabyte of page cache filled
/// underneath it is the recorded NvMap-error-12 / silent-CPU-fallback shape on the Orin.
const WARMUP_WAIT_CAP: Duration = Duration::from_secs(10 * 60);

/// How often a waiting worker looks at the network mode, so a household that opens it does not
/// wait out the backoff step it earned while it was closed.
const MODE_POLL: Duration = Duration::from_secs(5);

/// Progress is published to the status map at most once per this many bytes: the callback runs
/// once per chunk, fifteen thousand times for one encoder.
const PROGRESS_STEP: u64 = 1024 * 1024;

// ── Paths ───────────────────────────────────────────────────────────────────

/// The encoder file for `spec` under this pond, as it is actually spelled on disk.
///
/// The canonical path is lowercase (the Orin's ext4 is case-sensitive). An existing directory
/// that differs only in case is used rather than shadowed: the Mac's E2B encoder sits in
/// `gemma-4-E2B-it`, which APFS already resolves for the lowercase spelling, and on a
/// case-sensitive filesystem creating the lowercase twin would download a second copy of a
/// gigabyte file that is already there.
pub fn encoder_file(data_dir: &Path, spec: &EncoderSpec) -> PathBuf {
    let canonical = domain::encoder_path(data_dir, spec);
    if std::fs::symlink_metadata(&canonical).is_ok() {
        return canonical;
    }
    let root = data_dir.join("models").join("mmproj");
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name != spec.dir && name.eq_ignore_ascii_case(spec.dir) {
                let candidate = entry.path().join(spec.filename);
                if std::fs::symlink_metadata(&candidate).is_ok() {
                    return candidate;
                }
            }
        }
    }
    canonical
}

/// The chat model's GGUF under this pond, resolved through symlinks: the input the budget
/// arithmetic reads, so the goose side and `apply_jetson_settings` read the same file.
pub fn chat_gguf_path(data_dir: &Path, chat_model: &str) -> PathBuf {
    let gguf_dir = data_dir.join("models").join("gguf");
    let filename = crate::goose_agent::resolve_gguf_filename(chat_model, &gguf_dir);
    registry_rows::resolve(&gguf_dir.join(filename))
}

// ── Declaration ─────────────────────────────────────────────────────────────

/// Whether `chat_model` reads pictures on this device.
///
/// DECLARED, never downloaded: this feeds the `<vision>` prompt section, which sits in the
/// KV-cached static prefix, so it may change only when the model or its file does. Off a
/// budgeted device it is the name alone, with no I/O. On one it is the fit and the measured
/// list, and the GGUF is looked up only for an encoder the list names, so a model the device
/// will never carry costs nothing to ask about.
pub fn declaration(data_dir: Option<&Path>, chat_model: &str) -> VisionDeclaration {
    let needs_weights = device_budget::budgeted_device()
        && domain::encoder_for(chat_model)
            .is_some_and(|s| device_budget::DEVICE_MEASURED_VISION.contains(&s.dir));
    let gguf = data_dir
        .filter(|_| needs_weights)
        .map(|dd| chat_gguf_path(dd, chat_model));
    device_budget::vision_declaration(gguf.as_deref(), chat_model)
}

/// [`declaration`] for a GGUF already in hand (resolved).
pub fn declaration_at(gguf: Option<&Path>, chat_model: &str) -> VisionDeclaration {
    device_budget::vision_declaration(gguf, chat_model)
}

/// The state a file on disk reads as, with nothing live about it.
///
/// A file that is there but wrong reads as `Absent`: the fix is a download, which the serve
/// process starts by itself, and "not the right file" is not something the household can act on.
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

/// Lets a budgeted device's companion worker wait out the model load.
///
/// Waits for "a warm-up has finished and none is running", not "none is running": at boot the
/// worker is spawned before the warm-up starts, and the second reading would wave it straight
/// through into the load it exists to avoid.
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
    /// A warm-up is starting.
    pub fn begin(&self) -> WarmupRun<'_> {
        self.running.fetch_add(1, Ordering::SeqCst);
        WarmupRun(self)
    }

    fn clear(&self) -> bool {
        self.finished_once.load(Ordering::SeqCst) && self.running.load(Ordering::SeqCst) == 0
    }

    /// Wait until the warm-up is done, for at most [`WARMUP_WAIT_CAP`]. A pond that never warms
    /// up (a provider with no prefix cache, `POND_DISABLE_PREWARM`) is released by the cap.
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
    /// Downloading, Verifying, Failed or Blocked while an ensure is working on it (or after the
    /// engine refused to start it). `None` means the file on disk decides.
    live: Option<EncoderState>,
    in_flight: bool,
    backoff: RetryBackoff,
}

/// The fetch path's own refusal: provisioning is opted out, so the file stays absent.
#[derive(Debug)]
struct FetchDisabled;

impl std::fmt::Display for FetchDisabled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{PROVISIONING_OPT_OUT_ENV}=1; not downloading picture support"
        )
    }
}

impl std::error::Error for FetchDisabled {}

/// One status map, in-flight set and backoff ladder for every encoder this process knows,
/// shared by the provider build, the chat-stream backstop, `vision_state` and `prepare_model`.
pub struct PictureSupport {
    rows: Arc<dyn RegistryRows>,
    /// Set only by the serve process. Everything that hashes, renames or downloads checks it.
    provisioning: AtomicBool,
    /// Whether the fetch step may run at all once provisioning is on. Tests turn it off so a
    /// repair path can be exercised without a network.
    fetch_allowed: AtomicBool,
    dirs: Mutex<HashMap<&'static str, DirState>>,
    /// Every stamp decision is made and applied under this, so one is always applied against the
    /// disk state it was made from: a provider build that read "unverified" cannot clear the
    /// stamp an ensure wrote a moment after it hashed the file.
    stamp_lock: Mutex<()>,
    warmup: Arc<WarmupGate>,
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
            fetch_allowed: AtomicBool::new(true),
            dirs: Mutex::new(HashMap::new()),
            stamp_lock: Mutex::new(()),
            warmup: Arc::new(WarmupGate::default()),
        }
    }

    /// Opt this process in to hashing, quarantining and fetching. The serve process only.
    pub fn enable_provisioning(&self) {
        if !self.provisioning.swap(true, Ordering::SeqCst) {
            tracing::info!(
                target: "giap::vision",
                "picture support provisioning enabled in this process"
            );
        }
    }

    pub fn provisioning_enabled(&self) -> bool {
        self.provisioning.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn without_fetch(self) -> Self {
        self.fetch_allowed.store(false, Ordering::SeqCst);
        self
    }

    fn fetch_disabled(&self) -> bool {
        !self.fetch_allowed.load(Ordering::SeqCst)
            || std::env::var(PROVISIONING_OPT_OUT_ENV).as_deref() == Ok("1")
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

    /// The live state, if one still applies. A failure whose retry time has passed and that
    /// nothing is retrying (the engine's could-not-start mark) gives way to the disk again: the
    /// next picture is how that one is retried.
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

    /// Where picture support stands for `chat_model`. A pure read, cheap enough per model-list
    /// row: the declaration short-circuits before any file I/O, then header, size and sidecar.
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

    /// [`Self::state`] for a declared encoder.
    pub fn state_of(&self, data_dir: &Path, spec: &EncoderSpec) -> EncoderState {
        if let Some(live) = self.live_state(spec.dir) {
            return live;
        }
        disk_state(&encoder_file(data_dir, spec), spec)
    }

    /// The provider-build step: make every row naming `gguf` agree with this device's
    /// declaration and the file on disk, synchronously, before the provider is built.
    ///
    /// Declared and verified stamps them all (path, size, and the settings copy the engine sizes
    /// memory from); anything else, including a header-valid file nobody has hashed yet, clears
    /// them, because a stamped encoder is loaded eagerly at the next model load whether or not a
    /// picture is ever sent. Returns the encoder when it still has work for an ensure to do.
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

    /// The engine refused to start picture support for `spec` (a failed multimodal init, seen
    /// by the provider shim): hold the state at could-not-start for a backoff step, so the next
    /// picture is refused with an honest line instead of failing the same way.
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

    /// Start an ensure for `chat_model`'s encoder in the background, if this process provisions,
    /// the model declares one here, the file is not already ready, and none is running.
    ///
    /// Returns at once: the file is a gigabyte, and the callers are a provider build, a turn and
    /// an HTTP handler. Needs a Tokio runtime; without one it does nothing.
    pub fn spawn_ensure(self: &Arc<Self>, data_dir: &Path, chat_model: &str) {
        if !self.provisioning_enabled() {
            return;
        }
        let VisionDeclaration::Declared(spec) = declaration(Some(data_dir), chat_model) else {
            return;
        };
        if self.state_of(data_dir, &spec).is_ready() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let claimed = self.with_dir(spec.dir, |d| !std::mem::replace(&mut d.in_flight, true));
        if !claimed {
            return;
        }
        handle.spawn(self.clone().ensure(data_dir.to_path_buf(), spec));
    }

    /// The whole ensure, retried on the backoff ladder until it succeeds. Holds the dir's
    /// in-flight claim for its whole life, including the waits, so nothing else starts one.
    async fn ensure(self: Arc<Self>, data_dir: PathBuf, spec: EncoderSpec) {
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
        loop {
            let mode = egress::network_mode();
            self.with_dir(spec.dir, |d| d.backoff.on_network_mode(mode.as_str()));
            let err = match self.ensure_once(&data_dir, &spec).await {
                Ok(bytes) => {
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
                    return;
                }
                Err(e) => e,
            };
            if err.downcast_ref::<FetchDisabled>().is_some() {
                self.set_live(spec.dir, None);
                tracing::info!(target: "giap::vision", encoder = spec.dir, "{err}");
                return;
            }

            // A network-mode refusal is not a failure: no backoff step is earned, and the only
            // thing that can change the outcome is the household changing the mode.
            if let Some(blocked) = blocked_state(&err) {
                tracing::info!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    mode = mode.as_str(),
                    "picture support is waiting: the network mode refuses its download"
                );
                self.set_live(spec.dir, Some(blocked));
                wait_for_mode_change(mode).await;
                continue;
            }

            let wait = self.with_dir(spec.dir, |d| d.backoff.on_failure(mode.as_str()));
            let retry_at = now_ms().saturating_add(wait.as_millis() as u64);
            let state = classify(&err, retry_at);
            tracing::warn!(
                target: "giap::vision",
                encoder = spec.dir,
                error = %format!("{err:#}"),
                state = state.kind(),
                retry_in_s = wait.as_secs(),
                "picture support did not finish; it tries again after the wait"
            );
            self.set_live(spec.dir, Some(state));
            wait_until(retry_at, mode).await;
        }
    }

    /// One pass: validate what is there (no egress check, so an offline household whose encoder
    /// is already good is never told it is blocked), repair it if it is wrong, fetch it if it is
    /// missing, and stamp.
    async fn ensure_once(&self, data_dir: &Path, spec: &EncoderSpec) -> anyhow::Result<u64> {
        let file = encoder_file(data_dir, spec);
        match domain::encoder_on_disk(&file, spec) {
            OnDisk::Verified { bytes } => {
                self.stamp_declared(spec, &file);
                return Ok(bytes);
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
                    return Ok(bytes);
                }
                tracing::warn!(
                    target: "giap::vision",
                    encoder = spec.dir,
                    found = %sha,
                    expected = spec.sha256,
                    "the encoder on disk has the right shape but not the pinned bytes"
                );
                self.quarantine(data_dir, &file)?;
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
            }
            OnDisk::Missing => {}
        }
        if self.fetch_disabled() {
            return Err(anyhow::Error::new(FetchDisabled));
        }
        let bytes = self.fetch(data_dir, spec, &file).await?;
        self.stamp_declared(spec, &file);
        Ok(bytes)
    }

    /// Download the pinned file into the HF cache, check it is the pinned file, and link it at
    /// `dest`.
    async fn fetch(&self, data_dir: &Path, spec: &EncoderSpec, dest: &Path) -> anyhow::Result<u64> {
        // A pure verdict first, so a refused fetch never shows as "downloading". The recorded
        // gate (the activity feed's denial event) is pond-hf-cache's, per hop.
        let mode = egress::network_mode();
        if let Err(reason) = egress::egress_verdict(HF_HOST, mode) {
            return Err(anyhow::Error::new(egress::EgressDenied {
                host: HF_HOST.to_string(),
                mode,
                reason,
            }));
        }

        let cache = pond_hf_cache::HfCache::new(data_dir);
        let client = pond_hf_cache::build_redirect_aware_client(cache.token())?;
        let repo = cache.repo(spec.repo).with_revision(spec.revision);
        let fetch = repo
            .file(spec.filename)
            .expect_size(spec.size_bytes)
            .expect_etag(spec.sha256);

        self.set_live(
            spec.dir,
            Some(EncoderState::Downloading {
                done: 0,
                total: spec.size_bytes,
            }),
        );
        tracing::info!(
            target: "giap::vision",
            encoder = spec.dir,
            repo = spec.repo,
            revision = spec.revision,
            bytes = spec.size_bytes,
            "downloading picture support; text chat works meanwhile"
        );
        let mut published = 0u64;
        let blob = fetch
            .download_to_blob(&client, cache.token(), |done, total| {
                let total = if total > 0 { total } else { spec.size_bytes };
                if done < published || done - published >= PROGRESS_STEP || done >= total {
                    published = done;
                    self.set_live(spec.dir, Some(EncoderState::Downloading { done, total }));
                }
                // A mode change mid-transfer pauses it (`.incomplete` is kept) and reads as
                // Blocked: a privacy control the household has to restart the pond to apply
                // would not be one.
                egress::egress_verdict(HF_HOST, egress::network_mode()).is_ok()
            })
            .await?;

        // pond-hf-cache names a pinned download by its pin; anything else is not what was asked.
        if blob.file_name().and_then(|n| n.to_str()) != Some(spec.sha256) {
            return Err(anyhow::Error::new(EncoderInvalid::WrongFile)
                .context(format!("the download landed as {}", blob.display())));
        }
        domain::validate_encoder_header(&blob, spec).map_err(anyhow::Error::new)?;
        self.set_live(spec.dir, Some(EncoderState::Verifying));
        let sha = sha256_file(&blob).await?;
        if !sha.eq_ignore_ascii_case(spec.sha256) {
            let moved = move_aside(&blob)?;
            tracing::warn!(
                target: "giap::vision",
                encoder = spec.dir,
                found = %sha,
                moved_to = %moved.display(),
                "the downloaded encoder's sha256 is not the pinned one; set aside"
            );
            return Err(anyhow::Error::new(EncoderInvalid::WrongFile));
        }

        match pond_hf_cache::link_blob(&blob, dest).await {
            Ok(()) => {}
            Err(e) if e.downcast_ref::<pond_hf_cache::DestNotALink>().is_some() => {
                // Something appeared at the path during the transfer and it was never checked:
                // set it aside rather than overwrite it, then link.
                self.quarantine(data_dir, dest)?;
                pond_hf_cache::link_blob(&blob, dest).await?;
            }
            Err(e) => return Err(e),
        }
        write_sidecar(dest, &sha)?;
        Ok(spec.size_bytes)
    }

    /// Set a bad encoder aside and clear every row that pointed at it. Renames, never deletes.
    ///
    /// A link into the HF cache has its BLOB renamed: renaming only the link would let the
    /// refetch's fast path hand the same bad blob straight back, forever.
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

/// Whether `row`'s model declares `spec` on this device: by its id, or by its file's name when
/// the id is not a model name (an id minted from a URL, say).
fn row_declares(row: &RowSnapshot, spec: &EncoderSpec) -> bool {
    let file_name = row
        .resolved_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let name = if domain::encoder_for(&row.id).is_some() {
        row.id.as_str()
    } else {
        file_name
    };
    matches!(
        declaration_at(Some(&row.resolved_path), name),
        VisionDeclaration::Declared(s) if s.dir == spec.dir
    )
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

// ── Failure classification ──────────────────────────────────────────────────

/// Blocked, when this failure is the network mode's refusal: the gate's own error anywhere in
/// the chain, or a transfer the progress callback paused because the mode now refuses it.
fn blocked_state(err: &anyhow::Error) -> Option<EncoderState> {
    if let blocked @ EncoderState::Blocked { .. } = domain::classify_error(err, 0) {
        return Some(blocked);
    }
    if pond_hf_cache::is_stopped(err) {
        let mode = egress::network_mode();
        if egress::egress_verdict(HF_HOST, mode).is_err() {
            return Some(EncoderState::Blocked {
                mode: mode.as_str().to_string(),
                host: HF_HOST.to_string(),
            });
        }
    }
    None
}

/// The Failed state for an error that is not a refusal, by type and never by message text.
///
/// pond-core classifies the gate, a wrong file and socket errors; the transfer's own outcomes
/// are pond-hf-cache types it cannot see. A transfer that ended short is a dropped connection
/// (the `.incomplete` is kept, so the retry resumes); every other refused transfer means the
/// server described a different file.
fn classify(err: &anyhow::Error, retry_at_unix_ms: u64) -> EncoderState {
    if let Some(transfer) = pond_hf_cache::transfer_error(err) {
        let reason = match transfer {
            pond_hf_cache::TransferError::Short { .. } => FailReason::ConnectionDropped,
            _ => FailReason::WrongFile,
        };
        return EncoderState::Failed {
            reason,
            retry_at_unix_ms,
        };
    }
    domain::classify_error(err, retry_at_unix_ms)
}

/// Sleep until `until_ms`, in steps, returning early when the network mode changes.
async fn wait_until(until_ms: u64, mode: egress::NetworkMode) {
    loop {
        let now = now_ms();
        if now >= until_ms || egress::network_mode() != mode {
            return;
        }
        let step = Duration::from_millis(until_ms - now).min(MODE_POLL);
        tokio::time::sleep(step).await;
    }
}

async fn wait_for_mode_change(mode: egress::NetworkMode) {
    while egress::network_mode() == mode {
        tokio::time::sleep(MODE_POLL).await;
    }
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

/// Move the bytes at `file` out of the way and return where they went (`None` for a link whose
/// target is not a blob of the cache at `hub`: removing the link frees nobody's bytes).
///
/// A link into the cache has its blob renamed, then the link removed; a regular file is
/// renamed. The file's `.verified` sidecar is removed, since it now describes nothing.
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

/// Rename `path` to the first free `<name>.invalid`, `<name>.invalid.1`, ... and return where it
/// went. Never onto an existing name: that would delete an earlier quarantined copy.
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

    /// A GGUF encoder header of the kind `validate_encoder_header` reads, with one F32 tensor
    /// whose extent ends exactly at `size`. The file is made that long with `set_len`, so on
    /// APFS and ext4 it is sparse and costs no disk, whatever the pinned size.
    fn write_encoder(path: &Path, projector: &str, projection_dim: u32, size: u64) {
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
        let kv_count = 4u64;

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

    /// A sidecar that vouches for `file` without hashing it: only the adapter hashes, and a
    /// gigabyte of zeroes is not the pinned sha anyway.
    fn vouch(file: &Path, spec: &EncoderSpec) {
        let meta = std::fs::metadata(file).unwrap();
        let sidecar =
            EncoderSidecar::new(spec.sha256, meta.len(), domain::mtime_unix(&meta).unwrap());
        std::fs::write(domain::sidecar_path(file), sidecar.to_json()).unwrap();
    }

    fn spec(dir: &str) -> EncoderSpec {
        domain::encoder_by_dir(dir).unwrap()
    }

    /// Declarations here are by name, which holds only off a budgeted device. Under
    /// `scripts/jetson-emu.sh test` the Orin policy applies and ships with an empty measured
    /// list, so these tests assert that instead.
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
            pictures.state(tmp.path(), "gemma-4-E2B-it"),
            EncoderState::Absent,
            "even a sidecar cannot vouch for a file shorter than its own tensor table"
        );
    }

    /// qat and non-qat encoders are different files of the same size. The non-qat one on disk
    /// must not make the qat model ready, and must not be stamped onto its rows.
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
        assert!(pictures.state(tmp.path(), "gemma-4-E2B-it").is_ready());
        assert_eq!(
            pictures.state(tmp.path(), "gemma-4-E2B-it-qat"),
            EncoderState::Downloading { done: 5, total: 9 }
        );
    }

    /// One GGUF, three ids, one engine slot: the stamp and the clear reach every one of them and
    /// no other model's rows, in one save.
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

        // The sidecar goes (a file replaced in place): every row is cleared, and the ensure is
        // asked to verify again.
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
        // On a case-insensitive filesystem the canonical spelling already names it; on a
        // case-sensitive one the variant directory is found. Either way it is the same file.
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
        // The cache layout built by hand under the test's own directory, so a developer's
        // `HF_HOME` can never be written to.
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

    /// The repair path, without a network: a header-valid file that is not the pinned bytes is
    /// hashed, found wrong, and set aside, and with fetching off it stays absent.
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
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default())).without_fetch();

        let err = pictures.ensure_once(tmp.path(), &small).await.unwrap_err();
        assert!(err.downcast_ref::<FetchDisabled>().is_some(), "{err:#}");
        assert!(!file.exists());
        assert!(file.with_file_name("mmproj-BF16.gguf.invalid").exists());
        assert_eq!(disk_state(&file, &small), EncoderState::Absent);
    }

    /// A file that is not even an encoder is set aside before anything is hashed.
    #[tokio::test]
    async fn a_file_that_is_not_an_encoder_is_set_aside_without_a_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = spec("gemma-4-e2b-it");
        let file = domain::encoder_path(tmp.path(), &e2b);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"not really a gguf").unwrap();
        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default())).without_fetch();
        let err = pictures.ensure_once(tmp.path(), &e2b).await.unwrap_err();
        assert!(err.downcast_ref::<FetchDisabled>().is_some(), "{err:#}");
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

        let pictures = PictureSupport::with_rows(Arc::new(MemoryRows::default())).without_fetch();
        assert_eq!(
            pictures.ensure_once(tmp.path(), &small).await.unwrap(),
            size
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
            pictures.state(tmp.path(), "gemma-4-E2B-it"),
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
        assert!(pictures.state(tmp.path(), "gemma-4-E2B-it").is_ready());
    }

    #[test]
    fn a_short_transfer_is_a_dropped_connection_and_any_other_refusal_a_wrong_file() {
        let short = anyhow::Error::new(pond_hf_cache::TransferError::Short {
            received: 1,
            total: 2,
        });
        assert_eq!(
            classify(&short, 7),
            EncoderState::Failed {
                reason: FailReason::ConnectionDropped,
                retry_at_unix_ms: 7
            }
        );
        let etag = anyhow::Error::new(pond_hf_cache::TransferError::EtagMismatch {
            expected: "a".into(),
            found: vec!["b".into()],
        })
        .context("fetching the encoder");
        assert_eq!(
            classify(&etag, 7),
            EncoderState::Failed {
                reason: FailReason::WrongFile,
                retry_at_unix_ms: 7
            }
        );
        let denied = anyhow::Error::new(egress::EgressDenied {
            host: "huggingface.co".into(),
            mode: egress::NetworkMode::Offline,
            reason: "offline",
        });
        assert_eq!(
            blocked_state(&denied),
            Some(EncoderState::Blocked {
                mode: "offline".into(),
                host: "huggingface.co".into()
            })
        );
        assert_eq!(blocked_state(&short), None);
    }

    #[test]
    fn provisioning_is_off_until_a_process_opts_in() {
        let pictures = Arc::new(PictureSupport::with_rows(Arc::new(MemoryRows::default())));
        assert!(!pictures.provisioning_enabled());
        let tmp = tempfile::tempdir().unwrap();
        // Off: no claim is taken, so nothing could be running.
        pictures.spawn_ensure(tmp.path(), "gemma-4-E2B-it");
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
