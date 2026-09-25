//! Which vision encoder a chat model needs, how to tell a good copy from a bad one, and what
//! to tell the household while it is not ready.
//!
//! A Gemma 4 model reads pictures through a separate file, an `mmproj` encoder of about a
//! gigabyte. Everything about choosing it, checking it and describing its state is decided
//! here, so the adapters that fetch and stamp it only apply these decisions, and CI's fast
//! pass runs every one of them.
//!
//! # Why the table is keyed by (family, qat-ness), unlike the drafter's
//!
//! [`super::drafter::drafter_for`] maps qat and non-qat spellings of one family to ONE drafter,
//! because a drafter is tied to the architecture. Encoders are not: the qat releases ship their
//! own vision-to-text projector. The two files have IDENTICAL byte sizes and differ in
//! `mm.input_projection` (57% of its bytes, values 10-20% apart), so nothing short of the
//! sha256 tells them apart, and the wrong one loads without any error and answers about photos
//! through weights the model was not trained with. Each row therefore pins its own repo,
//! revision and LFS oid.
//!
//! # What is I/O and what is not
//!
//! Reading a header is cheap and bounded (about 85 KB), so [`validate_encoder_header`] lives
//! here. Hashing a gigabyte is not, so the adapter hashes and writes the `.verified` sidecar;
//! this module only builds, parses and judges it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::gguf::{parse_gguf_layout_file, GgufInfo};

/// One pinned encoder file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSpec {
    /// Directory under `models/mmproj/`, lowercase (the Orin's ext4 is case-sensitive). Also
    /// the key for the adapter's status map, in-flight set and single-flight lock.
    pub dir: &'static str,
    /// Hugging Face repository the file comes from.
    pub repo: &'static str,
    /// The commit the file is fetched at. Pinned, so an upstream re-upload cannot swap the
    /// bytes under a validated install.
    pub revision: &'static str,
    /// File name inside the repository, and on disk.
    pub filename: &'static str,
    /// Exact length of the file.
    pub size_bytes: u64,
    /// The file's sha256, which is its LFS oid on Hugging Face.
    pub sha256: &'static str,
    /// `clip.vision.projector_type` the header must carry.
    pub projector: &'static str,
    /// `clip.vision.projection_dim`: the chat model's `embedding_length`.
    pub projection_dim: u32,
    /// What the household reads. Never the filename.
    pub label: &'static str,
}

/// The chat architecture every encoder in [`ENCODER_SPECS`] pairs with.
pub const GEMMA4_ARCH: &str = "gemma4";

/// Every encoder GIAP provisions.
///
/// Read from the Hugging Face API on 2026-09-24: `revision` is the repository head, `sha256`
/// and `size_bytes` are the tree listing's `lfs.oid` and `lfs.size` for `mmproj-BF16.gguf` at
/// that revision. The two non-qat oids equal the sha256 of the complete files already on the
/// development Mac. The 12b row's projector and projection width were read from a range-read
/// of its header (`gemma4uv`, 3840), which the vendored mtmd knows.
pub const ENCODER_SPECS: &[EncoderSpec] = &[
    EncoderSpec {
        dir: "gemma-4-e2b-it",
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        revision: "0314792d7f1f7e229411f620751375812bb9faf2",
        filename: "mmproj-BF16.gguf",
        size_bytes: 986_833_728,
        sha256: "a402f10fb5780bf91d03a10cd89061139f522bee2e679b1291bbfdcd71d9547d",
        projector: "gemma4v",
        projection_dim: 1536,
        label: "Gemma 4 E2B",
    },
    EncoderSpec {
        dir: "gemma-4-e2b-it-qat",
        repo: "unsloth/gemma-4-E2B-it-qat-GGUF",
        revision: "66a399f68ddd113b06dff02fca9523e55465d11d",
        filename: "mmproj-BF16.gguf",
        size_bytes: 986_833_728,
        sha256: "38b33846f56426cd650e0e574d78de125abdfcedf35c0d7f6929f6ffe26efe02",
        projector: "gemma4v",
        projection_dim: 1536,
        label: "Gemma 4 E2B",
    },
    EncoderSpec {
        dir: "gemma-4-e4b-it",
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        revision: "bfc15c382204943c3a8fff0c750b94ae2364d7a3",
        filename: "mmproj-BF16.gguf",
        size_bytes: 991_552_320,
        sha256: "ee01cba03fd9c71ea2ea722225d24a84f72e7197714367e550ef705ef8851bc6",
        projector: "gemma4v",
        projection_dim: 2560,
        label: "Gemma 4 E4B",
    },
    EncoderSpec {
        dir: "gemma-4-e4b-it-qat",
        repo: "unsloth/gemma-4-E4B-it-qat-GGUF",
        revision: "8c5a9e4fd5482e2be20fe0bf013b4c262a8f4265",
        filename: "mmproj-BF16.gguf",
        size_bytes: 991_552_320,
        sha256: "7c9bafa27f82d658eda805c1d82ef62bb0368e1ff75f64f77de58ad318beaaf9",
        projector: "gemma4v",
        projection_dim: 2560,
        label: "Gemma 4 E4B",
    },
    EncoderSpec {
        dir: "gemma-4-12b-it",
        repo: "unsloth/gemma-4-12b-it-GGUF",
        revision: "fc034cfff751157913579611efad8462ac1be606",
        filename: "mmproj-BF16.gguf",
        size_bytes: 175_115_840,
        sha256: "2e269f906eb15169ee9ce880ea649bd6d42d4964c21f8ede10d0d0efc738bcbb",
        projector: "gemma4uv",
        projection_dim: 3840,
        label: "Gemma 4 12B",
    },
];

/// The encoder `chat_model` needs, if GIAP knows one.
///
/// Biased to `None`: a false positive tells a blind model it can see, and a missing row only
/// costs a feature. Matched on substrings of the lowercased name, so the settings spelling
/// (`gemma-4-E2B-it-qat-UD-Q4_K_XL`), the registry stem (`gemma-4-E2B-it-qat`) and the
/// owner/quant form (`unsloth/gemma-4-E4B-it-GGUF:IQ4_XS`) all land on one row.
///
/// Refused outright: drafters (`mtp-*`, `*-assistant*`), which are Gemma-named but are not chat
/// models; the `-mobile` releases; the 12B-A4B mixture of experts; and every family without a
/// pinned row (E1B, 26B, 27B, 31B), whose encoders nobody has checked.
pub fn encoder_for(chat_model: &str) -> Option<EncoderSpec> {
    let m = chat_model.to_ascii_lowercase();
    if !m.contains("gemma-4") && !m.contains("gemma4") {
        return None;
    }
    if m.contains("mtp") || m.contains("assistant") || m.contains("mobile") {
        return None;
    }
    let qat = m.contains("qat");
    let dir = if m.contains("e2b") {
        if qat {
            "gemma-4-e2b-it-qat"
        } else {
            "gemma-4-e2b-it"
        }
    } else if m.contains("e4b") {
        if qat {
            "gemma-4-e4b-it-qat"
        } else {
            "gemma-4-e4b-it"
        }
    } else if m.contains("12b") && !m.contains("a4b") && !qat {
        "gemma-4-12b-it"
    } else {
        return None;
    };
    encoder_by_dir(dir)
}

/// The row whose [`EncoderSpec::dir`] is `dir`.
pub fn encoder_by_dir(dir: &str) -> Option<EncoderSpec> {
    ENCODER_SPECS.iter().find(|s| s.dir == dir).copied()
}

/// Where an encoder lives under a pond's data directory.
pub fn encoder_path(data_dir: &Path, spec: &EncoderSpec) -> PathBuf {
    data_dir
        .join("models")
        .join("mmproj")
        .join(spec.dir)
        .join(spec.filename)
}

/// Whether `spec` can project into a chat model with this architecture and width.
///
/// Both, because a width alone is not a pairing: DeepSeek-R1-Distill-Qwen-1.5B has
/// `embedding_length` 1536, the same as the E2B encoder's projection.
pub fn pairs_with(spec: &EncoderSpec, model_arch: &str, model_embedding_length: u32) -> bool {
    model_arch == GEMMA4_ARCH && model_embedding_length == spec.projection_dim
}

