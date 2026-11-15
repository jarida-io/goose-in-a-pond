//! Model and voice-data downloads into the data directory.

use anyhow::{anyhow, Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};

// ── Piper TTS model download ───────────────────────────────────────────────────

/// Piper voice dir; still read to detect a pre-Kokoro install.
pub fn tts_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("tts")
}

// ── Piper binary download ──────────────────────────────────────────────────────

/// Still needed: `ensure_espeak_ng_data` takes its phoneme data from this release's tarball.
const PIPER_GITHUB_BASE: &str = "https://github.com/rhasspy/piper/releases/download/2023.11.14-2";

/// Byte-progress sink, called as `(filename, downloaded, total)`.
pub type DlProgress = std::sync::Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

/// The `filename` a download to `dest` reports its progress under.
fn progress_name(dest: &Path) -> String {
    dest.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

// ── Kokoro engine ─────────────────────────────────────────────────────────────

const KOKORO_REPO_BASE: &str =
    "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/";

/// `<data_dir>/models/kokoro/` — engine weights, tokenizer, and `voices/`.
pub fn kokoro_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("kokoro")
}

/// Voices start-up must fetch: always the default, plus the configured one if it is a valid
/// Kokoro id (pre-Kokoro installs still hold a Piper filename here).
fn voices_to_fetch(configured: &str) -> Vec<String> {
    let mut wanted = vec![pond_adapters_kokoro::DEFAULT_VOICE.to_string()];
    let configured = configured.trim();
    if configured.is_empty() || configured == pond_adapters_kokoro::DEFAULT_VOICE {
        return wanted;
    }
    // Throwaway root: only the name's validity matters.
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

/// One file a Kokoro fetch tried: the name its progress was reported under, where it was to
/// land, and how the download ended.
#[derive(Debug)]
pub struct FileFetch {
    pub name: String,
    pub dest: PathBuf,
    pub result: Result<()>,
}

/// Best-effort fetch of the Kokoro tokenizer, weights and voices (the catalogue has only
/// voices); `quality` picks the `.onnx`. Any failure leaves Piper as the engine.
pub async fn ensure_kokoro_engine(data_dir: &Path, quality: &str, voice: &str) {
    ensure_kokoro_engine_reporting(data_dir, quality, voice, None).await;
}

/// `ensure_kokoro_engine`, reporting byte progress, and each file it tried with how it ended.
pub async fn ensure_kokoro_engine_reporting(
    data_dir: &Path,
    quality: &str,
    voice: &str,
    report: Option<DlProgress>,
) -> Vec<FileFetch> {
    // Status to stderr only: under `--json-events` stdout carries NDJSON and nothing else.
    let dir = kokoro_dir(data_dir);
    let voices = dir.join("voices");
    if let Err(e) = std::fs::create_dir_all(&voices) {
        tracing::warn!("could not create {}: {e}", voices.display());
        return Vec::new();
    }

    let mut tried = Vec::new();
    // Tokenizer: 3 KB, and the one file whose absence stops the engine dead.
    let tokenizer = dir.join("tokenizer.json");
    if !tokenizer.exists() {
        eprintln!("  📥 Kokoro tokenizer...");
        let url = format!("{KOKORO_REPO_BASE}tokenizer.json");
        if !fetch_kokoro_file(&mut tried, &url, tokenizer, 1, &report, "tokenizer").await {
            return tried;
        }
    }

    let filename = pond_adapters_kokoro::model_filename(quality);
    let weights = dir.join(filename);
    if !weights.exists() {
        let mb = pond_adapters_kokoro::model_size_mb(quality);
        eprintln!("  📥 Kokoro voice engine ({quality}, ~{mb} MB) — one time...");
        let url = format!("{KOKORO_REPO_BASE}onnx/{filename}");
        if !fetch_kokoro_file(&mut tried, &url, weights, mb, &report, "weights").await {
            return tried;
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
        let url = format!("{KOKORO_REPO_BASE}voices/{name}.bin");
        let what = format!("voice {name}");
        fetch_kokoro_file(&mut tried, &url, voice_file, 1, &report, &what).await;
    }
    tried
}

/// The full-precision weights long speech runs on the GPU: one fetch at a time, none once the
/// file is there.
pub async fn ensure_kokoro_gpu_weights(data_dir: PathBuf) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FETCHING: AtomicBool = AtomicBool::new(false);

    let filename = pond_adapters_kokoro::model_filename("fp32");
    let weights = kokoro_dir(&data_dir).join(filename);
    if weights.exists() || FETCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let mb = pond_adapters_kokoro::model_size_mb("fp32");
    let url = format!("{KOKORO_REPO_BASE}onnx/{filename}");
    match download_file(&url, &weights, mb).await {
        Ok(()) => tracing::info!(
            kind = "kokoro_gpu_weights_ready",
            mb,
            "Kokoro's GPU weights are installed; long speech can use the GPU"
        ),
        Err(e) => tracing::warn!(
            kind = "kokoro_gpu_weights_failed",
            error = %format!("{e:#}"),
            "Kokoro's GPU weights could not be fetched; long speech stays on the CPU"
        ),
    }
    FETCHING.store(false, Ordering::SeqCst);
}

/// Fetch one Kokoro file and add it to `tried`; whether it arrived.
async fn fetch_kokoro_file(
    tried: &mut Vec<FileFetch>,
    url: &str,
    dest: PathBuf,
    approx_size_mb: u64,
    report: &Option<DlProgress>,
    what: &str,
) -> bool {
    let result = download_file_reporting(url, &dest, approx_size_mb, report.clone()).await;
    if let Err(e) = &result {
        tracing::warn!("Kokoro {what} download failed: {e}");
    }
    let arrived = result.is_ok();
    tried.push(FileFetch {
        name: progress_name(&dest),
        dest,
        result,
    });
    arrived
}

// ── Silero VAD ────────────────────────────────────────────────────────────────

/// Pinned: the adapter hard-codes this model's shape, and a changed model errors at run time,
/// which the detector reads as speech, so the mic never closes.
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

/// Fetch the Silero weights if absent; `None` means the caller falls back to the energy gate.
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

    // stderr: under `--json-events` stdout is NDJSON only.
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

pub fn piper_espeak_data_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("espeak-ng-data")
}

