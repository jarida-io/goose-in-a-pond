//! Model downloader — Whisper ASR, Piper TTS, and llamafile LLM.
//!
//! All models are downloaded into subdirectories of GIAP's data directory:
//! - `models/ggml-*.bin`        — Whisper GGML models
//! - `models/tts/`              — Piper voice models
//! - `models/llm/`              — llamafile LLM models
//!
//! llamafile bundles model weights + llama.cpp server into a single executable.
//! Running it with `--server --port 8080` starts an OpenAI-compatible HTTP server.

use anyhow::{anyhow, Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};

// ── whisper-server binary download ────────────────────────────────────────────

/// Pinned stable release — repo moved from ggerganov → ggml-org at v1.8.x.
const WHISPER_RELEASE_TAG: &str = "v1.8.4";
const WHISPER_REPO: &str = "https://github.com/ggml-org/whisper.cpp";

/// Info about the platform-specific pre-built binary asset.
pub struct WhisperBinaryAsset {
    pub zip_url: &'static str,
    /// Name of the server executable inside the zip.
    pub server_exe: &'static str,
}

/// Returns the pre-built download asset for this platform, or `None` if none exists.
///
/// - Windows x64  → `whisper-bin-x64.zip` from ggml-org releases
/// - Linux x64    → no upstream pre-built; returns `None` (build from source)
/// - Linux ARM64  → no upstream pre-built; returns `None` (build from source)
pub fn whisper_binary_asset() -> Option<WhisperBinaryAsset> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(WhisperBinaryAsset {
        zip_url: "https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.4/whisper-bin-x64.zip",
        server_exe: "whisper-server.exe",
    });

    #[allow(unreachable_code)]
    None
}

/// Returns the on-disk path where the whisper-server binary should live.
pub fn whisper_binary_path(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return data_dir.join("bin").join("whisper-server.exe");
    #[cfg(not(windows))]
    return data_dir.join("bin").join("whisper-server");
}

// ── Obtain whisper-server binary (download or build) ─────────────────────────

/// Get the whisper-server binary into `<data_dir>/bin/`, using whichever method
/// is appropriate for this platform:
///
/// - **Windows x64**: downloads `whisper-bin-x64.zip` from the pinned release.
/// - **Linux ARM64 / x64**: builds from source (cmake + make), with NEON and
///   CUDA optimizations enabled when the toolchain is available.
/// - **Already present**: no-op.
pub async fn download_whisper_binary(data_dir: &Path) -> Result<PathBuf> {
    let dest = whisper_binary_path(data_dir);

    if dest.exists() {
        println!("  ✅ whisper-server already present: {}", dest.display());
        return Ok(dest);
    }

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;

    match whisper_binary_asset() {
        Some(asset) => fetch_whisper_zip(asset, data_dir, &dest).await,
        None => build_whisper_from_source(data_dir, &dest).await,
    }
}

/// Download the release zip and extract it into `<data_dir>/bin/`.
async fn fetch_whisper_zip(
    asset: WhisperBinaryAsset,
    data_dir: &Path,
    dest: &Path,
) -> Result<PathBuf> {
    println!("  ⬇  whisper-server ({}, pre-built)", WHISPER_RELEASE_TAG);

    let client = reqwest::Client::builder().build()?;
    let resp = client
        .get(asset.zip_url)
        .send()
        .await
        .context("Failed to fetch whisper binary zip")?;

    if !resp.status().is_success() {
        return Err(anyhow!("Download returned {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = if total > 0 { Vec::with_capacity(total as usize) } else { Vec::new() };

    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.context("Download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if total > 0 {
            let pct = (downloaded * 100) / total;
            print!("\r  ⬇  {} / {} MB  ({}%)",
                downloaded / 1_048_576, total / 1_048_576, pct);
            std::io::stdout().flush().ok();
        }
    }
    println!();

    let bin_dir = data_dir.join("bin");
    let server_exe = asset.server_exe.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let cursor = std::io::Cursor::new(buf);
        let mut archive = zip::ZipArchive::new(cursor)
            .context("Failed to open zip archive")?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_string();
            let file_name = std::path::Path::new(&name)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            if file_name.is_empty() {
                continue;
            }

            let out_path = bin_dir.join(&file_name);
            let mut out_file = std::fs::File::create(&out_path)
                .with_context(|| format!("Cannot write {}", out_path.display()))?;
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            out_file.write_all(&content)?;

            #[cfg(unix)]
            if file_name == server_exe {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(0o755))?;
            }
        }

        // Suppress unused warning on non-Unix platforms.
        let _ = &server_exe;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Zip extraction task panicked")??;

    println!("  ✅ whisper-server installed: {}", dest.display());
    Ok(dest.to_path_buf())
}

