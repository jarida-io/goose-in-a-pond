//! Model downloader — Whisper ASR, Silero VAD, Kokoro TTS, and llamafile LLM.
//!
//! All models are downloaded into subdirectories of GIAP's data directory:
//! - `models/ggml-*.bin`        — Whisper GGML models
//! - `models/silero/`           — Silero VAD weights
//! - `models/kokoro/`           — Kokoro engine, tokenizer and voices
//! - `models/tts/`              — Piper voice models
//! - `models/llm/`              — llamafile LLM models
//!
//! llamafile bundles model weights + llama.cpp server into a single executable.
//! Running it with `--server --port 8080` starts an OpenAI-compatible HTTP server.

use anyhow::{anyhow, Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};

// ── Piper TTS model download ───────────────────────────────────────────────────

/// Directory for TTS voice models: `<data_dir>/models/tts/`.
pub fn tts_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("tts")
}

// `download_piper_model_entry` used to live here — it fetched a `.onnx` voice
// and its `.onnx.json` config. It lost its last caller when Kokoro replaced
// Piper as the engine, and the module comment that once licensed the resulting
// dead-code warning ("gating each item individually would noise up the module")
// described a `legacy-subprocess` feature that no longer exists. Deleted rather
// than re-explained. `tts_models_dir` above stays: two callers still read that
// directory to notice a pre-Kokoro install.

// ── Piper binary download ──────────────────────────────────────────────────────

/// KEEP. The piper *binary* download is dead, but `ensure_espeak_ng_data`
/// still borrows the phoneme data out of that release tarball — this constant
/// outlives the cluster it sits in. Do not remove it with the surrounding
/// dead code.
const PIPER_GITHUB_BASE: &str = "https://github.com/rhasspy/piper/releases/download/2023.11.14-2";

/// Somewhere to send byte-level progress while a file is being fetched.
///
/// Called as `(filename, downloaded, total)`. Exists so the Kokoro setup path
/// can report into the same tracker the Models page already polls, instead of
/// the UI growing a second, parallel idea of what "downloading" means.
pub type DlProgress = std::sync::Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

// ── Kokoro engine ─────────────────────────────────────────────────────────────

const KOKORO_REPO_BASE: &str =
    "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/";

/// `<data_dir>/models/kokoro/` — engine weights, tokenizer, and `voices/`.
pub fn kokoro_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("kokoro")
}

/// Which voices a start-up must have on disk, given the configured setting.
///
/// **Always includes the default**, then the configured voice when it is a
/// different, resolvable Kokoro id.
///
/// An install that predates the engine swap carries a Piper filename in
/// `voice_tts_voice` (`en_US-ryan-high.onnx`), which is not a Kokoro voice id
/// and never will be. This used to bail on that name and fetch nothing at all,
/// so a fresh install had no style table, the first utterance failed, and every
/// turn fell back to Piper — with the logs showing only "skipping download".
/// The adapter already falls back to the default voice; what it cannot do is
/// conjure the file.
fn voices_to_fetch(configured: &str) -> Vec<String> {
    let mut wanted = vec![pond_adapters_kokoro::DEFAULT_VOICE.to_string()];
    let configured = configured.trim();
    if configured.is_empty() || configured == pond_adapters_kokoro::DEFAULT_VOICE {
        return wanted;
    }
    // Validated against a throwaway root: this asks "is this a usable voice
    // id", which is a property of the name, not of where it would be written.
    if pond_adapters_kokoro::voices::voice_path(Path::new("/"), configured).is_ok() {
        wanted.push(configured.to_string());
    } else {
        tracing::info!(
            voice = configured,
            "configured voice is not a Kokoro id (likely a Piper filename from before \
             the engine swap); fetching the default voice instead"
        );
    }
    wanted
}

/// Ensure the Kokoro engine can start: the tokenizer, one set of weights, and
/// the default voice.
///
/// The catalogue lists voices (522 KB each) but not these — a voice is useless
/// without the shared weights, and nothing else would ever fetch them. Without
/// this, `KokoroOutput::new` fails on the missing tokenizer and TTS silently
/// stays on Piper, which looks exactly like the engine swap never happening.
///
/// Best-effort: every failure leaves Piper as the engine rather than leaving
/// the pond mute. `quality` picks which `.onnx` to fetch.
pub async fn ensure_kokoro_engine(data_dir: &Path, quality: &str, voice: &str) {
    ensure_kokoro_engine_reporting(data_dir, quality, voice, None).await
}

/// `ensure_kokoro_engine`, reporting byte progress for whatever it fetches.
///
/// The settings screen uses this so a 326 MB tier change shows a real bar
/// instead of a spinner that could mean anything.
pub async fn ensure_kokoro_engine_reporting(
    data_dir: &Path,
    quality: &str,
    voice: &str,
    report: Option<DlProgress>,
) {
    // Progress goes to STDERR, not stdout. Under `--json-events` — which is the
    // only mode the desktop's voice child runs in — stdout carries NDJSON and
    // nothing else, so a `println!` here is a contract violation that breaks
    // the session before it starts. `json_events_contract_test` caught exactly
    // that. `download_file` writes its own status to stderr for the same reason.
    let dir = kokoro_dir(data_dir);
    let voices = dir.join("voices");
    if let Err(e) = std::fs::create_dir_all(&voices) {
        tracing::warn!("could not create {}: {e}", voices.display());
        return;
    }

    // Tokenizer: 3 KB, and the one file whose absence stops the engine dead.
    let tokenizer = dir.join("tokenizer.json");
    if !tokenizer.exists() {
        eprintln!("  📥 Kokoro tokenizer...");
        if let Err(e) = download_file_reporting(
            &format!("{KOKORO_REPO_BASE}tokenizer.json"),
            &tokenizer,
            1,
            report.clone(),
        )
        .await
        {
            tracing::warn!("Kokoro tokenizer download failed: {e}");
            return;
        }
    }

    let filename = pond_adapters_kokoro::model_filename(quality);
    let weights = dir.join(filename);
    if !weights.exists() {
        let mb = pond_adapters_kokoro::model_size_mb(quality);
        eprintln!("  📥 Kokoro voice engine ({quality}, ~{mb} MB) — one time...");
        if let Err(e) = download_file_reporting(
            &format!("{KOKORO_REPO_BASE}onnx/{filename}"),
            &weights,
            mb,
            report.clone(),
        )
        .await
        {
            tracing::warn!("Kokoro weights download failed: {e}");
            return;
        }
    }

    for name in voices_to_fetch(voice) {
        let Ok(voice_file) = pond_adapters_kokoro::voices::voice_path(&voices, &name) else {
            continue;
        };
        if voice_file.exists() {
            continue;
        }
        eprintln!("  📥 Kokoro voice \"{name}\"...");
        if let Err(e) = download_file_reporting(
            &format!("{KOKORO_REPO_BASE}voices/{name}.bin"),
            &voice_file,
            1,
            report.clone(),
        )
        .await
        {
            tracing::warn!("Kokoro voice {name} download failed: {e}");
        }
    }
}