/// [`pairs_with`] over what a chat model's own header says. Unknown fields do not pair.
pub fn pairs_with_gguf(spec: &EncoderSpec, info: &GgufInfo) -> bool {
    match (info.architecture.as_deref(), info.embedding_length) {
        (Some(arch), Some(width)) => pairs_with(spec, arch, width),
        _ => false,
    }
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Why a file at an encoder's path cannot be used as that encoder.
///
/// Every variant except [`Self::Missing`] means "something is at the path and it is wrong":
/// the adapter quarantines it (renames, never deletes) before linking a fresh copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncoderInvalid {
    /// Nothing at the path (or a dangling link).
    #[error("no encoder file is present")]
    Missing,
    /// A complete-looking file of the wrong length: another precision or publisher.
    #[error("encoder is {actual} bytes, expected {expected}")]
    WrongSize { expected: u64, actual: u64 },
    /// Not a GGUF file at all, or not readable as one.
    #[error("encoder file is not GGUF")]
    NotGguf,
    /// A GGUF whose architecture is not `clip`: a chat model or a drafter in the wrong place.
    #[error("file is a GGUF but not an encoder (general.architecture is not clip)")]
    NotAnEncoder,
    /// An encoder with no vision tower.
    #[error("encoder has no vision tower")]
    NoVision,
    /// A vision projector of the wrong type or width for this chat model.
    #[error("encoder's vision projector does not match this model")]
    WrongProjector,
    /// The header describes more tensor data than the file holds: a download that stopped.
    #[error("encoder is truncated: {have} of {need} bytes present")]
    Truncated { need: u64, have: u64 },
    /// The right shape but not the pinned bytes: a sidecar or hash disagrees with the pin, or
    /// the tensor table does not account for the file.
    #[error("encoder file is not the pinned file")]
    WrongFile,
}

/// Check the file at `path` against `spec` from its header and length alone.
///
/// Cheap and read-only: one `metadata` and a walk of the header and tensor table, about 85 KB
/// for a Gemma 4 encoder. Returns the file's length on success. It does NOT prove the bytes are
/// the pinned ones (qat and non-qat pass it alike); the `.verified` sidecar does that.
///
/// Order: the header before the length, so a download that stopped reports [`EncoderInvalid::
/// Truncated`] with how much is present rather than a bare size mismatch. A file LONGER than
/// the pin is refused first, since it cannot be a truncation of it.
pub fn validate_encoder_header(path: &Path, spec: &EncoderSpec) -> Result<u64, EncoderInvalid> {
    let meta = std::fs::metadata(path).map_err(|_| EncoderInvalid::Missing)?;
    if !meta.is_file() {
        return Err(EncoderInvalid::NotGguf);
    }
    let len = meta.len();
    if len > spec.size_bytes {
        return Err(EncoderInvalid::WrongSize {
            expected: spec.size_bytes,
            actual: len,
        });
    }
    let layout = parse_gguf_layout_file(path).ok_or(EncoderInvalid::NotGguf)?;
    if layout.architecture.as_deref() != Some("clip") {
        return Err(EncoderInvalid::NotAnEncoder);
    }
    if layout.has_vision_encoder != Some(true) {
        return Err(EncoderInvalid::NoVision);
    }
    if layout.vision_projector_type.as_deref() != Some(spec.projector)
        || layout.vision_projection_dim != Some(spec.projection_dim)
    {
        return Err(EncoderInvalid::WrongProjector);
    }
    let Some(need) = layout.data_end else {
        // A right-looking header whose table cannot be summed: it is not the pinned file,
        // whose table this parser reads in full.
        return Err(EncoderInvalid::WrongFile);
    };
    if need > len {
        return Err(EncoderInvalid::Truncated { need, have: len });
    }
    // A writer may pad the last tensor to the alignment; anything past that is not the file.
    let padded = need.div_ceil(layout.alignment.max(1)) * layout.alignment.max(1);
    if len > padded {
        return Err(EncoderInvalid::WrongFile);
    }
    if len != spec.size_bytes {
        return Err(EncoderInvalid::WrongSize {
            expected: spec.size_bytes,
            actual: len,
        });
    }
    Ok(len)
}

// ── Identity sidecar ────────────────────────────────────────────────────────

/// `<file>.verified`: the record that a file's sha256 was computed once and matched.
///
/// It lets every later readiness check stay header-cheap. It is trusted only while the file's
/// size and mtime are what they were when it was hashed, so a file replaced in place (same
/// name, new bytes) is hashed again rather than inheriting the old verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderSidecar {
    pub sha256: String,
    pub size: u64,
    /// Whole seconds since the epoch, from the file's modification time.
    pub mtime_unix: i64,
}

impl EncoderSidecar {
    /// The record to write after hashing a file.
    pub fn new(sha256: impl Into<String>, size: u64, mtime_unix: i64) -> Self {
        Self {
            sha256: sha256.into().to_ascii_lowercase(),
            size,
            mtime_unix,
        }
    }

    /// Serialised form, for the adapter to write.
    pub fn to_json(&self) -> String {
        // A struct of a string and two integers cannot fail to serialise.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse a sidecar. `None` for anything malformed, which reads as "not verified".
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// Whether this record vouches for the file as `spec` describes it right now.
    pub fn matches(&self, spec: &EncoderSpec, size: u64, mtime_unix: i64) -> bool {
        self.sha256.eq_ignore_ascii_case(spec.sha256)
            && self.size == spec.size_bytes
            && size == spec.size_bytes
            && self.mtime_unix == mtime_unix
    }
}

/// Where the sidecar for `file` lives: next to it, the file name with `.verified` appended.
pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut name = file
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".verified");
    file.with_file_name(name)
}

/// A file's modification time as whole Unix seconds. `None` where the platform cannot say.
pub fn mtime_unix(meta: &std::fs::Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    let secs = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok()?,
        Err(e) => -i64::try_from(e.duration().as_secs()).ok()?,
    };
    Some(secs)
}

/// Whether a sidecar (its text, if one was read) vouches for a file of `size` and `mtime_unix`.
pub fn sidecar_matches(
    sidecar_json: Option<&str>,
    spec: &EncoderSpec,
    size: u64,
    mtime_unix: i64,
) -> bool {
    sidecar_json
        .and_then(EncoderSidecar::parse)
        .is_some_and(|s| s.matches(spec, size, mtime_unix))
}

/// What is at an encoder's path, read without hashing, renaming or fetching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDisk {
    /// Nothing there.
    Missing,
    /// Something there that is not usable as this encoder.
    Invalid(EncoderInvalid),
    /// The header is right but no current sidecar vouches for the bytes: it must be hashed.
    Unverified { bytes: u64 },
    /// Header right and a current sidecar matches the pinned sha256.
    Verified { bytes: u64 },
}

/// Inspect `path` as `spec`: header, length and sidecar, nothing more. The pure read that
/// readiness checks and model lists use; only the adapter's ensure path hashes or repairs.
pub fn encoder_on_disk(path: &Path, spec: &EncoderSpec) -> OnDisk {
    let bytes = match validate_encoder_header(path, spec) {
        Ok(b) => b,
        Err(EncoderInvalid::Missing) => return OnDisk::Missing,
        Err(e) => return OnDisk::Invalid(e),
    };
    let Some(mtime) = std::fs::metadata(path).ok().as_ref().and_then(mtime_unix) else {
        return OnDisk::Unverified { bytes };
    };
    let sidecar = std::fs::read_to_string(sidecar_path(path)).ok();
    if sidecar_matches(sidecar.as_deref(), spec, bytes, mtime) {
        OnDisk::Verified { bytes }
    } else {
        OnDisk::Unverified { bytes }
    }
}

// ── State ───────────────────────────────────────────────────────────────────

/// Why a fetch did not finish, in the few kinds the household copy distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    ConnectionDropped,
    DiskFull,
    WrongFile,
    CouldNotStart,
    Other,
}

/// Where a model's picture support stands. Serialised as `{"kind": "...", ...}` for the API
/// and the desktop's closed union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EncoderState {
    /// Nothing is known (a backend that does not report). Callers fail open.
    Unknown,
    /// This model has no encoder GIAP knows of.
    NotDeclared,
    /// It has one, but this device will not carry it (see `device_budget`).
    NotOnThisDevice,
    /// Declared and not on disk yet.
    Absent,
    /// On disk, being hashed before its first use.
    Verifying,
    /// Being fetched. `total` is 0 when the size is not yet known.
    Downloading { done: u64, total: u64 },
    /// Usable. `bytes` is the encoder's size where one is involved.
    Ready { bytes: Option<u64> },
    /// The last attempt failed; the next starts at `retry_at_unix_ms`.
    Failed {
        reason: FailReason,
        retry_at_unix_ms: u64,
    },
    /// The network mode refused the fetch. `mode` is `NetworkMode::as_str`.
    Blocked { mode: String, host: String },
}

impl EncoderState {
    /// The serialised `kind`, for logs and API codes.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NotDeclared => "not_declared",
            Self::NotOnThisDevice => "not_on_this_device",
            Self::Absent => "absent",
            Self::Verifying => "verifying",
            Self::Downloading { .. } => "downloading",
            Self::Ready { .. } => "ready",
            Self::Failed { .. } => "failed",
            Self::Blocked { .. } => "blocked",
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// The model will never read pictures here as configured: a different model is the fix.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::NotDeclared | Self::NotOnThisDevice)
    }

    /// Picture support is on its way or stuck on the way: waiting (or the network mode) is the fix.
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            Self::Absent
                | Self::Verifying
                | Self::Downloading { .. }
                | Self::Failed { .. }
                | Self::Blocked { .. }
        )
    }

    /// Whether the desktop should poll fast: the state is expected to move on its own soon.
    pub fn is_moving(&self) -> bool {
        matches!(
            self,
            Self::Absent | Self::Verifying | Self::Downloading { .. }
        )
    }
}

