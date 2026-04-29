use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

/// Handle to an optionally-spawned pond-server child process.
/// If the user already has pond-server running, we connect to it without spawning.
pub struct ServerProcess {
    child: Mutex<Option<Child>>,
    pub url: Mutex<String>,
    recovery_lock: AsyncMutex<()>,
    /// True when the desktop was launched as a child of an already-running
    /// pond-server (`pond-server serve --native` sets `GIAP_SERVER_PORT`).
    /// In that case we MUST NOT try to spawn our own — port 4000 is already
    /// bound and a second spawn would either fail or fight for the socket,
    /// leaving the WebView blank ("the app launches but it shows nothing").
    /// Instead we just patiently poll for health.
    parent_managed: bool,
}

impl ServerProcess {
    pub fn new() -> Self {
        // The parent process can pin our server URL via GIAP_SERVER_PORT so
        // the WebView talks to the same instance, instead of falling back to
        // 4000 and racing the parent for the port.
        let (url, parent_managed) = match std::env::var("GIAP_SERVER_PORT") {
            Ok(port) if !port.is_empty() => (
                format!("http://127.0.0.1:{}", port),
                true,
            ),
            _ => ("http://127.0.0.1:4000".to_string(), false),
        };
        Self {
            child: Mutex::new(None),
            url: Mutex::new(url),
            recovery_lock: AsyncMutex::new(()),
            parent_managed,
        }
    }

    /// Try to connect to a running pond-server; if none is found, spawn the
    /// bundled binary from the app's resource directory.
    #[allow(dead_code)]
    pub async fn connect_or_spawn(
        &self,
        resource_dir: &std::path::Path,
    ) -> Result<String, String> {
        self.ensure_running(resource_dir).await
    }

    /// Ensure pond-server is reachable at the configured URL.
    ///
    /// Recovery is serialized so concurrent probes from startup and periodic health
    /// checks cannot race and spawn duplicate child processes.
    pub async fn ensure_running(&self, resource_dir: &std::path::Path) -> Result<String, String> {
        let _guard = self.recovery_lock.lock().await;
        let url = self.url.lock().unwrap().clone();

        // 1. Probe a running server
        if self.health_check(&url).await {
            tracing::info!("Connected to existing pond-server at {}", url);
            return Ok(url);
        }

        // 1b. Parent-managed mode (`pond-server serve --native` set
        //     GIAP_SERVER_PORT). The parent has already bound the socket — we
        //     MUST NOT spawn our own. Poll patiently while the parent finishes
        //     loading models. Use a long timeout because cold-start with face
        //     recognition + Whisper + TTS can take well over a minute.
        if self.parent_managed {
            tracing::info!(
                "Parent-managed pond-server detected; waiting for {} to become healthy",
                url
            );
            for _ in 0..240 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if self.health_check(&url).await {
                    tracing::info!("Parent pond-server is ready at {}", url);
                    return Ok(url);
                }
            }
            return Err(format!(
                "Parent-managed pond-server at {url} did not become healthy within 120 s"
            ));
        }

        self.cleanup_orphaned_child();

        // 2. Look for the pond-server binary.
        //    In production the binary sits in the app bundle's Resources/ dir
        //    (where Tauri places externalBin entries).
        //    In dev mode (`tauri dev`) the resource_dir is different, so we
        //    also probe the `binaries/` folder inside src-tauri/ for convenience.
        let binary_name = server_binary_name();
        let binary_path = resolve_binary_path(resource_dir, binary_name).ok_or_else(|| {
            format!(
                "No pond-server running at {url} and no binary found. \
                 Place pond-server at src-tauri/binaries/{binary_name} or bundle it with the app."
            )
        })?;

        tracing::info!("Spawning pond-server from {}", binary_path.display());
        let child = Command::new(&binary_path)
            .arg("serve")
            .arg("--port")
            .arg("4000")
            .spawn()
            .map_err(|e| format!("Failed to spawn pond-server: {e}"))?;