// ── Silero VAD ────────────────────────────────────────────────────────────────

/// The exact revision the detector was measured against.
///
/// Pinned rather than `main` because `pond_adapters_silero` hard-codes this
/// model's shape — a 512-sample window and a `[2, 1, 128]` recurrent state —
/// and a retag upstream would not fail the build. It would fail one inference
/// per window at run time, and the detector deliberately treats an inference
/// error as *speech* so a dead VAD cannot cut a sentence in half. The symptom
/// of a silently changed model is therefore a microphone that never closes
/// until the hard cap, with nothing in the log that points upstream. A pin
/// costs nothing.
const SILERO_REVISION: &str = "e71cae966052b992a7eca6b17738916ce0eca4ec";

/// Where the detector looks: `<data_dir>/models/silero/silero_vad.onnx`.
pub fn silero_model_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join("models")
        .join("silero")
        .join("silero_vad.onnx")
}

/// Where the weights come from, in one place a test can read.
fn silero_url() -> String {
    format!(
        "https://huggingface.co/onnx-community/silero-vad/resolve/{SILERO_REVISION}/onnx/model.onnx"
    )
}

/// Fetch the Silero VAD weights unless they are already on disk.
///
/// 2 MB, once. Returns `None` when the file is neither present nor fetchable;
/// the caller degrades to the energy gate rather than failing, because a pond
/// with no network still has to be able to listen.
pub async fn ensure_silero_model(data_dir: &Path) -> Option<PathBuf> {
    let dest = silero_model_path(data_dir);
    if dest.exists() {
        return Some(dest);
    }

    let parent = dest.parent()?;
    if let Err(e) = tokio::fs::create_dir_all(parent).await {
        tracing::warn!("could not create {}: {e}", parent.display());
        return None;
    }

    // stderr, like every other status line in this module: the voice child runs
    // under `--json-events`, where stdout carries NDJSON and nothing else.
    eprintln!("  Listen   fetching the speech detector (2 MB, one time)...");
    match download_file(&silero_url(), &dest, 2).await {
        Ok(()) => Some(dest),
        Err(e) => {
            tracing::warn!("silero VAD download failed: {e}");
            eprintln!("  Listen   speech detector download failed: {e}");
            None
        }
    }
}

/// Returns the path where espeak-ng-data should live: `<data_dir>/bin/espeak-ng-data/`.
pub fn piper_espeak_data_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("espeak-ng-data")
}

/// Ensure espeak-ng-data is installed at `<data_dir>/bin/espeak-ng-data/`.
///
/// On first run (or after a source-only build that didn't copy the data):
///   1. macOS: try `brew install espeak-ng` and copy from Homebrew prefix.
///   2. All platforms fallback: download the Linux x86_64 piper tarball and
///      extract only the `espeak-ng-data/` subtree.  The phoneme data files
///      are platform-agnostic (text/binary tables, not native code).
pub async fn ensure_espeak_ng_data(data_dir: &Path) {
    let dest = piper_espeak_data_path(data_dir);
    if dest.exists() {
        return;
    }

    println!("  📥 espeak-ng-data missing — installing...");

    // ── Option 1: brew prefix on macOS ──────────────────────────────────────
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = tokio::process::Command::new("brew")
            .args(["install", "espeak-ng"])
            .output()
            .await
        {
            if output.status.success() {
                // Find data dir in Homebrew prefix.
                for candidate in &[
                    "/opt/homebrew/lib/espeak-ng-data",
                    "/usr/local/lib/espeak-ng-data",
                    "/opt/homebrew/Cellar",
                ] {
                    let p = std::path::Path::new(candidate);
                    if p.is_dir() && p.file_name().map_or(false, |n| n == "espeak-ng-data") {
                        if copy_dir_all(p, &dest).is_ok() {
                            println!("  espeak-ng-data (Homebrew): {}", dest.display());
                            return;
                        }
                    }
                }
                // Homebrew installed but path detection failed — do broader search.
                if let Ok(out) = tokio::process::Command::new("brew")
                    .args(["--prefix", "espeak-ng"])
                    .output()
                    .await
                {
                    let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let data_p = std::path::Path::new(&prefix)
                        .join("lib")
                        .join("espeak-ng-data");
                    if data_p.is_dir() {
                        if copy_dir_all(&data_p, &dest).is_ok() {
                            println!("  espeak-ng-data (Homebrew): {}", dest.display());
                            return;
                        }
                    }
                }
            }
        }
    }

    // ── Option 2: download from piper Linux x86_64 tarball ──────────────────
    // The phoneme data files are platform-independent; we borrow them from the
    // Linux release and they work on macOS/Windows just as well.
    let url = format!("{}/piper_linux_x86_64.tar.gz", PIPER_GITHUB_BASE);
    println!("  downloading espeak-ng-data...");

    // PAI-2 P6a: github.com is not a curated public suffix, so it classifies
    // Sensitive and both restrictive modes refuse it. That is the intended
    // polarity -- espeak-ng-data is a convenience fetch, and the caller already
    // treats a failure as non-fatal.
    let call = match pond_core::shared::services::egress::begin(&url, "GET") {
        Ok(call) => call,
        Err(denied) => {
            tracing::warn!("espeak-ng-data download refused: {denied}");
            return;
        }
    };
    let sent = reqwest::get(&url).await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));

    let bytes = match sent {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                tracing::warn!("espeak-ng-data download failed: {e}");
                return;
            }
        },
        Ok(resp) => {
            tracing::warn!("espeak-ng-data download: HTTP {}", resp.status());
            return;
        }
        Err(e) => {
            tracing::warn!("espeak-ng-data download failed: {e}");
            return;
        }
    };

    let dest_clone = dest.clone();
    let result = tokio::task::spawn_blocking(move || {
        use flate2::read::GzDecoder;
        use tar::Archive;
        let gz = GzDecoder::new(std::io::Cursor::new(bytes));
        let mut tar = Archive::new(gz);
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            // Only extract entries under espeak-ng-data/
            let mut comps = path.components();
            comps.next(); // strip top-level "piper/"
            let relative: std::path::PathBuf = comps.collect();
            let rel_str = relative.to_string_lossy();
            if !rel_str.starts_with("espeak-ng-data") {
                continue;
            }
            let out_path = dest_clone.parent().unwrap_or(&dest_clone).join(&relative);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry.unpack(&out_path)?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) if dest.exists() => {
            println!("  espeak-ng-data: {}", dest.display())
        }
        Ok(Ok(())) => tracing::warn!("espeak-ng-data not found in tarball"),
        Ok(Err(e)) => tracing::warn!("espeak-ng-data extraction failed: {e}"),
        Err(e) => tracing::warn!("espeak-ng-data task panicked: {e}"),
    }
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// ── llamafile LLM model registry ──────────────────────────────────────────────

