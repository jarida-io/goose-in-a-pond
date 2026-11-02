//! What the budgeted device (8 GB Orin Nano) can hold beside the KV cache: each model's window
//! and whether it may carry a vision encoder.
//!
//! The window sizer and the encoder decision must both call [`device_window`] /
//! [`vision_fit_on_device`] with the RESOLVED GGUF path, or they disagree and the board OOMs.
//! Every constant is an Orin measurement or says it is not; only an Orin reading may move one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::gguf::parse_gguf_header;
use super::vision_encoder::{EncoderSpec, EncoderState};
use super::vision_pairing::encoder_for_model;

const MIB: u64 = 1024 * 1024;

// ── Jetson memory constants ─────────────────────────────────────────────────

/// Orin Nano RAM (MB) as `free -m` reports it after carveouts, not the marketed 8192.
/// Overstating it never errors: an over-large context just swaps.
pub const JETSON_TOTAL_RAM_MB: u64 = 7620;
/// Approximate headroom used by OS + GIAP server + UI at idle (MB).
pub const SYSTEM_OVERHEAD_MB: u64 = 1500;
/// Whisper base model resident size (MB). Since #427 the accurate-profile state also stays
/// allocated between transcriptions: 208 MB with five-beam decoding, plus a CUDA scratch pool,
/// measured on the Orin with `base` on 2026-10-05 (docs/voice-pipeline-efficiency.md). It is NOT
/// reserved here: adding it halves E4B's window to 4096, below its ~4,500-token preamble. It
/// comes out of headroom instead -- the Orin had 1,061 MB available after an E4B prewarm on
/// 2026-10-06, before #427. Re-read free memory on the Orin after the first transcription with
/// #427 deployed before moving this.
pub const STT_RESERVED_MB: u64 = 200;
/// Reserved TTS resident size (MB).
pub const TTS_RESERVED_MB: u64 = 100;
/// Everything the LLM slot does not get; shared by [`LLM_BUDGET_MB`] and `llm_budget_mb()`.
pub const RESERVED_MB: u64 = SYSTEM_OVERHEAD_MB + STT_RESERVED_MB + TTS_RESERVED_MB;
/// Approximate MB available for a single LLM slot.
pub const LLM_BUDGET_MB: u64 = JETSON_TOTAL_RAM_MB - RESERVED_MB;

/// Total RAM of the device this process believes it is: [`JETSON_TOTAL_RAM_MB`] unless a
/// device profile overrides it. Never probed, or a dev Mac's 64 GB would make everything fit.
pub fn total_ram_mb() -> u64 {
    super::device_profile::active()
        .map(|p| p.total_ram_mb)
        .unwrap_or(JETSON_TOTAL_RAM_MB)
}

/// Runtime twin of [`LLM_BUDGET_MB`]; equal to it unless a device profile is active.
pub fn llm_budget_mb() -> u64 {
    total_ram_mb().saturating_sub(RESERVED_MB)
}

// ── Desktop budget ──────────────────────────────────────────────────────────
// Not Orin measurements: a desktop reports its own memory, and these only decide how much of it
// the LLM slot may take.

/// The least a desktop keeps from the LLM slot (MB): the system, the household's other programs
/// and the rest of the pond.
pub const HOST_RESERVED_MIN_MB: u64 = 4096;
/// The share of a desktop's memory it keeps from the LLM slot (percent), when larger than the
/// floor. A quarter is what macOS keeps back from Metal's working set on most Macs.
pub const HOST_RESERVED_PERCENT: u64 = 25;

/// The LLM slot's budget on a desktop with `total_mb` of memory: total less the larger of the
/// floor and the share. A budgeted device uses [`llm_budget_mb`] instead.
pub fn host_llm_budget_mb(total_mb: u64) -> u64 {
    let reserve = (total_mb * HOST_RESERVED_PERCENT / 100).max(HOST_RESERVED_MIN_MB);
    total_mb.saturating_sub(reserve)
}

// ── Window arithmetic ───────────────────────────────────────────────────────

