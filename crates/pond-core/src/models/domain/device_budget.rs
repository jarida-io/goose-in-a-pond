//! What the budgeted device (the 8 GB Orin Nano) can hold beside the LLM's KV cache, and so
//! which window a model gets there and whether it may carry a vision encoder.
//!
//! # Why this lives in the domain, once
//!
//! Two crates need the same answer and neither can call the other the right way round:
//! `pond-adapters-local-inference` sizes the window (`apply_jetson_settings`), and the goose
//! adapter decides whether to declare, stamp and load an encoder. When the arithmetic lived only
//! in the first, the second would have used the 56 KiB/token fallback where the first used the
//! header's 18 for E2B, and the two would disagree about whether E2B fits: the window sized
//! without the encoder while the encoder was stamped anyway. That is an OOM on the board. Both
//! now call [`vision_fit_on_device`] / [`device_window`] with the RESOLVED GGUF path, so their
//! inputs cannot diverge.
//!
//! # What only the Orin may move
//!
//! Every constant here is a measurement or is marked as not one. A single hardcoded window
//! OOM-killed a board once, and a Mac-measured KV cost understated the device's by three times,
//! so none of these moves on anything but a reading from the Orin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::drafter::drafter_for;
use super::gguf::parse_gguf_header;
use super::vision_encoder::{encoder_for, EncoderSpec, EncoderState};

const MIB: u64 = 1024 * 1024;

// ── Jetson memory constants ─────────────────────────────────────────────────

/// Total device RAM on a Jetson Orin Nano 8 GB (MB), as the KERNEL reports it: `free -m` on the
/// Orin says 7620, not the marketed 8192, because carveouts are taken before Linux sees the
/// memory. Any phantom MB here is spent silently, since an over-large context does not fail to
/// allocate, it swaps: the symptom is slowness, not an out-of-memory error.
pub const JETSON_TOTAL_RAM_MB: u64 = 7620;
/// Approximate headroom used by OS + GIAP server + UI at idle (MB).
pub const SYSTEM_OVERHEAD_MB: u64 = 1500;
/// Whisper base model resident size (MB).
pub const STT_RESERVED_MB: u64 = 200;
/// Reserved TTS resident size (MB).
pub const TTS_RESERVED_MB: u64 = 100;
/// Everything the LLM slot does not get: OS, GIAP server, UI, STT, TTS.
///
/// Named separately from [`LLM_BUDGET_MB`] because the budget is derived twice, once as a
/// constant and once at runtime from the device profile; both subtract the same reservation.
pub const RESERVED_MB: u64 = SYSTEM_OVERHEAD_MB + STT_RESERVED_MB + TTS_RESERVED_MB;
/// Approximate MB available for a single LLM slot.
pub const LLM_BUDGET_MB: u64 = JETSON_TOTAL_RAM_MB - RESERVED_MB;

/// Total RAM of the device this process should believe it is.
///
/// [`JETSON_TOTAL_RAM_MB`] unless a device profile overrides it, deliberately not a host probe:
/// reading a developer Mac's real 64 GB makes every derivation downstream trivially satisfiable.
pub fn total_ram_mb() -> u64 {
    super::device_profile::active()
        .map(|p| p.total_ram_mb)
        .unwrap_or(JETSON_TOTAL_RAM_MB)
}

/// MB available for a single LLM slot on the device we believe we are.
///
/// The runtime twin of [`LLM_BUDGET_MB`]; identical to it when no profile is
/// active, which is the case in production, in `deploy.sh` and in CI.
pub fn llm_budget_mb() -> u64 {
    total_ram_mb().saturating_sub(RESERVED_MB)
}

// ── Window arithmetic ───────────────────────────────────────────────────────

/// Per-token KV cost for the widest geometry we ship, MEASURED on the Orin (the Mac said
/// 16): E4B is 56 KiB/token across both caches, E2B 18. No padding here (it lives in the
/// budget) and no constant term, since both caches carry `n_ctx` cells on this llama.cpp.
/// Padding to 64 floored E4B to 4096, under its own 4,678-token turn-1 prompt.
pub const KV_KIB_PER_TOKEN: u64 = 56;
/// llama.cpp's compute buffers. Nearly flat in `n_ctx` -- measured
/// 522 MiB at both 4096 and 16384, rising to 582 MiB at 32768 -- so 600
/// covers the range this function can return.
pub const COMPUTE_BUFFER_MB: u64 = 600;
/// The drafter's compute buffers and graph, on top of its weights.
///
/// Its KV is NOT here, and that is the point: with `ctx_other` the
/// drafter shares the target's cache, which is visible in the phase
/// timings as `process` costing 0.0 ms/step -- llama.cpp skips the
/// catch-up decode only when the memory is shared. So the drafter's
/// cost is flat in `n_ctx` and belongs in the budget, not in the slope.
///
/// 64 rather than a measured figure: the honest measurement (MemAvailable
/// either side of building a drafter context) read 38-47 MB at 8192 and
/// 16384 against a 57 MB file, and it is an UNDER-estimate -- mmap'd
/// weights come out of reclaimable page cache, which MemAvailable counts
/// as available. Rounding up past the file size costs a few hundred
/// tokens of window and buys the margin that measurement could not
/// establish.
pub const DRAFTER_COMPUTE_MB: u64 = 64;
/// The vision encoder's compute buffers and warm-up, on top of its weights.
///
/// UNMEASURED. Only a reading from the Orin may move it: MemAvailable either side of the
/// encoder's init during the boot prewarm, with the model and drafter resident. The encoder's
/// weights are read into one GPU buffer (not mmapped), so unlike the drafter's they are
/// counted in full by [`vision_fit`] already; this is the graph on top. Nothing depends on the
/// exact value today, because [`DEVICE_MEASURED_VISION`] is empty, and the tests pin that the
/// declare/refuse split for every shipped model holds anywhere in 0..=900.
pub const ENCODER_COMPUTE_MB: u64 = 256;
/// The narrowest window handed out, whatever the budget.
pub const MIN_CTX: u32 = 2048;
/// The widest window handed out: a latency decision (prefill is 19.97 s cold at 16384), not a
/// memory one.
pub const MAX_CTX: u32 = 16384;
/// Round the answer DOWN to a multiple of this. Not a power of two: those are 2x apart,
/// so flooring to one discards up to HALF of a window the budget already proved
/// affordable (E4B IQ4_XS: 13,220 allowed, 8,192 handed out). `n_ctx` needs no power of
/// two in llama.cpp; safety comes from the slope, the compute buffer and the budget.
pub const CTX_GRANULARITY: u32 = 1024;
/// The weights assumed when the model file cannot be read: the largest model we ship, so the
/// first load is conservative rather than fatal. Never used to prove a vision fit.
pub const ASSUMED_LARGEST_MODEL_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// MB left for the KV cache once everything resident is paid for.
fn kv_allowance_mb(
    budget_mb: u64,
    model_bytes: u64,
    drafter_bytes: u64,
    encoder_bytes: u64,
    encoder_compute_mb: u64,
) -> u64 {
    let model_mb = model_bytes / MIB;
    // A drafter is a second set of weights resident for the whole session.
    // Leaving it out of the budget is what let the window be sized as though
    // only one model were loaded.
    let drafter_mb = if drafter_bytes > 0 {
        drafter_bytes / MIB + DRAFTER_COMPUTE_MB
    } else {
        0
    };
    // The encoder loads eagerly at every model load and cannot be reclaimed as page cache.
    let encoder_mb = if encoder_bytes > 0 {
        encoder_bytes / MIB + encoder_compute_mb
    } else {
        0
    };
    budget_mb
        .saturating_sub(model_mb)
        .saturating_sub(COMPUTE_BUFFER_MB)
        .saturating_sub(drafter_mb)
        .saturating_sub(encoder_mb)
}