// ── Generic file download helper ──────────────────────────────────────────────

/// Download `url` to `dest`, showing a live progress line.  Skips if `dest` exists.
/// Read a Hugging Face access token from one of the conventional env vars.
/// Used to download gated models (Gemma, Llama-Guard, etc.) without manual
/// curl invocations. Returns `None` when neither var is set, in which case
/// callers fall back to anonymous access (which works fine for public repos).
fn hugging_face_token() -> Option<String> {
    for var in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Mirrors `main::default_data_dir()` — kept here because `model_download` runs
/// inside the binary AND as a library helper without access to main's privates.
fn resolve_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("POND_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("goose-in-a-pond")
}

/// Stream a Hugging Face URL through the hf_cache (resumable, etag-aware,
/// auth survives redirects). Symlinks the legacy flat `dest` path to the
/// content-addressed blob so `filesystem_model_storage::path_for()` still
/// returns the same on-disk location.
async fn download_via_hf_cache(
    repo_id: &str,
    revision: &str,
    filename: &str,
    dest: &Path,
    approx_size_mb: u64,
    report: Option<DlProgress>,
) -> Result<()> {
    let data_dir = resolve_data_dir();
    let cache = pond_hf_cache::HfCache::new(&data_dir);

    // Token precedence: existing env-var helper first, then HfCache's token file.
    let token: Option<String> = hugging_face_token().or_else(|| cache.token().map(String::from));

    let client = pond_hf_cache::build_redirect_aware_client(token.as_deref())?;

    let repo = cache
        .repo(repo_id.to_string())
        .with_revision(revision.to_string());
    let fetch = repo.file(filename.to_string());

    // Progress closure: reuses the verbatim CLI progress line from the legacy path.
    let approx_total = approx_size_mb * 1_048_576;
    let mut last_printed = 0u64;
    let reported_name = dest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let progress = |downloaded: u64, total: u64| {
        let effective_total = if total == 0 {
            approx_total.max(1)
        } else {
            total
        };
        // Every chunk, not throttled like the console line below: the tracker
        // is polled on its own cadence and a throttle here would only make the
        // bar lag behind the transfer.
        if let Some(r) = report.as_ref() {
            r(&reported_name, downloaded, effective_total);
        }
        // Throttle stdout updates to ~256 KiB to avoid flooding. Returning
        // `true` here as well as at the end: the value is "keep going", not
        // "I printed something".
        if downloaded < effective_total && downloaded.saturating_sub(last_printed) < 262_144 {
            return true;
        }
        last_printed = downloaded;
        let pct = (downloaded * 100) / effective_total.max(1);
        // stderr: reachable from the --json-events chat path (see download_file).
        eprint!(
            "\r  downloading {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            effective_total / 1_048_576,
            pct
        );
        std::io::stderr().flush().ok();
        // The CLI download has no way to be asked to stop — there is no UI
        // holding it — so it always continues.
        true
    };

    let blob_path = fetch
        .download_to_blob(&client, token.as_deref(), progress)
        .await
        .with_context(|| format!("hf_cache fetch {repo_id}/{filename}@{revision}"))?;
    eprintln!();

    // Symlink (or copy fallback on non-unix) the legacy dest path to the blob.
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let _ = tokio::fs::remove_file(dest).await;
    link_or_copy(&blob_path, dest).await?;

    eprintln!("  saved: {}", dest.display());
    Ok(())
}

#[cfg(unix)]
async fn link_or_copy(src: &Path, dest: &Path) -> Result<()> {
    let src = src.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::os::unix::fs::symlink(&src, &dest)
            .with_context(|| format!("symlink {} -> {}", dest.display(), src.display()))
    })
    .await
    .map_err(|e| anyhow!("symlink task panicked: {e}"))?
}

#[cfg(not(unix))]
async fn link_or_copy(src: &Path, dest: &Path) -> Result<()> {
    tokio::fs::copy(src, dest)
        .await
        .map(|_| ())
        .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))
}

pub async fn download_file(url: &str, dest: &Path, approx_size_mb: u64) -> Result<()> {
    download_file_reporting(url, dest, approx_size_mb, None).await
}