/// KV KiB/token of the widest shipped geometry (E4B; E2B is 18), measured on the Orin. Unpadded
/// (margin is in the budget), no constant term: llama.cpp gives both caches `n_ctx` cells.
pub const KV_KIB_PER_TOKEN: u64 = 56;
/// llama.cpp's compute buffers: measured 522 MiB at 4096-16384 and 582 at 32768, nearly flat.
pub const COMPUTE_BUFFER_MB: u64 = 600;
/// Vision encoder compute beyond its weights (read whole, not mmapped). UNMEASURED; tests pin
/// that no shipped model's declaration flips anywhere in 0..=900.
pub const ENCODER_COMPUTE_MB: u64 = 256;
/// The narrowest window handed out, whatever the budget.
pub const MIN_CTX: u32 = 2048;
/// Widest window handed out; a latency cap (cold prefill at 16384 is ~20 s), not a memory one.
pub const MAX_CTX: u32 = 16384;
/// Windows floor to a multiple of this. Not a power of two (llama.cpp needs none): flooring to
/// one discards up to half an affordable window.
pub const CTX_GRANULARITY: u32 = 1024;
/// Weights assumed for an unreadable model file (the largest we ship); never proves a vision fit.
pub const ASSUMED_LARGEST_MODEL_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// MB left for the KV cache once everything resident is paid for.
fn kv_allowance_mb(
    budget_mb: u64,
    model_bytes: u64,
    encoder_bytes: u64,
    encoder_compute_mb: u64,
) -> u64 {
    let model_mb = model_bytes / MIB;
    // The encoder loads eagerly at every model load and cannot be reclaimed as page cache.
    let encoder_mb = if encoder_bytes > 0 {
        encoder_bytes / MIB + encoder_compute_mb
    } else {
        0
    };
    budget_mb
        .saturating_sub(model_mb)
        .saturating_sub(COMPUTE_BUFFER_MB)
        .saturating_sub(encoder_mb)
}

/// The model's header slope if known, else the conservative fallback `KV_KIB_PER_TOKEN`.
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
fn window_for_tokens(tokens: u64) -> u32 {
    let granularity = u64::from(CTX_GRANULARITY);
    let floored = (tokens / granularity) * granularity;
    floored.min(u64::from(MAX_CTX)).max(u64::from(MIN_CTX)) as u32
}

/// Window that fits a model in `budget_mb`, floored to [`CTX_GRANULARITY`]. The budget is an
/// argument because reading it from the environment races in threaded tests.
pub fn context_size_for_budget(
    budget_mb: u64,
    model_bytes: u64,
    kv_kib_per_token: Option<u64>,
) -> u32 {
    context_size_with_encoder(budget_mb, model_bytes, 0, kv_kib_per_token)
}

/// [`context_size_for_budget`] plus a resident encoder: its weights and [`ENCODER_COMPUTE_MB`].
pub fn context_size_with_encoder(
    budget_mb: u64,
    model_bytes: u64,
    encoder_bytes: u64,
    kv_kib_per_token: Option<u64>,
) -> u32 {
    let kv_mb = kv_allowance_mb(budget_mb, model_bytes, encoder_bytes, ENCODER_COMPUTE_MB);
    window_for_tokens(tokens_for(kv_mb, slope(kv_kib_per_token)))
}

/// MB a model switch would free: the in-use in-process model's weights (and add-on), but never
/// more than the budget is short of right now. Where `available` is the budget itself (no live
/// reading, as off Linux), nothing is held back from it, so nothing is reclaimable.
pub fn reclaimable_mb(in_use_mb: u64, budget_mb: u64, available_for_llm_mb: u64) -> u64 {
    in_use_mb.min(budget_mb.saturating_sub(available_for_llm_mb))
}

// ── The header slope ────────────────────────────────────────────────────────

/// Architectures whose global:SWA layer ratio was confirmed against llama.cpp's KV-cache log
/// lines on the Orin (Gemma 4: 1 global per 5 sliding).
pub const CONFIRMED_SWA_PATTERNS: &[(&str, u32)] = &[("gemma4", 5)];

/// Generous cover for the GGUF geometry, which sits in the first ~2 KB of every file measured.
pub const HEAD_BYTES: usize = 64 * 1024;