        *self.child.lock().unwrap() = Some(child);

        // Wait for the server to be ready (up to 10s)
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.health_check(&url).await {
                tracing::info!("Spawned pond-server is ready at {}", url);
                return Ok(url);
            }
        }

        Err("Spawned pond-server did not become healthy within 10s".to_string())
    }

    pub async fn health_check(&self, url: &str) -> bool {
        let endpoint = format!("{}/api/v1/health", url);
        reqwest::Client::new()
            .get(&endpoint)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Kill the spawned child process on shutdown.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        if let Ok(mut lock) = self.child.lock() {
            if let Some(mut child) = lock.take() {
                let _ = child.kill();
                tracing::info!("pond-server child process terminated");
            }
        }
    }

    pub fn get_url(&self) -> String {
        self.url.lock().unwrap().clone()
    }

    pub fn set_url(&self, url: String) {
        *self.url.lock().unwrap() = url;
    }

    fn cleanup_orphaned_child(&self) {
        let mut lock = self.child.lock().unwrap();
        let Some(child) = lock.as_mut() else {
            return;
        };

        match child.try_wait() {
            Ok(Some(status)) => {
                tracing::warn!("pond-server child already exited with status: {}", status);
                *lock = None;
            }
            Ok(None) => {
                // Child exists but health check failed. Terminate and replace to recover.
                tracing::warn!("pond-server child is running but unhealthy; restarting");
                let _ = child.kill();
                let _ = child.wait();
                *lock = None;
            }
            Err(e) => {
                tracing::warn!("failed to inspect pond-server child status: {e}");
                *lock = None;
            }
        }
    }
}

impl Default for ServerProcess {
    fn default() -> Self {
        Self::new()
    }
}

fn server_binary_name() -> &'static str {
    if cfg!(windows) {
        "pond-server.exe"
    } else {
        "pond-server"
    }
}

fn resolve_binary_path(resource_dir: &std::path::Path, binary_name: &str) -> Option<std::path::PathBuf> {
    // Optional override for local debugging and tests.
    if let Ok(override_path) = std::env::var("POND_SERVER_BIN") {
        let path = std::path::PathBuf::from(override_path);
        if path.exists() {
            return Some(path);
        }
    }

    candidate_binary_paths(resource_dir, binary_name)
        .into_iter()
        .find(|p| p.exists())
}

fn candidate_binary_paths(
    resource_dir: &std::path::Path,
    binary_name: &str,
) -> Vec<std::path::PathBuf> {
    vec![
        resource_dir.join(binary_name),
        resource_dir.join("..").join("binaries").join(binary_name),
        std::path::PathBuf::from("binaries").join(binary_name),
    ]
}

#[cfg(test)]
mod tests {
    use super::{candidate_binary_paths, resolve_binary_path};

    #[test]
    fn candidate_paths_include_bundle_dev_and_cwd_locations() {
        let resource_dir = std::path::PathBuf::from("/tmp/resources");
        let paths = candidate_binary_paths(&resource_dir, "pond-server");

        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], std::path::PathBuf::from("/tmp/resources/pond-server"));
        assert_eq!(
            paths[1],
            std::path::PathBuf::from("/tmp/resources/../binaries/pond-server")
        );
        assert_eq!(paths[2], std::path::PathBuf::from("binaries/pond-server"));
    }

    #[test]
    fn resolve_binary_path_uses_override_when_present() {
        let test_bin = std::env::temp_dir().join(format!(
            "pond-server-test-{}",
            std::process::id()
        ));
        std::fs::write(&test_bin, b"#!/bin/sh\n").expect("should write temp binary");

        std::env::set_var("POND_SERVER_BIN", &test_bin);

        let resolved = resolve_binary_path(std::path::Path::new("/definitely/missing"), "pond-server");

        std::env::remove_var("POND_SERVER_BIN");
        std::fs::remove_file(&test_bin).expect("should remove temp binary");

        assert_eq!(resolved, Some(test_bin));
    }
}
