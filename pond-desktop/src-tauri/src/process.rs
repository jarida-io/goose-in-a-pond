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
            Ok(port) if !port.is_empty() => (format!("http://127.0.0.1:{}", port), true),
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
    pub async fn connect_or_spawn(&self, resource_dir: &std::path::Path) -> Result<String, String> {
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
        let mut cmd = Command::new(&binary_path);
        cmd.arg("serve").arg("--port").arg("4000");
        // In dev builds, ground the child's cwd at the repo root so extension
        // paths like `extensions/music/src/server.ts` resolve regardless of
        // where `tauri dev` itself was invoked from. No-op in production
        // bundles, where this compile-time path won't exist on the user's
        // machine and the binary's own resource-relative paths are absolute.
        if let Some(root) = dev_repo_root() {
            cmd.current_dir(root);
        }
        let child = cmd
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

/// Resolves to the workspace root (`src-tauri/../..`) baked in at compile
/// time, but only if that path still exists on disk — true on the dev
/// machine that built this binary, false anywhere else (e.g. a bundled app
/// on an end user's machine).
pub(crate) fn dev_repo_root() -> Option<std::path::PathBuf> {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .ok()
}

pub(crate) fn server_binary_name() -> &'static str {
    if cfg!(windows) {
        "pond-server.exe"
    } else {
        "pond-server"
    }
}

pub(crate) fn resolve_binary_path(
    resource_dir: &std::path::Path,
    binary_name: &str,
) -> Option<std::path::PathBuf> {
    // Optional override for local debugging and tests. Highest priority so the
    // dev flow (POND_SERVER_BIN pointing at target/release/pond-server) keeps
    // working even inside a packaged app.
    if let Ok(override_path) = std::env::var("POND_SERVER_BIN") {
        let path = std::path::PathBuf::from(override_path);
        if path.exists() {
            return Some(path);
        }
    }

    // Bundled sidecar (Tauri 2 externalBin). The macOS bundler copies the
    // sidecar into Contents/MacOS/ next to the main executable and strips the
    // target-triple suffix, so it lives as a *sibling* of our own binary
    // (e.g. `<App>.app/Contents/MacOS/pond-server`). Probe there first — before
    // the dev/workspace fallbacks below — so a packaged app never falls back to
    // a stray `binaries/` folder in the cwd. In `tauri dev` nothing is placed
    // next to the dev executable, so this candidate simply does not exist and
    // resolution falls through cleanly.
    if let Some(sidecar) = current_exe_sibling(binary_name) {
        return Some(sidecar);
    }

    candidate_binary_paths(resource_dir, binary_name)
        .into_iter()
        .find(|p| p.exists())
        .or_else(|| {
            // `stage-server-sidecar.sh` stages the dev sidecar with a Rust
            // target-triple suffix (e.g. `pond-server-aarch64-apple-darwin`) --
            // that is Tauri's own `externalBin` convention, and the exact
            // triple is only known at STAGE time via `rustc -vV`, not
            // something to hardcode or recompute here. The bundler strips
            // that suffix when it copies the sidecar into a packaged app
            // (which is why `current_exe_sibling` above finds a bare name),
            // but nothing un-suffixed it for `tauri dev`, so a freshly staged
            // sidecar sat in `binaries/` invisible to every candidate above.
            candidate_binary_dirs(resource_dir)
                .into_iter()
                .find_map(|dir| find_triple_suffixed_binary(&dir, binary_name))
        })
}

/// Directories searched for `binary_name`, in priority order. Kept separate
/// from `candidate_binary_paths` so the triple-suffixed fallback can scan the
/// same directories without duplicating this list.
fn candidate_binary_dirs(resource_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    vec![
        resource_dir.to_path_buf(),
        resource_dir.join("..").join("binaries"),
        std::path::PathBuf::from("binaries"),
    ]
}