// ── Retry policy ────────────────────────────────────────────────────────────

/// Waits between attempts, after the 1st, 2nd, 3rd and every later consecutive failure.
///
/// A fixed ladder rather than exponential growth: the file is a gigabyte on a household link,
/// so hammering helps nobody, and two hours is short enough that a fix upstream or a restored
/// connection is picked up the same evening.
pub const RETRY_SCHEDULE: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(2 * 60 * 60),
];

/// How long to wait after the `attempt`-th consecutive failure (1-based; 0 reads as 1).
pub fn retry_after(attempt: u32) -> Duration {
    let i = (attempt.max(1) - 1) as usize;
    RETRY_SCHEDULE[i.min(RETRY_SCHEDULE.len() - 1)]
}

/// Consecutive-failure bookkeeping for [`retry_after`].
///
/// Resets on success, and when the network mode changes: a household that just opened the
/// network should not wait out a two-hour step earned while it was closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryBackoff {
    failures: u32,
    mode: Option<String>,
}

impl RetryBackoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure under `network_mode` and return the wait before the next attempt.
    pub fn on_failure(&mut self, network_mode: &str) -> Duration {
        self.on_network_mode(network_mode);
        self.failures = self.failures.saturating_add(1);
        retry_after(self.failures)
    }

    /// A success clears the ladder.
    pub fn on_success(&mut self) {
        self.failures = 0;
    }

    /// Note the network mode in force; a change since the last note clears the ladder.
    pub fn on_network_mode(&mut self, network_mode: &str) {
        if self.mode.as_deref() != Some(network_mode) {
            if self.mode.is_some() {
                self.failures = 0;
            }
            self.mode = Some(network_mode.to_string());
        }
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

// ── Failure classification ──────────────────────────────────────────────────

/// The state a failed ensure lands in, from what the adapter knows about the failure.
///
/// A network-mode refusal is not a failure and wins over everything: it is the household's
/// own choice, and the copy says how to change it. `wrong_file` wins over an I/O kind: bytes
/// that arrived and were wrong say more than how the connection ended.
pub fn classify_failure(
    denied: Option<&crate::shared::services::egress::EgressDenied>,
    io_kind: Option<std::io::ErrorKind>,
    wrong_file: bool,
    retry_at_unix_ms: u64,
) -> EncoderState {
    use std::io::ErrorKind as K;
    if let Some(d) = denied {
        return EncoderState::Blocked {
            mode: d.mode.as_str().to_string(),
            host: d.host.clone(),
        };
    }
    let reason = if wrong_file {
        FailReason::WrongFile
    } else {
        match io_kind {
            Some(K::StorageFull | K::QuotaExceeded) => FailReason::DiskFull,
            Some(
                K::ConnectionReset
                | K::ConnectionAborted
                | K::ConnectionRefused
                | K::NotConnected
                | K::BrokenPipe
                | K::UnexpectedEof
                | K::TimedOut
                | K::NetworkDown
                | K::NetworkUnreachable
                | K::HostUnreachable,
            ) => FailReason::ConnectionDropped,
            _ => FailReason::Other,
        }
    };
    EncoderState::Failed {
        reason,
        retry_at_unix_ms,
    }
}

/// [`classify_failure`] over an error chain, by type and never by message text.
///
/// Looks for, anywhere in the chain: an `EgressDenied` (Blocked); an [`EncoderInvalid`] (the
/// file that arrived was wrong); a `std::io::Error` (its kind; HTTP clients carry the socket's
/// error as a source). Anything else is [`FailReason::Other`], which claims nothing.
pub fn classify_error(err: &anyhow::Error, retry_at_unix_ms: u64) -> EncoderState {
    use crate::shared::services::egress::EgressDenied;
    let mut denied = None;
    let mut io_kind = None;
    let mut wrong_file = false;
    for cause in err.chain() {
        if denied.is_none() {
            denied = cause.downcast_ref::<EgressDenied>();
        }
        if cause.downcast_ref::<EncoderInvalid>().is_some() {
            wrong_file = true;
        }
        if io_kind.is_none() {
            io_kind = cause.downcast_ref::<std::io::Error>().map(|e| e.kind());
        }
    }
    classify_failure(denied, io_kind, wrong_file, retry_at_unix_ms)
}

// ── Registry stamp plan ─────────────────────────────────────────────────────

/// One registry row, as the stamp plan needs to see it. Paths are as the adapter resolved
/// them (canonicalised), so two spellings of one file compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowView<'a> {
    pub id: &'a str,
    /// The row's `local_path`, resolved through symlinks.
    pub resolved_path: &'a Path,
    /// The entry-level `mmproj_path`, if stamped.
    pub mmproj_path: Option<&'a Path>,
    /// The entry-level `mmproj_size_bytes` (0 when unstamped).
    pub mmproj_size_bytes: u64,
}

/// What to do with the rows that resolve to the chat model's GGUF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampDecision<'a> {
    /// Declared on this device and valid: point every such row at the encoder.
    Stamp {
        mmproj_path: &'a Path,
        size_bytes: u64,
    },
    /// Not declared, or not valid: no such row may carry an encoder, since the engine loads a
    /// stamped encoder eagerly at every model load.
    Clear,
}

/// One change for the adapter to apply through the registry's mutable list, before one save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowChange {
    Stamp {
        id: String,
        mmproj_path: PathBuf,
        size_bytes: u64,
    },
    Clear {
        id: String,
    },
}

