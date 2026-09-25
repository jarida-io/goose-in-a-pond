//! Finds and starts Ollama; never stops it, since it may be a shared system service.

use anyhow::{Context, Result};
use std::path::PathBuf;

const OLLAMA_PORT: u16 = 11434;

const VERSION_PATH: &str = "/api/version";

// ── Health check ──────────────────────────────────────────────────────────────

pub async fn is_running() -> bool {
    let url = format!("http://127.0.0.1:{}{}", OLLAMA_PORT, VERSION_PATH);
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ── Binary lookup ─────────────────────────────────────────────────────────────

pub fn find_binary() -> Option<PathBuf> {
    if let Ok(output) = std::process::Command::new("which").arg("ollama").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let path = PathBuf::from(path_str.trim());
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let candidates = ["/usr/local/bin/ollama", "/opt/homebrew/bin/ollama"];
        for candidate in &candidates {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = ["/usr/local/bin/ollama", "/usr/bin/ollama"];
        for candidate in &candidates {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}

// ── Start service ─────────────────────────────────────────────────────────────

const STARTUP_TIMEOUT_SECS: u64 = 30;

async fn start_service(binary: &std::path::Path) -> Result<()> {
    // Prefer systemd: many Linux installs run Ollama as a service.
    #[cfg(target_os = "linux")]
    {
        let systemctl = tokio::process::Command::new("systemctl")
            .args(["start", "ollama"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;

        if let Ok(status) = systemctl {
            if status.success() {
                tracing::info!("Started Ollama via systemctl");
                return wait_for_ready().await;
            }
            tracing::debug!(
                "systemctl start ollama failed (exit {}); falling back to direct spawn",
                status.code().unwrap_or(-1)
            );
        }
    }

    let _child = tokio::process::Command::new(binary)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {}", binary.display()))?;

    // Handle dropped on purpose: Ollama must outlive GIAP.

    tracing::info!("Spawned `ollama serve` (pid will detach)");
    wait_for_ready().await
}

async fn wait_for_ready() -> Result<()> {
    for attempt in 1..=STARTUP_TIMEOUT_SECS {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running().await {
            tracing::info!("Ollama ready after ~{attempt}s");
            return Ok(());
        }
        if attempt == 15 {
            tracing::info!(
                "Still waiting for Ollama to start — this can take up to 30 s on first launch"
            );
        }
    }
    anyhow::bail!(
        "Ollama did not respond within {STARTUP_TIMEOUT_SECS}s — \
         check `ollama serve` output for errors"
    )
}

// ── High-level entry point ────────────────────────────────────────────────────

/// `Ok(true)` if this call started Ollama, `Ok(false)` if it was already running.
pub async fn ensure_running() -> Result<bool> {
    if is_running().await {
        return Ok(false);
    }

    let binary = find_binary()
        .context("Ollama binary not found. Install it: brew install ollama (macOS) or curl -fsSL https://ollama.com/install.sh | sh (Linux)")?;

    tracing::info!(binary = %binary.display(), "Starting Ollama");
    start_service(&binary).await?;
    Ok(true)
}

// ── OllamaManager implementation ─────────────────────────────────────────────

/// Stateless [`pond_api::OllamaManager`]: Ollama owns its process and model state.
pub struct OllamaProcessManager;

impl OllamaProcessManager {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl pond_api::OllamaManager for OllamaProcessManager {
    async fn ensure_started(&self) -> bool {
        match ensure_running().await {
            Ok(started) => {
                if started {
                    tracing::info!("Ollama auto-started successfully");
                }
                true
            }
            Err(e) => {
                tracing::warn!("Failed to start Ollama: {e:#}");
                false
            }
        }
    }

    async fn is_running(&self) -> bool {
        is_running().await
    }

    async fn ensure_started_and_wait(&self, timeout_secs: u64) -> bool {
        if is_running().await {
            return true;
        }

        match ensure_running().await {
            Ok(_) => return true, // wait_for_ready already polled inside ensure_running
            Err(e) => {
                tracing::warn!("Ollama startup failed: {e:#}");
            }
        }

        // It may still be coming up (e.g. a systemd restart race), so keep polling.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if is_running().await {
                return true;
            }
        }
        false
    }
}