/// The model's own header slope when it could answer, else the conservative fallback.
/// `kv_cost_from_header` returns None rather than guessing, so an unreadable or unfamiliar
/// model gets exactly the fallback behaviour and this is never a new risk.
fn slope(kv_kib_per_token: Option<u64>) -> u64 {
    kv_kib_per_token
        .filter(|k| *k > 0)
        .unwrap_or(KV_KIB_PER_TOKEN)
}

/// Tokens a KV allowance buys, unclamped and unrounded.
fn tokens_for(kv_mb: u64, slope: u64) -> u64 {
    kv_mb.saturating_mul(1024) / slope
}

/// Largest multiple of [`CTX_GRANULARITY`] that fits, clamped to [MIN_CTX, MAX_CTX].
/// Saturating at MAX_CTX before the cast keeps a huge allowance (E2B's is ~41k) from
/// wrapping u32.
fn window_for_tokens(tokens: u64) -> u32 {
    let granularity = u64::from(CTX_GRANULARITY);
    let floored = (tokens / granularity) * granularity;
    floored.min(u64::from(MAX_CTX)).max(u64::from(MIN_CTX)) as u32
}

/// Context size that fits a model in `budget_mb`: the budget less the weights, the compute
/// buffers and the drafter, divided by the per-token KV cost, floored to [`CTX_GRANULARITY`].
///
/// Moved unchanged from `LocalInferenceLlmAdapter::context_size_for_budget`, and the tests
/// below are that function's, so the move is proved behaviour-identical. Takes the budget
/// explicitly so another board's can be asked for without touching the process environment, a
/// data race in a threaded test binary.
pub fn context_size_for_budget(
    budget_mb: u64,
    model_bytes: u64,
    drafter_bytes: u64,
    kv_kib_per_token: Option<u64>,
) -> u32 {
    context_size_with_encoder(budget_mb, model_bytes, drafter_bytes, 0, kv_kib_per_token)
}

/// [`context_size_for_budget`] with a resident vision encoder of `encoder_bytes` (0 for none),
/// charged its weights plus [`ENCODER_COMPUTE_MB`].
pub fn context_size_with_encoder(
    budget_mb: u64,
    model_bytes: u64,
    drafter_bytes: u64,
    encoder_bytes: u64,
    kv_kib_per_token: Option<u64>,
) -> u32 {
    let kv_mb = kv_allowance_mb(
        budget_mb,
        model_bytes,
        drafter_bytes,
        encoder_bytes,
        ENCODER_COMPUTE_MB,
    );
    window_for_tokens(tokens_for(kv_mb, slope(kv_kib_per_token)))
}

/// What the window charges for `chat_model`'s drafter: its catalogue size whenever it HAS one.
///
/// Always the catalogue figure, never the file on disk and never the speculation switch: the
/// real file is 56 MiB against a catalogue 57, and a window that moved when the drafter landed
/// or the switch flipped would change `n_ctx`, which busts the KV snapshot and, on a runtime
/// ON, would load 121 MB into a window sized without it.
pub fn drafter_budget_bytes(chat_model: &str) -> u64 {
    drafter_for(chat_model).map_or(0, |d| d.approx_mb * MIB)
}

// ── The header slope ────────────────────────────────────────────────────────

/// Architectures whose global:SWA layer ratio has been confirmed against
/// llama.cpp's own `llama_kv_cache ... size = N MiB (C cells, L layers)`
/// lines on the Orin. Gemma 4: 1 global per 5 sliding, verified at both
/// sizes (E4B 4+20 of 24 owning layers, E2B 3+12 of 15).
pub const CONFIRMED_SWA_PATTERNS: &[(&str, u32)] = &[("gemma4", 5)];

/// Geometry sits in the first ~2 KB of every file measured -- offsets
/// 924-1,829 on gemma-4-E2B, well before `tokenizer.ggml.tokens` at
/// 2,061. 64 KiB is generous cover for that without reading the token
/// array, let alone the 15 MB it takes to reach the chat template.
pub const HEAD_BYTES: usize = 64 * 1024;

/// The model's KV cost per token (KiB) from its own GGUF head, or `None` when the header cannot
/// settle it and the caller must keep the measured constant. Exact for a dense model; where
/// `key_length_swa` is present the global-to-SWA layer ratio the header omits is worth a
/// factor of two in the direction that OOMs a board, so trust only confirmed architectures.
pub fn kv_cost_from_head(head: &[u8]) -> Option<u64> {
    let info = parse_gguf_header(head)?;
    let arch = info.architecture.as_deref().unwrap_or_default();

    if info.key_length_swa.is_none() {
        // Dense: exact whatever the architecture. The ratio argument is
        // unused on this path.
        return info.kv_kib_per_token(0);
    }

    let (_, swa_per_global) = CONFIRMED_SWA_PATTERNS
        .iter()
        .find(|(name, _)| *name == arch)?;
    info.kv_kib_per_token(*swa_per_global)
}