/// The changes that make every row naming `target_path` agree with `decision`.
///
/// Every row, not one: a GGUF is registered under the settings spelling and the canonical stem,
/// and they share one engine slot, so whichever loads first decides whether the encoder loads.
/// Rows already in the wanted state produce nothing, so an empty plan means "do not save".
pub fn stamp_plan(
    rows: &[RowView<'_>],
    target_path: &Path,
    decision: StampDecision<'_>,
) -> Vec<RowChange> {
    rows.iter()
        .filter(|r| r.resolved_path == target_path)
        .filter_map(|r| match decision {
            StampDecision::Stamp {
                mmproj_path,
                size_bytes,
            } => (r.mmproj_path != Some(mmproj_path) || r.mmproj_size_bytes != size_bytes).then(
                || RowChange::Stamp {
                    id: r.id.to_string(),
                    mmproj_path: mmproj_path.to_path_buf(),
                    size_bytes,
                },
            ),
            StampDecision::Clear => {
                (r.mmproj_path.is_some() || r.mmproj_size_bytes != 0).then(|| RowChange::Clear {
                    id: r.id.to_string(),
                })
            }
        })
        .collect()
}

/// Clear every row, whatever GGUF it names, that is stamped with `encoder_path`: run after an
/// encoder is quarantined, so no row points the engine at the renamed file's old path.
pub fn unstamp_plan(rows: &[RowView<'_>], encoder_path: &Path) -> Vec<RowChange> {
    rows.iter()
        .filter(|r| r.mmproj_path == Some(encoder_path))
        .map(|r| RowChange::Clear {
            id: r.id.to_string(),
        })
        .collect()
}

// ── Household copy ──────────────────────────────────────────────────────────
//
// Every string here is pinned by a test. Sizes are bytes / 1,048,576 labelled "MB". The model
// is named by `EncoderSpec::label`, never by a filename.

/// Bytes as the whole megabytes the copy shows.
pub fn mb(bytes: u64) -> u64 {
    bytes / 1_048_576
}

/// The refusal for a model with no encoder.
pub const NOT_DECLARED_MESSAGE: &str = "This model cannot look at pictures. To send one, choose a \
     model marked Reads pictures on the Models page.";

/// The refusal while the chat provider is another pond: the mesh wire carries text only.
pub const MESH_MESSAGE: &str = "Pictures cannot be sent to another pond yet. Switch back to a \
     model on this device to send one.";

/// The refusal for a picture the server could not decode (415). `index_one_based` counts the
/// turn's pictures from 1, as the household does.
pub fn image_unreadable_message(index_one_based: usize) -> String {
    format!(
        "Picture {index_one_based} could not be read. Save it as a JPEG or PNG and attach it again."
    )
}

/// What "Network reach" shows for a stored `network_mode`. Only the two restrictive modes can
/// block, so anything that is not `offline` reads as the allowlist.
fn network_reach_label(mode: &str) -> &'static str {
    if mode.eq_ignore_ascii_case("offline") {
        "Offline"
    } else {
        "Allowed only"
    }
}

/// The not-on-this-device line, naming a model that CAN read pictures here when one exists.
///
/// `measured` is the list of encoder dirs this device has been measured to carry
/// (`device_budget::DEVICE_MEASURED_VISION`); with none, no model can, and the copy says so
/// rather than sending the household to a model that would refuse too.
pub fn not_on_this_device_message(label: Option<&str>, measured: &[&str]) -> String {
    let alternative = measured
        .iter()
        .filter_map(|d| encoder_by_dir(d))
        .map(|s| s.label)
        .find(|l| Some(*l) != label);
    match (label, alternative) {
        (_, None) => "Picture support is not available on this device yet.".to_string(),
        (Some(label), Some(alt)) => format!(
            "{label} cannot look at pictures on this device: picture support would take room it \
             needs for conversation. {alt} can; choose it on the Models page."
        ),
        (None, Some(alt)) => format!(
            "This model cannot look at pictures on this device: picture support would take room \
             it needs for conversation. {alt} can; choose it on the Models page."
        ),
    }
}

/// The line for a failed fetch. The client appends "It tries again at HH:MM." from
/// `retry_at_unix_ms`, so this never states a time.
pub fn failed_message(reason: FailReason, spec: Option<&EncoderSpec>) -> String {
    match reason {
        FailReason::ConnectionDropped => {
            "Picture support did not finish downloading: the connection dropped.".to_string()
        }
        FailReason::WrongFile => "Picture support did not finish downloading: the file that \
                                  arrived was not the right one."
            .to_string(),
        FailReason::DiskFull => match spec {
            Some(s) => format!(
                "Picture support needs {} MB of free space on this device. Free some on the \
                 Models page.",
                mb(s.size_bytes)
            ),
            None => "Picture support needs more free space on this device. Free some on the \
                     Models page."
                .to_string(),
        },
        FailReason::CouldNotStart => "Picture support could not start on this device.".to_string(),
        FailReason::Other => "Picture support did not finish downloading.".to_string(),
    }
}

/// The status line for `state`, or `None` where the desktop shows nothing (ready, not
/// declared, unknown). `measured` is as for [`not_on_this_device_message`].
pub fn status_message_with(
    state: &EncoderState,
    spec: Option<&EncoderSpec>,
    measured: &[&str],
) -> Option<String> {
    Some(match state {
        EncoderState::Unknown | EncoderState::NotDeclared | EncoderState::Ready { .. } => {
            return None
        }
        EncoderState::NotOnThisDevice => {
            not_on_this_device_message(spec.map(|s| s.label), measured)
        }
        EncoderState::Absent => match spec {
            Some(s) => format!(
                "Picture support for {} needs a one-time {} MB download. It starts by itself; \
                 text chat works meanwhile.",
                s.label,
                mb(s.size_bytes)
            ),
            None => "Picture support needs a one-time download. It starts by itself; text chat \
                     works meanwhile."
                .to_string(),
        },
        EncoderState::Downloading { done, total } => {
            let total = if *total > 0 {
                *total
            } else {
                spec.map_or(0, |s| s.size_bytes)
            };
            format!(
                "Getting picture support ready: {} MB of {} MB. Text chat works meanwhile.",
                mb(*done),
                mb(total)
            )
        }
        EncoderState::Verifying => {
            "Checking picture support before its first use. Text chat works meanwhile.".to_string()
        }
        EncoderState::Failed { reason, .. } => failed_message(*reason, spec),
        EncoderState::Blocked { mode, host } => {
            let size = spec.map_or(String::new(), |s| format!(" {} MB", mb(s.size_bytes)));
            format!(
                "Picture support needs a one-time{size} download from {host}, and Network reach is \
                 set to {}, which blocks it. To allow it, set Network reach to Open in Settings, \
                 under Privacy & Security.",
                network_reach_label(mode)
            )
        }
    })
}

/// [`status_message_with`] against this build's measured list.
pub fn status_message(state: &EncoderState, spec: Option<&EncoderSpec>) -> Option<String> {
    status_message_with(state, spec, super::device_budget::DEVICE_MEASURED_VISION)
}

/// Which 409 an image turn gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCode {
    /// A different model (or provider) is the fix.
    Unsupported,
    /// Waiting is the fix.
    NotReady,
}

impl RefusalCode {
    /// The API's `code` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unsupported => "vision_unsupported",
            Self::NotReady => "vision_not_ready",
        }
    }
}

/// An image turn refused before anything is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionRefusal {
    pub code: RefusalCode,
    pub message: String,
}

