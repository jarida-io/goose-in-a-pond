//! Auto-starts a downloaded llamafile as the local OpenAI-compatible LLM server.

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

/// First `.llamafile` (`.llamafile.exe` on Windows) in `<data_dir>/models/llm/`.
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

/// Whether the binary takes `--jinja`; builds ≤ v0.9.0 crash if it is passed.
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

/// Spawn the server on loopback only (privacy) and wait up to 60 s for it to load.
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

    // Still loading after 60 s: return the guard; the FallbackProvider covers early requests.
    println!("  ⚠  LLM did not respond within 60 s — it may still be loading in the background.");
    Ok(proc)
}

// ── High-level entry point ────────────────────────────────────────────────────

/// Base URL for llamafile given a port.
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Check, find (downloading if needed), spawn on the first free port. Never errors: `None`
/// if the base port already serves, or on any failure, which is printed.
pub async fn try_start(
    data_dir: &Path,
    model_service: std::sync::Arc<pond_core::models::services::model_service::ModelService>,
    model_name: Option<&str>,
) -> Option<(LlamafileProcess, u16)> {
    let base_port = crate::ports::llamafile_port();

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

    // Resolve a model name (argument, chat role, then catalog) and download it if needed.
    let preferred_model = {
        use pond_core::models::domain::model_record::ModelCategory;

        let resolved_name: Option<String> = match model_name {
            Some(name) if !name.is_empty() => Some(name.to_string()),
            _ => if let Ok(Some(record)) = model_service.model_for_role("chat").await {
                if record.category == ModelCategory::Llamafile {
                    Some(record.name.clone())
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| None),
        };

        let resolved_name = match resolved_name {
            Some(n) => Some(n),
            None => {
                match model_service
                    .list_by_category(&ModelCategory::Llamafile)
                    .await
                {
                    Ok(models) => {
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