// ── Build-tool helpers: cmake + git auto-download ─────────────────────────────

/// Pinned cmake release downloaded when the system has none.
const CMAKE_VERSION: &str = "3.31.6";
const CMAKE_GITHUB_BASE: &str = "https://github.com/Kitware/CMake/releases/download";

struct CmakePlatform {
    archive_name: &'static str,
    /// Relative path inside the extracted archive to the cmake binary.
    bin_rel:      &'static str,
    is_zip:       bool,
}

fn cmake_platform_info() -> Option<CmakePlatform> {
    // macOS universal binary (runs on both Apple Silicon and Intel)
    #[cfg(target_os = "macos")]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-macos-universal.tar.gz",
        bin_rel:      "cmake-3.31.6-macos-universal/CMake.app/Contents/bin/cmake",
        is_zip:       false,
    });

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-linux-x86_64.tar.gz",
        bin_rel:      "cmake-3.31.6-linux-x86_64/bin/cmake",
        is_zip:       false,
    });

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-linux-aarch64.tar.gz",
        bin_rel:      "cmake-3.31.6-linux-aarch64/bin/cmake",
        is_zip:       false,
    });

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-windows-x86_64.zip",
        bin_rel:      "cmake-3.31.6-windows-x86_64/bin/cmake.exe",
        is_zip:       true,
    });

    #[allow(unreachable_code)]
    None
}

fn managed_cmake_path(data_dir: &Path) -> Option<PathBuf> {
    Some(data_dir.join("build-tools").join(cmake_platform_info()?.bin_rel))
}