/// [`kv_cost_from_head`] over the first [`HEAD_BYTES`] of the file at `path`.
pub fn kv_cost_from_header(path: &Path) -> Option<u64> {
    use std::io::Read as _;
    let mut buf = Vec::with_capacity(HEAD_BYTES);
    std::fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    kv_cost_from_head(&buf)
}

// ── Vision fit ──────────────────────────────────────────────────────────────

/// Whether a model can carry its vision encoder on the budgeted device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionFit {
    /// The model has no encoder.
    NotDeclared,
    /// Carrying the encoder costs no window: the model keeps `window` either way.
    Fits { window: u32 },
    /// Carrying it would shrink the window from `window_without` to `window_with`, or the
    /// weights could not be read, which never proves a fit.
    CostsWindow {
        window_without: u32,
        window_with: u32,
    },
}

impl VisionFit {
    pub fn fits(&self) -> bool {
        matches!(self, Self::Fits { .. })
    }
}

/// The fit decision over plain numbers.
///
/// The predicate is on the UNCLAMPED allowance: it fits iff the tokens the KV budget buys with
/// the encoder resident reach the window the model gets without it. Comparing the two clamped
/// windows instead fails open at the floor: a model already at [`MIN_CTX`] gets `MIN_CTX` with
/// the encoder too, the encoder looks free, and a gigabyte lands on a board that was already
/// over budget. `model_bytes` of `None` (unreadable weights) is never a fit.
pub fn vision_fit(
    budget_mb: u64,
    model_bytes: Option<u64>,
    chat_model: &str,
    kv_kib_per_token: Option<u64>,
    encoder_compute_mb: u64,
) -> VisionFit {
    let Some(spec) = encoder_for(chat_model) else {
        return VisionFit::NotDeclared;
    };
    let drafter = drafter_budget_bytes(chat_model);
    let slope = slope(kv_kib_per_token);
    let weights = model_bytes.unwrap_or(ASSUMED_LARGEST_MODEL_BYTES);

    let without = tokens_for(kv_allowance_mb(budget_mb, weights, drafter, 0, 0), slope);
    let window_without = window_for_tokens(without);
    let with = tokens_for(
        kv_allowance_mb(
            budget_mb,
            weights,
            drafter,
            spec.size_bytes,
            encoder_compute_mb,
        ),
        slope,
    );
    let window_with = window_for_tokens(with);

    if model_bytes.is_some() && with >= u64::from(window_without) {
        VisionFit::Fits {
            window: window_without,
        }
    } else {
        VisionFit::CostsWindow {
            window_without,
            window_with,
        }
    }
}

/// [`vision_fit`] for the GGUF at `gguf_path` (resolved: `metadata` follows symlinks) on the
/// device this process believes it is. Reads the file's length and its 64 KiB head, nothing
/// more; a model with no encoder returns before either.
pub fn vision_fit_on_device(gguf_path: &Path, chat_model: &str) -> VisionFit {
    if encoder_for(chat_model).is_none() {
        return VisionFit::NotDeclared;
    }
    let model_bytes = std::fs::metadata(gguf_path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len());
    let kv = kv_cost_from_header(gguf_path);
    vision_fit(
        llm_budget_mb(),
        model_bytes,
        chat_model,
        kv,
        ENCODER_COMPUTE_MB,
    )
}

// ── The budgeted device ─────────────────────────────────────────────────────

/// Set once by pond-server from the CUDA build flag. Only ever raised: see [`budgeted_device`].
static BUDGETED_OVERRIDE: AtomicBool = AtomicBool::new(false);

/// Record that this binary is a CUDA build, which only the Orin runs.
///
/// Called once in pond-server's `async_main` from `pond_adapters_local_inference::CUDA_ENABLED`,
/// because a `cfg!` of that crate's feature cannot be read from anywhere else. `false` is a
/// no-op, never a reset: the getter must fail closed to the Orin policy, and a later caller
/// with less information must not talk it out of it.
pub fn set_budgeted_device(cuda_build: bool) {
    if cuda_build {
        BUDGETED_OVERRIDE.store(true, Ordering::SeqCst);
    }
}

/// Whether this process must live within the Orin's memory budget.
///
/// Any of three signals is enough, so a forgotten setter, an emulation run or a CPU build on
/// the board all land on the NARROW policy: the CUDA override; a device profile that stamps
/// device settings (`scripts/jetson-emu.sh`); or the host's own tegra evidence
/// (`/proc/device-tree/model`, `/etc/nv_tegra_release`), which catches the CPU-only binary
/// `build-docker.sh` produces.
pub fn budgeted_device() -> bool {
    budgeted_from(
        BUDGETED_OVERRIDE.load(Ordering::SeqCst),
        super::device_profile::stamping_device_model_settings(),
        host_is_tegra(),
    )
}

/// [`budgeted_device`] over its three signals.
pub fn budgeted_from(cuda_override: bool, stamping_profile: bool, tegra_host: bool) -> bool {
    cuda_override || stamping_profile || tegra_host
}

/// The host's own evidence, read once. The same reads `report_acceleration` makes.
fn host_is_tegra() -> bool {
    static TEGRA: OnceLock<bool> = OnceLock::new();
    *TEGRA.get_or_init(|| {
        let model = std::fs::read_to_string("/proc/device-tree/model").ok();
        // The device tree pads with NULs.
        let model = model.as_deref().map(|m| m.trim_end_matches('\0').trim());
        super::acceleration::host_is_accelerated(model, Path::new("/etc/nv_tegra_release").exists())
    })
}

/// Encoder dirs whose resident cost has been MEASURED on the Orin: the release gate for
/// picture support on a budgeted device.
///
/// On a budgeted device a model declares vision only if it fits AND its dir is here. It ships
/// EMPTY because nothing has been measured: the arithmetic says E2B and E2B-qat fit, but goose
/// re-subtracts a resident encoder from MemAvailable on every cold text turn, and whether the
/// board clears that during the boot prewarm with E2B, its drafter and its encoder all resident
/// is a question only the board answers. Until it has, the Orin says honestly that it has no
/// picture support and never loads an encoder, which is also better than today, where its
/// truncated E2B stamp loads and fails at every E2B load. Candidates: `gemma-4-e2b-it`,
/// `gemma-4-e2b-it-qat`.
pub const DEVICE_MEASURED_VISION: &[&str] = &[];