/// Whether an image turn for `provider` may go ahead, given the model's picture-support state.
///
/// `None` (the backend does not report) and `Unknown` pass, and so does `Ready`: unknown fails
/// OPEN to the adapter's own backstop, since refusing on no information would block every
/// backend that does not implement the port. Under the mesh provider any REPORTED state
/// refuses with the mesh line, because the wire drops images whatever the local model could
/// do (the goose adapter reports `NotDeclared` there).
pub fn refusal_for(
    state: Option<&EncoderState>,
    spec: Option<&EncoderSpec>,
    provider: &str,
) -> Option<VisionRefusal> {
    let state = state?;
    if provider.eq_ignore_ascii_case("mesh") && *state != EncoderState::Unknown {
        return Some(VisionRefusal {
            code: RefusalCode::Unsupported,
            message: MESH_MESSAGE.to_string(),
        });
    }
    let code = if state.is_unsupported() {
        RefusalCode::Unsupported
    } else if state.is_pending() {
        RefusalCode::NotReady
    } else {
        return None;
    };
    let message = match state {
        EncoderState::NotDeclared => NOT_DECLARED_MESSAGE.to_string(),
        other => status_message(other, spec)?,
    };
    Some(VisionRefusal { code, message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::gguf::test_gguf::{GgufWriter, BF16, F32};

    // ── The table ───────────────────────────────────────────────────────────

    /// Every row, pinned. These numbers came off the Hugging Face API and, for the non-qat rows,
    /// off the complete files on the development Mac (`shasum -a 256`, 2026-09-24). A change
    /// here is a change of which bytes every pond downloads and trusts.
    /// (dir, repo, revision, size, sha256, projector, projection_dim, label)
    type PinnedRow = (
        &'static str,
        &'static str,
        &'static str,
        u64,
        &'static str,
        &'static str,
        u32,
        &'static str,
    );

    #[test]
    fn the_encoder_table_is_pinned() {
        #[rustfmt::skip]
        let want: [PinnedRow; 5] = [
            ("gemma-4-e2b-it", "unsloth/gemma-4-E2B-it-GGUF",
             "0314792d7f1f7e229411f620751375812bb9faf2", 986_833_728,
             "a402f10fb5780bf91d03a10cd89061139f522bee2e679b1291bbfdcd71d9547d", "gemma4v", 1536, "Gemma 4 E2B"),
            ("gemma-4-e2b-it-qat", "unsloth/gemma-4-E2B-it-qat-GGUF",
             "66a399f68ddd113b06dff02fca9523e55465d11d", 986_833_728,
             "38b33846f56426cd650e0e574d78de125abdfcedf35c0d7f6929f6ffe26efe02", "gemma4v", 1536, "Gemma 4 E2B"),
            ("gemma-4-e4b-it", "unsloth/gemma-4-E4B-it-GGUF",
             "bfc15c382204943c3a8fff0c750b94ae2364d7a3", 991_552_320,
             "ee01cba03fd9c71ea2ea722225d24a84f72e7197714367e550ef705ef8851bc6", "gemma4v", 2560, "Gemma 4 E4B"),
            ("gemma-4-e4b-it-qat", "unsloth/gemma-4-E4B-it-qat-GGUF",
             "8c5a9e4fd5482e2be20fe0bf013b4c262a8f4265", 991_552_320,
             "7c9bafa27f82d658eda805c1d82ef62bb0368e1ff75f64f77de58ad318beaaf9", "gemma4v", 2560, "Gemma 4 E4B"),
            ("gemma-4-12b-it", "unsloth/gemma-4-12b-it-GGUF",
             "fc034cfff751157913579611efad8462ac1be606", 175_115_840,
             "2e269f906eb15169ee9ce880ea649bd6d42d4964c21f8ede10d0d0efc738bcbb", "gemma4uv", 3840, "Gemma 4 12B"),
        ];
        assert_eq!(ENCODER_SPECS.len(), want.len());
        for (spec, (dir, repo, rev, size, sha, proj, dim, label)) in ENCODER_SPECS.iter().zip(want)
        {
            assert_eq!(spec.dir, dir);
            assert_eq!(spec.repo, repo, "{dir}");
            assert_eq!(spec.revision, rev, "{dir}");
            assert_eq!(spec.filename, "mmproj-BF16.gguf", "{dir}");
            assert_eq!(spec.size_bytes, size, "{dir}");
            assert_eq!(spec.sha256, sha, "{dir}");
            assert_eq!(spec.projector, proj, "{dir}");
            assert_eq!(spec.projection_dim, dim, "{dir}");
            assert_eq!(spec.label, label, "{dir}");
        }
    }

    /// The fact the table's shape exists for: qat and non-qat encoders are the same size and
    /// different files. A table that let them share a row, or a check by size alone, would
    /// attach the wrong projector with no error anywhere.
    #[test]
    fn qat_and_non_qat_encoders_share_a_size_and_not_an_identity() {
        for family in ["gemma-4-e2b-it", "gemma-4-e4b-it"] {
            let plain = encoder_by_dir(family).unwrap();
            let qat = encoder_by_dir(&format!("{family}-qat")).unwrap();
            assert_eq!(plain.size_bytes, qat.size_bytes, "{family}");
            assert_ne!(plain.sha256, qat.sha256, "{family}");
            assert_ne!(plain.repo, qat.repo, "{family}");
        }
    }

    #[test]
    fn every_row_is_well_formed() {
        let hex = |s: &str, n: usize| {
            s.len() == n
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let mut dirs = std::collections::BTreeSet::new();
        let mut shas = std::collections::BTreeSet::new();
        for s in ENCODER_SPECS {
            assert!(
                hex(s.revision, 40),
                "{}: revision must be a 40-hex commit",
                s.dir
            );
            assert!(
                hex(s.sha256, 64),
                "{}: sha256 must be 64 lowercase hex",
                s.dir
            );
            assert_eq!(
                s.dir,
                s.dir.to_ascii_lowercase(),
                "{}: the Orin's filesystem is case-sensitive",
                s.dir
            );
            assert!(
                !s.label.contains(".gguf"),
                "{}: the label is not a filename",
                s.dir
            );
            assert!(dirs.insert(s.dir), "{}: duplicate dir", s.dir);
            assert!(shas.insert(s.sha256), "{}: duplicate sha256", s.dir);
        }
    }

    // ── Which model gets which encoder ──────────────────────────────────────

    /// Every model file on both machines, as its settings spelling, registry stem and
    /// owner/quant form. `None` is the answer for everything GIAP has not pinned.
    #[test]
    fn every_model_on_both_machines_resolves_to_the_right_row() {
        let cases: &[(&str, Option<&str>)] = &[
            // Mac, models/gguf
            ("gemma-4-E2B-it-Q4_K_M", Some("gemma-4-e2b-it")),
            ("gemma-4-E2B-it-qat-UD-Q4_K_XL", Some("gemma-4-e2b-it-qat")),
            ("gemma-4-E4B-it-Q4_K_M", Some("gemma-4-e4b-it")),
            ("gemma-4-E4B-it-Q5_K_M", Some("gemma-4-e4b-it")),
            ("gemma-4-E4B-it-qat-UD-Q4_K_XL", Some("gemma-4-e4b-it-qat")),
            ("gemma-4-12b-it-IQ4_XS", Some("gemma-4-12b-it")),
            ("Llama-3.2-3B-Instruct-Q4_K_M", None),
            ("NVIDIA-Nemotron3-Nano-4B-Q4_K_M", None),
            ("Nanbeige_Nanbeige4.2-3B-Q4_K_M", None),
            ("granite-4.1-3b-Q4_K_M", None),
            ("DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M", None),
            ("mtp-gemma-4-E2B-it", None),
            ("mtp-gemma-4-E4B-it.gguf", None),
            ("gemma-4-E2B-it-assistant-F16", None),
            ("gemma-4-E2B-it-assistant-Q8_0", None),
            ("gemma-4-E4B-it-assistant.Q8_0", None),
            ("old_functiongemma-270m-it-Q4_K_M", None),
            // Registry stems
            ("gemma-4-E2B-it", Some("gemma-4-e2b-it")),
            ("gemma-4-E2B-it-qat", Some("gemma-4-e2b-it-qat")),
            ("gemma-4-E4B-it-qat", Some("gemma-4-e4b-it-qat")),
            ("gemma-4-12b-it", Some("gemma-4-12b-it")),
            // Orin, and the colon / owner spellings its registry and catalogue use
            ("gemma-4-E4B-it-IQ4_XS", Some("gemma-4-e4b-it")),
            ("gemma-4-E4B-it:IQ4_XS", Some("gemma-4-e4b-it")),
            ("unsloth/gemma-4-E4B-it-GGUF:IQ4_XS", Some("gemma-4-e4b-it")),
            (
                "unsloth/gemma-4-E4B-it-qat-GGUF:UD-Q4_K_XL",
                Some("gemma-4-e4b-it-qat"),
            ),
            ("unsloth/gemma-4-E2B-it-GGUF:Q4_K_M", Some("gemma-4-e2b-it")),
            ("GEMMA-4-e2b-IT", Some("gemma-4-e2b-it")),
            // Families with no pinned row, and releases that are not these models
            ("gemma-4-E1B-it", None),
            ("gemma-4-12B-A4B-it", None),
            ("unsloth/gemma-4-12B-A4B-it-GGUF:Q4_K_M", None),
            ("gemma-4-26B-A4B-it-Q4_K_M", None),
            ("gemma-4-27B-it", None),
            ("gemma-4-31B-it", None),
            ("gemma-4-E2B-it-qat-mobile", None),
            ("gemma-4-12b-it-qat", None),
            ("gemma-3-4b-it", None),
            ("", None),
        ];
        for (name, want) in cases {
            assert_eq!(
                encoder_for(name).map(|s| s.dir),
                *want,
                "encoder_for({name:?})"
            );
        }
    }

    /// Drafters are shared across qat-ness and encoders are not; the two mappings must stay
    /// separate functions with different answers for the same name.
    #[test]
    fn a_qat_model_shares_its_drafter_but_not_its_encoder() {
        use crate::models::domain::drafter::drafter_for;
        let qat = "gemma-4-E2B-it-qat-UD-Q4_K_XL";
        let plain = "gemma-4-E2B-it-Q4_K_M";
        assert_eq!(drafter_for(qat).unwrap().id, drafter_for(plain).unwrap().id);
        assert_ne!(
            encoder_for(qat).unwrap().dir,
            encoder_for(plain).unwrap().dir
        );
    }

    #[test]
    fn pairing_needs_the_architecture_and_the_width() {
        let e2b = encoder_by_dir("gemma-4-e2b-it").unwrap();
        assert!(pairs_with(&e2b, "gemma4", 1536));
        assert!(
            !pairs_with(&e2b, "qwen2", 1536),
            "DeepSeek-R1-Distill-Qwen-1.5B is 1536 wide and is not Gemma"
        );
        assert!(!pairs_with(&e2b, "gemma4", 2560), "E4B's width");
        let e4b = encoder_by_dir("gemma-4-e4b-it-qat").unwrap();
        let header = GgufInfo {
            architecture: Some("gemma4".into()),
            embedding_length: Some(2560),
            ..Default::default()
        };
        assert!(pairs_with_gguf(&e4b, &header));
        assert!(!pairs_with_gguf(&e4b, &GgufInfo::default()));
    }

    #[test]
    fn the_encoder_lives_in_its_own_lowercase_dir() {
        let spec = encoder_by_dir("gemma-4-e2b-it-qat").unwrap();
        assert_eq!(
            encoder_path(Path::new("/pond"), &spec),
            Path::new("/pond/models/mmproj/gemma-4-e2b-it-qat/mmproj-BF16.gguf")
        );
    }

    // ── Validation ──────────────────────────────────────────────────────────

    /// A small encoder with the real keys, and a spec that pins exactly it.
    fn tiny(projector: &str, dim: u32) -> (Vec<u8>, EncoderSpec) {
        let bytes = GgufWriter::new()
            .str("general.architecture", "clip")
            .bool("clip.has_vision_encoder", true)
            .bool("clip.has_audio_encoder", true)
            .str("clip.vision.projector_type", projector)
            .u32("clip.vision.projection_dim", dim)
            .tensor("v.patch_embd.weight", &[16, 16, 3, 5], BF16)
            .tensor("a.conv1d.0.bias", &[77], F32)
            .tensor("mm.input_projection.weight", &[12, 9], BF16)
            .build_complete();
        let spec = EncoderSpec {
            dir: "test",
            repo: "test/test",
            revision: "0000000000000000000000000000000000000000",
            filename: "mmproj-BF16.gguf",
            size_bytes: bytes.len() as u64,
            sha256: "ab",
            projector: "gemma4v",
            projection_dim: 1536,
            label: "Test",
        };
        (bytes, spec)
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_complete_encoder_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let (bytes, spec) = tiny("gemma4v", 1536);
        let p = write(tmp.path(), "e.gguf", &bytes);
        assert_eq!(validate_encoder_header(&p, &spec), Ok(bytes.len() as u64));
    }

    /// The Orin's defect: a download that ended early, with a perfect header. Its length is the
    /// only tell, and the header says how long it should be.
    #[test]
    fn a_download_that_stopped_is_truncated_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let (bytes, spec) = tiny("gemma4v", 1536);
        let cut = bytes.len() * 645 / 1000;
        let p = write(tmp.path(), "e.gguf", &bytes[..cut]);
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::Truncated {
                need: bytes.len() as u64,
                have: cut as u64
            })
        );
    }

    #[test]
    fn each_wrong_file_says_what_is_wrong() {
        let tmp = tempfile::tempdir().unwrap();
        let (good, spec) = tiny("gemma4v", 1536);

        assert_eq!(
            validate_encoder_header(&tmp.path().join("absent.gguf"), &spec),
            Err(EncoderInvalid::Missing)
        );

        let mut longer = good.clone();
        longer.extend_from_slice(&[0u8; 64]);
        let p = write(tmp.path(), "long.gguf", &longer);
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::WrongSize {
                expected: spec.size_bytes,
                actual: longer.len() as u64
            })
        );

        let p = write(tmp.path(), "zero.gguf", b"");
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::NotGguf)
        );
        let p = write(tmp.path(), "onnx.gguf", b"ONNX not a gguf");
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::NotGguf)
        );

        // A chat model or drafter dropped where the encoder belongs.
        let model = GgufWriter::new()
            .str("general.architecture", "gemma4")
            .tensor("t", &[4], F32)
            .build_complete();
        let p = write(tmp.path(), "model.gguf", &model);
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::NotAnEncoder)
        );

        let audio_only = GgufWriter::new()
            .str("general.architecture", "clip")
            .bool("clip.has_vision_encoder", false)
            .tensor("t", &[4], F32)
            .build_complete();
        let p = write(tmp.path(), "audio.gguf", &audio_only);
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::NoVision)
        );

        // Sized to each file, so the projector is the only thing wrong with it.
        let sized = |b: &[u8]| EncoderSpec {
            size_bytes: b.len() as u64,
            ..spec
        };
        let (other_proj, _) = tiny("gemma4uv", 1536);
        let p = write(tmp.path(), "proj.gguf", &other_proj);
        assert_eq!(
            validate_encoder_header(&p, &sized(&other_proj)),
            Err(EncoderInvalid::WrongProjector)
        );

        // The E4B encoder where the E2B one belongs: same projector type, wrong width.
        let (wide, _) = tiny("gemma4v", 2560);
        let p = write(tmp.path(), "wide.gguf", &wide);
        assert_eq!(
            validate_encoder_header(&p, &sized(&wide)),
            Err(EncoderInvalid::WrongProjector)
        );

        // A whole, valid file of another length: another precision or publisher.
        let short_spec = EncoderSpec {
            size_bytes: spec.size_bytes + 4096,
            ..spec
        };
        let p = write(tmp.path(), "other.gguf", &good);
        assert_eq!(
            validate_encoder_header(&p, &short_spec),
            Err(EncoderInvalid::WrongSize {
                expected: short_spec.size_bytes,
                actual: good.len() as u64
            })
        );
    }

    /// A file whose table accounts for less than the file holds is not the pinned file, even
    /// at the pinned length.
    #[test]
    fn a_table_that_does_not_account_for_the_file_is_the_wrong_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut bytes, _) = tiny("gemma4v", 1536);
        bytes.extend_from_slice(&[7u8; 256]);
        let spec = EncoderSpec {
            size_bytes: bytes.len() as u64,
            ..tiny("gemma4v", 1536).1
        };
        let p = write(tmp.path(), "padded.gguf", &bytes);
        assert_eq!(
            validate_encoder_header(&p, &spec),
            Err(EncoderInvalid::WrongFile)
        );
    }

    /// A models/mmproj link into hf_cache blobs validates as the blob; a dangling one is Missing,
    /// which is what lets the link be replaced without quarantining anything.
    #[cfg(unix)]
    #[test]
    fn links_are_followed_and_a_dangling_link_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let (bytes, spec) = tiny("gemma4v", 1536);
        let blob = write(tmp.path(), "blob", &bytes);
        let link = tmp.path().join("link.gguf");
        std::os::unix::fs::symlink(&blob, &link).unwrap();
        assert!(validate_encoder_header(&link, &spec).is_ok());
        std::fs::remove_file(&blob).unwrap();
        assert_eq!(
            validate_encoder_header(&link, &spec),
            Err(EncoderInvalid::Missing)
        );
    }

    /// The real encoders, read-only. `#[ignore]` and env-gated like the model-file tests in
    /// gguf.rs; run with
    ///
    /// ```text
    /// GIAP_TEST_MMPROJ_DIR="$HOME/Library/Application Support/goose-in-a-pond/models/mmproj" \
    ///   cargo test -p pond-core --lib vision_encoder -- --ignored --nocapture
    /// ```
    ///
    /// Each `<dir>/mmproj-BF16.gguf` found is validated against the row whose dir matches it
    /// case-insensitively (the Mac's E2B dir is spelled `gemma-4-E2B-it`).
    #[test]
    #[ignore = "needs real encoder files; set GIAP_TEST_MMPROJ_DIR"]
    fn the_real_encoders_validate_against_their_rows() {
        let Ok(root) = std::env::var("GIAP_TEST_MMPROJ_DIR") else {
            eprintln!("GIAP_TEST_MMPROJ_DIR unset");
            return;
        };
        let mut checked = 0;
        for entry in std::fs::read_dir(&root).expect("dir") {
            let dir = entry.expect("entry").path();
            let name = dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            let Some(spec) = encoder_by_dir(&name) else {
                continue;
            };
            let file = dir.join(spec.filename);
            let got = validate_encoder_header(&file, &spec);
            eprintln!("{}: {got:?}", file.display());
            assert_eq!(got, Ok(spec.size_bytes), "{}", file.display());
            checked += 1;
        }
        assert!(checked > 0, "no encoder dirs under {root}");
    }

    // ── Sidecar ─────────────────────────────────────────────────────────────

    #[test]
    fn the_sidecar_sits_beside_the_file() {
        assert_eq!(
            sidecar_path(Path::new(
                "/p/models/mmproj/gemma-4-e2b-it/mmproj-BF16.gguf"
            )),
            Path::new("/p/models/mmproj/gemma-4-e2b-it/mmproj-BF16.gguf.verified")
        );
    }

    #[test]
    fn a_sidecar_round_trips_and_vouches_only_for_the_file_it_hashed() {
        let spec = encoder_by_dir("gemma-4-e2b-it").unwrap();
        let s = EncoderSidecar::new(
            spec.sha256.to_ascii_uppercase(),
            spec.size_bytes,
            1_753_670_820,
        );
        let json = s.to_json();
        assert_eq!(
            json,
            format!(
                r#"{{"sha256":"{}","size":986833728,"mtime_unix":1753670820}}"#,
                spec.sha256
            )
        );
        assert_eq!(EncoderSidecar::parse(&json), Some(s.clone()));

        assert!(sidecar_matches(
            Some(&json),
            &spec,
            spec.size_bytes,
            1_753_670_820
        ));
        assert!(
            !sidecar_matches(Some(&json), &spec, spec.size_bytes, 1_753_670_821),
            "a file touched since it was hashed must be hashed again"
        );
        assert!(!sidecar_matches(
            Some(&json),
            &spec,
            spec.size_bytes - 1,
            1_753_670_820
        ));
        let qat = encoder_by_dir("gemma-4-e2b-it-qat").unwrap();
        assert!(
            !sidecar_matches(Some(&json), &qat, qat.size_bytes, 1_753_670_820),
            "the non-qat hash must never vouch for the qat row, whose size is identical"
        );
        assert!(!sidecar_matches(
            None,
            &spec,
            spec.size_bytes,
            1_753_670_820
        ));
        assert!(!sidecar_matches(
            Some("{not json"),
            &spec,
            spec.size_bytes,
            1
        ));
    }

    #[test]
    fn on_disk_reads_header_and_sidecar_and_nothing_more() {
        let tmp = tempfile::tempdir().unwrap();
        let (bytes, mut spec) = tiny("gemma4v", 1536);
        spec.sha256 = "00ff";
        let p = tmp.path().join("mmproj-BF16.gguf");
        assert_eq!(encoder_on_disk(&p, &spec), OnDisk::Missing);

        std::fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
        assert!(matches!(
            encoder_on_disk(&p, &spec),
            OnDisk::Invalid(EncoderInvalid::Truncated { .. })
        ));

        std::fs::write(&p, &bytes).unwrap();
        let n = bytes.len() as u64;
        assert_eq!(encoder_on_disk(&p, &spec), OnDisk::Unverified { bytes: n });

        let mtime = mtime_unix(&std::fs::metadata(&p).unwrap()).unwrap();
        std::fs::write(
            sidecar_path(&p),
            EncoderSidecar::new("00ff", n, mtime).to_json(),
        )
        .unwrap();
        assert_eq!(encoder_on_disk(&p, &spec), OnDisk::Verified { bytes: n });

        std::fs::write(
            sidecar_path(&p),
            EncoderSidecar::new("11ee", n, mtime).to_json(),
        )
        .unwrap();
        assert_eq!(
            encoder_on_disk(&p, &spec),
            OnDisk::Unverified { bytes: n },
            "a sidecar for other bytes is no sidecar"
        );
    }

    // ── State ───────────────────────────────────────────────────────────────

    /// The wire shape the desktop's closed union reads.
    #[test]
    fn states_serialise_as_the_desktop_expects() {
        let cases = [
            (EncoderState::Unknown, r#"{"kind":"unknown"}"#),
            (EncoderState::NotDeclared, r#"{"kind":"not_declared"}"#),
            (
                EncoderState::NotOnThisDevice,
                r#"{"kind":"not_on_this_device"}"#,
            ),
            (EncoderState::Absent, r#"{"kind":"absent"}"#),
            (EncoderState::Verifying, r#"{"kind":"verifying"}"#),
            (
                EncoderState::Downloading { done: 5, total: 9 },
                r#"{"kind":"downloading","done":5,"total":9}"#,
            ),
            (
                EncoderState::Ready { bytes: None },
                r#"{"kind":"ready","bytes":null}"#,
            ),
            (
                EncoderState::Ready { bytes: Some(7) },
                r#"{"kind":"ready","bytes":7}"#,
            ),
            (
                EncoderState::Failed {
                    reason: FailReason::DiskFull,
                    retry_at_unix_ms: 12,
                },
                r#"{"kind":"failed","reason":"disk_full","retry_at_unix_ms":12}"#,
            ),
            (
                EncoderState::Blocked {
                    mode: "offline".into(),
                    host: "huggingface.co".into(),
                },
                r#"{"kind":"blocked","mode":"offline","host":"huggingface.co"}"#,
            ),
        ];
        for (state, json) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
            assert_eq!(serde_json::from_str::<EncoderState>(json).unwrap(), state);
            assert_eq!(
                serde_json::to_value(&state).unwrap()["kind"],
                state.kind(),
                "kind() must agree with the serialised tag"
            );
        }
        for (r, s) in [
            (FailReason::ConnectionDropped, "\"connection_dropped\""),
            (FailReason::WrongFile, "\"wrong_file\""),
            (FailReason::CouldNotStart, "\"could_not_start\""),
            (FailReason::Other, "\"other\""),
        ] {
            assert_eq!(serde_json::to_string(&r).unwrap(), s);
        }
    }

    #[test]
    fn every_state_is_exactly_one_of_ready_unsupported_pending_or_unknown() {
        let all = [
            EncoderState::Unknown,
            EncoderState::NotDeclared,
            EncoderState::NotOnThisDevice,
            EncoderState::Absent,
            EncoderState::Verifying,
            EncoderState::Downloading { done: 0, total: 0 },
            EncoderState::Ready { bytes: None },
            EncoderState::Failed {
                reason: FailReason::Other,
                retry_at_unix_ms: 0,
            },
            EncoderState::Blocked {
                mode: "allowlist".into(),
                host: "h".into(),
            },
        ];
        for s in &all {
            let n = [
                s.is_ready(),
                s.is_unsupported(),
                s.is_pending(),
                *s == EncoderState::Unknown,
            ]
            .iter()
            .filter(|b| **b)
            .count();
            assert_eq!(n, 1, "{s:?}");
            if s.is_moving() {
                assert!(s.is_pending(), "{s:?}");
            }
        }
    }

    // ── Retry and classification ────────────────────────────────────────────

    #[test]
    fn retries_climb_a_fixed_ladder_to_a_two_hour_cap() {
        let mins = |a| retry_after(a).as_secs() / 60;
        assert_eq!(retry_after(0), Duration::from_secs(60));
        assert_eq!(retry_after(1), Duration::from_secs(60));
        assert_eq!(mins(2), 5);
        assert_eq!(mins(3), 30);
        assert_eq!(mins(4), 120);
        assert_eq!(mins(50), 120);
        assert_eq!(mins(u32::MAX), 120);
    }

    #[test]
    fn the_ladder_resets_on_success_and_on_a_network_mode_change() {
        let mut b = RetryBackoff::new();
        assert_eq!(b.on_failure("offline"), Duration::from_secs(60));
        assert_eq!(b.on_failure("offline").as_secs(), 300);
        assert_eq!(b.on_failure("offline").as_secs(), 1800);
        assert_eq!(b.failures(), 3);
        assert_eq!(
            b.on_failure("open").as_secs(),
            60,
            "opening the network must not wait out a step earned while it was closed"
        );
        b.on_failure("open");
        b.on_success();
        assert_eq!(b.failures(), 0);
        assert_eq!(b.on_failure("open").as_secs(), 60);
        b.on_network_mode("open");
        assert_eq!(b.failures(), 1, "the same mode again is not a change");
    }

    #[test]
    fn a_network_mode_refusal_is_blocked_not_failed() {
        use crate::shared::services::egress::{EgressDenied, NetworkMode};
        let denied = EgressDenied {
            host: "cas-bridge.xethub.hf.co".into(),
            mode: NetworkMode::Allowlist,
            reason: "r",
        };
        let err = anyhow::Error::new(denied.clone()).context("fetching the encoder");
        assert_eq!(
            classify_error(&err, 99),
            EncoderState::Blocked {
                mode: "allowlist".into(),
                host: "cas-bridge.xethub.hf.co".into()
            },
            "the host is the refused hop, which need not be huggingface.co"
        );
        // It wins even over an I/O error elsewhere in the picture.
        assert!(matches!(
            classify_failure(
                Some(&denied),
                Some(std::io::ErrorKind::StorageFull),
                true,
                1
            ),
            EncoderState::Blocked { .. }
        ));
    }

    #[test]
    fn failures_classify_by_type_never_by_text() {
        use std::io::{Error, ErrorKind};
        let at = |e: anyhow::Error| match classify_error(&e, 5) {
            EncoderState::Failed {
                reason,
                retry_at_unix_ms: 5,
            } => reason,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            at(anyhow::Error::new(Error::from(ErrorKind::StorageFull)).context("write")),
            FailReason::DiskFull
        );
        assert_eq!(
            at(anyhow::Error::new(Error::from(ErrorKind::ConnectionReset)).context("body")),
            FailReason::ConnectionDropped
        );
        assert_eq!(
            at(anyhow::Error::new(Error::from(ErrorKind::UnexpectedEof))),
            FailReason::ConnectionDropped
        );
        assert_eq!(
            at(anyhow::Error::new(EncoderInvalid::WrongFile).context("after the transfer")),
            FailReason::WrongFile
        );
        assert_eq!(
            at(anyhow::anyhow!(
                "the connection dropped (disk full, timed out)"
            )),
            FailReason::Other,
            "message text must never decide the reason"
        );
    }

    // ── Stamp plan ──────────────────────────────────────────────────────────

    #[test]
    fn stamping_reaches_every_row_naming_the_gguf_and_only_those() {
        let gguf = Path::new("/blobs/e2b");
        let other = Path::new("/blobs/e4b");
        let enc = Path::new("/p/models/mmproj/gemma-4-e2b-it/mmproj-BF16.gguf");
        let rows = [
            RowView {
                id: "gemma-4-E2B-it-Q4_K_M",
                resolved_path: gguf,
                mmproj_path: None,
                mmproj_size_bytes: 0,
            },
            RowView {
                id: "gemma-4-E2B-it",
                resolved_path: gguf,
                mmproj_path: Some(enc),
                mmproj_size_bytes: 986_833_728,
            },
            RowView {
                id: "gemma-4-E4B-it",
                resolved_path: other,
                mmproj_path: None,
                mmproj_size_bytes: 0,
            },
        ];
        let stamp = StampDecision::Stamp {
            mmproj_path: enc,
            size_bytes: 986_833_728,
        };
        assert_eq!(
            stamp_plan(&rows, gguf, stamp),
            vec![RowChange::Stamp {
                id: "gemma-4-E2B-it-Q4_K_M".into(),
                mmproj_path: enc.to_path_buf(),
                size_bytes: 986_833_728
            }],
            "the already-stamped row and the other model's row are left alone"
        );

        assert_eq!(
            stamp_plan(&rows, gguf, StampDecision::Clear),
            vec![RowChange::Clear {
                id: "gemma-4-E2B-it".into()
            }]
        );

        // A wrong size on a right path is re-stamped: the engine sizes memory from it.
        let stale = [RowView {
            mmproj_size_bytes: 636_790_074,
            ..rows[1]
        }];
        assert_eq!(stamp_plan(&stale, gguf, stamp).len(), 1);
    }

    #[test]
    fn a_plan_that_changes_nothing_is_empty() {
        let gguf = Path::new("/g");
        let enc = Path::new("/e");
        let rows = [RowView {
            id: "a",
            resolved_path: gguf,
            mmproj_path: Some(enc),
            mmproj_size_bytes: 10,
        }];
        let stamp = StampDecision::Stamp {
            mmproj_path: enc,
            size_bytes: 10,
        };
        assert!(stamp_plan(&rows, gguf, stamp).is_empty());
        let bare = [RowView {
            mmproj_path: None,
            mmproj_size_bytes: 0,
            ..rows[0]
        }];
        assert!(stamp_plan(&bare, gguf, StampDecision::Clear).is_empty());
        assert!(stamp_plan(&[], gguf, StampDecision::Clear).is_empty());
    }

    #[test]
    fn a_quarantined_encoder_is_unstamped_everywhere() {
        let enc = Path::new("/e2b/mmproj-BF16.gguf");
        let keep = Path::new("/e4b/mmproj-BF16.gguf");
        let rows = [
            RowView {
                id: "a",
                resolved_path: Path::new("/deleted.gguf"),
                mmproj_path: Some(enc),
                mmproj_size_bytes: 1,
            },
            RowView {
                id: "b",
                resolved_path: Path::new("/x"),
                mmproj_path: Some(keep),
                mmproj_size_bytes: 1,
            },
        ];
        assert_eq!(
            unstamp_plan(&rows, enc),
            vec![RowChange::Clear { id: "a".into() }],
            "a row whose GGUF is gone still points the engine at the encoder"
        );
    }

    // ── Household copy ──────────────────────────────────────────────────────

    fn e2b() -> EncoderSpec {
        encoder_by_dir("gemma-4-e2b-it").unwrap()
    }

    #[test]
    fn the_status_lines_are_the_agreed_copy() {
        let spec = e2b();
        let s = |st: EncoderState| status_message_with(&st, Some(&spec), &[]);
        assert_eq!(
            s(EncoderState::Absent).unwrap(),
            "Picture support for Gemma 4 E2B needs a one-time 941 MB download. It starts by \
             itself; text chat works meanwhile."
        );
        assert_eq!(
            s(EncoderState::Downloading {
                done: 300 * 1_048_576 + 5,
                total: 986_833_728
            })
            .unwrap(),
            "Getting picture support ready: 300 MB of 941 MB. Text chat works meanwhile."
        );
        assert_eq!(
            s(EncoderState::Downloading { done: 0, total: 0 }).unwrap(),
            "Getting picture support ready: 0 MB of 941 MB. Text chat works meanwhile.",
            "an unknown total falls back to the pinned size"
        );
        assert_eq!(
            s(EncoderState::Verifying).unwrap(),
            "Checking picture support before its first use. Text chat works meanwhile."
        );
        assert_eq!(
            s(EncoderState::Blocked {
                mode: "allowlist".into(),
                host: "huggingface.co".into()
            })
            .unwrap(),
            "Picture support needs a one-time 941 MB download from huggingface.co, and Network \
             reach is set to Allowed only, which blocks it. To allow it, set Network reach to \
             Open in Settings, under Privacy & Security."
        );
        assert_eq!(
            s(EncoderState::Blocked {
                mode: "offline".into(),
                host: "huggingface.co".into()
            })
            .unwrap(),
            "Picture support needs a one-time 941 MB download from huggingface.co, and Network \
             reach is set to Offline, which blocks it. To allow it, set Network reach to Open in \
             Settings, under Privacy & Security."
        );
        for quiet in [
            EncoderState::Unknown,
            EncoderState::NotDeclared,
            EncoderState::Ready { bytes: Some(1) },
        ] {
            assert_eq!(s(quiet.clone()), None, "{quiet:?} shows no line");
        }
    }

    #[test]
    fn the_failure_lines_are_the_agreed_copy() {
        let spec = e2b();
        let f = |r| failed_message(r, Some(&spec));
        assert_eq!(
            f(FailReason::ConnectionDropped),
            "Picture support did not finish downloading: the connection dropped."
        );
        assert_eq!(
            f(FailReason::WrongFile),
            "Picture support did not finish downloading: the file that arrived was not the right \
             one."
        );
        assert_eq!(
            f(FailReason::DiskFull),
            "Picture support needs 941 MB of free space on this device. Free some on the Models \
             page."
        );
        assert_eq!(
            f(FailReason::CouldNotStart),
            "Picture support could not start on this device."
        );
        assert_eq!(
            f(FailReason::Other),
            "Picture support did not finish downloading."
        );
        let st = EncoderState::Failed {
            reason: FailReason::ConnectionDropped,
            retry_at_unix_ms: 1,
        };
        assert_eq!(
            status_message_with(&st, Some(&spec), &[]).unwrap(),
            f(FailReason::ConnectionDropped),
            "the client adds the retry time, so the server line carries none"
        );
    }

    #[test]
    fn not_on_this_device_names_a_model_that_can_or_says_none_can() {
        let e4b = encoder_by_dir("gemma-4-e4b-it-qat").unwrap();
        assert_eq!(
            not_on_this_device_message(Some(e4b.label), &["gemma-4-e2b-it", "gemma-4-e2b-it-qat"]),
            "Gemma 4 E4B cannot look at pictures on this device: picture support would take room \
             it needs for conversation. Gemma 4 E2B can; choose it on the Models page."
        );
        assert_eq!(
            not_on_this_device_message(Some(e4b.label), &[]),
            "Picture support is not available on this device yet.",
            "with nothing measured, no model can, and the copy must not send anyone to one"
        );
        assert_eq!(
            not_on_this_device_message(Some("Gemma 4 E2B"), &["gemma-4-e2b-it"]),
            "Picture support is not available on this device yet.",
            "never suggest the model that was just refused"
        );
        assert_eq!(
            status_message_with(&EncoderState::NotOnThisDevice, Some(&e4b), &[]).unwrap(),
            "Picture support is not available on this device yet."
        );
    }

    #[test]
    fn the_refusals_are_the_agreed_copy_with_the_agreed_codes() {
        let spec = e2b();
        let r = |st: Option<EncoderState>, provider: &str| {
            refusal_for(st.as_ref(), Some(&spec), provider)
        };
        let nd = r(Some(EncoderState::NotDeclared), "local").unwrap();
        assert_eq!(nd.code.as_str(), "vision_unsupported");
        assert_eq!(
            nd.message,
            "This model cannot look at pictures. To send one, choose a model marked Reads \
             pictures on the Models page."
        );
        // What the goose adapter reports under mesh.
        let mesh = r(Some(EncoderState::NotDeclared), "mesh").unwrap();
        assert_eq!(mesh.code, RefusalCode::Unsupported);
        assert_eq!(
            mesh.message,
            "Pictures cannot be sent to another pond yet. Switch back to a model on this device \
             to send one."
        );
        assert_eq!(
            r(Some(EncoderState::Ready { bytes: None }), "mesh").unwrap(),
            mesh,
            "the mesh wire drops images whatever the local model could do"
        );
        assert_eq!(
            r(None, "mesh"),
            None,
            "a backend that does not report fails open"
        );
        assert_eq!(r(Some(EncoderState::Unknown), "mesh"), None);
        assert_eq!(
            r(Some(EncoderState::NotOnThisDevice), "local")
                .unwrap()
                .code,
            RefusalCode::Unsupported
        );
        let absent = r(Some(EncoderState::Absent), "gguf").unwrap();
        assert_eq!(absent.code.as_str(), "vision_not_ready");
        assert!(absent
            .message
            .starts_with("Picture support for Gemma 4 E2B"));
        for pending in [
            EncoderState::Verifying,
            EncoderState::Downloading { done: 1, total: 2 },
            EncoderState::Failed {
                reason: FailReason::Other,
                retry_at_unix_ms: 0,
            },
            EncoderState::Blocked {
                mode: "offline".into(),
                host: "h".into(),
            },
        ] {
            assert_eq!(
                r(Some(pending.clone()), "local").unwrap().code,
                RefusalCode::NotReady,
                "{pending:?}"
            );
        }
        for pass in [
            None,
            Some(EncoderState::Unknown),
            Some(EncoderState::Ready { bytes: None }),
        ] {
            assert_eq!(r(pass.clone(), "local"), None, "{pass:?} must fail open");
        }
    }

    #[test]
    fn an_unreadable_picture_is_counted_from_one() {
        assert_eq!(
            image_unreadable_message(2),
            "Picture 2 could not be read. Save it as a JPEG or PNG and attach it again."
        );
    }

    /// The model is named by its label everywhere, never by a file name, and no line carries
    /// a character outside plain ASCII (the house forbids emoji in copy; ASCII rules them out).
    #[test]
    fn no_line_names_a_file_or_leaves_ascii() {
        let mut lines = vec![NOT_DECLARED_MESSAGE.to_string(), MESH_MESSAGE.to_string()];
        for spec in ENCODER_SPECS {
            for st in [
                EncoderState::Absent,
                EncoderState::Verifying,
                EncoderState::Downloading { done: 1, total: 2 },
                EncoderState::NotOnThisDevice,
                EncoderState::Blocked {
                    mode: "offline".into(),
                    host: "huggingface.co".into(),
                },
            ] {
                lines.extend(status_message_with(&st, Some(spec), &["gemma-4-e2b-it"]));
            }
            for r in [
                FailReason::ConnectionDropped,
                FailReason::DiskFull,
                FailReason::WrongFile,
                FailReason::CouldNotStart,
                FailReason::Other,
            ] {
                lines.push(failed_message(r, Some(spec)));
            }
        }
        for line in &lines {
            assert!(
                !line.contains(".gguf") && !line.contains("mmproj"),
                "{line}"
            );
            assert!(line.is_ascii(), "{line}");
        }
    }
}