/// Find a sidecar in `dir` named `binary_name` with a target-triple suffix
/// inserted before any extension, e.g. `pond-server-aarch64-apple-darwin` or
/// (Windows) `pond-server-x86_64-pc-windows-msvc.exe`. Exactly one triple is
/// ever staged on a given dev machine, so the first match wins.
fn find_triple_suffixed_binary(
    dir: &std::path::Path,
    binary_name: &str,
) -> Option<std::path::PathBuf> {
    let (stem, ext) = match binary_name.rsplit_once('.') {
        Some((s, e)) => (s, Some(e)),
        None => (binary_name, None),
    };
    let prefix = format!("{stem}-");
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name();
        let name = name.to_str()?;
        let matches_ext = match ext {
            Some(e) => name.ends_with(&format!(".{e}")),
            None => !name.contains('.'),
        };
        (name.starts_with(&prefix) && matches_ext).then(|| entry.path())
    })
}

/// Path to a binary sitting next to the currently-running executable, if it
/// exists. Used to locate the Tauri-bundled `pond-server` sidecar, which the
/// macOS bundler places in `Contents/MacOS/` alongside the main app binary.
fn current_exe_sibling(binary_name: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(binary_name);
    candidate.exists().then_some(candidate)
}

fn candidate_binary_paths(
    resource_dir: &std::path::Path,
    binary_name: &str,
) -> Vec<std::path::PathBuf> {
    candidate_binary_dirs(resource_dir)
        .into_iter()
        .map(|dir| dir.join(binary_name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        candidate_binary_paths, current_exe_sibling, dev_repo_root, find_triple_suffixed_binary,
        resolve_binary_path,
    };

    /// These tests mutate the process-global `POND_SERVER_BIN` env var, which is
    /// not safe to interleave with other tests reading it. Rust runs tests in a
    /// module concurrently by default, so serialize the env-touching ones behind
    /// a single mutex to keep them hermetic.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn dev_repo_root_resolves_to_the_actual_workspace_root() {
        let root = dev_repo_root().expect("resolvable in this dev checkout");
        assert!(root.join("extensions").join("music").is_dir());
    }

    #[test]
    fn candidate_paths_include_bundle_dev_and_cwd_locations() {
        let resource_dir = std::path::PathBuf::from("/tmp/resources");
        let paths = candidate_binary_paths(&resource_dir, "pond-server");

        assert_eq!(paths.len(), 3);
        assert_eq!(
            paths[0],
            std::path::PathBuf::from("/tmp/resources/pond-server")
        );
        assert_eq!(
            paths[1],
            std::path::PathBuf::from("/tmp/resources/../binaries/pond-server")
        );
        assert_eq!(paths[2], std::path::PathBuf::from("binaries/pond-server"));
    }

    #[test]
    fn resolve_binary_path_uses_override_when_present() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let test_bin =
            std::env::temp_dir().join(format!("pond-server-test-{}", std::process::id()));
        std::fs::write(&test_bin, b"#!/bin/sh\n").expect("should write temp binary");

        std::env::set_var("POND_SERVER_BIN", &test_bin);

        let resolved =
            resolve_binary_path(std::path::Path::new("/definitely/missing"), "pond-server");

        std::env::remove_var("POND_SERVER_BIN");
        std::fs::remove_file(&test_bin).expect("should remove temp binary");

        assert_eq!(resolved, Some(test_bin));
    }

    /// The bundled sidecar sits next to the running executable. Prove the helper
    /// resolves it by dropping a file next to `current_exe()` and asserting it is
    /// found. Uses a unique name so it never collides with a real binary or a
    /// concurrent test run sharing the same target/ directory.
    #[test]
    fn current_exe_sibling_resolves_bundled_sidecar() {
        let exe = std::env::current_exe().expect("current_exe should be available in tests");
        let dir = exe.parent().expect("exe should have a parent dir");

        let name = format!("pond-server-sidecar-probe-{}", std::process::id());
        let sidecar = dir.join(&name);
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("should write sibling probe");

        let resolved = current_exe_sibling(&name);

        std::fs::remove_file(&sidecar).expect("should remove sibling probe");

        assert_eq!(resolved, Some(sidecar));
    }

    /// Absent a sibling, the helper returns None so resolution falls through to
    /// the dev/workspace fallbacks (the `tauri dev` case).
    #[test]
    fn current_exe_sibling_returns_none_when_absent() {
        let missing = format!("pond-server-absent-{}", std::process::id());
        assert_eq!(current_exe_sibling(&missing), None);
    }

    /// Precedence: the POND_SERVER_BIN override must win even when a bundled
    /// sidecar exists next to the current executable, so the dev flow is never
    /// shadowed by a stray sibling binary.
    #[test]
    fn override_takes_precedence_over_current_exe_sibling() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let exe = std::env::current_exe().expect("current_exe should be available in tests");
        let dir = exe.parent().expect("exe should have a parent dir");

        let name = format!("pond-server-precedence-{}", std::process::id());
        let sidecar = dir.join(&name);
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("should write sibling probe");

        let override_bin =
            std::env::temp_dir().join(format!("pond-server-override-{}", std::process::id()));
        std::fs::write(&override_bin, b"#!/bin/sh\n").expect("should write override binary");
        std::env::set_var("POND_SERVER_BIN", &override_bin);

        let resolved = resolve_binary_path(std::path::Path::new("/definitely/missing"), &name);

        std::env::remove_var("POND_SERVER_BIN");
        std::fs::remove_file(&sidecar).expect("should remove sibling probe");
        std::fs::remove_file(&override_bin).expect("should remove override binary");

        assert_eq!(resolved, Some(override_bin));
    }

    /// `stage-server-sidecar.sh` stages exactly this shape --
    /// `pond-server-<rustc-host-triple>` -- and nothing un-suffixes it for
    /// `tauri dev`. Without the fallback this is invisible to
    /// `candidate_binary_paths`, which only ever looks for the bare name, so
    /// `ensure_running` fails with "no binary found" even though a freshly
    /// staged sidecar is sitting right there.
    #[test]
    fn finds_a_triple_suffixed_sidecar() {
        let dir = std::env::temp_dir().join(format!("pond-triple-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("should create probe dir");

        let sidecar = dir.join("pond-server-aarch64-apple-darwin");
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("should write triple-suffixed probe");

        let resolved = find_triple_suffixed_binary(&dir, "pond-server");

        std::fs::remove_dir_all(&dir).expect("should remove probe dir");

        assert_eq!(resolved, Some(sidecar));
    }

    /// The suffix goes before the extension, not after -- `pond-server.exe`
    /// becomes `pond-server-<triple>.exe`, never `pond-server.exe-<triple>`.
    #[test]
    fn finds_a_triple_suffixed_sidecar_with_an_extension() {
        let dir = std::env::temp_dir().join(format!("pond-triple-exe-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("should create probe dir");

        let sidecar = dir.join("pond-server-x86_64-pc-windows-msvc.exe");
        std::fs::write(&sidecar, b"").expect("should write triple-suffixed probe");
        // A same-stemmed file with the wrong extension must not match.
        std::fs::write(dir.join("pond-server-x86_64-pc-windows-msvc.txt"), b"")
            .expect("should write decoy file");

        let resolved = find_triple_suffixed_binary(&dir, "pond-server.exe");

        std::fs::remove_dir_all(&dir).expect("should remove probe dir");

        assert_eq!(resolved, Some(sidecar));
    }

    /// A no-extension `binary_name` (the Unix case) must not match a
    /// same-stemmed file that happens to carry an extension -- otherwise a
    /// stray `pond-server-notes.txt` in `binaries/` would resolve as the
    /// sidecar.
    #[test]
    fn an_extensioned_decoy_does_not_match_an_extensionless_binary_name() {
        let dir = std::env::temp_dir().join(format!("pond-triple-decoy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("should create probe dir");
        std::fs::write(dir.join("pond-server-notes.txt"), b"").expect("should write decoy");

        let resolved = find_triple_suffixed_binary(&dir, "pond-server");

        std::fs::remove_dir_all(&dir).expect("should remove probe dir");

        assert_eq!(resolved, None);
    }

    /// End-to-end through `resolve_binary_path`: a triple-suffixed sidecar in
    /// the `binaries/` fallback directory is found even though the bare name
    /// candidates all miss.
    #[test]
    fn resolve_binary_path_falls_back_to_a_triple_suffixed_sidecar() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let resource_dir =
            std::env::temp_dir().join(format!("pond-resolve-probe-{}", std::process::id()));
        std::fs::create_dir_all(&resource_dir).expect("should create resource dir");

        let sidecar = resource_dir.join("pond-server-aarch64-apple-darwin");
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("should write triple-suffixed probe");

        let resolved = resolve_binary_path(&resource_dir, "pond-server");

        std::fs::remove_dir_all(&resource_dir).expect("should remove probe dir");

        assert_eq!(resolved, Some(sidecar));
    }
}