/// Whether, and with which encoder, a model reads pictures on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionDeclaration {
    /// No encoder for this model.
    NotDeclared,
    /// It has one, and this device will not carry it.
    NotOnThisDevice(EncoderSpec),
    /// It has one, and this device carries it.
    Declared(EncoderSpec),
}

impl VisionDeclaration {
    pub fn spec(&self) -> Option<&EncoderSpec> {
        match self {
            Self::NotDeclared => None,
            Self::NotOnThisDevice(s) | Self::Declared(s) => Some(s),
        }
    }

    pub fn is_declared(&self) -> bool {
        matches!(self, Self::Declared(_))
    }

    /// The state a model that is not declared reports, before any encoder file is looked at.
    /// `None` when declared: then the file decides.
    pub fn undeclared_state(&self) -> Option<EncoderState> {
        match self {
            Self::NotDeclared => Some(EncoderState::NotDeclared),
            Self::NotOnThisDevice(_) => Some(EncoderState::NotOnThisDevice),
            Self::Declared(_) => None,
        }
    }
}

/// The declaration over its inputs. `fit` is called only on a budgeted device and only for a
/// listed encoder, so an unlisted one costs no file I/O.
pub fn declare(
    chat_model: &str,
    budgeted: bool,
    measured: &[&str],
    fit: impl FnOnce(&EncoderSpec) -> VisionFit,
) -> VisionDeclaration {
    let Some(spec) = encoder_for(chat_model) else {
        return VisionDeclaration::NotDeclared;
    };
    if !budgeted {
        return VisionDeclaration::Declared(spec);
    }
    if !measured.contains(&spec.dir) {
        return VisionDeclaration::NotOnThisDevice(spec);
    }
    if fit(&spec).fits() {
        VisionDeclaration::Declared(spec)
    } else {
        VisionDeclaration::NotOnThisDevice(spec)
    }
}

/// Whether `chat_model` (whose weights are at `gguf_path`, resolved) reads pictures here.
///
/// Off a budgeted device this is the name alone, with no file I/O. On one it is the fit AND
/// the measured list, cached per (path, length, mtime, model): the `<vision>` section sits in
/// the KV-cached static prefix, so the answer must be stable for a model across a process and
/// may change only when the file itself does. `gguf_path` of `None` cannot prove a fit.
pub fn vision_declaration(gguf_path: Option<&Path>, chat_model: &str) -> VisionDeclaration {
    budgeted_declaration(gguf_path, chat_model, budgeted_device())
}

fn budgeted_declaration(
    gguf_path: Option<&Path>,
    chat_model: &str,
    budgeted: bool,
) -> VisionDeclaration {
    declare(
        chat_model,
        budgeted,
        DEVICE_MEASURED_VISION,
        |_| match gguf_path {
            Some(p) => cached_fit(p, chat_model),
            None => VisionFit::CostsWindow {
                window_without: MIN_CTX,
                window_with: MIN_CTX,
            },
        },
    )
}

/// [`vision_declaration`] as a bool.
pub fn declares_vision(gguf_path: Option<&Path>, chat_model: &str) -> bool {
    vision_declaration(gguf_path, chat_model).is_declared()
}

/// [`vision_fit_on_device`], cached per (path, length, mtime, model).
fn cached_fit(path: &Path, chat_model: &str) -> VisionFit {
    type Key = (PathBuf, u64, Option<SystemTime>, String);
    static CACHE: OnceLock<Mutex<HashMap<Key, VisionFit>>> = OnceLock::new();
    let meta = std::fs::metadata(path).ok();
    let key: Key = (
        path.to_path_buf(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.as_ref().and_then(|m| m.modified().ok()),
        chat_model.to_ascii_lowercase(),
    );
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return hit;
    }
    let fit = vision_fit_on_device(path, chat_model);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, fit);
    }
    fit
}

// ── The window apply_jetson_settings stamps ─────────────────────────────────

/// The window for a model on the budgeted device, with the inputs that produced it (for the
/// log line that has to accompany every stamp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceWindow {
    pub window: u32,
    /// The weights charged: the file's length, or [`ASSUMED_LARGEST_MODEL_BYTES`].
    pub model_bytes: u64,
    /// Whether `model_bytes` came from the file.
    pub weights_known: bool,
    /// The header slope, or `None` for the fallback [`KV_KIB_PER_TOKEN`].
    pub kv_kib_per_token: Option<u64>,
    /// See [`drafter_budget_bytes`].
    pub drafter_bytes: u64,
    /// The encoder charged: its size when the model declares vision on the budgeted device.
    pub encoder_bytes: u64,
}