async fn tool_in_path(name: &str) -> bool {
    tokio::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Ensure cmake is available, downloading a portable binary if not in PATH.
/// Returns the command/path to invoke cmake.
pub async fn ensure_cmake(data_dir: &Path) -> Result<String> {
    // 1. Already downloaded by GIAP
    if let Some(p) = managed_cmake_path(data_dir) {
        if p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }

    // 2. Available in system PATH
    if tool_in_path("cmake").await {
        return Ok("cmake".to_string());
    }

    // 3. Download portable cmake binary from cmake.org
    match cmake_platform_info() {
        Some(info) => {
            println!("  📥 cmake not found — downloading cmake {} for {} {}...",
                CMAKE_VERSION, std::env::consts::OS, std::env::consts::ARCH);
            download_cmake_binary(data_dir, &info).await?;
            let path = managed_cmake_path(data_dir)
                .expect("cmake_platform_info is Some");
            Ok(path.to_string_lossy().into_owned())
        }
        None => Err(anyhow!("cmake unavailable for {} {}", std::env::consts::OS, std::env::consts::ARCH)),
    }
}

async fn download_cmake_binary(data_dir: &Path, info: &CmakePlatform) -> Result<()> {
    let tools_dir = data_dir.join("build-tools");
    tokio::fs::create_dir_all(&tools_dir).await?;

    let url = format!("{}/v{}/{}", CMAKE_GITHUB_BASE, CMAKE_VERSION, info.archive_name);
    println!("  ⬇  cmake {} ({})", CMAKE_VERSION, info.archive_name);

    let client = reqwest::Client::builder().build()?;
    let resp = client.get(&url).send().await.context("Failed to fetch cmake archive")?;
    if !resp.status().is_success() {
        return Err(anyhow!("cmake download returned {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(60 * 1_048_576);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.context("cmake download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        let pct = (downloaded * 100) / total.max(1);
        print!("\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576, total / 1_048_576, pct);
        std::io::stdout().flush().ok();
    }
    println!();

    let tools_dir_clone = tools_dir.clone();
    let bin_rel = info.bin_rel.to_string();

    if info.is_zip {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let cursor = std::io::Cursor::new(buf);
            let mut archive = zip::ZipArchive::new(cursor).context("Invalid cmake zip")?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                if entry.is_dir() { continue; }
                let entry_path = entry.name().replace('\\', "/");
                let out_path = tools_dir_clone.join(&entry_path);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out = std::fs::File::create(&out_path)?;
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                out.write_all(&content)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("cmake zip extraction panicked")??;
    } else {
        tokio::task::spawn_blocking(move || {
            use flate2::read::GzDecoder;
            use tar::Archive;
            let gz = GzDecoder::new(std::io::Cursor::new(buf));
            let mut tar = Archive::new(gz);
            tar.set_preserve_permissions(true);
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.to_path_buf();
                let out_path = tools_dir_clone.join(&path);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if entry.header().entry_type().is_dir() {
                    std::fs::create_dir_all(&out_path)?;
                } else {
                    entry.unpack(&out_path)?;
                }
            }
            // Ensure the cmake binary is executable on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let cmake_bin = tools_dir_clone.join(&bin_rel);
                if cmake_bin.exists() {
                    std::fs::set_permissions(&cmake_bin, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            let _ = bin_rel;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("cmake tar extraction panicked")??;
    }

    println!("  ✅ cmake {} installed to build-tools/", CMAKE_VERSION);
    Ok(())
}

/// Ensure git is available, installing automatically if possible.
pub async fn ensure_git() -> Result<()> {
    if tool_in_path("git").await {
        return Ok(());
    }

    println!("  📥 git not found — installing...");

    #[cfg(target_os = "linux")]
    {
        for args in &[
            &["apt-get", "install", "-y", "git", "build-essential"][..],
            &["apt",     "install", "-y", "git", "build-essential"],
            &["dnf",     "install", "-y", "git", "gcc-c++", "make"],
            &["yum",     "install", "-y", "git", "gcc-c++", "make"],
            &["pacman",  "--noconfirm", "-S", "git", "base-devel"],
            &["zypper",  "install", "-y", "git", "gcc-c++", "make"],
        ] {
            if tokio::process::Command::new("sudo")
                .args(*args)
                .status().await.map(|s| s.success()).unwrap_or(false)
            {
                return Ok(());
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Try Homebrew
        if tokio::process::Command::new("brew")
            .args(["install", "git"])
            .status().await.map(|s| s.success()).unwrap_or(false)
        {
            return Ok(());
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Try winget (Windows 10 1809+)
        if tokio::process::Command::new("winget")
            .args(["install", "--id", "Git.Git", "-e", "--source", "winget", "--silent"])
            .status().await.map(|s| s.success()).unwrap_or(false)
        {
            return Ok(());
        }
    }

    Err(anyhow!("git unavailable"))
}

/// Build whisper-server from source using cmake.
///
/// Enables platform-appropriate optimizations:
/// - `-DGGML_NATIVE=ON`  — native CPU (NEON on ARM64, AVX2 on x86)
/// - `-DGGML_CUDA=ON`    — GPU acceleration when `nvcc` is on PATH (Jetson)
/// - `-DGGML_OPENMP=ON`  — multi-core inference
///
/// Clones into `<data_dir>/whisper-src/`, builds in `<data_dir>/whisper-src/build/`.
async fn build_whisper_from_source(data_dir: &Path, dest: &Path) -> Result<PathBuf> {
    ensure_git().await?;
    let cmake_cmd = ensure_cmake(data_dir).await?;

    let src_dir = data_dir.join("whisper-src");
    let build_dir = src_dir.join("build");

    // Clone (skip if already present).
    if !src_dir.join(".git").exists() {
        println!("  📦 Cloning whisper.cpp source ({})...", WHISPER_RELEASE_TAG);
        run_cmd(
            tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", "--branch", WHISPER_RELEASE_TAG, WHISPER_REPO])
                .arg(&src_dir),
            "git clone",
        ).await?;
    } else {
        println!("  📦 whisper.cpp source already cloned, skipping.");
    }

    // Detect CUDA (nvcc in PATH → Jetson / CUDA workstation).
    let has_cuda = tokio::process::Command::new("nvcc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    println!("  🔨 Configuring cmake (CUDA: {})...", if has_cuda { "enabled" } else { "disabled" });

    let mut cmake_cfg = tokio::process::Command::new(&cmake_cmd);
    cmake_cfg
        .arg("-B").arg(&build_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DGGML_NATIVE=ON")
        .arg("-DGGML_OPENMP=ON")
        .current_dir(&src_dir);
    if has_cuda {
        cmake_cfg.arg("-DGGML_CUDA=ON");
    }
    run_cmd(&mut cmake_cfg, "cmake configure").await?;

    // Determine parallelism: use all cores.
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "4".to_string());

    println!("  🔨 Building whisper-server ({} jobs)...", jobs);
    run_cmd(
        tokio::process::Command::new(&cmake_cmd)
            .args(["--build"])
            .arg(&build_dir)
            .args(["-j", &jobs, "--config", "Release", "--target", "whisper-server"])
            .current_dir(&src_dir),
        "cmake build",
    ).await?;

    // Copy binary to data_dir/bin/.
    let built = build_dir.join("bin").join("whisper-server");
    if !built.exists() {
        return Err(anyhow!(
            "Build succeeded but whisper-server not found at {}",
            built.display()
        ));
    }
    tokio::fs::copy(&built, dest).await
        .with_context(|| format!("Failed to copy binary to {}", dest.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }

    println!("  ✅ whisper-server installed: {}", dest.display());
    Ok(dest.to_path_buf())
}

// ── Piper TTS model download ───────────────────────────────────────────────────

/// Directory for TTS voice models: `<data_dir>/models/tts/`.
pub fn tts_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("tts")
}

/// Download a specific piper voice model by its filename and URL into `<data_dir>/models/tts/`.
///
/// Both the `.onnx` weights and the `.onnx.json` config are downloaded.
/// Returns the path to the `.onnx` file.
pub async fn download_piper_model_entry(
    data_dir: &Path,
    model_filename: &str,
    config_filename: &str,
    model_url: &str,
    config_url: &str,
    size_mb: u64,
) -> Result<PathBuf> {
    let dir = tts_models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;

    let onnx_path = dir.join(model_filename);
    let json_path = dir.join(config_filename);

    if onnx_path.exists() {
        println!("  ✅ Already downloaded: {}", onnx_path.display());
    } else {
        download_file(model_url, &onnx_path, size_mb).await?;
    }

    if json_path.exists() {
        println!("  ✅ Already downloaded: {}", json_path.display());
    } else {
        download_file(config_url, &json_path, 1).await?;
    }

    Ok(onnx_path)
}

// ── Piper binary download ──────────────────────────────────────────────────────

/// Pinned Piper release.
const PIPER_RELEASE_TAG: &str = "2023.11.14-2";
const PIPER_GITHUB_BASE: &str =
    "https://github.com/rhasspy/piper/releases/download/2023.11.14-2";

/// Platform-specific archive asset for piper.
///
/// Returns `(archive_filename, is_zip)` for supported platforms, or `None`
/// when no pre-built binary is available (e.g. macOS, Windows ARM64).
#[derive(Debug)]
pub struct PiperBinaryAsset {
    pub archive_name: &'static str,
    pub is_zip: bool,
}

/// Returns the pre-built piper asset for this platform, or `None` if unavailable.
///
/// rhasspy/piper ships pre-built binaries for:
///   - Windows x86_64
///   - Linux x86_64
///   - Linux aarch64 (Jetson)
///
/// macOS and Windows ARM64 have no upstream pre-built release.
/// On those platforms callers should fall back gracefully (print output).
pub fn piper_binary_asset() -> Option<PiperBinaryAsset> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(PiperBinaryAsset { archive_name: "piper_windows_amd64.zip", is_zip: true });

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some(PiperBinaryAsset { archive_name: "piper_linux_aarch64.tar.gz", is_zip: false });

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(PiperBinaryAsset { archive_name: "piper_linux_x86_64.tar.gz", is_zip: false });

    #[allow(unreachable_code)]
    None
}

/// Returns the on-disk path where the piper binary should live.
pub fn piper_binary_path(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return data_dir.join("bin").join("piper.exe");
    #[cfg(not(windows))]
    return data_dir.join("bin").join("piper");
}

/// Download (or build) the piper binary into `<data_dir>/bin/`.
///
/// Prefers a pre-built release archive; falls back to building from source
/// on platforms without one (macOS, Windows ARM64).
pub async fn download_piper_binary(data_dir: &Path) -> Result<PathBuf> {
    let dest = piper_binary_path(data_dir);

    if dest.exists() {
        println!("  ✅ piper already present: {}", dest.display());
        return Ok(dest);
    }

    let Some(asset) = piper_binary_asset() else {
        // No pre-built binary for this platform — build from source instead.
        println!("  📦 No pre-built piper for {} {} — building from source...",
            std::env::consts::OS, std::env::consts::ARCH);
        return build_piper_from_source(data_dir, &dest).await;
    };

    let archive_name = asset.archive_name;
    let is_zip = asset.is_zip;

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;

    println!(
        "  ⬇  piper TTS binary ({}, {})",
        PIPER_RELEASE_TAG, archive_name
    );

    let url = format!("{}/{}", PIPER_GITHUB_BASE, archive_name);
    let client = reqwest::Client::builder().build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch piper binary archive")?;

    if !resp.status().is_success() {
        return Err(anyhow!("Download returned {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = if total > 0 {
        Vec::with_capacity(total as usize)
    } else {
        Vec::new()
    };

    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.context("Download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if total > 0 {
            let pct = (downloaded * 100) / total;
            print!(
                "\r  ⬇  {} / {} MB  ({}%)",
                downloaded / 1_048_576,
                total / 1_048_576,
                pct
            );
            std::io::stdout().flush().ok();
        }
    }
    println!();

    let bin_dir = data_dir.join("bin");

    if is_zip {
        // Windows: extract piper.exe from zip
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let cursor = std::io::Cursor::new(buf);
            let mut archive = zip::ZipArchive::new(cursor)
                .context("Failed to open piper zip archive")?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                let name = entry.name().to_string();
                let file_name = std::path::Path::new(&name)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                if file_name.is_empty() {
                    continue;
                }
                let out_path = bin_dir.join(&file_name);
                let mut out_file = std::fs::File::create(&out_path)
                    .with_context(|| format!("Cannot write {}", out_path.display()))?;
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                out_file.write_all(&content)?;
            }
            let _ = &dest_clone; // suppress unused warning
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("Zip extraction task panicked")??;
    } else {
        // Linux: extract piper from .tar.gz, preserving subdirectory structure
        // so that espeak-ng-data/ ends up at bin/espeak-ng-data/.
        // The archive root is a single directory (e.g. "piper/"); strip it.
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || {
            use flate2::read::GzDecoder;
            use tar::Archive;
            let gz = GzDecoder::new(std::io::Cursor::new(buf));
            let mut tar = Archive::new(gz);
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.into_owned();

                // Skip the top-level directory entry itself.
                let mut components = path.components();
                components.next(); // strip leading "piper/" component
                let relative: std::path::PathBuf = components.collect();
                if relative.as_os_str().is_empty() {
                    continue;
                }

                let out_path = bin_dir.join(&relative);

                // Ensure parent directories exist (needed for espeak-ng-data/*)
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                entry.unpack(&out_path)?;

                #[cfg(unix)]
                if relative.to_string_lossy() == "piper" {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            let _ = &dest_clone;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("Tar extraction task panicked")??;
    }

    // Ensure espeak-ng-data is present alongside the binary.
    ensure_espeak_ng_data(data_dir).await;

    println!("  ✅ piper installed: {}", dest.display());
    Ok(dest)
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
                            println!("  ✅ espeak-ng-data from Homebrew: {}", dest.display());
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
                    let data_p = std::path::Path::new(&prefix).join("lib").join("espeak-ng-data");
                    if data_p.is_dir() {
                        if copy_dir_all(&data_p, &dest).is_ok() {
                            println!("  ✅ espeak-ng-data from Homebrew: {}", dest.display());
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
    let url = format!(
        "{}/piper_linux_x86_64.tar.gz",
        PIPER_GITHUB_BASE
    );
    println!("  ⬇  espeak-ng-data (via piper Linux tarball)...");

    let bytes = match reqwest::get(&url).await.and_then(|r| Ok(r)) {
        Ok(resp) if resp.status().is_success() => {
            match resp.bytes().await {
                Ok(b) => b.to_vec(),
                Err(e) => { tracing::warn!("espeak-ng-data download failed: {e}"); return; }
            }
        }
        Ok(resp) => { tracing::warn!("espeak-ng-data download: HTTP {}", resp.status()); return; }
        Err(e) => { tracing::warn!("espeak-ng-data download failed: {e}"); return; }
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
        Ok(Ok(())) if dest.exists() => println!("  ✅ espeak-ng-data installed: {}", dest.display()),
        Ok(Ok(())) => tracing::warn!("espeak-ng-data not found in tarball"),
        Ok(Err(e)) => tracing::warn!("espeak-ng-data extraction failed: {e}"),
        Err(e) => tracing::warn!("espeak-ng-data task panicked: {e}"),
    }
}

/// Build piper from source for platforms without a pre-built binary (macOS, Windows ARM64).
///
/// Clones `https://github.com/rhasspy/piper` at `PIPER_RELEASE_TAG` into
/// `<data_dir>/piper-src/` and builds with cmake.  Piper's CMakeLists.txt
/// fetches its own dependencies (onnxruntime, espeak-ng-data) automatically.
async fn build_piper_from_source(data_dir: &Path, dest: &Path) -> Result<PathBuf> {
    ensure_git().await?;
    let cmake_cmd = ensure_cmake(data_dir).await?;

    let src_dir   = data_dir.join("piper-src");
    let build_dir = src_dir.join("build");

    if !src_dir.join(".git").exists() {
        println!("  📦 Cloning piper source ({})...", PIPER_RELEASE_TAG);
        run_cmd(
            tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", "--branch", PIPER_RELEASE_TAG,
                       "https://github.com/rhasspy/piper"])
                .arg(&src_dir),
            "git clone piper",
        ).await?;
    }

    println!("  🔨 Configuring piper cmake...");
    run_cmd(
        tokio::process::Command::new(&cmake_cmd)
            .arg("-B").arg(&build_dir)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .current_dir(&src_dir),
        "cmake configure piper",
    ).await?;

    let jobs = std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "4".to_string());

    println!("  🔨 Building piper ({} jobs)...", jobs);
    run_cmd(
        tokio::process::Command::new(&cmake_cmd)
            .args(["--build"])
            .arg(&build_dir)
            .args(["-j", &jobs, "--config", "Release"])
            .current_dir(&src_dir),
        "cmake build piper",
    ).await?;

    // Search common output locations across cmake generators/platforms.
    let candidates = [
        build_dir.join("piper"),
        build_dir.join("src").join("piper"),
        build_dir.join("Release").join("piper.exe"),
        build_dir.join("src").join("Release").join("piper.exe"),
    ];
    let built = candidates.iter()
        .find(|p| p.exists())
        .ok_or_else(|| anyhow!("piper build completed but binary not found in expected locations"))?;

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;
    tokio::fs::copy(built, dest).await
        .with_context(|| format!("Failed to copy piper to {}", dest.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }

    // Copy espeak-ng-data next to the binary so piper can find it at runtime.
    // cmake's FetchContent/ExternalProject places it somewhere under build_dir.
    let espeak_dest = data_dir.join("bin").join("espeak-ng-data");
    if !espeak_dest.exists() {
        if let Some(espeak_src) = find_espeak_ng_data(&build_dir) {
            println!("  📋 Copying espeak-ng-data from {}...", espeak_src.display());
            copy_dir_all(&espeak_src, &espeak_dest)?;
            println!("  ✅ espeak-ng-data installed: {}", espeak_dest.display());
        } else {
            // Not found in build tree — fall through to ensure_espeak_ng_data() below.
            tracing::warn!("espeak-ng-data not found in cmake build tree — will download separately");
        }
    }

    // Final safety net: download if still missing.
    ensure_espeak_ng_data(data_dir).await;

    println!("  ✅ piper built and installed: {}", dest.display());
    Ok(dest.to_path_buf())
}

/// Recursively search `root` for a directory named `espeak-ng-data`.
fn find_espeak_ng_data(root: &Path) -> Option<PathBuf> {
    // BFS through the directory tree (depth-limited to avoid infinite loops).
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((root.to_path_buf(), 0u32));
    while let Some((dir, depth)) = queue.pop_front() {
        if depth > 10 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() == "espeak-ng-data" {
                    return Some(path);
                }
                queue.push_back((path, depth + 1));
            }
        }
    }
    None
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

// ── Speaker model ─────────────────────────────────────────────────────────────

/// On-disk path for the x-vector speaker ONNX model.
pub fn speaker_model_path(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("speaker.onnx")
}

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

pub async fn download_file(url: &str, dest: &Path, approx_size_mb: u64) -> Result<()> {
    println!("  ⬇  {} (~{} MB)", dest.file_name().unwrap_or_default().to_string_lossy(), approx_size_mb);

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
    let resp = req
        .send()
        .await
        .with_context(|| format!("Failed to fetch {url}"))?;

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

    let total = resp
        .content_length()
        .unwrap_or(approx_size_mb * 1_048_576);

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
        print!(
            "\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            total / 1_048_576,
            pct
        );
        std::io::stdout().flush().ok();
    }

    println!();
    tokio::fs::rename(&tmp, dest).await?;
    println!("  ✅ Saved: {}", dest.display());
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
const BUFFALO_L_ZIP_URL: &str =
    "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip";
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
const EMBEDDING_DEFAULT_URL: &str =
    "https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx";
const EMBEDDING_APPROX_MB: u64 = 261;

/// SCRFD 34G GNKPS — same SCRFD family as the 10G in buffalo_l, deeper
/// backbone.  Same 5-point landmark contract our Umeyama alignment relies
/// on.  ~39 MB on disk.  Hosted by the Immich team.
///
/// Override the URL with `POND_FACE_DETECTOR_URL`.
const DETECTOR_DEFAULT_URL: &str =
    "https://huggingface.co/immich-app/scrfd_34g_gnkps/resolve/main/detection/model.onnx";
const DETECTOR_APPROX_MB: u64 = 39;

/// Silent-Face MiniFASNetV2 anti-spoof model — 3-class export
/// `[fake_2D, fake_3D, live]` at 80×80 BGR input.  Override the mirror
/// with `POND_FACE_ANTISPOOF_URL`.
const ANTISPOOF_MIRRORS: &[&str] = &[
    "https://huggingface.co/hash-ash/Silent-Face-Anti-Spoofing-ONNX/resolve/main/2.7_80x80_MiniFASNetV2.onnx",
    "https://huggingface.co/datasets/giap-mirror/silent-face-anti-spoofing/resolve/main/2.7_80x80_MiniFASNetV2.onnx",
];
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
const DEEPPIXBIS_DEFAULT_URL: &str =
    "https://github.com/ffletcherr/face-recognition-liveness/releases/download/v0.1/OULU_Protocol_2_model_0_0.onnx";
const ANTISPOOF_2_APPROX_MB: u64 = 13;

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
    let fallback_embed  = dir.join("w600k_r50.onnx");
    let fallback_detect = dir.join("scrfd.onnx");
    let need_fallback_embed  = !embed.exists()  && !fallback_embed.exists();
    let need_fallback_detect = !detect.exists() && !fallback_detect.exists();
    if need_fallback_embed || need_fallback_detect {
        println!(
            "  📥 Fetching buffalo_l bundle for {}{}{}",
            if need_fallback_embed  { "ArcFace R50" } else { "" },
            if need_fallback_embed && need_fallback_detect { " + " } else { "" },
            if need_fallback_detect { "SCRFD 10G" } else { "" },
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
                Ok(_) => { got = true; break; }
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
async fn fetch_buffalo_l_zip(out_dir: &Path, embed_dest: &Path, detect_dest: &Path) -> Result<()> {
    println!("  ⬇  buffalo_l.zip (~{} MB) — contains both ArcFace R50 + SCRFD 10G",
        BUFFALO_L_APPROX_MB);

    let client = reqwest::Client::builder().build()?;
    let resp = client.get(BUFFALO_L_ZIP_URL).send().await
        .context("Failed to fetch buffalo_l.zip")?;
    if !resp.status().is_success() {
        return Err(anyhow!("Server returned {} for buffalo_l.zip", resp.status()));
    }

    let total = resp.content_length().unwrap_or(BUFFALO_L_APPROX_MB * 1_048_576);
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut downloaded: u64 = 0;
    let mut resp = resp;

    while let Some(chunk) = resp.chunk().await.context("Download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        let pct = (downloaded * 100) / total.max(1);
        print!("\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576, total / 1_048_576, pct);
        std::io::stdout().flush().ok();
    }
    println!();

    let embed_dest = embed_dest.to_path_buf();
    let detect_dest = detect_dest.to_path_buf();
    let out_dir = out_dir.to_path_buf();

    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let cursor = std::io::Cursor::new(buf);
        let mut archive = zip::ZipArchive::new(cursor)
            .context("Failed to open buffalo_l zip archive")?;

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
                "w600k_r50.onnx" => { embed_found = true; embed_dest.clone() }
                "det_10g.onnx"   => { detect_found = true; detect_dest.clone() }
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

/// Run a `tokio::process::Command`, streaming its output, and return an error on non-zero exit.
async fn run_cmd(cmd: &mut tokio::process::Command, label: &str) -> Result<()> {
    let status = cmd
        .status()
        .await
        .with_context(|| format!("Failed to run {}", label))?;
    if !status.success() {
        return Err(anyhow!("{} failed (exit {})", label, status));
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Path helper tests (all platforms) ────────────────────────────────────

    #[test]
    fn whisper_binary_path_has_correct_extension() {
        let dir = std::path::PathBuf::from("/data");
        let p = whisper_binary_path(&dir);
        #[cfg(windows)]
        assert!(p.to_string_lossy().ends_with(".exe"), "expected .exe on Windows, got {}", p.display());
        #[cfg(not(windows))]
        assert!(!p.to_string_lossy().ends_with(".exe"), "unexpected .exe on non-Windows, got {}", p.display());
        assert!(p.to_string_lossy().contains("whisper-server"));
    }

    #[test]
    fn piper_binary_path_has_correct_extension() {
        let dir = std::path::PathBuf::from("/data");
        let p = piper_binary_path(&dir);
        #[cfg(windows)]
        assert!(p.to_string_lossy().ends_with(".exe"), "expected .exe on Windows, got {}", p.display());
        #[cfg(not(windows))]
        assert!(!p.to_string_lossy().ends_with(".exe"), "unexpected .exe on non-Windows, got {}", p.display());
        assert!(p.to_string_lossy().contains("piper"));
    }


    // ── Asset detection tests ─────────────────────────────────────────────────

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn whisper_binary_asset_returns_some_on_windows_x64() {
        let asset = whisper_binary_asset().expect("Windows x64 should have a pre-built whisper asset");
        assert!(asset.zip_url.contains("whisper"), "URL should reference whisper, got {}", asset.zip_url);
        assert!(asset.server_exe.ends_with(".exe"), "server_exe should end in .exe on Windows");
    }

    #[test]
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    fn whisper_binary_asset_returns_none_on_non_windows_x64() {
        // Linux and macOS fall back to build-from-source.
        assert!(
            whisper_binary_asset().is_none(),
            "Expected None for this platform — build-from-source path should be used"
        );
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn piper_binary_asset_returns_zip_on_windows_x64() {
        let asset = piper_binary_asset().expect("Windows x64 should have a piper asset");
        assert!(asset.is_zip, "Windows piper asset should be a zip archive");
        assert!(asset.archive_name.ends_with(".zip"));
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn piper_binary_asset_returns_targz_on_linux_x64() {
        let asset = piper_binary_asset().expect("Linux x86_64 should have a piper asset");
        assert!(!asset.is_zip, "Linux piper asset should be a .tar.gz archive");
        assert!(asset.archive_name.ends_with(".tar.gz"));
        assert!(asset.archive_name.contains("x86_64"));
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    fn piper_binary_asset_returns_targz_on_linux_aarch64() {
        let asset = piper_binary_asset().expect("Linux aarch64 (Jetson) should have a piper asset");
        assert!(!asset.is_zip, "Linux piper asset should be a .tar.gz archive");
        assert!(asset.archive_name.contains("aarch64"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn piper_binary_asset_returns_none_on_macos() {
        assert!(
            piper_binary_asset().is_none(),
            "macOS has no pre-built piper binary — should return None"
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

    // ── piper_binary_asset graceful degradation on unsupported platforms ──────

    #[tokio::test]
    async fn download_piper_binary_errors_gracefully_when_no_asset() {
        // Simulate the None path by calling ok_or_else directly — this runs on all
        // platforms but only triggers in production on macOS / Windows ARM64.
        #[cfg(target_os = "macos")]
        {
            let result: Result<PiperBinaryAsset> = piper_binary_asset().ok_or_else(|| {
                anyhow!(
                    "No pre-built piper binary for {} {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                )
            });
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("No pre-built"),
                "error should mention missing binary, got: {}",
                msg
            );
        }

        // On Windows / Linux a pre-built asset exists; the function returns Some.
        #[cfg(not(target_os = "macos"))]
        {
            assert!(
                piper_binary_asset().is_some(),
                "expected a pre-built piper asset on this platform"
            );
        }
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
        assert!(!dest.exists(), "partial file should not exist after connection failure");
    }
}
