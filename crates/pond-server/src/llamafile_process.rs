//! Auto-start helper for the llamafile LLM server.
//!
//! llamafile bundles model weights + llama.cpp into a single executable.
//! Running it with `--server --port 8080` starts an OpenAI-compatible HTTP
//! server at `http://127.0.0.1:8080/v1/chat/completions`.
//!
//! Flow (called by `run_server`):
//!   1. If something is already answering on port 8080, do nothing.
//!   2. Find the first downloaded llamafile model in `<data_dir>/models/llm/`.
//!   3. Spawn it as a background process.
//!   4. Return a `LlamafileProcess` guard that kills it on drop.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Child;

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Holds the spawned llamafile child process.  Kills it on drop.
pub struct LlamafileProcess(Child);

impl Drop for LlamafileProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if something is already serving on `port`.
pub async fn is_running(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}", port);
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

// ── Binary lookup ─────────────────────────────────────────────────────────────

/// Find the first downloaded llamafile model in `<data_dir>/models/llm/`.
///
/// Scans the directory for any `.llamafile` (or `.llamafile.exe` on Windows) file.
pub fn find_model(data_dir: &Path) -> Option<PathBuf> {
    let llm_dir = data_dir.join("models").join("llm");
    let entries = std::fs::read_dir(&llm_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let is_llamafile = name.ends_with(".llamafile") || name.ends_with(".llamafile.exe");
        if is_llamafile {
            return Some(path);
        }
    }
    None
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Returns true if the llamafile binary supports the `--jinja` flag.
///
/// Older builds (≤ v0.9.0 / build ~1500) do not have this flag and will crash
/// if it is passed.  Running `--help` and checking the output is safe and fast.
fn supports_jinja(binary: &Path) -> bool {
    std::process::Command::new(binary)
        .arg("--help")
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout).to_string()
                + String::from_utf8_lossy(&o.stderr).as_ref();
            out.contains("--jinja")
        })
        .unwrap_or(false)
}

/// Spawn the llamafile server and wait up to 60 s for it to become ready.
///
/// Flags:
///   `--server`        — HTTP server mode (serves `/v1/chat/completions`)
///   `--port <port>`   — listen port
///   `--host 127.0.0.1` — loopback only (local privacy, matches GIAP policy)
///   `--nobrowser`     — don't open a browser tab
///   `--jinja`         — Jinja2 chat templates (only passed on compatible builds)
///
/// Loading a 1–2 GB model typically takes 5–30 seconds depending on hardware.
async fn spawn(binary: &Path, port: u16) -> Result<LlamafileProcess> {
    let jinja = supports_jinja(binary);
    if jinja {
        println!("  ✅ llamafile supports --jinja — Jinja2 chat templates enabled");
    } else {
        println!("  ⚠  llamafile does not support --jinja (old build) — skipping flag; upgrade for better chat template support");
    }

    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--server")
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .arg("--nobrowser")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if jinja {
        cmd.arg("--jinja");
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn {}", binary.display()))?;

    let proc = LlamafileProcess(child);

    for attempt in 1..=60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running(port).await {
            return Ok(proc);
        }
        if attempt == 15 {
            println!("  ⏳ Still loading LLM model — this can take up to 30 s on first launch...");
        }
    }

    // Model still loading after 60 s — return the guard anyway.
    // The first chat request will either succeed (if it finishes loading) or
    // the FallbackProvider will catch the error.
    println!("  ⚠  LLM did not respond within 60 s — it may still be loading in the background.");
    Ok(proc)
}

// ── High-level entry point ────────────────────────────────────────────────────

/// Base URL for llamafile given a port.
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Check → find → spawn.  Never returns an error — failures are printed as warnings.
///
/// The base port is [`crate::ports::LLAMAFILE`].  If busy, the next port in
/// arithmetic sequence is tried automatically.
///
/// When `model_name` is provided, uses `ModelService::ensure_downloaded()` to
/// autonomously download the model if it's not yet on disk.  Falls back to
/// `find_model()` to locate any previously-downloaded model.
///
/// Returns `Some((guard, port))` with the actual port the process was started
/// on, or `None` if already running (port = base) or no model was found.
pub async fn try_start(
    data_dir: &Path,
    model_service: std::sync::Arc<pond_core::models::services::model_service::ModelService>,
    model_name: Option<&str>,
) -> Option<(LlamafileProcess, u16)> {
    let base_port = crate::ports::LLAMAFILE;

    if is_running(base_port).await {
        println!("  🧠 LLM already running at {}", url_for(base_port));
        return None;
    }

    let port = match crate::ports::find_free_port(base_port).await {
        Some(p) => p,
        None => {
            println!("  ⚠  No free port found near {} for llamafile", base_port);
            return None;
        }
    };

    // Try autonomous download via ModelService.
    // When the name is empty/None, try to resolve from the assigned chat role
    // or the first available llamafile model in the catalog.
    let preferred_model = {
        use pond_core::models::domain::model_record::ModelCategory;

        let resolved_name: Option<String> = match model_name {
            Some(name) if !name.is_empty() => Some(name.to_string()),
            _ => {
                // Try role assignment first
                if let Ok(Some(record)) = model_service.model_for_role("chat").await {
                    if record.category == ModelCategory::Llamafile {
                        Some(record.name.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
                .or_else(|| {
                    // Fall back to first available llamafile from catalog (blocking is fine at startup)
                    None
                })
            }
        };

        // If we still don't have a name, try listing llamafile models from DB
        let resolved_name = match resolved_name {
            Some(n) => Some(n),
            None => {
                match model_service
                    .list_by_category(&ModelCategory::Llamafile)
                    .await
                {
                    Ok(models) => {
                        // Prefer a downloaded model; otherwise take the first one
                        let downloaded = models.iter().find(|m| m.downloaded);
                        let first = downloaded.or(models.first());
                        first.map(|m| m.name.clone())
                    }
                    Err(_) => None,
                }
            }
        };

        if let Some(ref name) = resolved_name {
            let model_id = format!("llamafile/{}", name);
            match model_service.ensure_downloaded(&model_id).await {
                Ok(path) => {
                    println!("  ✅ Model '{}' is ready", name);
                    Some(path)
                }
                Err(e) => {
                    println!("  ⚠  Could not ensure model '{}': {}", name, e);
                    None
                }
            }
        } else {
            println!("  ⚠  No llamafile model configured or found in catalog");
            None
        }
    };

    // Prefer the downloaded model; fall back to any model in the models/llm/ directory.
    let model = preferred_model
        .filter(|p| p.exists())
        .or_else(|| find_model(data_dir));

    let model = match model {
        Some(p) => p,
        None => {
            println!("  ⚠  No LLM model found — run `pond-server setup` to download one.");
            println!("     Chat will fall back to mock echo until a model is available.");
            return None;
        }
    };

    println!(
        "  🧠 Starting llamafile  ({})...",
        model.file_name().unwrap_or_default().to_string_lossy()
    );

    match spawn(&model, port).await {
        Ok(proc) => {
            println!("  ✅ LLM ready on port {}", port);
            Some((proc, port))
        }
        Err(e) => {
            println!("  ⚠  Failed to start llamafile: {}", e);
            None
        }
    }
}