/// KV KiB/token from a GGUF head, or `None` to keep the measured constant. Exact for dense
/// models; SWA ratios are absent from the header (a 2x OOM risk), so only confirmed ones count.
pub fn kv_cost_from_head(head: &[u8]) -> Option<u64> {
    let info = parse_gguf_header(head)?;
    let arch = info.architecture.as_deref().unwrap_or_default();

    if info.key_length_swa.is_none() {
        // Dense: exact for any architecture; the ratio argument is unused.
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

/// Fits iff the UNCLAMPED token allowance with the encoder reaches the window without it
/// (clamped windows fail open at [`MIN_CTX`]). `model_bytes` of `None` never fits.
pub fn vision_fit(
    budget_mb: u64,
    model_bytes: Option<u64>,
    encoder: Option<&EncoderSpec>,
    kv_kib_per_token: Option<u64>,
    encoder_compute_mb: u64,
) -> VisionFit {
    let Some(spec) = encoder else {
        return VisionFit::NotDeclared;
    };
    let slope = slope(kv_kib_per_token);
    let weights = model_bytes.unwrap_or(ASSUMED_LARGEST_MODEL_BYTES);

    let without = tokens_for(kv_allowance_mb(budget_mb, weights, 0, 0), slope);
    let window_without = window_for_tokens(without);
    let with = tokens_for(
        kv_allowance_mb(budget_mb, weights, spec.size_bytes, encoder_compute_mb),
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

/// [`vision_fit`] for the GGUF at `gguf_path` on this device; reads only its length and head.
pub fn vision_fit_on_device(gguf_path: &Path, encoder: Option<&EncoderSpec>) -> VisionFit {
    if encoder.is_none() {
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
        encoder,
        kv,
        ENCODER_COMPUTE_MB,
    )
}

// ── The budgeted device ─────────────────────────────────────────────────────

/// Set once by pond-server from the CUDA build flag. Only ever raised: see [`budgeted_device`].
static BUDGETED_OVERRIDE: AtomicBool = AtomicBool::new(false);

/// Record a CUDA build (only the Orin runs one); the feature's `cfg!` is not visible here.
/// `false` is a no-op, never a reset: the getter must fail closed to the Orin policy.
pub fn set_budgeted_device(cuda_build: bool) {
    if cuda_build {
        BUDGETED_OVERRIDE.store(true, Ordering::SeqCst);
    }
}

/// Whether this process must live within the Orin's memory budget. Any one signal suffices, so
/// a missed setter, an emulator run or a CPU build on the board all get the narrow policy.
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

/// Encoder dirs whose resident cost was MEASURED on the Orin; a budgeted device declares vision
/// only for a model that fits AND is listed. Ships empty: goose re-subtracts a resident encoder
/// from MemAvailable each cold text turn, which only a board reading can clear.
pub const DEVICE_MEASURED_VISION: &[&str] = &[];

/// Pinned Gemma family vision encoder directories supported on Jetson devices.
pub const GEMMA_VISION_ALLOWLIST: &[&str] = &[
    "gemma-4-e2b-it-qat",
    "gemma-4-e2b-it",
    "gemma-4-e4b-it-qat",
    "gemma-4-e4b-it",
];

static RUNTIME_VISION_ALLOWLIST: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

/// Set the runtime allowlist of vision encoder directories.
pub fn set_vision_allowlist(dirs: &[&str]) {
    let mut list = RUNTIME_VISION_ALLOWLIST
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap();
    list.clear();
    for d in dirs {
        list.push((*d).to_string());
    }
}

/// Allow the Gemma family vision encoders on this process.
pub fn allow_gemma_vision() {
    set_vision_allowlist(GEMMA_VISION_ALLOWLIST);
}

/// Clear the runtime vision allowlist.
pub fn clear_vision_allowlist() {
    if let Some(lock) = RUNTIME_VISION_ALLOWLIST.get() {
        if let Ok(mut list) = lock.lock() {
            list.clear();
        }
    }
}

/// Check whether an encoder directory is on the vision allowlist.
pub fn is_vision_allowlisted(dir: &str) -> bool {
    if let Some(lock) = RUNTIME_VISION_ALLOWLIST.get() {
        if let Ok(guard) = lock.lock() {
            if guard.iter().any(|d| d.eq_ignore_ascii_case(dir)) {
                return true;
            }
        }
    }
    if let Ok(env_val) = std::env::var("POND_VISION_ALLOWLIST") {
        let val = env_val.trim();
        if val == "1" || val.eq_ignore_ascii_case("gemma") || val.eq_ignore_ascii_case("all") {
            return GEMMA_VISION_ALLOWLIST
                .iter()
                .any(|d| d.eq_ignore_ascii_case(dir));
        }
        return val
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case(dir));
    }
    false
}

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

/// The declaration over its inputs; `fit` runs only for a listed encoder on a budgeted device.
pub fn declare(
    encoder: Option<EncoderSpec>,
    budgeted: bool,
    measured: &[&str],
    fit: impl FnOnce(&EncoderSpec) -> VisionFit,
) -> VisionDeclaration {
    let Some(spec) = encoder else {
        return VisionDeclaration::NotDeclared;
    };
    if !budgeted {
        return VisionDeclaration::Declared(spec);
    }
    let allowlisted = is_vision_allowlisted(spec.dir);
    if !measured.contains(&spec.dir) && !allowlisted {
        return VisionDeclaration::NotOnThisDevice(spec);
    }
    if allowlisted || fit(&spec).fits() {
        VisionDeclaration::Declared(spec)
    } else {
        VisionDeclaration::NotOnThisDevice(spec)
    }
}

/// Whether `chat_model` reads pictures here: by its pairing off a budgeted device, else fit AND
/// listed. Cached per file: `<vision>` sits in the KV-cached prefix, so it may change only with
/// the file.
pub fn vision_declaration(gguf_path: Option<&Path>, chat_model: &str) -> VisionDeclaration {
    budgeted_declaration(gguf_path, chat_model, budgeted_device())
}

fn budgeted_declaration(
    gguf_path: Option<&Path>,
    chat_model: &str,
    budgeted: bool,
) -> VisionDeclaration {
    declare(
        encoder_for_model(chat_model, gguf_path),
        budgeted,
        DEVICE_MEASURED_VISION,
        |spec| match gguf_path {
            Some(p) => cached_fit(p, spec),
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

/// [`vision_fit_on_device`], cached per (path, length, mtime, encoder).
fn cached_fit(path: &Path, spec: &EncoderSpec) -> VisionFit {
    type Key = (PathBuf, u64, Option<SystemTime>, &'static str);
    static CACHE: OnceLock<Mutex<HashMap<Key, VisionFit>>> = OnceLock::new();
    let meta = std::fs::metadata(path).ok();
    let key: Key = (
        path.to_path_buf(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.as_ref().and_then(|m| m.modified().ok()),
        spec.dir,
    );
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return hit;
    }
    let fit = vision_fit_on_device(path, Some(spec));
    if let Ok(mut c) = cache.lock() {
        c.insert(key, fit);
    }
    fit
}

// ── The window apply_jetson_settings stamps ─────────────────────────────────

/// A model's window on the budgeted device, with the inputs behind it for the stamp's log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceWindow {
    pub window: u32,
    /// The weights charged: the file's length, or [`ASSUMED_LARGEST_MODEL_BYTES`].
    pub model_bytes: u64,
    /// Whether `model_bytes` came from the file.
    pub weights_known: bool,
    /// The header slope, or `None` for the fallback [`KV_KIB_PER_TOKEN`].
    pub kv_kib_per_token: Option<u64>,
    /// The encoder charged: its size when the model declares vision on the budgeted device.
    pub encoder_bytes: u64,
}

/// The window to stamp for `chat_model`, always under the budgeted policy. Charges the encoder
/// only if declared, by the same [`vision_fit_on_device`] the goose adapter's stamp reads.
pub fn device_window(gguf_path: Option<&Path>, chat_model: &str) -> DeviceWindow {
    let file_len = gguf_path
        .and_then(|p| std::fs::metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len());
    let kv_kib_per_token = gguf_path.and_then(kv_cost_from_header);
    let encoder_spec = match budgeted_declaration(gguf_path, chat_model, true) {
        VisionDeclaration::Declared(spec) => Some(spec),
        _ => None,
    };
    let encoder_bytes = encoder_spec.as_ref().map_or(0, |s| s.size_bytes);
    let model_bytes = file_len.unwrap_or(ASSUMED_LARGEST_MODEL_BYTES);
    let mut window = context_size_with_encoder(
        llm_budget_mb(),
        model_bytes,
        encoder_bytes,
        kv_kib_per_token,
    );
    if let Some(spec) = encoder_spec {
        if is_vision_allowlisted(&spec.dir) && window < 16384 {
            window = 16384;
        }
    }
    DeviceWindow {
        window,
        model_bytes,
        weights_known: file_len.is_some(),
        kv_kib_per_token,
        encoder_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::gguf::test_gguf::GgufWriter;
    use crate::models::domain::vision_encoder::encoder_by_dir;

    /// The pairing a model's own file name finds, with no header to read.
    fn enc(model: &str) -> Option<EncoderSpec> {
        encoder_for_model(model, None)
    }

    fn by_dir(dir: &str) -> EncoderSpec {
        encoder_by_dir(dir).unwrap_or_else(|| panic!("no pairing for {dir}"))
    }

    /// The Orin's LLM budget, fixed, so these tests do not read the environment.
    const BUDGET: u64 = LLM_BUDGET_MB;

    /// The window at the Orin's constant budget.
    fn jetson(model_bytes: u64, kv: Option<u64>) -> u32 {
        context_size_for_budget(BUDGET, model_bytes, kv)
    }

    // The real files on the Orin, `ls -lL` / `stat -Lc %s`.
    const ORIN_E2B_Q4_K_M: u64 = 3_106_738_272;
    const ORIN_E2B_QAT_UD: u64 = 2_620_370_976;
    const ORIN_E4B_QAT_UD: u64 = 4_215_695_776;
    const ORIN_E4B_IQ4_XS: u64 = 4_715_416_704;
    const E4B_Q4_K_M: u64 = 4_977_171_584;
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

    // ── Window arithmetic ──────────────────────────────────────────────────

    /// Uses the exact on-device sizes: the window is a step function of weight size.
    #[test]
    fn jetson_context_fits_each_model_in_the_budget() {
        let e2b = jetson(ORIN_E2B_Q4_K_M, None);
        let e4b = jetson(E4B_Q4_K_M, None);
        assert_eq!(e2b, 16384, "E2B should keep the full window");
        assert_eq!(
            e4b, 8192,
            "E4B should get half the window. It briefly got 16384, on a budget that claimed the \
             marketing 8192 MB of RAM; the kernel reports 7620, and at the real figure E4B's \
             16384 needs 896 MiB of KV it does not have -- it was running out of swap."
        );
        assert!(e4b <= e2b);
    }

    /// `scripts/jetson-emu.sh` relies on this; otherwise it would silently test the Mac.
    #[test]
    fn a_different_device_budget_produces_a_different_window() {
        let nano = 7620 - RESERVED_MB;
        let nx = 15564 - RESERVED_MB;
        let on_nano = context_size_for_budget(nano, E4B_Q4_K_M, None);
        let on_nx = context_size_for_budget(nx, E4B_Q4_K_M, None);
        assert_eq!(on_nano, 8192, "the board we actually have");
        assert!(on_nx > on_nano, "got {on_nx} against {on_nano}");
    }

    #[test]
    fn the_orin_profile_reproduces_the_devices_own_windows() {
        let budget = 7620 - (1500 + 200 + 100);
        assert_eq!(
            context_size_for_budget(budget, ORIN_E2B_Q4_K_M, None),
            16384
        );
        assert_eq!(context_size_for_budget(budget, E4B_Q4_K_M, None), 8192);
    }

    /// A budget smaller than the weights must clamp, not underflow into a huge window.
    #[test]
    fn an_impossible_budget_clamps_instead_of_wrapping() {
        assert_eq!(context_size_for_budget(512, E4B_Q4_K_M, None), 2048);
    }

    /// Redoes the arithmetic by hand, so the test cannot share the function's mistake.
    #[test]
    fn e4b_fits_its_window_and_could_not_take_another_doubling() {
        let weights_mb = 4_640_000_000u64 / MIB;
        let free_mb = LLM_BUDGET_MB - weights_mb - 600;
        let chosen = u64::from(jetson(4_640_000_000, None));
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
        assert_eq!(jetson(4_739_563_520, None), 12288);
    }

    /// E4B IQ4_XS affords 13,220 tokens; a power-of-two floor handed back 8,192.
    #[test]
    fn rounding_does_not_discard_context_the_budget_affords() {
        assert_eq!(jetson(ORIN_E4B_IQ4_XS, None), 12288);
        for bytes in [4_600_000_000u64, 4_700_000_000, 4_800_000_000] {
            let ctx = u64::from(jetson(bytes, None));
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

    #[test]
    fn header_derived_cost_is_a_no_op_for_the_shipped_models() {
        for (bytes, kv, want) in [
            (ORIN_E2B_Q4_K_M, E2B_KV, 16384u32),
            (E4B_Q4_K_M, E4B_KV, 8192),
            (ORIN_E4B_IQ4_XS, E4B_KV, 12288),
        ] {
            let derived = jetson(bytes, Some(kv));
            assert_eq!(derived, want, "{bytes} at {kv}");
            assert_eq!(derived, jetson(bytes, None), "{bytes}");
        }
    }

    #[test]
    fn a_cheaper_model_is_no_longer_charged_the_widest_geometry() {
        let bytes = 4_500u64 * MIB;
        assert!(jetson(bytes, Some(28)) > jetson(bytes, None));
    }

    #[test]
    fn a_wider_model_is_charged_for_it() {
        let bytes = 4_000_000_000u64;
        assert!(jetson(bytes, Some(168)) < jetson(bytes, None));
    }

    #[test]
    fn a_useless_slope_falls_back_rather_than_dividing_by_zero() {
        assert_eq!(jetson(E4B_Q4_K_M, Some(0)), jetson(E4B_Q4_K_M, None));
    }

    #[test]
    fn a_switch_frees_the_model_in_use_and_never_more_than_is_missing() {
        // The Orin with E4B loaded: 1 GB free of the 5,820 MB budget, the model holds 4,020.
        assert_eq!(reclaimable_mb(4020, BUDGET, 1000), 4020);
        // Never past what the budget is short of.
        assert_eq!(reclaimable_mb(4020, BUDGET, 3000), BUDGET - 3000);
        // No live reading: available is the budget, so nothing is held back.
        assert_eq!(reclaimable_mb(4020, BUDGET, BUDGET), 0);
        assert_eq!(reclaimable_mb(0, BUDGET, 100), 0);
    }

    #[test]
    fn a_desktop_keeps_a_quarter_of_its_memory_and_never_less_than_the_floor() {
        assert_eq!(host_llm_budget_mb(65_536), 49_152);
        assert_eq!(host_llm_budget_mb(32_768), 24_576);
        assert_eq!(host_llm_budget_mb(16_384), 12_288);
        assert_eq!(host_llm_budget_mb(8_192), 4_096, "the floor, not a quarter");
        assert_eq!(host_llm_budget_mb(3_000), 0);
    }

    #[test]
    fn jetson_context_floors_for_an_oversized_model() {
        assert_eq!(jetson(9_000_000_000, None), 2048);
    }

    // ── Encoder charges ────────────────────────────────────────────────────

    /// Both picks stay at the latency clamp: the board's E4B log line reads 16384.
    #[test]
    fn the_e2b_and_e4b_picks_keep_the_16384_clamp() {
        assert_eq!(jetson(ORIN_E4B_QAT_UD, Some(E4B_KV)), 16384);
        assert_eq!(jetson(ORIN_E2B_QAT_UD, Some(E2B_KV)), 16384);
        assert_eq!(jetson(ORIN_E2B_Q4_K_M, Some(E2B_KV)), 16384);
    }

    /// A zero encoder matches `context_size_for_budget` across a sweep of sizes and slopes.
    #[test]
    fn a_zero_encoder_is_the_old_arithmetic() {
        for bytes in (0..9_000_000_000u64).step_by(97_000_000) {
            for kv in [None, Some(0), Some(18), Some(56), Some(168)] {
                assert_eq!(
                    context_size_with_encoder(BUDGET, bytes, 0, kv),
                    context_size_for_budget(BUDGET, bytes, kv)
                );
            }
        }
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

    /// Unconfirmed SWA keeps the fallback; a dense model is exact whatever its architecture.
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

    /// E2B fits and E4B doesn't for any encoder compute in 0..=900, so the guess can't flip either.
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
                let fit = vision_fit(5820, Some(bytes), enc(model).as_ref(), Some(kv), compute);
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
                Some(&by_dir("gemma-4-e4b-it-qat")),
                Some(E4B_KV),
                0
            ),
            VisionFit::CostsWindow {
                window_without: 16384,
                window_with: 4096
            }
        ));
    }

    #[test]
    fn a_model_already_at_the_floor_is_not_declared() {
        // Weights past budget less compute leave no KV with or without the encoder.
        for bytes in [5_500_000_000, 6_000_000_000, 9_000_000_000] {
            let fit = vision_fit(
                5820,
                Some(bytes),
                Some(&by_dir("gemma-4-e2b-it")),
                Some(E2B_KV),
                0,
            );
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
            let fit = vision_fit(
                budget,
                None,
                Some(&by_dir("gemma-4-e2b-it")),
                Some(E2B_KV),
                0,
            );
            assert!(!fit.fits(), "budget {budget}: {fit:?}");
        }
        assert!(!vision_fit_on_device(
            Path::new("/nowhere/at/all.gguf"),
            Some(&by_dir("gemma-4-e2b-it"))
        )
        .fits());
    }

    #[test]
    fn a_model_with_no_encoder_is_not_declared_before_any_arithmetic() {
        assert_eq!(
            vision_fit(
                5820,
                Some(1),
                enc("Llama-3.2-3B-Instruct").as_ref(),
                Some(28),
                0
            ),
            VisionFit::NotDeclared
        );
        assert_eq!(
            vision_fit_on_device(Path::new("/nowhere"), enc("granite-4.1-3b").as_ref()),
            VisionFit::NotDeclared
        );
    }

    /// With the encoder charged, E4B-qat drops to the floor, so the Orin must not declare it.
    #[test]
    fn the_encoder_is_charged_its_weights_and_compute() {
        let e4b_qat = by_dir("gemma-4-e4b-it-qat").size_bytes;
        assert_eq!(
            context_size_with_encoder(BUDGET, ORIN_E4B_QAT_UD, e4b_qat, Some(E4B_KV)),
            MIN_CTX
        );
        let e2b = by_dir("gemma-4-e2b-it").size_bytes;
        assert_eq!(
            context_size_with_encoder(BUDGET, ORIN_E2B_Q4_K_M, e2b, Some(E2B_KV)),
            16384
        );
    }

    /// A sparse GGUF with the real file's length and header, read the way the adapters read it.
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
            vision_fit_on_device(&e2b, enc("gemma-4-E2B-it-Q4_K_M").as_ref()),
            VisionFit::Fits { window: 16384 }
        );
        assert!(!vision_fit_on_device(&e4b, enc("gemma-4-E4B-it-qat-UD-Q4_K_XL").as_ref()).fits());

        // Through a link, as models/gguf holds them.
        let link = tmp.path().join("link.gguf");
        std::os::unix::fs::symlink(&e2b, &link).unwrap();
        assert!(vision_fit_on_device(&link, Some(&by_dir("gemma-4-e2b-it"))).fits());

        // No encoder (nothing measured, nothing declared): the board's 16384.
        let w = device_window(Some(&e4b), "gemma-4-E4B-it-qat-UD-Q4_K_XL");
        assert_eq!(w.window, 16384);
        assert_eq!(w.kv_kib_per_token, Some(E4B_KV));
        assert_eq!(w.encoder_bytes, 0);
        assert!(w.weights_known);

        let missing = device_window(Some(&tmp.path().join("gone.gguf")), "gemma-4-E4B-it");
        assert!(!missing.weights_known);
        assert_eq!(missing.model_bytes, ASSUMED_LARGEST_MODEL_BYTES);
        assert_eq!(missing.window, MIN_CTX);
    }

    // ── Declaration and the release gate ───────────────────────────────────

    /// Adding an entry is a release decision backed by an Orin reading.
    #[test]
    fn the_measured_list_ships_empty_and_names_only_real_rows() {
        assert!(
            DEVICE_MEASURED_VISION.is_empty(),
            "an entry here puts a ~1 GB GPU-resident encoder on the Orin; it needs the \
             MemAvailable reading during the boot prewarm first"
        );
        for d in DEVICE_MEASURED_VISION {
            assert!(encoder_by_dir(d).is_some(), "{d}");
        }
    }

    #[test]
    fn off_the_budgeted_device_the_pairing_decides_with_no_file_io() {
        let never = |_: &EncoderSpec| -> VisionFit { panic!("no fit may be computed off-device") };
        assert!(declare(enc("gemma-4-E4B-it-qat-UD-Q4_K_XL"), false, &[], never).is_declared());
        assert_eq!(
            declare(enc("Llama-3.2-3B-Instruct"), false, &[], never),
            VisionDeclaration::NotDeclared
        );
    }

    #[test]
    fn on_the_budgeted_device_a_model_needs_the_list_and_the_fit() {
        let never = |_: &EncoderSpec| -> VisionFit { panic!("an unlisted encoder costs no I/O") };
        let e2b = Some(by_dir("gemma-4-e2b-it"));
        assert!(matches!(
            declare(e2b, true, &[], never),
            VisionDeclaration::NotOnThisDevice(s) if s.dir == "gemma-4-e2b-it"
        ));
        let listed = &["gemma-4-e2b-it"];
        assert!(declare(e2b, true, listed, |_| VisionFit::Fits { window: 16384 }).is_declared());
        assert!(!declare(e2b, true, listed, |_| VisionFit::CostsWindow {
            window_without: 16384,
            window_with: 2048
        })
        .is_declared());
        assert!(
            !declare(Some(by_dir("gemma-4-e2b-it-qat")), true, listed, |_| {
                VisionFit::Fits { window: 1 }
            })
            .is_declared(),
            "the qat row is its own entry"
        );
        assert_eq!(
            declare(enc("granite-4.1-3b"), true, listed, never),
            VisionDeclaration::NotDeclared
        );
    }

    #[test]
    fn a_declaration_reports_its_own_state_before_any_file_is_read() {
        let spec = by_dir("gemma-4-e4b-it");
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
        assert!(budgeted_declaration(None, "gemma-4-E2B-it-Q4_K_M", false).is_declared());
    }

    /// Nothing is measured on the Orin, so no model the pairing table knows reads pictures there,
    /// by its file name, by the name a household gives the row, or by an encoder's own `dir`.
    #[test]
    fn the_budgeted_policy_declares_no_model_the_table_lists() {
        use crate::models::domain::vision_pairing::pairings;
        assert!(DEVICE_MEASURED_VISION.is_empty());
        let never = |_: &EncoderSpec| -> VisionFit { panic!("an unlisted encoder costs no I/O") };
        for p in pairings() {
            assert!(
                matches!(
                    declare(Some(p.spec()), true, DEVICE_MEASURED_VISION, never),
                    VisionDeclaration::NotOnThisDevice(_)
                ),
                "{}",
                p.encoder.dir
            );
            for file in &p.model_files {
                let stem = file.rsplit('/').next().unwrap_or(file);
                let stem = stem.strip_suffix(".gguf").unwrap_or(stem);
                let declared = budgeted_declaration(None, stem, true);
                assert!(!declared.is_declared(), "{file}");
                assert!(
                    matches!(declared, VisionDeclaration::NotOnThisDevice(_)),
                    "{file}: a paired file is refused for the device, not unknown to it"
                );
            }
        }
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

    #[test]
    fn allowlist_enables_gemma_vision_on_budgeted_device() {
        let e2b = Some(by_dir("gemma-4-e2b-it"));
        clear_vision_allowlist();
        assert!(!is_vision_allowlisted("gemma-4-e2b-it"));
        assert!(!declare(e2b, true, &[], |_| VisionFit::Fits { window: 16384 }).is_declared());

        allow_gemma_vision();
        assert!(is_vision_allowlisted("gemma-4-e2b-it"));
        assert!(is_vision_allowlisted("gemma-4-e4b-it"));
        assert!(declare(e2b, true, &[], |_| VisionFit::Fits { window: 16384 }).is_declared());

        clear_vision_allowlist();
        assert!(!is_vision_allowlisted("gemma-4-e2b-it"));
    }
}