/// `download_file`, with somewhere to send progress.
pub async fn download_file_reporting(
    url: &str,
    dest: &Path,
    approx_size_mb: u64,
    report: Option<DlProgress>,
) -> Result<()> {
    // Progress/status output goes to stderr: this downloader is reachable from the
    // `--json-events` chat path (first-run model fetch), where stdout is reserved
    // exclusively for NDJSON. Interactive callers still see it on the terminal.
    eprintln!(
        "  downloading {} (~{} MB)",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        approx_size_mb
    );

    // ── HF dispatch: route HF URLs through the hardened cache path ───────────
    if let Some((repo_id, revision, filename)) = pond_hf_cache::parse_hf_url(url) {
        return download_via_hf_cache(&repo_id, &revision, &filename, dest, approx_size_mb, report)
            .await;
    }

    let client = reqwest::Client::builder().build()?;
    // Hugging Face gates models behind both repo-level licenses (e.g. Gemma)
    // AND auth tokens. Forward `HF_TOKEN` (or the standard `HUGGING_FACE_HUB_TOKEN`)
    // when present so gated downloads succeed without hand-fetching the file.
    let mut req = client.get(url);
    if url.contains("huggingface.co") {
        if let Some(tok) = hugging_face_token() {
            req = req.bearer_auth(tok);
        }
    }
    // PAI-2 P6a. Stated out loud rather than discovered: neither
    // `huggingface.co` nor `github.com` is in `KNOWN_PUBLIC_SUFFIXES`, so both
    // classify Sensitive, so `network_mode = "allowlist"` refuses every model
    // download from here on. That is the correct polarity -- invariant 4 says
    // the fail-Sensitive default stands and public suffixes are added
    // deliberately, not to soften a refusal -- and it IS a behaviour change for
    // anyone already on `allowlist`. The fix is an actionable message, which
    // `EgressDenied` already renders (mode, host, and what to set), plus the
    // URL for context so the operator knows which download stopped.
    let call = pond_core::shared::services::egress::begin(url, "GET")
        .with_context(|| format!("Failed to fetch {url}"))?;
    let sent = req.send().await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent.with_context(|| format!("Failed to fetch {url}"))?;

    if !resp.status().is_success() {
        // Surface the most common error (gated repo + missing token) in plain
        // English so operators see a clear next step instead of "Server returned 401".
        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            return Err(anyhow!(
                "{} {} for {url} — this looks like a gated Hugging Face repo. \
                 Accept the model licence on the model's HF page, generate a \
                 read-only token at https://huggingface.co/settings/tokens, then \
                 export HF_TOKEN=<token> before re-running the server. \
                 Alternatively, download the GGUF manually and place it at the \
                 expected path so auto-download is skipped.",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or(""),
            ));
        }
        return Err(anyhow!("Server returned {} for {url}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(approx_size_mb * 1_048_576);

    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut downloaded: u64 = 0;
    let mut resp = resp;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow!("Download interrupted: {}", e))?
    {
        use tokio::io::AsyncWriteExt as _;
        file.write_all(&chunk)
            .await
            .map_err(|e| anyhow!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;
        let pct = (downloaded * 100) / total.max(1);
        eprint!(
            "\r  downloading {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            total / 1_048_576,
            pct
        );
        std::io::stderr().flush().ok();
    }

    eprintln!();
    tokio::fs::rename(&tmp, dest).await?;
    eprintln!("  saved: {}", dest.display());
    Ok(())
}

// ── Face recognition models ──────────────────────────────────────────────────
//
// Stack (preferred → fallback):
//
//   Embedder:  AdaFace IR-101 (250 MB) → ArcFace R50 from buffalo_l (174 MB)
//   Detector:  SCRFD 34G       (140 MB) → SCRFD 10G  from buffalo_l ( 17 MB)
//   PAD:       Silent-Face V2  (  2 MB) primary
//              DeepPixBis      (  2 MB) secondary  ── ensembled in adapter
//
// AdaFace beats ArcFace on low-light / blurry crops (IJCB 2022 winner) and
// SCRFD 34G catches faces at smaller pixel sizes than 10G.  DeepPixBis is
// patch-based PAD — pairs well with Silent-Face's full-image classifier
// for stronger replay-attack rejection.  Each URL is overridable via env
// var so a dead mirror can be swapped without rebuilding.

/// On-disk directory where face models live: `<data_dir>/models/face/`.
#[cfg(feature = "face-onnx")]
pub fn face_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("face")
}

/// Returns the canonical (default) paths for the four face-recognition model
/// files.  `(embedding, detector, antispoof_primary, antispoof_secondary)`.
///
/// All four are env-overridable in `build_face_recognition` (`POND_FACE_*_PATH`).
/// The auto-downloader prefers the new defaults but keeps the old buffalo_l
/// files (`w600k_r50.onnx` + `scrfd.onnx`) as fallback when a fresh download
/// of a new model fails (e.g. mirror 404).
#[cfg(feature = "face-onnx")]
pub fn face_model_paths(data_dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = face_models_dir(data_dir);
    (
        // Embedder & detector slot filenames are kept neutral so a future
        // upgrade (e.g. AdaFace once an ONNX export materialises) can land
        // without renaming on disk. The boot lookup in
        // `build_face_recognition` prefers these over the buffalo_l fallback.
        dir.join("adaface_ir101.onnx"),
        dir.join("scrfd_34g.onnx"),
        dir.join("antispoof.onnx"),
        // The secondary PAD filename tracks the model identity so the
        // adapter's filename-based variant heuristic recognises it as
        // DeepPixBis without needing an env-var override.
        dir.join("OULU_Protocol_2_model_0_0.onnx"),
    )
}

/// `buffalo_l.zip` from InsightFace ships both the SCRFD 10G detector
/// (`det_10g.onnx`) and the ArcFace R50 embedder (`w600k_r50.onnx`) in a
/// single ~281 MB archive.  Kept as the fallback bundle when the AdaFace +
/// SCRFD-34G mirrors fail.
#[cfg(feature = "face-onnx")]
const BUFFALO_L_ZIP_URL: &str =
    "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip";
#[cfg(feature = "face-onnx")]
const BUFFALO_L_APPROX_MB: u64 = 281;

/// Glint-R100 embedder — ArcFace ResNet-100 trained on the cleaned
/// Glint360K corpus.  112×112 input, 512-d output (drop-in for the R50
/// in buffalo_l: same matcher math, same threshold table).  Deeper
/// backbone + larger training set → +0.3-0.6 % on hard verification
/// benchmarks vs. R50, with the same ~261 MB on-disk footprint as
/// AdaFace.  Hosted by the Immich team — the most reliable ONNX mirror
/// for InsightFace-family weights.
///
/// Override the URL with `POND_FACE_EMBEDDING_URL` if needed.
#[cfg(feature = "face-onnx")]
const EMBEDDING_DEFAULT_URL: &str =
    "https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx";
#[cfg(feature = "face-onnx")]
const EMBEDDING_APPROX_MB: u64 = 261;

/// SCRFD 34G GNKPS — same SCRFD family as the 10G in buffalo_l, deeper
/// backbone.  Same 5-point landmark contract our Umeyama alignment relies
/// on.  ~39 MB on disk.  Hosted by the Immich team.
///
/// Override the URL with `POND_FACE_DETECTOR_URL`.
#[cfg(feature = "face-onnx")]
const DETECTOR_DEFAULT_URL: &str =
    "https://huggingface.co/immich-app/scrfd_34g_gnkps/resolve/main/detection/model.onnx";
#[cfg(feature = "face-onnx")]
const DETECTOR_APPROX_MB: u64 = 39;

/// Silent-Face MiniFASNetV2 anti-spoof model — 3-class export
/// `[fake_2D, fake_3D, live]` at 80×80 BGR input.  Override the mirror
/// with `POND_FACE_ANTISPOOF_URL`.
#[cfg(feature = "face-onnx")]
const ANTISPOOF_MIRRORS: &[&str] = &[
    "https://huggingface.co/hash-ash/Silent-Face-Anti-Spoofing-ONNX/resolve/main/2.7_80x80_MiniFASNetV2.onnx",
    "https://huggingface.co/datasets/giap-mirror/silent-face-anti-spoofing/resolve/main/2.7_80x80_MiniFASNetV2.onnx",
];
#[cfg(feature = "face-onnx")]
const ANTISPOOF_APPROX_MB: u64 = 2;

/// DeepPixBis (OULU-NPU Protocol-2) PAD — patch-based binary supervision,
/// 224×224 RGB input, sigmoid scalar `output_binary` head.  Complementary
/// to Silent-Face's full-image classifier — better at print + screen-replay
/// rejection.  The ONNX file is hosted on the GitHub release of
/// `ffletcherr/face-recognition-liveness` and matches the architecture
/// from the Deep Pixel-wise Binary Supervision paper (IDIAP).
///
/// The OnnxAntispoof adapter auto-detects DeepPixBis from the filename
/// (`OULU_*` ⇒ DeepPixBis224) and switches to the right preprocessing.
/// Override the URL with `POND_FACE_ANTISPOOF_2_URL` if needed.
#[cfg(feature = "face-onnx")]
const DEEPPIXBIS_DEFAULT_URL: &str =
    "https://github.com/ffletcherr/face-recognition-liveness/releases/download/v0.1/OULU_Protocol_2_model_0_0.onnx";
#[cfg(feature = "face-onnx")]
const ANTISPOOF_2_APPROX_MB: u64 = 13;

// Shared by the face (#face-onnx) and vision (#vision-onnx) downloaders.
#[cfg(any(feature = "face-onnx", feature = "vision-onnx"))]
fn env_url_override(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

/// Download face recognition models into `<data_dir>/models/face/`.
///
/// Preferred stack (4 files, ~400 MB total):
/// - `adaface_ir101.onnx`  (AdaFace IR-101 embedder, ~250 MB)
/// - `scrfd_34g.onnx`      (SCRFD 34G detector with 5-pt landmarks, ~140 MB)
/// - `antispoof.onnx`      (Silent-Face MiniFASNetV2 PAD, ~2 MB)
/// - `deeppixbis.onnx`     (DeepPixBis secondary PAD, ~5 MB)
///
/// Fallback (when the AdaFace / SCRFD-34G mirrors are unreachable) reuses
/// the buffalo_l bundle to populate `w600k_r50.onnx` (ArcFace R50) and
/// `scrfd.onnx` (SCRFD 10G).  `build_face_recognition` then prefers the
/// new files when present and silently uses the buffalo_l fallback
/// otherwise — so a missing mirror downgrades quality but never breaks
/// face recognition.
///
/// Each URL is overridable via env so a dead mirror can be replaced
/// without recompiling: `POND_FACE_EMBEDDING_URL`,
/// `POND_FACE_DETECTOR_URL`, `POND_FACE_ANTISPOOF_URL`,
/// `POND_FACE_ANTISPOOF_2_URL`.
#[cfg(feature = "face-onnx")]
pub async fn download_face_models(data_dir: &Path) -> Result<()> {
    let (embed, detect, antispoof, antispoof_2) = face_model_paths(data_dir);
    let dir = face_models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;

    // ── Embedder: Glint-R100 (preferred) ────────────────────────────────────
    // Verified ONNX mirror at immich-app/antelopev2.  On download failure
    // we let the buffalo_l block below fetch ArcFace R50 instead — same
    // matcher math, lower low-light tolerance.
    if !embed.exists() {
        let url = env_url_override("POND_FACE_EMBEDDING_URL")
            .unwrap_or_else(|| EMBEDDING_DEFAULT_URL.to_string());
        println!("  📥 Glint-R100 embedder not found — downloading from {url}");
        if let Err(e) = download_file(&url, &embed, EMBEDDING_APPROX_MB).await {
            println!(
                "  ⚠  Glint-R100 download failed: {e}\n     \
                 Will fall back to ArcFace R50 from buffalo_l bundle."
            );
        }
    } else {
        println!("  ✅ Glint-R100 embedder already present");
    }

    // ── Detector: SCRFD 34G GNKPS (preferred) ───────────────────────────────
    if !detect.exists() {
        let url = env_url_override("POND_FACE_DETECTOR_URL")
            .unwrap_or_else(|| DETECTOR_DEFAULT_URL.to_string());
        println!("  📥 SCRFD 34G detector not found — downloading from {url}");
        if let Err(e) = download_file(&url, &detect, DETECTOR_APPROX_MB).await {
            println!(
                "  ⚠  SCRFD 34G download failed: {e}\n     \
                 Will fall back to SCRFD 10G from buffalo_l bundle."
            );
        }
    } else {
        println!("  ✅ SCRFD 34G detector already present");
    }

    // ── Buffalo_L fallback for whichever of {embed, detect} is still missing.
    // Always runs when needed, regardless of whether the env-overrides above
    // were attempted, so a fresh install ends up with a working stack out of
    // the box (just with the smaller buffalo_l models, not the upgrades).
    let fallback_embed = dir.join("w600k_r50.onnx");
    let fallback_detect = dir.join("scrfd.onnx");
    let need_fallback_embed = !embed.exists() && !fallback_embed.exists();
    let need_fallback_detect = !detect.exists() && !fallback_detect.exists();
    if need_fallback_embed || need_fallback_detect {
        println!(
            "  📥 Fetching buffalo_l bundle for {}{}{}",
            if need_fallback_embed {
                "ArcFace R50"
            } else {
                ""
            },
            if need_fallback_embed && need_fallback_detect {
                " + "
            } else {
                ""
            },
            if need_fallback_detect {
                "SCRFD 10G"
            } else {
                ""
            },
        );
        if let Err(e) = fetch_buffalo_l_zip(&dir, &fallback_embed, &fallback_detect).await {
            println!("  ⚠  buffalo_l download failed: {e}");
        }
    }

    // ── Primary anti-spoof: Silent-Face V2 ──────────────────────────────────
    if !antispoof.exists() {
        let mirrors: Vec<String> = match env_url_override("POND_FACE_ANTISPOOF_URL") {
            Some(u) => vec![u],
            None => ANTISPOOF_MIRRORS.iter().map(|s| s.to_string()).collect(),
        };
        println!("  📥 Silent-Face PAD not found — trying mirrors...");
        let mut got = false;
        for url in &mirrors {
            match download_file(url, &antispoof, ANTISPOOF_APPROX_MB).await {
                Ok(_) => {
                    got = true;
                    break;
                }
                Err(e) => println!("  ⚠  Mirror {url} failed: {e}"),
            }
        }
        if !got {
            println!(
                "  ⚠  Silent-Face PAD unavailable — heuristic PAD + burst liveness gates \
                 will still run, but the strongest photo-attack defence is missing.\n     \
                 Place the file manually at: {}",
                antispoof.display()
            );
        }
    } else {
        println!("  ✅ Silent-Face PAD already present");
    }

    // ── Secondary anti-spoof: DeepPixBis (OULU-NPU Protocol 2) ──────────────
    // Verified ONNX mirror at the GitHub release of
    // `ffletcherr/face-recognition-liveness`.  When both primary +
    // secondary load successfully the adapter ensembles via
    // `max(spoof_score)`, which strictly improves replay-attack
    // rejection.  Failure is non-fatal — Silent-Face V2 still runs alone.
    if !antispoof_2.exists() {
        let url = env_url_override("POND_FACE_ANTISPOOF_2_URL")
            .unwrap_or_else(|| DEEPPIXBIS_DEFAULT_URL.to_string());
        println!("  📥 DeepPixBis secondary PAD not found — downloading from {url}");
        if let Err(e) = download_file(&url, &antispoof_2, ANTISPOOF_2_APPROX_MB).await {
            println!(
                "  ⚠  DeepPixBis download failed: {e}\n     \
                 Ensemble PAD disabled — Silent-Face V2 still runs alone."
            );
        }
    } else {
        println!("  ✅ DeepPixBis secondary PAD already present");
    }

    Ok(())
}

/// Stream `buffalo_l.zip`, extracting only `det_10g.onnx` → `scrfd.onnx`
/// and `w600k_r50.onnx` → `w600k_r50.onnx` into `out_dir`.
#[cfg(feature = "face-onnx")]
async fn fetch_buffalo_l_zip(out_dir: &Path, embed_dest: &Path, detect_dest: &Path) -> Result<()> {
    println!(
        "  ⬇  buffalo_l.zip (~{} MB) — contains both ArcFace R50 + SCRFD 10G",
        BUFFALO_L_APPROX_MB
    );

    let client = reqwest::Client::builder().build()?;
    // PAI-2 P6a: a `cfg`-gated sender is still a sender. This one only
    // compiles under `face-onnx`, which is exactly why it is easy to miss --
    // the egress guard scans source text, not the built binary.
    let call = pond_core::shared::services::egress::begin(BUFFALO_L_ZIP_URL, "GET")
        .context("Failed to fetch buffalo_l.zip")?;
    let sent = client.get(BUFFALO_L_ZIP_URL).send().await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent.context("Failed to fetch buffalo_l.zip")?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "Server returned {} for buffalo_l.zip",
            resp.status()
        ));
    }

    let total = resp
        .content_length()
        .unwrap_or(BUFFALO_L_APPROX_MB * 1_048_576);
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut downloaded: u64 = 0;
    let mut resp = resp;

    while let Some(chunk) = resp.chunk().await.context("Download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        let pct = (downloaded * 100) / total.max(1);
        print!(
            "\r  downloading {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            total / 1_048_576,
            pct
        );
        std::io::stdout().flush().ok();
    }
    println!();

    let embed_dest = embed_dest.to_path_buf();
    let detect_dest = detect_dest.to_path_buf();
    let out_dir = out_dir.to_path_buf();

    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let cursor = std::io::Cursor::new(buf);
        let mut archive =
            zip::ZipArchive::new(cursor).context("Failed to open buffalo_l zip archive")?;

        let mut embed_found = false;
        let mut detect_found = false;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let raw_name = entry.name().to_string();
            let file_name = std::path::Path::new(&raw_name)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();

            let target = match file_name.as_str() {
                "w600k_r50.onnx" => {
                    embed_found = true;
                    embed_dest.clone()
                }
                "det_10g.onnx" => {
                    detect_found = true;
                    detect_dest.clone()
                }
                _ => continue,
            };

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = target.with_extension("part");
            {
                let mut out_file = std::fs::File::create(&tmp)
                    .with_context(|| format!("Cannot write {}", tmp.display()))?;
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                out_file.write_all(&content)?;
            }
            std::fs::rename(&tmp, &target)?;
            println!("  ✅ Extracted {} → {}", file_name, target.display());
        }

        let _ = &out_dir;

        if !embed_found {
            return Err(anyhow!("buffalo_l.zip did not contain w600k_r50.onnx"));
        }
        if !detect_found {
            return Err(anyhow!("buffalo_l.zip did not contain det_10g.onnx"));
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("buffalo_l zip extraction task panicked")??;

    Ok(())
}