/// Install espeak-ng-data if missing: brew on macOS, else the (portable) Linux piper tarball.
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
    let url = format!("{}/piper_linux_x86_64.tar.gz", PIPER_GITHUB_BASE);
    println!("  downloading espeak-ng-data...");

    // Restrictive modes refuse github.com (Sensitive); intended, this fetch is optional.
    let call = match pond_core::shared::services::egress::begin_for(
        &url,
        "GET",
        pond_core::shared::services::egress::EgressPurpose::Models,
        "",
    ) {
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

#[cfg(target_os = "macos")]
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

// ── Generic file download helper ──────────────────────────────────────────────

/// HF token from the conventional env vars, for gated repos; `None` means anonymous access.
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

/// Mirrors `main::default_data_dir()`, which this library code can't reach.
fn resolve_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("POND_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("goose-in-a-pond")
}

/// Fetch via the hf_cache and link `dest` to the blob, where `path_for()` still expects it.
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

    let token: Option<String> = hugging_face_token().or_else(|| cache.token().map(String::from));

    let client = pond_hf_cache::build_redirect_aware_client(token.as_deref())?;

    let repo = cache
        .repo(repo_id.to_string())
        .with_revision(revision.to_string());
    let fetch = repo.file(filename.to_string());
    let fetch = match pond_core::models::domain::curated::file_pin(repo_id, revision, filename) {
        Some(pin) => fetch.expect_size(pin.size_bytes).expect_etag(pin.sha256),
        None => fetch,
    };

    let approx_total = approx_size_mb * 1_048_576;
    let mut last_printed = 0u64;
    let reported_name = progress_name(dest);
    let mut announced: Option<u32> = None;
    let mut progress = |downloaded: u64, total: u64| {
        let effective_total = if total == 0 {
            approx_total.max(1)
        } else {
            total
        };
        // Unthrottled: the tracker is polled on its own cadence.
        if let Some(r) = report.as_ref() {
            r(&reported_name, downloaded, effective_total);
        }
        // Print every 256 KiB; `true` means "keep going", not "printed".
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
        // Nothing can cancel a CLI download.
        true
    };
    let on_event = |event: pond_hf_cache::DownloadEvent| match event {
        pond_hf_cache::DownloadEvent::Progress { downloaded, total } => {
            announced = None;
            progress(downloaded, total)
        }
        pond_hf_cache::DownloadEvent::Reconnecting(r) => {
            // Reported on every poll of the wait; said once per wait.
            if announced != Some(r.attempt) {
                announced = Some(r.attempt);
                eprint!(
                    "\n  the connection dropped; reconnecting in {}s (attempt {} of {})",
                    r.delay.as_secs(),
                    r.attempt,
                    r.max_attempts
                );
                std::io::stderr().flush().ok();
            }
            true
        }
    };

    let blob_path = fetch
        .download_to_blob(&client, token.as_deref(), on_event)
        .await
        .with_context(|| format!("hf_cache fetch {repo_id}/{filename}@{revision}"))?;
    eprintln!();

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

/// Download `url` to `dest` (HF URLs via the hf_cache), with progress on stderr.
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
    // stderr: reachable from the `--json-events` chat path, where stdout is NDJSON only.
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

    // A per-read limit, as the hf_cache path has: a silent connection fails instead of hanging.
    let client = pond_net::client_builder()
        .read_timeout(pond_hf_cache::DOWNLOAD_READ_TIMEOUT)
        .build()?;
    let mut req = client.get(url);
    if url.contains("huggingface.co") {
        if let Some(tok) = hugging_face_token() {
            req = req.bearer_auth(tok);
        }
    }
    // HF and github.com classify Sensitive, so `allowlist` refuses model downloads by design;
    // don't add them to `KNOWN_PUBLIC_SUFFIXES` to soften that.
    let call = pond_core::shared::services::egress::begin_for(
        url,
        "GET",
        pond_core::shared::services::egress::EgressPurpose::Models,
        "",
    )
    .with_context(|| format!("Failed to fetch {url}"))?;
    let sent = req.send().await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent.with_context(|| format!("Failed to fetch {url}"))?;

    if !resp.status().is_success() {
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
    let reported_name = progress_name(dest);

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
        if let Some(r) = report.as_ref() {
            r(&reported_name, downloaded, total);
        }
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

#[cfg(feature = "face-onnx")]
pub fn face_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("face")
}

/// Default `(embedder, detector, antispoof, antispoof_2)` paths (`POND_FACE_*_PATH` overrides).
#[cfg(feature = "face-onnx")]
pub fn face_model_paths(data_dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = face_models_dir(data_dir);
    (
        // Fixed slot names, not model ids: the embedder slot holds Glint-R100.
        dir.join("adaface_ir101.onnx"),
        dir.join("scrfd_34g.onnx"),
        dir.join("antispoof.onnx"),
        // Must stay `OULU_*`: the adapter picks DeepPixBis preprocessing by filename.
        dir.join("OULU_Protocol_2_model_0_0.onnx"),
    )
}

/// Fallback bundle (SCRFD 10G + ArcFace R50) for when the preferred mirrors fail.
#[cfg(feature = "face-onnx")]
const BUFFALO_L_ZIP_URL: &str =
    "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip";
#[cfg(feature = "face-onnx")]
const BUFFALO_L_APPROX_MB: u64 = 281;

/// Glint-R100: drop-in for buffalo_l's R50 (112×112 in, 512-d out, same thresholds).
#[cfg(feature = "face-onnx")]
const EMBEDDING_DEFAULT_URL: &str =
    "https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx";
#[cfg(feature = "face-onnx")]
const EMBEDDING_APPROX_MB: u64 = 261;

/// SCRFD 34G GNKPS: keeps the 5-point landmarks the Umeyama alignment relies on.
#[cfg(feature = "face-onnx")]
const DETECTOR_DEFAULT_URL: &str =
    "https://huggingface.co/immich-app/scrfd_34g_gnkps/resolve/main/detection/model.onnx";
#[cfg(feature = "face-onnx")]
const DETECTOR_APPROX_MB: u64 = 39;

/// Silent-Face MiniFASNetV2: 3-class `[fake_2D, fake_3D, live]`, 80×80 BGR input.
#[cfg(feature = "face-onnx")]
const ANTISPOOF_MIRRORS: &[&str] = &[
    "https://huggingface.co/garciafido/minifasnet-v2-anti-spoofing-onnx/resolve/main/minifasnet_v2.onnx",
    "https://huggingface.co/hash-ash/Silent-Face-Anti-Spoofing-ONNX/resolve/main/2.7_80x80_MiniFASNetV2.onnx",
    "https://huggingface.co/datasets/giap-mirror/silent-face-anti-spoofing/resolve/main/2.7_80x80_MiniFASNetV2.onnx",
];
#[cfg(feature = "face-onnx")]
const ANTISPOOF_APPROX_MB: u64 = 2;

/// DeepPixBis PAD: 224×224 RGB input, sigmoid `output_binary` head.
#[cfg(feature = "face-onnx")]
const DEEPPIXBIS_DEFAULT_URL: &str =
    "https://github.com/ffletcherr/face-recognition-liveness/releases/download/v0.1/OULU_Protocol_2_model_0_0.onnx";
#[cfg(feature = "face-onnx")]
const ANTISPOOF_2_APPROX_MB: u64 = 13;

#[cfg(any(feature = "face-onnx", feature = "vision-onnx"))]
fn env_url_override(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

/// What [`download_face_models`] brought onto disk this time.
#[cfg(feature = "face-onnx")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FaceModelsFetched {
    /// Model files present now that were not before.
    pub files: usize,
    /// Their combined size.
    pub bytes: u64,
}

/// Every face model file the download may write: the preferred four, then the buffalo_l pair.
#[cfg(feature = "face-onnx")]
fn face_model_candidates(data_dir: &Path) -> [PathBuf; 6] {
    let (embed, detect, antispoof, antispoof_2) = face_model_paths(data_dir);
    let dir = face_models_dir(data_dir);
    [
        embed,
        detect,
        antispoof,
        antispoof_2,
        dir.join("w600k_r50.onnx"),
        dir.join("scrfd.onnx"),
    ]
}

/// The preferred face models not on disk yet: what [`download_face_models`] will try to fetch.
#[cfg(feature = "face-onnx")]
pub fn missing_face_models(data_dir: &Path) -> Vec<PathBuf> {
    let (embed, detect, antispoof, antispoof_2) = face_model_paths(data_dir);
    [embed, detect, antispoof, antispoof_2]
        .into_iter()
        .filter(|p| !p.exists())
        .collect()
}

/// Fetch missing face models; buffalo_l stands in if the preferred embedder/detector fail.
/// `report` hears each file's progress, for a caller that shows it while it waits.
#[cfg(feature = "face-onnx")]
pub async fn download_face_models(
    data_dir: &Path,
    report: Option<DlProgress>,
) -> Result<FaceModelsFetched> {
    let (embed, detect, antispoof, antispoof_2) = face_model_paths(data_dir);
    let dir = face_models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let candidates = face_model_candidates(data_dir);
    let present_before: Vec<bool> = candidates.iter().map(|p| p.exists()).collect();

    // ── Embedder: Glint-R100 (preferred) ────────────────────────────────────
    if !embed.exists() {
        let url = env_url_override("POND_FACE_EMBEDDING_URL")
            .unwrap_or_else(|| EMBEDDING_DEFAULT_URL.to_string());
        println!("  📥 Glint-R100 embedder not found — downloading from {url}");
        if let Err(e) =
            download_file_reporting(&url, &embed, EMBEDDING_APPROX_MB, report.clone()).await
        {
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
        if let Err(e) =
            download_file_reporting(&url, &detect, DETECTOR_APPROX_MB, report.clone()).await
        {
            println!(
                "  ⚠  SCRFD 34G download failed: {e}\n     \
                 Will fall back to SCRFD 10G from buffalo_l bundle."
            );
        }
    } else {
        println!("  ✅ SCRFD 34G detector already present");
    }

    // ── Buffalo_L fallback for whichever of {embed, detect} is still missing.
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
            match download_file_reporting(url, &antispoof, ANTISPOOF_APPROX_MB, report.clone())
                .await
            {
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
    // With both PADs loaded, the adapter ensembles them via `max(spoof_score)`.
    if !antispoof_2.exists() {
        let url = env_url_override("POND_FACE_ANTISPOOF_2_URL")
            .unwrap_or_else(|| DEEPPIXBIS_DEFAULT_URL.to_string());
        println!("  📥 DeepPixBis secondary PAD not found — downloading from {url}");
        if let Err(e) =
            download_file_reporting(&url, &antispoof_2, ANTISPOOF_2_APPROX_MB, report).await
        {
            println!(
                "  ⚠  DeepPixBis download failed: {e}\n     \
                 Ensemble PAD disabled — Silent-Face V2 still runs alone."
            );
        }
    } else {
        println!("  ✅ DeepPixBis secondary PAD already present");
    }

    let mut fetched = FaceModelsFetched::default();
    for (path, was_there) in candidates.iter().zip(present_before) {
        if was_there {
            continue;
        }
        // Follows the hf_cache symlink to the blob, so this is the model's real size.
        if let Ok(meta) = tokio::fs::metadata(path).await {
            fetched.files += 1;
            fetched.bytes += meta.len();
        }
    }
    Ok(fetched)
}

/// Extract `w600k_r50.onnx` and `det_10g.onnx` from `buffalo_l.zip` to the two dest paths.
#[cfg(feature = "face-onnx")]
async fn fetch_buffalo_l_zip(out_dir: &Path, embed_dest: &Path, detect_dest: &Path) -> Result<()> {
    println!(
        "  ⬇  buffalo_l.zip (~{} MB) — contains both ArcFace R50 + SCRFD 10G",
        BUFFALO_L_APPROX_MB
    );

    let client = pond_net::client_builder()
        .read_timeout(pond_hf_cache::DOWNLOAD_READ_TIMEOUT)
        .build()?;
    // `cfg`-gated senders still need the egress gate: the guard scans source, not the binary.
    let call = pond_core::shared::services::egress::begin_for(
        BUFFALO_L_ZIP_URL,
        "GET",
        pond_core::shared::services::egress::EgressPurpose::Models,
        "",
    )
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

// ── Vision classifier model ──────────────────────────────────────────────────
// YOLOX-Nano (Apache-2.0): labels motion events person/pet/package.

#[cfg(feature = "vision-onnx")]
pub fn vision_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("vision")
}

/// The auto-downloaded classifier; an empty `vision_classifier_model` resolves to it.
#[cfg(feature = "vision-onnx")]
pub const VISION_CLASSIFIER_DEFAULT_FILE: &str = "yolox_nano.onnx";

#[cfg(feature = "vision-onnx")]
const VISION_CLASSIFIER_DEFAULT_URL: &str =
    "https://github.com/Megvii-BaseDetection/YOLOX/releases/download/0.1.1rc0/yolox_nano.onnx";
#[cfg(feature = "vision-onnx")]
const VISION_CLASSIFIER_APPROX_MB: u64 = 4;

/// Fetch the default classifier unless it's on disk; returns its path.
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

    #[test]
    fn the_silero_weights_are_pinned_to_a_commit_not_a_branch() {
        assert_eq!(
            SILERO_REVISION.len(),
            40,
            "expected a full commit sha, got {SILERO_REVISION:?}"
        );
        assert!(SILERO_REVISION.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A malformed URL silently skips the hf_cache path, and only a fresh install would notice.
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

        // No network is mocked: any fetch fails the test.
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

        // Model URLs come from the catalogue, so exercise `download_file` directly.
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

    // ── Network-mode gate on the download path ───────────────────────────────
    // Only `download_file` is tested here; `ensure_espeak_ng_data` (brew-dependent early
    // returns) and `fetch_buffalo_l_zip` (`face-onnx` only) are gated in source, untested.

    static MODE_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores the mode on drop, before its turn is released. `network_mode` is
    /// process-global, so tests that set it take turns; `Offline` is safe for the rest only
    /// because it still permits loopback, which is all sibling tests send to.
    pub(super) struct ModeGuard {
        previous: pond_core::shared::services::egress::NetworkMode,
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl ModeGuard {
        pub(super) fn set(mode: pond_core::shared::services::egress::NetworkMode) -> Self {
            let turn = MODE_TURN.lock().unwrap_or_else(|e| e.into_inner());
            let previous = pond_core::shared::services::egress::network_mode();
            pond_core::shared::services::egress::set_network_mode(mode);
            Self {
                previous,
                _turn: turn,
            }
        }
    }

    impl Drop for ModeGuard {
        fn drop(&mut self) {
            pond_core::shared::services::egress::set_network_mode(self.previous);
        }
    }

    #[tokio::test]
    async fn offline_refuses_a_download_and_says_which_setting_did_it() {
        use pond_core::shared::services::egress::NetworkMode;

        // Serves the file, so the permitted half below is a real download.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".as_slice()))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let _mode = ModeGuard::set(NetworkMode::Offline);

        // ── refused ──────────────────────────────────────────────────────────
        // `.invalid` never resolves, so an ungated fetch fails with a DNS error, not this one.
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
        // Vacuity control: a gate refusing everything would pass the checks above.
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

    #[test]
    fn a_valid_voice_is_fetched_alongside_the_default() {
        let v = voices_to_fetch("bm_george");
        assert_eq!(v, vec![DEFAULT.to_string(), "bm_george".to_string()]);
    }

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

    /// A missing tokenizer or engine stops the rest; a voice that fails does not.
    #[tokio::test]
    async fn every_file_a_fetch_tried_comes_back_with_how_it_ended() {
        // Offline refuses huggingface.co at the first HEAD: each fetch fails, nothing leaves.
        let _mode =
            super::tests::ModeGuard::set(pond_core::shared::services::egress::NetworkMode::Offline);
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = kokoro_dir(tmp.path());
        let weights = pond_adapters_kokoro::model_filename("q8");
        let names =
            |tried: &[FileFetch]| -> Vec<String> { tried.iter().map(|f| f.name.clone()).collect() };

        let tried = ensure_kokoro_engine_reporting(tmp.path(), "q8", "bm_george", None).await;
        assert_eq!(names(&tried), ["tokenizer.json"]);

        std::fs::write(dir.join("tokenizer.json"), "{}").unwrap();
        let tried = ensure_kokoro_engine_reporting(tmp.path(), "q8", "bm_george", None).await;
        assert_eq!(names(&tried), [weights]);

        std::fs::write(dir.join(weights), "").unwrap();
        let tried = ensure_kokoro_engine_reporting(tmp.path(), "q8", "bm_george", None).await;
        assert_eq!(
            names(&tried),
            [format!("{DEFAULT}.bin"), "bm_george.bin".to_string()]
        );

        for f in &tried {
            assert_eq!(f.dest, dir.join("voices").join(&f.name));
            let err = f.result.as_ref().expect_err("offline must refuse it");
            let said = pond_api::download_failure::plain_failure(err);
            assert!(said.contains("network setting"), "{}: {said}", f.name);
        }
    }
}