/// The window to stamp for `chat_model` at `gguf_path` (resolved), on the budgeted device.
///
/// Always the budgeted policy, whatever [`budgeted_device`] says, since only the device branch
/// stamps a window. Charges the drafter whenever the model has one ([`drafter_budget_bytes`])
/// and the encoder whenever the model declares vision here, which is the same
/// [`vision_fit_on_device`] the goose adapter's stamp decision reads.
pub fn device_window(gguf_path: Option<&Path>, chat_model: &str) -> DeviceWindow {
    let file_len = gguf_path
        .and_then(|p| std::fs::metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len());
    let kv_kib_per_token = gguf_path.and_then(kv_cost_from_header);
    let drafter_bytes = drafter_budget_bytes(chat_model);
    let encoder_bytes = match budgeted_declaration(gguf_path, chat_model, true) {
        VisionDeclaration::Declared(spec) => spec.size_bytes,
        _ => 0,
    };
    let model_bytes = file_len.unwrap_or(ASSUMED_LARGEST_MODEL_BYTES);
    DeviceWindow {
        window: context_size_with_encoder(
            llm_budget_mb(),
            model_bytes,
            drafter_bytes,
            encoder_bytes,
            kv_kib_per_token,
        ),
        model_bytes,
        weights_known: file_len.is_some(),
        kv_kib_per_token,
        drafter_bytes,
        encoder_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::gguf::test_gguf::GgufWriter;

    /// The Orin's LLM budget, fixed, so these tests do not read the environment.
    const BUDGET: u64 = LLM_BUDGET_MB;

    /// `jetson_context_size` as local-inference computed it: the constant budget.
    fn jetson(model_bytes: u64, drafter_bytes: u64, kv: Option<u64>) -> u32 {
        context_size_for_budget(BUDGET, model_bytes, drafter_bytes, kv)
    }

    // The real files on the Orin, `ls -lL` / `stat -Lc %s`.
    const ORIN_E2B_Q4_K_M: u64 = 3_106_738_272;
    const ORIN_E2B_QAT_UD: u64 = 2_620_370_976;
    const ORIN_E4B_QAT_UD: u64 = 4_215_695_776;
    const ORIN_E4B_IQ4_XS: u64 = 4_715_416_704;
    const E4B_Q4_K_M: u64 = 4_977_171_584;
    /// The real drafter files.
    const E2B_DRAFTER: u64 = 59_235_648;
    const E4B_DRAFTER: u64 = 59_678_016;
    /// Header slopes, KiB/token.
    const E2B_KV: u64 = 18;
    const E4B_KV: u64 = 56;

    #[test]
    fn the_budget_is_the_kernels_ram_less_the_reservation() {
        assert_eq!(LLM_BUDGET_MB, 5820);
        assert_eq!(
            LLM_BUDGET_MB,
            JETSON_TOTAL_RAM_MB - SYSTEM_OVERHEAD_MB - STT_RESERVED_MB - TTS_RESERVED_MB
        );
        if crate::models::domain::device_profile::active().is_none() {
            assert_eq!(llm_budget_mb(), LLM_BUDGET_MB);
            assert_eq!(total_ram_mb(), JETSON_TOTAL_RAM_MB);
        }
    }

    // ── Moved from pond-adapters-local-inference, unchanged in substance ────

    /// Both shipped models, against the DEVICE-measured cost. The EXACT sizes of the two GGUFs
    /// on the device (`stat -Lc %s`, 2026-08-16): the answer is a step function of weight size,
    /// so approximations can land on a different step than the board does (a 336 MB gap once
    /// hid over half of E4B's free KV budget).
    #[test]
    fn jetson_context_fits_each_model_in_the_budget() {
        let e2b = jetson(ORIN_E2B_Q4_K_M, 0, None);
        let e4b = jetson(E4B_Q4_K_M, 0, None);
        assert_eq!(e2b, 16384, "E2B should keep the full window");
        assert_eq!(
            e4b, 8192,
            "E4B should get half the window. It briefly got 16384, on a budget that claimed the \
             marketing 8192 MB of RAM; the kernel reports 7620, and at the real figure E4B's \
             16384 needs 896 MiB of KV it does not have -- it was running out of swap."
        );
        assert!(e4b <= e2b);
    }

    /// The emulator's whole claim, as arithmetic: a different device budget produces a different
    /// window for the same weights. If the budget stopped reaching the derivation,
    /// `scripts/jetson-emu.sh` would still announce it was emulating while testing the Mac.
    #[test]
    fn a_different_device_budget_produces_a_different_window() {
        let nano = 7620 - RESERVED_MB;
        let nx = 15564 - RESERVED_MB;
        let on_nano = context_size_for_budget(nano, E4B_Q4_K_M, 0, None);
        let on_nx = context_size_for_budget(nx, E4B_Q4_K_M, 0, None);
        assert_eq!(on_nano, 8192, "the board we actually have");
        assert!(on_nx > on_nano, "got {on_nx} against {on_nano}");
    }

    #[test]
    fn the_orin_profile_reproduces_the_devices_own_windows() {
        let budget = 7620 - (1500 + 200 + 100);
        assert_eq!(
            context_size_for_budget(budget, ORIN_E2B_Q4_K_M, 0, None),
            16384
        );
        assert_eq!(context_size_for_budget(budget, E4B_Q4_K_M, 0, None), 8192);
    }

    /// A budget smaller than the weights must clamp, not underflow into a huge window.
    #[test]
    fn an_impossible_budget_clamps_instead_of_wrapping() {
        assert_eq!(context_size_for_budget(512, E4B_Q4_K_M, 0, None), 2048);
    }

    /// E4B at its window fits the MEASURED budget with real headroom, and doubling again does not
    /// fit at all. The arithmetic is redone here rather than copied from the function, so a test
    /// cannot agree with the same mistake.
    #[test]
    fn e4b_fits_its_window_and_could_not_take_another_doubling() {
        let weights_mb = 4_640_000_000u64 / MIB;
        let free_mb = LLM_BUDGET_MB - weights_mb - 600;
        let chosen = u64::from(jetson(4_640_000_000, 0, None));
        let needed_mb = (chosen * E4B_KV) / 1024;
        assert!(
            needed_mb < free_mb,
            "{chosen} tokens need {needed_mb} of {free_mb} MiB"
        );
        let doubled_mb = (chosen * 2 * E4B_KV) / 1024;
        assert!(
            doubled_mb > free_mb,
            "doubling now fits: {doubled_mb} vs {free_mb}"
        );
    }

    #[test]
    fn the_per_token_slope_is_observable_on_a_model_the_ceiling_does_not_cap() {
        assert_eq!(jetson(4_739_563_520, 0, None), 12288);
    }

    /// E4B IQ4_XS affords 13,220 tokens; a power-of-two floor handed back 8,192.
    #[test]
    fn rounding_does_not_discard_context_the_budget_affords() {
        assert_eq!(jetson(ORIN_E4B_IQ4_XS, 0, None), 12288);
        for bytes in [4_600_000_000u64, 4_700_000_000, 4_800_000_000] {
            let ctx = u64::from(jetson(bytes, 0, None));
            let kv_mb = LLM_BUDGET_MB
                .saturating_sub(bytes / MIB)
                .saturating_sub(600);
            let affords = (kv_mb * 1024) / 56;
            assert!(ctx <= affords.max(2048), "{bytes}: {ctx} vs {affords}");
            assert_eq!(ctx % 1024, 0, "{bytes}: {ctx}");
        }
    }

    #[test]
    fn e2b_is_ceiling_bound_and_e4b_is_budget_bound() {
        let e2b_free = LLM_BUDGET_MB - 2_890_000_000u64 / MIB - 600;
        assert!((e2b_free * 1024) / E2B_KV > 100_000);
        let e4b_free = LLM_BUDGET_MB - 4_640_000_000u64 / MIB - 600;
        assert!((8_192..16_384).contains(&((e4b_free * 1024) / E4B_KV)));
    }

    /// Wiring the header-derived cost in did not move either shipped model.
    #[test]
    fn header_derived_cost_is_a_no_op_for_the_shipped_models() {
        for (bytes, kv, want) in [
            (ORIN_E2B_Q4_K_M, E2B_KV, 16384u32),
            (E4B_Q4_K_M, E4B_KV, 8192),
            (ORIN_E4B_IQ4_XS, E4B_KV, 12288),
        ] {
            let derived = jetson(bytes, 0, Some(kv));
            assert_eq!(derived, want, "{bytes} at {kv}");
            assert_eq!(derived, jetson(bytes, 0, None), "{bytes}");
        }
    }

    #[test]
    fn a_cheaper_model_is_no_longer_charged_the_widest_geometry() {
        let bytes = 4_500u64 * MIB;
        assert!(jetson(bytes, 0, Some(28)) > jetson(bytes, 0, None));
    }

    #[test]
    fn a_wider_model_is_charged_for_it() {
        let bytes = 4_000_000_000u64;
        assert!(jetson(bytes, 0, Some(168)) < jetson(bytes, 0, None));
    }

    #[test]
    fn a_useless_slope_falls_back_rather_than_dividing_by_zero() {
        assert_eq!(jetson(E4B_Q4_K_M, 0, Some(0)), jetson(E4B_Q4_K_M, 0, None));
    }

    #[test]
    fn jetson_context_floors_for_an_oversized_model() {
        assert_eq!(jetson(9_000_000_000, 0, None), 2048);
    }

    #[test]
    fn a_drafter_costs_window() {
        let without = jetson(ORIN_E2B_Q4_K_M, 0, Some(E2B_KV));
        let with = jetson(ORIN_E2B_Q4_K_M, E2B_DRAFTER, Some(E2B_KV));
        assert!(with <= without);
        assert!(with >= 8192, "got {with}");
    }

    /// The drafter's cost is flat in `n_ctx`, so it must not be folded into the slope.
    #[test]
    fn the_drafter_is_charged_once_not_per_token() {
        let without = u64::from(context_size_for_budget(BUDGET, E4B_Q4_K_M, 0, Some(E4B_KV)));
        let with = u64::from(context_size_for_budget(
            BUDGET,
            E4B_Q4_K_M,
            E4B_DRAFTER,
            Some(E4B_KV),
        ));
        let expected_loss = (57 + 64) * 1024 / 56;
        assert!(without.saturating_sub(with) <= expected_loss + 1024);
    }

    // ── The move is behaviour-identical ────────────────────────────────────

    /// The Orin's live model with its drafter keeps 16384 through the move, charged either the
    /// real drafter file (the old input) or the catalogue size (the new one).
    #[test]
    fn e4b_qat_with_its_drafter_still_gets_16384() {
        assert_eq!(
            jetson(ORIN_E4B_QAT_UD, E4B_DRAFTER, Some(E4B_KV)),
            16384,
            "the old input: the drafter file's own size"
        );
        assert_eq!(
            jetson(
                ORIN_E4B_QAT_UD,
                drafter_budget_bytes("gemma-4-E4B-it-qat-UD-Q4_K_XL"),
                Some(E4B_KV)
            ),
            16384,
            "the new input: the catalogue size"
        );
    }

    /// No encoder means the old function exactly, across a sweep of sizes and slopes.
    #[test]
    fn a_zero_encoder_is_the_old_arithmetic() {
        for bytes in (0..9_000_000_000u64).step_by(97_000_000) {
            for kv in [None, Some(0), Some(18), Some(56), Some(168)] {
                for drafter in [0, E2B_DRAFTER] {
                    assert_eq!(
                        context_size_with_encoder(BUDGET, bytes, drafter, 0, kv),
                        context_size_for_budget(BUDGET, bytes, drafter, kv)
                    );
                }
            }
        }
    }

    /// Charging the catalogue figure rather than the file lands every shipped model on the same
    /// window, so the switch to it moves nothing on the board.
    #[test]
    fn the_catalogue_drafter_size_lands_on_the_same_windows_as_the_file() {
        for (model, bytes, file, kv) in [
            (
                "gemma-4-E2B-it-Q4_K_M",
                ORIN_E2B_Q4_K_M,
                E2B_DRAFTER,
                E2B_KV,
            ),
            (
                "gemma-4-E2B-it-qat-UD-Q4_K_XL",
                ORIN_E2B_QAT_UD,
                E2B_DRAFTER,
                E2B_KV,
            ),
            (
                "gemma-4-E4B-it-qat-UD-Q4_K_XL",
                ORIN_E4B_QAT_UD,
                E4B_DRAFTER,
                E4B_KV,
            ),
            (
                "gemma-4-E4B-it-IQ4_XS",
                ORIN_E4B_IQ4_XS,
                E4B_DRAFTER,
                E4B_KV,
            ),
            ("gemma-4-E4B-it-Q4_K_M", E4B_Q4_K_M, E4B_DRAFTER, E4B_KV),
        ] {
            assert_eq!(
                jetson(bytes, drafter_budget_bytes(model), Some(kv)),
                jetson(bytes, file, Some(kv)),
                "{model}"
            );
        }
        assert_eq!(drafter_budget_bytes("gemma-4-E2B-it"), 57 * MIB);
        assert_eq!(drafter_budget_bytes("Llama-3.2-3B-Instruct"), 0);
        assert_eq!(drafter_budget_bytes("gemma-4-12b-it"), 0);
    }

    // ── The header slope ───────────────────────────────────────────────────

    fn gemma4_head(blocks: u32, kv_heads: u32, shared: u32) -> Vec<u8> {
        GgufWriter::new()
            .str("general.architecture", "gemma4")
            .u32("gemma4.block_count", blocks)
            .u32(
                "gemma4.embedding_length",
                if kv_heads == 1 { 1536 } else { 2560 },
            )
            .u32("gemma4.attention.head_count_kv", kv_heads)
            .u32("gemma4.attention.key_length", 512)
            .u32("gemma4.attention.value_length", 512)
            .u32("gemma4.attention.key_length_swa", 256)
            .u32("gemma4.attention.value_length_swa", 256)
            .u32("gemma4.attention.shared_kv_layers", shared)
            .build_header()
    }

    /// The two device measurements, reproduced from headers written the way the real files are.
    #[test]
    fn the_header_slope_reproduces_the_device_measurements() {
        assert_eq!(kv_cost_from_head(&gemma4_head(35, 1, 20)), Some(E2B_KV));
        assert_eq!(kv_cost_from_head(&gemma4_head(42, 2, 18)), Some(E4B_KV));
    }

    /// An unconfirmed sliding-window architecture keeps the fallback rather than a guess; a
    /// dense one is exact whatever it is called.
    #[test]
    fn only_confirmed_swa_patterns_are_trusted() {
        let unconfirmed = GgufWriter::new()
            .str("general.architecture", "newarch")
            .u32("newarch.block_count", 30)
            .u32("newarch.attention.head_count_kv", 2)
            .u32("newarch.attention.key_length", 128)
            .u32("newarch.attention.value_length", 128)
            .u32("newarch.attention.key_length_swa", 64)
            .u32("newarch.attention.value_length_swa", 64)
            .build_header();
        assert_eq!(kv_cost_from_head(&unconfirmed), None);

        let dense = GgufWriter::new()
            .str("general.architecture", "llama")
            .u32("llama.block_count", 28)
            .u32("llama.attention.head_count_kv", 2)
            .u32("llama.attention.key_length", 128)
            .u32("llama.attention.value_length", 128)
            .build_header();
        assert_eq!(kv_cost_from_head(&dense), Some(28));
        assert_eq!(kv_cost_from_head(b"not gguf"), None);
    }

    // ── Vision fit ─────────────────────────────────────────────────────────

    /// The Orin's real files, at every plausible encoder compute cost: E2B fits (it is
    /// ceiling-bound with ~900 MiB to spare), E4B does not (the encoder floors E4B-qat to 2048,
    /// under its own 4,678-token turn-1 prompt). Because the split holds across 0..=900, the
    /// unmeasured constant cannot flip a declaration.
    #[test]
    fn the_orin_files_fit_or_not_whatever_the_encoder_compute_costs() {
        let cases = [
            ("gemma-4-E2B-it-Q4_K_M", ORIN_E2B_Q4_K_M, E2B_KV, true),
            (
                "gemma-4-E2B-it-qat-UD-Q4_K_XL",
                ORIN_E2B_QAT_UD,
                E2B_KV,
                true,
            ),
            (
                "gemma-4-E4B-it-qat-UD-Q4_K_XL",
                ORIN_E4B_QAT_UD,
                E4B_KV,
                false,
            ),
            ("gemma-4-E4B-it-IQ4_XS", ORIN_E4B_IQ4_XS, E4B_KV, false),
        ];
        for (model, bytes, kv, fits) in cases {
            for compute in 0..=900u64 {
                let fit = vision_fit(5820, Some(bytes), model, Some(kv), compute);
                assert_eq!(
                    fit.fits(),
                    fits,
                    "{model} at ENCODER_COMPUTE_MB={compute}: {fit:?}"
                );
                if let VisionFit::Fits { window } = fit {
                    assert_eq!(window, 16384, "{model}");
                }
            }
        }
        assert!(matches!(
            vision_fit(
                5820,
                Some(ORIN_E4B_QAT_UD),
                "gemma-4-E4B-it-qat",
                Some(E4B_KV),
                0
            ),
            VisionFit::CostsWindow {
                window_without: 16384,
                window_with: 2048
            }
        ));
    }

    /// The fail-open the unclamped predicate exists to close: a model already at the floor gets
    /// the floor with the encoder too, and must still not be declared.
    #[test]
    fn a_model_already_at_the_floor_is_not_declared() {
        for bytes in [ASSUMED_LARGEST_MODEL_BYTES, 5_300_000_000, 9_000_000_000] {
            let fit = vision_fit(5820, Some(bytes), "gemma-4-E2B-it", Some(E2B_KV), 0);
            assert_eq!(
                fit,
                VisionFit::CostsWindow {
                    window_without: MIN_CTX,
                    window_with: MIN_CTX
                },
                "{bytes}: the encoder must not look free just because both windows clamp"
            );
        }
    }

    #[test]
    fn unknown_weights_are_never_a_fit() {
        for budget in [5820, 13764, 1_000_000] {
            let fit = vision_fit(budget, None, "gemma-4-E2B-it", Some(E2B_KV), 0);
            assert!(!fit.fits(), "budget {budget}: {fit:?}");
        }
        assert!(!vision_fit_on_device(Path::new("/nowhere/at/all.gguf"), "gemma-4-E2B-it").fits());
    }

    #[test]
    fn a_model_with_no_encoder_is_not_declared_before_any_arithmetic() {
        assert_eq!(
            vision_fit(5820, Some(1), "Llama-3.2-3B-Instruct", Some(28), 0),
            VisionFit::NotDeclared
        );
        assert_eq!(
            vision_fit_on_device(Path::new("/nowhere"), "granite-4.1-3b"),
            VisionFit::NotDeclared
        );
    }

    /// With the encoder charged, E4B-qat's window drops to the floor: the reason the Orin must not
    /// declare it.
    #[test]
    fn the_encoder_is_charged_its_weights_and_compute() {
        let enc = crate::models::domain::vision_encoder::encoder_by_dir("gemma-4-e4b-it-qat")
            .unwrap()
            .size_bytes;
        assert_eq!(
            context_size_with_encoder(BUDGET, ORIN_E4B_QAT_UD, 57 * MIB, enc, Some(E4B_KV)),
            MIN_CTX
        );
        let e2b = crate::models::domain::vision_encoder::encoder_by_dir("gemma-4-e2b-it")
            .unwrap()
            .size_bytes;
        assert_eq!(
            context_size_with_encoder(BUDGET, ORIN_E2B_Q4_K_M, 57 * MIB, e2b, Some(E2B_KV)),
            16384
        );
    }

    /// A GGUF with the real file's length and header, as a sparse file: the end-to-end path the
    /// adapters take, reading the length through `metadata` and the slope from the head.
    #[cfg(unix)]
    fn sparse_model(dir: &Path, name: &str, len: u64, head: &[u8]) -> PathBuf {
        use std::io::Write as _;
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(head).unwrap();
        f.set_len(len).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn the_fit_reads_the_resolved_file_itself() {
        if crate::models::domain::device_profile::active().is_some() {
            return; // the runtime budget is a profile's, not the Orin's
        }
        let tmp = tempfile::tempdir().unwrap();
        let e2b = sparse_model(
            tmp.path(),
            "e2b.gguf",
            ORIN_E2B_Q4_K_M,
            &gemma4_head(35, 1, 20),
        );
        let e4b = sparse_model(
            tmp.path(),
            "e4b.gguf",
            ORIN_E4B_QAT_UD,
            &gemma4_head(42, 2, 18),
        );
        assert_eq!(
            vision_fit_on_device(&e2b, "gemma-4-E2B-it-Q4_K_M"),
            VisionFit::Fits { window: 16384 }
        );
        assert!(!vision_fit_on_device(&e4b, "gemma-4-E4B-it-qat-UD-Q4_K_XL").fits());

        // Through a link, as models/gguf holds them.
        let link = tmp.path().join("link.gguf");
        std::os::unix::fs::symlink(&e2b, &link).unwrap();
        assert!(vision_fit_on_device(&link, "gemma-4-E2B-it").fits());

        // The window apply_jetson_settings stamps: the drafter charged, no encoder (nothing is
        // measured, so nothing is declared), and the same 16384 E4B-qat has on the board.
        let w = device_window(Some(&e4b), "gemma-4-E4B-it-qat-UD-Q4_K_XL");
        assert_eq!(w.window, 16384);
        assert_eq!(w.kv_kib_per_token, Some(E4B_KV));
        assert_eq!(w.drafter_bytes, 57 * MIB);
        assert_eq!(w.encoder_bytes, 0);
        assert!(w.weights_known);

        let missing = device_window(Some(&tmp.path().join("gone.gguf")), "gemma-4-E4B-it");
        assert!(!missing.weights_known);
        assert_eq!(missing.model_bytes, ASSUMED_LARGEST_MODEL_BYTES);
        assert_eq!(missing.window, MIN_CTX);
    }

    // ── Declaration and the release gate ───────────────────────────────────

    /// Nothing has been measured on the Orin, so nothing is declared there. Adding an entry is a
    /// release decision backed by an Orin reading, and must name a real row.
    #[test]
    fn the_measured_list_ships_empty_and_names_only_real_rows() {
        assert!(
            DEVICE_MEASURED_VISION.is_empty(),
            "an entry here puts a ~1 GB GPU-resident encoder on the Orin; it needs the \
             MemAvailable reading during the boot prewarm first"
        );
        for d in DEVICE_MEASURED_VISION {
            assert!(
                crate::models::domain::vision_encoder::encoder_by_dir(d).is_some(),
                "{d}"
            );
        }
    }

    #[test]
    fn off_the_budgeted_device_the_name_decides_with_no_file_io() {
        let never = |_: &EncoderSpec| -> VisionFit { panic!("no fit may be computed off-device") };
        assert!(declare("gemma-4-E4B-it-qat-UD-Q4_K_XL", false, &[], never).is_declared());
        assert_eq!(
            declare("Llama-3.2-3B-Instruct", false, &[], never),
            VisionDeclaration::NotDeclared
        );
    }

    #[test]
    fn on_the_budgeted_device_a_model_needs_the_list_and_the_fit() {
        let never = |_: &EncoderSpec| -> VisionFit { panic!("an unlisted encoder costs no I/O") };
        assert!(matches!(
            declare("gemma-4-E2B-it", true, &[], never),
            VisionDeclaration::NotOnThisDevice(s) if s.dir == "gemma-4-e2b-it"
        ));
        let listed = &["gemma-4-e2b-it"];
        assert!(
            declare("gemma-4-E2B-it", true, listed, |_| VisionFit::Fits {
                window: 16384
            })
            .is_declared()
        );
        assert!(
            !declare("gemma-4-E2B-it", true, listed, |_| VisionFit::CostsWindow {
                window_without: 16384,
                window_with: 2048
            })
            .is_declared()
        );
        assert!(
            !declare("gemma-4-E2B-it-qat", true, listed, |_| VisionFit::Fits {
                window: 1
            })
            .is_declared(),
            "the qat row is its own entry"
        );
        assert_eq!(
            declare("granite-4.1-3b", true, listed, never),
            VisionDeclaration::NotDeclared
        );
    }

    #[test]
    fn a_declaration_reports_its_own_state_before_any_file_is_read() {
        let spec = crate::models::domain::vision_encoder::encoder_by_dir("gemma-4-e4b-it").unwrap();
        assert_eq!(
            VisionDeclaration::NotDeclared.undeclared_state(),
            Some(EncoderState::NotDeclared)
        );
        assert_eq!(
            VisionDeclaration::NotOnThisDevice(spec).undeclared_state(),
            Some(EncoderState::NotOnThisDevice)
        );
        assert_eq!(VisionDeclaration::Declared(spec).undeclared_state(), None);
        assert_eq!(VisionDeclaration::Declared(spec).spec(), Some(&spec));
    }

    /// Today, with nothing measured, the budgeted policy declares nothing for any model and never
    /// needs the file, which is what keeps the Orin from loading an encoder.
    #[test]
    fn the_budgeted_policy_today_declares_nothing() {
        for model in [
            "gemma-4-E2B-it-Q4_K_M",
            "gemma-4-E2B-it-qat-UD-Q4_K_XL",
            "gemma-4-E4B-it-qat-UD-Q4_K_XL",
            "gemma-4-E4B-it-IQ4_XS",
        ] {
            assert!(
                !budgeted_declaration(None, model, true).is_declared(),
                "{model}"
            );
        }
        assert!(budgeted_declaration(None, "gemma-4-E2B-it", false).is_declared());
    }

    #[test]
    fn any_one_signal_makes_the_device_budgeted() {
        assert!(!budgeted_from(false, false, false));
        assert!(budgeted_from(true, false, false), "a CUDA build");
        assert!(budgeted_from(false, true, false), "an emulation profile");
        assert!(
            budgeted_from(false, false, true),
            "a CPU build on the board"
        );
    }
}