// ── Vision classifier model (#130 follow-up) ─────────────────────────────────
//
// YOLOX-Nano (Apache-2.0, ~3.7 MB) from the official Megvii release — labels
// motion events person/pet/package via pond-adapters-vision-onnx. Mirrors the
// face-model pattern: fetched automatically at serve startup on `vision-onnx`
// builds so a fresh `cargo run` works out of the box, env-overridable mirror.

/// On-disk directory where vision models live: `<data_dir>/models/vision/`.
#[cfg(feature = "vision-onnx")]
pub fn vision_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("vision")
}

/// Filename of the default vision classifier (the auto-downloaded model).
/// `vision_classifier_model` left empty resolves to this file.
#[cfg(feature = "vision-onnx")]
pub const VISION_CLASSIFIER_DEFAULT_FILE: &str = "yolox_nano.onnx";

#[cfg(feature = "vision-onnx")]
const VISION_CLASSIFIER_DEFAULT_URL: &str =
    "https://github.com/Megvii-BaseDetection/YOLOX/releases/download/0.1.1rc0/yolox_nano.onnx";
#[cfg(feature = "vision-onnx")]
const VISION_CLASSIFIER_APPROX_MB: u64 = 4;

/// Ensure the default vision classifier model is on disk, downloading it on
/// first run. No-op when the file already exists. Override the mirror with
/// `POND_VISION_CLASSIFIER_URL`.
#[cfg(feature = "vision-onnx")]
pub async fn download_vision_classifier(data_dir: &Path) -> Result<PathBuf> {
    let dir = vision_models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let dest = dir.join(VISION_CLASSIFIER_DEFAULT_FILE);
    if dest.exists() {
        println!("  ✅ YOLOX-Nano vision classifier already present");
        return Ok(dest);
    }
    let url = env_url_override("POND_VISION_CLASSIFIER_URL")
        .unwrap_or_else(|| VISION_CLASSIFIER_DEFAULT_URL.to_string());
    println!("  📥 YOLOX-Nano vision classifier not found — downloading from {url}");
    download_file(&url, &dest, VISION_CLASSIFIER_APPROX_MB).await?;
    Ok(dest)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Silero VAD ────────────────────────────────────────────────────────────

    /// A branch here would be a live dependency on whatever `main` points at
    /// today, and the adapter hard-codes this model's window and state shape.
    /// The failure mode is not a build break — it is one inference error per
    /// window at run time, which the detector deliberately reports as *speech*,
    /// which is a microphone that never closes.
    #[test]
    fn the_silero_weights_are_pinned_to_a_commit_not_a_branch() {
        assert_eq!(
            SILERO_REVISION.len(),
            40,
            "expected a full commit sha, got {SILERO_REVISION:?}"
        );
        assert!(SILERO_REVISION.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The URL is only ever exercised on a fresh install, so a typo in it
    /// survives every run on a machine that already has the file. This is the
    /// one place it can be checked cheaply: `download_file` dispatches on
    /// `parse_hf_url` returning `Some`, and a malformed URL silently falls
    /// through to the plain-reqwest path instead.
    #[test]
    fn the_silero_url_routes_through_the_hf_cache() {
        let (repo, revision, filename) =
            pond_hf_cache::parse_hf_url(&silero_url()).expect("must parse as a Hugging Face URL");
        assert_eq!(repo, "onnx-community/silero-vad");
        assert_eq!(revision, SILERO_REVISION);
        assert_eq!(filename, "onnx/model.onnx");
    }

    /// Called on every `chat --voice`, so the common case is the second one.
    #[tokio::test]
    async fn an_existing_silero_model_is_not_fetched_again() {
        let tmp = TempDir::new().unwrap();
        let dest = silero_model_path(tmp.path());
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"not really a model, but present").unwrap();

        // No network is mocked: reaching for one would fail the test rather
        // than pass it silently.
        let got = ensure_silero_model(tmp.path()).await;

        assert_eq!(got.as_deref(), Some(dest.as_path()));
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"not really a model, but present"
        );
    }

    // ── Whisper model download via mock HTTP server ───────────────────────────

    #[tokio::test]
    async fn download_whisper_model_writes_file_to_disk() {
        let server = MockServer::start().await;
        let fake_model_bytes = b"fake whisper model data";

        Mock::given(method("GET"))
            .and(path("/ggml-base.en.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fake_model_bytes.as_slice()))
            .mount(&server)
            .await;

        // Temporarily redirect the model URL by downloading from our mock URL directly.
        // We test via download_file (the raw HTTP helper) since model URLs come from the catalog.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("ggml-base.en.bin");
        let url = format!("{}/ggml-base.en.bin", server.uri());

        download_file(&url, &dest, 1).await.unwrap();

        assert!(dest.exists(), "model file should exist after download");
        assert_eq!(std::fs::read(&dest).unwrap(), fake_model_bytes);
    }

    #[tokio::test]
    async fn download_whisper_model_fails_on_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("fail.bin");
        let url = format!("{}/fail.bin", server.uri());

        let err = download_file(&url, &dest, 1).await.unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "expected 500 error, got: {}",
            err
        );
    }

    // ── Generic download_file: connection refused ─────────────────────────────

    #[tokio::test]
    async fn download_file_fails_on_connection_refused() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("nope.bin");
        // Port 1 is reserved — instant connection refused.
        let err = download_file("http://127.0.0.1:1/nope.bin", &dest, 1)
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "expected a non-empty error on connection refused"
        );
        assert!(
            !dest.exists(),
            "partial file should not exist after connection failure"
        );
    }

    // ── PAI-2 P6a: the network-mode gate on the download path ────────────────
    //
    // `egress_guard.rs` checks that this FILE mentions a tracker symbol. This
    // file has THREE senders (`download_file`, `ensure_espeak_ng_data`, and the
    // `face-onnx`-gated `fetch_buffalo_l_zip`), so that check would go green on
    // one of them while the other two still phoned out. This is the behavioural
    // half for the one that matters: `download_file` is the path every model,
    // voice and ONNX fetch in the product goes through.
    //
    // The other two are NOT covered behaviourally and this says so rather than
    // implying otherwise. `ensure_espeak_ng_data` shells out to `brew` and has
    // half a dozen environment-dependent early returns before it reaches the
    // network, so a test of it would pass on this machine without ever touching
    // the gate — a vacuous test wearing a coverage badge. `fetch_buffalo_l_zip`
    // only compiles under `--features face-onnx`. Both are gated in source and
    // reviewed; neither is proven here.

    /// Restores the previous mode however the test exits, panic included.
    ///
    /// `network_mode` is a process-global `RwLock` shared with every other test
    /// in this binary. Flipping it to `Offline` is safe here only because
    /// `Offline` still permits loopback and every sibling test in this crate
    /// that sends anything sends to 127.0.0.1 (wiremock, or the reserved port 1
    /// above). A test that wanted `Allowlist` would refuse those and would need
    /// a serialising lock instead.
    struct ModeGuard(pond_core::shared::services::egress::NetworkMode);

    impl ModeGuard {
        fn set(mode: pond_core::shared::services::egress::NetworkMode) -> Self {
            let previous = pond_core::shared::services::egress::network_mode();
            pond_core::shared::services::egress::set_network_mode(mode);
            Self(previous)
        }
    }

    impl Drop for ModeGuard {
        fn drop(&mut self) {
            pond_core::shared::services::egress::set_network_mode(self.0);
        }
    }

    #[tokio::test]
    async fn offline_refuses_a_download_and_says_which_setting_did_it() {
        use pond_core::shared::services::egress::NetworkMode;

        // A loopback server that WOULD serve the file, so the permitted half
        // below is a real download and not an assertion about nothing.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".as_slice()))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let _mode = ModeGuard::set(NetworkMode::Offline);

        // ── refused ──────────────────────────────────────────────────────────
        // `.invalid` is reserved and never resolves (RFC 2606). If the gate
        // stopped firing this would still fail, but with a DNS error — which is
        // exactly what the two assertions below tell apart. An error message
        // that does not name the setting is indistinguishable from the network
        // being down, which is the defect P5 found in the weather route.
        let err = download_file(
            "https://cdn.invalid/model.bin",
            &tmp.path().join("refused.bin"),
            1,
        )
        .await
        .expect_err("offline must refuse a non-loopback download");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("network_mode") && rendered.contains("offline"),
            "the refusal must name the setting and its value, or the user \
             cannot tell it from an outage: {rendered}"
        );
        assert!(
            rendered.contains("cdn.invalid"),
            "the refusal must name the host it refused: {rendered}"
        );
        assert!(
            !tmp.path().join("refused.bin").exists(),
            "a refused download must leave nothing on disk"
        );

        // ── still permitted ──────────────────────────────────────────────────
        // The vacuity control. A gate that refused everything would satisfy the
        // assertions above and take the pond off its own loopback model server.
        let allowed = tmp.path().join("ok.bin");
        download_file(&format!("{}/ok.bin", server.uri()), &allowed, 1)
            .await
            .expect("offline still permits loopback");
        assert_eq!(std::fs::read(&allowed).unwrap(), b"payload");
    }
}

#[cfg(test)]
mod kokoro_engine_tests {
    use super::*;

    const DEFAULT: &str = pond_adapters_kokoro::DEFAULT_VOICE;

    /// The reported case, from a real log line:
    ///
    /// ```text
    /// WARN invalid Kokoro voice name; skipping download voice="en_US-ryan-high.onnx"
    /// ```
    ///
    /// A stale Piper filename made start-up fetch NOTHING, so a fresh install
    /// had no style table and fell back to Piper on every turn. The default
    /// must be fetched regardless of what the stale setting says.
    #[test]
    fn a_legacy_piper_filename_still_fetches_the_default_voice() {
        let v = voices_to_fetch("en_US-ryan-high.onnx");
        assert!(
            v.contains(&DEFAULT.to_string()),
            "the default voice must be fetched even when the setting is unusable, got {v:?}"
        );
        assert!(
            !v.iter().any(|n| n.contains(".onnx")),
            "a Piper filename must never be treated as a Kokoro voice id, got {v:?}"
        );
    }

    #[test]
    fn an_empty_setting_fetches_the_default() {
        assert_eq!(voices_to_fetch(""), vec![DEFAULT.to_string()]);
        assert_eq!(voices_to_fetch("   "), vec![DEFAULT.to_string()]);
    }

    /// A real, different voice is fetched alongside the default — the default
    /// is the safety net, not a replacement for what was asked for.
    #[test]
    fn a_valid_voice_is_fetched_alongside_the_default() {
        let v = voices_to_fetch("bm_george");
        assert_eq!(v, vec![DEFAULT.to_string(), "bm_george".to_string()]);
    }

    /// Asking for the default names it once, not twice.
    #[test]
    fn the_default_is_not_requested_twice() {
        assert_eq!(voices_to_fetch(DEFAULT), vec![DEFAULT.to_string()]);
    }

    /// Voice names reach this from settings and are joined onto a path.
    #[test]
    fn a_traversal_attempt_is_refused_and_leaves_the_default() {
        let v = voices_to_fetch("../../etc/passwd");
        assert_eq!(v, vec![DEFAULT.to_string()]);
    }
}

// Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98), so
// the drafter's download, its validation and their tests are commented out rather than deleted;
// restore them with the startup block in main.rs if it returns.
// // ── MTP drafter ───────────────────────────────────────────────────────────────
//
// pub use pond_core::models::domain::drafter::drafter_for;
//
// /// Is this file a drafter this engine can actually load?
// ///
// /// A present-but-wrong drafter is worse than a missing one: it fails inside
// /// context creation on the first turn, where the error says "null reference"
// /// and points nowhere. Two files are easy to confuse here -- the ik_llama.cpp
// /// centroid drafters declare `gemma4_mtp` and upstream llama.cpp will not load
// /// them -- so check for the architecture upstream registers rather than
// /// trusting the filename.
// fn is_loadable_drafter(path: &std::path::Path) -> bool {
//     use std::io::Read;
//     let Ok(mut f) = std::fs::File::open(path) else {
//         return false;
//     };
//     let mut head = vec![0u8; 16 * 1024];
//     let Ok(n) = f.read(&mut head) else {
//         return false;
//     };
//     head.truncate(n);
//     if !head.starts_with(b"GGUF") {
//         return false;
//     }
//     head.windows(16).any(|w| w == b"gemma4-assistant")
// }
//
// /// Make sure the drafter for `chat_model` is on disk, fetching it if it is not.
// ///
// /// Returns the path when speculative decoding can be used. Every failure path
// /// returns `None` and leaves the pond decoding without speculation, because a
// /// missing drafter is a lost optimisation and not a broken assistant.
// ///
// /// Self-correcting in the two ways that matter: a partial download never lands
// /// under the real name (it is written to `.part` and renamed only after it
// /// validates), and a file that is present but not loadable is deleted and
// /// re-fetched rather than being handed to the engine to fail on.
// pub async fn ensure_mtp_drafter(data_dir: &Path, chat_model: &str) -> Option<std::path::PathBuf> {
//     let spec = drafter_for(chat_model)?;
//     let dir = data_dir.join("models").join("gguf");
//     if let Err(e) = std::fs::create_dir_all(&dir) {
//         tracing::warn!("could not create {}: {e}", dir.display());
//         return None;
//     }
//     let dest = dir.join(spec.filename);
//
//     if dest.exists() {
//         if is_loadable_drafter(&dest) {
//             return Some(dest);
//         }
//         tracing::warn!(
//             path = %dest.display(),
//             "drafter present but not loadable (truncated, or the ik_llama centroid format); re-fetching"
//         );
//         let _ = std::fs::remove_file(&dest);
//     }
//
//     let url = format!(
//         "https://huggingface.co/{}/resolve/main/{}",
//         spec.repo, spec.filename
//     );
//     let part = dest.with_extension("gguf.part");
//     let _ = std::fs::remove_file(&part);
//     eprintln!(
//         "  📥 speculative-decoding drafter ({} MB) — one time...",
//         spec.approx_mb
//     );
//     if let Err(e) = download_file(&url, &part, spec.approx_mb).await {
//         tracing::warn!("drafter download failed ({url}): {e}; continuing without speculation");
//         let _ = std::fs::remove_file(&part);
//         return None;
//     }
//     if !is_loadable_drafter(&part) {
//         tracing::warn!("downloaded drafter did not validate; continuing without speculation");
//         let _ = std::fs::remove_file(&part);
//         return None;
//     }
//     if let Err(e) = std::fs::rename(&part, &dest) {
//         tracing::warn!("could not install drafter: {e}");
//         let _ = std::fs::remove_file(&part);
//         return None;
//     }
//     tracing::info!(path = %dest.display(), "MTP drafter ready");
//     Some(dest)
// }
//
// #[cfg(test)]
// mod drafter_tests {
//     use super::*;
//
//     #[test]
//     fn a_file_that_is_not_a_drafter_is_rejected() {
//         let tmp = tempfile::tempdir().unwrap();
//         let junk = tmp.path().join("x.gguf");
//         std::fs::write(&junk, b"not a gguf at all").unwrap();
//         assert!(!is_loadable_drafter(&junk));
//
//         // GGUF magic alone is not enough: the ik_llama centroid drafters are
//         // real GGUFs that upstream llama.cpp cannot load.
//         let wrong_arch = tmp.path().join("y.gguf");
//         let mut body = b"GGUF".to_vec();
//         body.extend_from_slice(&[0u8; 512]);
//         body.extend_from_slice(b"gemma4_mtp");
//         std::fs::write(&wrong_arch, &body).unwrap();
//         assert!(!is_loadable_drafter(&wrong_arch));
//
//         let right = tmp.path().join("z.gguf");
//         let mut body = b"GGUF".to_vec();
//         body.extend_from_slice(&[0u8; 512]);
//         body.extend_from_slice(b"gemma4-assistant");
//         std::fs::write(&right, &body).unwrap();
//         assert!(is_loadable_drafter(&right));
//     }
// }
