//! Local matter.js controller: adopt one on the port, else copy the shipped sources to a
//! writable `<data_dir>/matter-server/app/` (so `npm ci` works) and spawn it. Loopback only.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::connect_async;

use crate::notify::MatterNotifier;
use crate::protocol::{check_greeting, PROTOCOL_NAME};

/// The controller GIAP started (`None`: user-run, or Matter off). Shared: the reconciler kills
/// it on teardown and the reconnect supervisor replaces it when it dies.
pub type SharedServerChild = Arc<AsyncMutex<Option<Child>>>;

/// Floor of matter.js 0.17's engine range `>=20.19.0 <22.0.0 || >=22.13.0`, which has a hole.
pub const MIN_NODE: (u32, u32) = (20, 19);

/// The excluded range: 22.0 up to, but not including, 22.13.
const EXCLUDED_NODE: ((u32, u32), (u32, u32)) = ((22, 0), (22, 13));

/// Stderr lines kept for the readiness-timeout error; 20 fits a Node stack trace.
const STDERR_TAIL_LINES: usize = 20;

/// Parse `"v20.19.4"` into `(20, 19)`.
pub fn parse_node_version(output: &str) -> Option<(u32, u32)> {
    let version = output.trim().trim_start_matches(['v', 'V']);
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Does this Node satisfy matter.js's engine range, hole included (see [`MIN_NODE`])?
pub fn meets_min_node(version: (u32, u32)) -> bool {
    if version < MIN_NODE {
        return false;
    }
    let (excluded_from, excluded_until) = EXCLUDED_NODE;
    !(version >= excluded_from && version < excluded_until)
}

/// Why this Node won't do. The hole gets its own wording: "needs 20.19+" contradicts 22.5.
fn node_version_objection(version: (u32, u32)) -> String {
    let (from, until) = EXCLUDED_NODE;
    if version >= from && version < until {
        format!(
            "Node {}.{} is on PATH, and matter.js does not support {}.{} to {}.{} — the range              is 20.19 or newer, EXCEPT 22.0 through 22.12. Upgrade to {}.{} or newer",
            version.0, version.1, from.0, from.1, until.0, until.1 - 1, until.0, until.1
        )
    } else {
        format!(
            "Node {}.{} is on PATH but the Matter controller needs {}.{}+. Upgrade it",
            version.0, version.1, MIN_NODE.0, MIN_NODE.1
        )
    }
}

/// The loopback port to auto-start for; `None` for any other host.
pub fn local_port_from_ws_url(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("ws://")
        .or_else(|| url.strip_prefix("wss://"))?;
    let authority = rest.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
        return None;
    }
    port.parse().ok()
}

/// Everything GIAP owns for the controller lives under one directory.
pub fn controller_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("matter-server")
}
/// The controller's own copy of `matter-server/`, with its `node_modules`.
pub fn app_dir(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("app")
}
pub fn entrypoint(data_dir: &Path) -> PathBuf {
    app_dir(data_dir).join("src").join("server.ts")
}
/// Records the controller pid so one that outlived its Pond is reaped on the next start.
fn pidfile(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("controller.pid")
}

/// Records the fingerprint the installed tree was built from.
fn install_marker(data_dir: &Path) -> PathBuf {
    app_dir(data_dir).join(".giap-install")
}
/// The fabric store — commissioned nodes live here, so it must be stable.
pub fn storage_dir(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("storage-js")
}

/// Bare TCP probe, for waiting on a just-spawned controller; adoption uses [`probe_controller`].
pub async fn is_running(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

/// What is on the controller port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Occupant {
    /// Nothing is listening.
    Free,
    /// A controller GIAP can talk to. Reuse it.
    Ours,
    /// Something is listening and it is not one of ours, with the reason.
    Foreign(String),
}

/// Adoption probe timeout: loopback answers in ms, so this only guards a wedged listener.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Find out what is on `port` by speaking to it; a foreign server answers bare TCP too.
pub async fn probe_controller(port: u16, url: &str) -> Occupant {
    if !is_running(port).await {
        return Occupant::Free;
    }

    let handshake = tokio::time::timeout(PROBE_TIMEOUT, connect_async(url)).await;
    let mut socket = match handshake {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(e)) => {
            return Occupant::Foreign(format!("it refused a {PROTOCOL_NAME} connection ({e})"))
        }
        Err(_) => return Occupant::Foreign("it did not answer a connection in time".to_string()),
    };

    let frame = match tokio::time::timeout(PROBE_TIMEOUT, socket.next()).await {
        Ok(Some(Ok(frame))) => frame,
        _ => return Occupant::Foreign("it did not send a greeting".to_string()),
    };

    let outcome = match check_greeting(frame.to_text().unwrap_or_default()) {
        Ok(_) => Occupant::Ours,
        Err(reason) => Occupant::Foreign(reason),
    };
    let _ = socket.close(None).await;
    outcome
}

/// What to tell the user when the port belongs to something else.
fn port_is_taken(port: u16, why: &str) -> anyhow::Error {
    anyhow!(
        "port {port} is already in use by something that is not a {PROTOCOL_NAME} controller: \
         {why}. Usually that is another Matter controller, or one left running from an earlier \
         release. Stop it (`lsof -nP -iTCP:{port} -sTCP:LISTEN` names the process), or point the \
         Matter controller address at a different port."
    )
}

/// Find the shipped controller sources: `GIAP_ASSET_ROOT`, the repo, then beside the exe.
fn source_dir() -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(root) = std::env::var("GIAP_ASSET_ROOT") {
        candidates.push(PathBuf::from(root).join("matter-server"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../matter-server"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("matter-server"));
            candidates.push(dir.join("../Resources/matter-server"));
            candidates.push(dir.join("../matter-server"));
        }
    }

    for candidate in &candidates {
        if candidate.join("package.json").is_file() {
            return Ok(candidate.clone());
        }
    }
    Err(anyhow!(
        "cannot find the matter-server sources — looked in {}. This is a packaging fault: \
         matter-server/ has to ship beside the binary.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// First `node` on PATH meeting [`MIN_NODE`].
async fn find_node() -> Result<PathBuf> {
    if let Ok(out) = Command::new("node")
        .arg("--version")
        .kill_on_drop(true)
        .output()
        .await
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(version) = parse_node_version(&text) {
            if meets_min_node(version) {
                return Ok(PathBuf::from("node"));
            }
            return Err(anyhow!(
                "{} (Debian/Jetson: `curl -fsSL https://deb.nodesource.com/setup_22.x | sudo \
                 bash - && sudo apt-get install -y nodejs`; macOS: `brew install node`), or run \
                 your own controller and point matter_ws_url at it.",
                node_version_objection(version)
            ));
        }
    }
    Err(anyhow!(
        "no Node {}.{}+ found on PATH — the Matter controller needs it. Install one \
         (Debian/Jetson: `curl -fsSL https://deb.nodesource.com/setup_22.x | sudo bash - && \
         sudo apt-get install -y nodejs`; macOS: `brew install node`) and restart, or run your \
         own controller and point matter_ws_url at it.",
        MIN_NODE.0,
        MIN_NODE.1
    ))
}

/// Not copied into the install; `npm ci` rebuilds `node_modules` anyway.
const NOT_COPIED: &[&str] = &["node_modules", "test", ".git"];

/// Replace the installed sources but keep `node_modules` (`copy_tree` would clear it).
fn refresh_sources(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dst).with_context(|| format!("reading {}", dst.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .with_context(|| format!("clearing {}", path.display()))?;
    }

    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if NOT_COPIED.iter().any(|skip| name == *skip) {
            continue;
        }
        let target = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Copy `src` into `dst`, replacing whatever is there.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).with_context(|| format!("clearing {}", dst.display()))?;
    }
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if NOT_COPIED.iter().any(|skip| name == *skip) {
            continue;
        }
        let target = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// What the installed tree was built from; deps apart from sources so only a deps change
/// pays for `npm ci`.
#[derive(PartialEq, Eq)]
struct Fingerprint {
    deps: String,
    sources: String,
}

impl Fingerprint {
    fn render(&self) -> String {
        format!("{}\n{}", self.deps, self.sources)
    }

    fn parse(raw: &str) -> Option<Self> {
        let (deps, sources) = raw.split_once('\n')?;
        Some(Self {
            deps: deps.to_string(),
            sources: sources.to_string(),
        })
    }
}

/// Short hex hash of `bytes`; detects change, not forgery, so `DefaultHasher` suffices.
fn digest(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Hash every file under `dir` with its relative path (so renames count), in sorted order.
fn hash_tree(root: &Path, dir: &Path, into: &mut Vec<(String, String)>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();

    for entry in entries {
        let name = entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if NOT_COPIED.contains(&name.as_str()) {
            continue;
        }
        if entry.is_dir() {
            hash_tree(root, &entry, into)?;
        } else {
            let relative = entry
                .strip_prefix(root)
                .unwrap_or(&entry)
                .display()
                .to_string();
            let bytes =
                std::fs::read(&entry).with_context(|| format!("reading {}", entry.display()))?;
            into.push((relative, digest(&bytes)));
        }
    }
    Ok(())
}

fn fingerprint(dir: &Path) -> Result<Fingerprint> {
    let lock = dir.join("package-lock.json");
    let deps =
        digest(&std::fs::read(&lock).with_context(|| format!("reading {}", lock.display()))?);

    let mut files: Vec<(String, String)> = Vec::new();
    hash_tree(dir, dir, &mut files)?;
    let joined = files
        .into_iter()
        .map(|(path, hash)| format!("{path}:{hash}"))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(Fingerprint {
        deps,
        sources: digest(joined.as_bytes()),
    })
}

/// Turn node's stderr into an actionable message; unrecognised faults keep node's own words.
fn explain_startup_failure(stderr: &str, app: &Path) -> String {
    if stderr.contains("ERR_MODULE_NOT_FOUND") {
        // Only the wording depends on which module is missing; the remedy is the same.
        let what = if stderr.contains("'tsx'") {
            "its dependencies are missing"
        } else {
            "part of it is missing"
        };
        return format!(
            "The Matter controller could not start because {what}. Its install at \
             {} is incomplete -- usually an npm install that was interrupted. Delete \
             that directory and start again; it will be reinstalled, and no \
             commissioned devices are stored there.",
            app.display()
        );
    }

    if stderr.contains("EADDRINUSE") || stderr.contains("Address already in use") {
        return "The Matter controller could not start because its port is already \
                taken, most likely by a controller from a previous run that is still \
                going."
            .to_string();
    }

    if stderr.trim().is_empty() {
        return "The Matter controller printed nothing, which usually means node \
                could not start at all."
            .to_string();
    }

    format!("The Matter controller said:\n{stderr}")
}

/// Can node start this install? The marker records what was asked for, not what survived.
fn install_is_runnable(app: &Path) -> bool {
    app.join("src/server.ts").is_file() && app.join("node_modules/tsx").is_dir()
}

/// Serialises installs, held across the whole install: concurrent callers (reconciler,
/// revive, start) would clear each other's tree mid-`npm ci`.
static INSTALL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn ensure_installed(data_dir: &Path, notifier: &MatterNotifier) -> Result<()> {
    let _installing = INSTALL_LOCK.lock().await;

    let source = source_dir()?;
    let wanted = fingerprint(&source)?;
    let app = app_dir(data_dir);
    let has_modules = app.join("node_modules").is_dir();

    let installed = std::fs::read_to_string(install_marker(data_dir))
        .ok()
        .and_then(|raw| Fingerprint::parse(&raw));

    if installed.as_ref() == Some(&wanted) && has_modules && install_is_runnable(&app) {
        return Ok(());
    }

    // Sources changed, deps did not: refresh by copying instead of a minutes-long `npm ci`.
    if has_modules
        && install_is_runnable(&app)
        && installed.as_ref().is_some_and(|i| i.deps == wanted.deps)
    {
        tracing::info!(
            target: "giap::trace",
            kind = "matter_sources_refreshed",
            path = %app.display(),
            "matter: controller sources changed; refreshing them without reinstalling"
        );
        refresh_sources(&source, &app)?;
        std::fs::write(install_marker(data_dir), wanted.render())
            .with_context(|| format!("writing {}", install_marker(data_dir).display()))?;
        return Ok(());
    }

    let node = find_node().await?;
    let started = Instant::now();
    tracing::info!(
        target: "giap::trace",
        kind = "matter_setup_started",
        path = %app.display(),
        node = %node.display(),
        upgrade = installed.is_some(),
        "matter: installing the controller"
    );
    notifier.setup_started().await;

    std::fs::create_dir_all(controller_dir(data_dir))
        .with_context(|| format!("creating {}", controller_dir(data_dir).display()))?;
    copy_tree(&source, &app)?;

    // `kill_on_drop`: this future is dropped when Matter is toggled off mid-install, and an
    // orphaned `npm ci` would keep writing into the tree.
    let output = Command::new("npm")
        .args(["ci", "--omit=dev", "--no-audit", "--no-fund"])
        .current_dir(&app)
        .kill_on_drop(true)
        .output()
        .await
        .context("running npm ci — is npm on PATH?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "installing the Matter controller failed ({}). npm said:\n{}",
            output.status,
            tail(&stderr, STDERR_TAIL_LINES)
        ));
    }

    // Before the marker: a marker over an unrunnable install would persist across restarts.
    if !install_is_runnable(&app) {
        return Err(anyhow!(
            "the Matter controller was installed into {} but cannot run from it: \
             either src/server.ts or node_modules/tsx is missing. This is what a \
             cancelled or raced npm ci leaves behind. Delete that directory and \
             start again to reinstall it.",
            app.display()
        ));
    }

    std::fs::write(install_marker(data_dir), wanted.render())
        .with_context(|| format!("writing {}", install_marker(data_dir).display()))?;

    tracing::info!(
        target: "giap::trace",
        kind = "matter_setup_finished",
        duration_ms = started.elapsed().as_millis() as u64,
        "matter: controller installed"
    );
    notifier.setup_finished().await;
    Ok(())
}

#[cfg(test)]
mod startup_message_tests {
    use super::*;

    #[test]
    fn a_missing_dependency_reads_as_something_to_do() {
        let stderr = "node:internal/modules/package_json_reader:301\n  \
                      throw new ERR_MODULE_NOT_FOUND(packageName, fileURLToPath(base), null);\n\
                      Error [ERR_MODULE_NOT_FOUND]: Cannot find package 'tsx' imported from /x/app/";
        let explained = explain_startup_failure(stderr, Path::new("/x/app"));

        assert!(
            explained.contains("dependencies are missing"),
            "{explained}"
        );
        assert!(explained.contains("/x/app"), "{explained}");
        // The one thing a user is most likely to fear about deleting it.
        assert!(explained.contains("no commissioned devices"), "{explained}");
        assert!(!explained.contains("package_json_reader"), "{explained}");
    }

    #[test]
    fn a_taken_port_is_named_as_one() {
        let explained =
            explain_startup_failure("Error: listen EADDRINUSE :::5580", Path::new("/x/app"));
        assert!(explained.contains("port is already taken"), "{explained}");
    }

    #[test]
    fn anything_unrecognised_keeps_the_controllers_own_words() {
        let explained = explain_startup_failure("TypeError: x is not a function", Path::new("/x"));
        assert!(
            explained.contains("TypeError: x is not a function"),
            "{explained}"
        );
    }

    #[test]
    fn silence_is_reported_as_silence() {
        let explained = explain_startup_failure("   ", Path::new("/x"));
        assert!(explained.contains("printed nothing"), "{explained}");
    }

    #[test]
    fn an_install_missing_its_loader_is_not_runnable() {
        let app = std::env::temp_dir().join(format!("giap-runnable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&app);
        let app = app.as_path();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(app.join("src/server.ts"), "// entry").unwrap();
        assert!(!install_is_runnable(app), "no node_modules/tsx yet");

        std::fs::create_dir_all(app.join("node_modules/tsx")).unwrap();
        assert!(install_is_runnable(app));

        // Sources cleared by a racing install, dependencies left behind.
        std::fs::remove_file(app.join("src/server.ts")).unwrap();
        assert!(!install_is_runnable(app));

        std::fs::remove_dir_all(app).unwrap();
    }
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A bounded ring of the controller's most recent stderr lines.
type StderrTail = Arc<Mutex<VecDeque<String>>>;

/// Spawn the controller; it is `kill_on_drop`, so dropping the handle stops it.
fn spawn_server(data_dir: &Path, port: u16, ble: bool) -> Result<(Child, StderrTail)> {
    let storage = storage_dir(data_dir);
    std::fs::create_dir_all(&storage).with_context(|| format!("creating {}", storage.display()))?;

    let mut command = Command::new("node");
    command
        .arg("--import")
        .arg("tsx")
        .arg(entrypoint(data_dir))
        .args(["--port", &port.to_string()])
        .arg("--storage-path")
        .arg(&storage);
    if ble {
        command.arg("--ble");
    }
    let mut child = command
        .current_dir(app_dir(data_dir))
        // Piped: the controller writes NDJSON logs, relayed below at each record's level.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawning the Matter controller")?;

    // Recorded before anything else can fail, so an orphan is always findable.
    if let Some(pid) = child.id() {
        let _ = std::fs::write(pidfile(data_dir), pid.to_string());
    }

    let tail: StderrTail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));

    if let Some(stderr) = child.stderr.take() {
        let ring = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                relay(&line);
                let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                if ring.len() == STDERR_TAIL_LINES {
                    ring.pop_front();
                }
                ring.push_back(line);
            }
        });
    }

    // The controller keeps stdout clean, so anything here is unexpected; surface it.
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "matter_server_stdout", "{line}");
            }
        });
    }

    Ok((child, tail))
}

/// Re-emit a controller stderr line into `tracing`: records at their level, other text at debug.
fn relay(line: &str) {
    match serde_json::from_str::<crate::protocol::WireLog>(line) {
        Ok(record) if !record.level.is_empty() => record.relay(),
        _ => tracing::debug!(target: "matter_server_stderr", "{line}"),
    }
}

/// Forget the recorded controller, after stopping it deliberately.
pub(crate) fn clear_pidfile(data_dir: &Path) {
    let _ = std::fs::remove_file(pidfile(data_dir));
}

/// Via `ps`: `Some(true)` = alive and ours (kill); `Some(false)` = gone or an unrelated pid
/// (clear, never kill); `None` = `ps` failed, so keep the pidfile.
async fn pid_is_our_controller(pid: u32, data_dir: &Path) -> Option<bool> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .kill_on_drop(true)
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            let cmdline = String::from_utf8_lossy(&out.stdout);
            let cmdline = cmdline.trim();
            // The entrypoint path is per data dir, so two Ponds never reap each other's.
            let ours = entrypoint(data_dir).display().to_string();
            Some(!cmdline.is_empty() && cmdline.contains(&ours))
        }
        // `ps` ran and exited non-zero → no such pid → the process is gone.
        Ok(_) => Some(false),
        Err(e) => {
            tracing::debug!(pid, error = %e, "matter: could not run `ps` to classify the pidfile");
            None
        }
    }
}

/// Kill a controller orphaned by a hard exit (SIGKILL, abort, OOM), which `kill_on_drop`
/// misses; the next start would otherwise adopt it unowned.
async fn reap_orphan(data_dir: &Path) {
    let path = pidfile(data_dir);
    let Some(pid) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| c.trim().parse::<u32>().ok())
    else {
        return;
    };

    match pid_is_our_controller(pid, data_dir).await {
        Some(true) => {
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_orphan_reaped",
                pid,
                "matter: a controller from a previous run was still going; stopping it"
            );
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .kill_on_drop(true)
                .status()
                .await;
            // Give it a moment to release the port before anything probes it.
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = std::fs::remove_file(&path);
        }
        Some(false) => {
            tracing::debug!(pid, "matter: stale pidfile; clearing it");
            let _ = std::fs::remove_file(&path);
        }
        None => {
            tracing::warn!(
                pid,
                "matter: could not determine whether the recorded controller is alive; \
                 keeping the pidfile so a later start can retry"
            );
        }
    }
}

/// Ensure a controller is on `port`; returns the child if GIAP started it (keep it alive).
pub async fn ensure_running(
    data_dir: &Path,
    port: u16,
    ready_timeout: Duration,
    notifier: &MatterNotifier,
    url: &str,
    ble: bool,
) -> Result<Option<Child>> {
    // Before the probe, or an orphan would be found healthy and adopted.
    reap_orphan(data_dir).await;

    match probe_controller(port, url).await {
        Occupant::Ours => {
            tracing::info!(
                target: "giap::trace",
                kind = "matter_controller_reused",
                port,
                "matter: controller already running; reusing it"
            );
            return Ok(None);
        }
        // Neither adopt nor spawn: binding would fail with a worse message than this.
        Occupant::Foreign(why) => {
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_port_taken",
                port,
                reason = %why,
                "matter: the controller port belongs to something else"
            );
            return Err(port_is_taken(port, &why));
        }
        Occupant::Free => {}
    }

    tracing::info!(port, "matter: no controller found; setting one up");
    ensure_installed(data_dir, notifier).await?;

    match start_and_wait(data_dir, port, ready_timeout, ble).await {
        Ok(child) => Ok(Some(child)),
        // Retry without BLE: macOS TCC SIGKILLs a process touching CoreBluetooth without
        // `NSBluetoothAlwaysUsageDescription` in its Info.plist, uncatchably.
        Err(first) if ble => {
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_ble_start_failed",
                port,
                error = %format!("{first:#}"),
                "matter: the controller would not start with BLE; retrying over IP only"
            );
            let child = start_and_wait(data_dir, port, ready_timeout, false)
                .await
                .map_err(|second| ble_and_ip_both_failed(&first, &second))?;
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_ble_disabled",
                port,
                "matter: BLE is off for this controller; a device that has never been on \
                 the network cannot be paired until the cause above is fixed"
            );
            Ok(Some(child))
        }
        Err(only) => Err(only),
    }
}

/// Spawn a controller and wait for it to accept connections.
async fn start_and_wait(
    data_dir: &Path,
    port: u16,
    ready_timeout: Duration,
    ble: bool,
) -> Result<Child> {
    let (child, stderr_tail) = spawn_server(data_dir, port, ble)?;
    tracing::info!(
        target: "giap::trace",
        kind = "matter_controller_spawned",
        port,
        pid = child.id(),
        ble,
        "matter: controller started"
    );

    let deadline = Instant::now() + ready_timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if is_running(port).await {
            tracing::info!(
                target: "giap::trace",
                kind = "matter_controller_ready",
                port,
                ble,
                "matter: controller ready"
            );
            return Ok(child);
        }
    }

    // A controller that dies during startup says why only on stderr.
    let reason = {
        let ring = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
        ring.iter().cloned().collect::<Vec<_>>().join("\n")
    };
    tracing::warn!(
        target: "giap::trace",
        kind = "matter_controller_exited",
        port,
        ble,
        stderr_tail = %reason,
        "matter: controller did not become ready"
    );
    Err(anyhow!(
        "the Matter controller did not start listening on port {port} within {:?}. {}",
        ready_timeout,
        explain_startup_failure(&reason, &app_dir(data_dir))
    ))
}

/// Both attempts failed, so BLE was not the cause; the IP-only error leads.
fn ble_and_ip_both_failed(with_ble: &anyhow::Error, without: &anyhow::Error) -> anyhow::Error {
    anyhow!(
        "{without} (it also failed with BLE enabled, which is therefore not the cause: \
         {with_ble})"
    )
}

/// What a revival did: tells "dead and now back" from "fine, the fault is elsewhere".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revival {
    /// The URL is another host's controller; GIAP never installs or spawns for it.
    NotLocal,
    /// A controller was already listening; nothing was installed or spawned.
    Reused,
    /// The controller was gone and a fresh one is now accepting connections.
    Restarted,
}

/// Re-run [`ensure_running`] for a local controller, storing any new child in `child` so
/// teardown kills the live one. Idempotent: a live port is reused, never doubled.
pub async fn revive_local_controller(
    data_dir: &Path,
    url: &str,
    child: &SharedServerChild,
    ready_timeout: Duration,
    ble: bool,
) -> Result<Revival> {
    let Some(port) = local_port_from_ws_url(url) else {
        return Ok(Revival::NotLocal);
    };

    // No setup announcement: the user is already told the controller is unreachable.
    match ensure_running(
        data_dir,
        port,
        ready_timeout,
        &MatterNotifier::disabled(),
        url,
        ble,
    )
    .await?
    {
        // Dropping the dead handle is harmless: `kill_on_drop` on an exited process is a no-op.
        Some(fresh) => {
            *child.lock().await = Some(fresh);
            Ok(Revival::Restarted)
        }
        // Leave the stored handle: GIAP did not start whatever is serving the port.
        None => Ok(Revival::Reused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;

    #[tokio::test]
    async fn revival_never_touches_a_remote_controller() {
        let child: SharedServerChild = Arc::new(AsyncMutex::new(None));

        let outcome = revive_local_controller(
            // Unreachable on purpose: nothing here may be read or written.
            Path::new("/nonexistent"),
            "ws://192.168.1.50:5580/giap",
            &child,
            Duration::from_millis(1),
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome, Revival::NotLocal);
        assert!(child.lock().await.is_none(), "no child for a remote server");
    }

    #[tokio::test]
    async fn revival_reuses_a_controller_that_is_still_listening() {
        // Must speak the greeting: a bare listener is refused as foreign.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                serde_json::json!({
                                    "protocol": PROTOCOL_NAME,
                                    "version": crate::protocol::PROTOCOL_VERSION,
                                })
                                .to_string()
                                .into(),
                            ))
                            .await;
                        while ws.next().await.is_some() {}
                    }
                });
            }
        });

        let child: SharedServerChild = Arc::new(AsyncMutex::new(None));
        let outcome = revive_local_controller(
            // The port answers as ours, so setup returns before the data dir is used.
            Path::new("/nonexistent"),
            &format!("ws://127.0.0.1:{port}/giap"),
            &child,
            Duration::from_millis(1),
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome, Revival::Reused);
        assert!(
            child.lock().await.is_none(),
            "reusing a live controller must not claim ownership of it"
        );
    }

    #[test]
    fn parses_node_versions_and_gates_on_20_19() {
        assert_eq!(parse_node_version("v20.19.4"), Some((20, 19)));
        assert_eq!(parse_node_version("v24.14.1\n"), Some((24, 14)));
        assert_eq!(parse_node_version("v18.20.8"), Some((18, 20)));
        assert_eq!(parse_node_version("not a version"), None);
        assert_eq!(parse_node_version(""), None);

        assert!(meets_min_node((20, 19)));
        assert!(meets_min_node((22, 13)));
        assert!(meets_min_node((24, 0)));
        assert!(!meets_min_node((20, 18)));
        assert!(!meets_min_node((18, 20)));
    }

    /// Early Node 22 is not contrived: NodeSource's `setup_22.x` installs it.
    #[test]
    fn the_hole_in_matter_js_engine_range_is_not_a_floor() {
        assert!(!meets_min_node((22, 0)), "22.0 is excluded");
        assert!(!meets_min_node((22, 5)), "22.5 is excluded");
        assert!(!meets_min_node((22, 12)), "22.12 is the last excluded");
        assert!(meets_min_node((22, 13)), "22.13 is where support resumes");
        assert!(meets_min_node((21, 7)), "21.x is inside >=20.19 <22.0");
        assert!(meets_min_node((24, 14)));
    }

    #[test]
    fn an_excluded_node_is_refused_in_its_own_words() {
        let excluded = node_version_objection((22, 5));
        assert!(excluded.contains("22.5"), "{excluded}");
        assert!(
            excluded.contains("22.0") && excluded.contains("22.12"),
            "the excluded range has to be named: {excluded}"
        );
        assert!(excluded.contains("22.13"), "{excluded}");

        // Below the floor keeps the simpler sentence.
        let old = node_version_objection((18, 20));
        assert!(old.contains("18.20") && old.contains("20.19+"), "{old}");
        assert!(!old.contains("EXCEPT"), "{old}");
    }

    #[test]
    fn only_loopback_urls_are_auto_started() {
        assert_eq!(
            local_port_from_ws_url("ws://127.0.0.1:5580/giap"),
            Some(5580)
        );
        assert_eq!(
            local_port_from_ws_url("ws://localhost:5580/giap"),
            Some(5580)
        );
        assert_eq!(local_port_from_ws_url("ws://127.0.0.1:6000"), Some(6000));

        // Someone else's controller: never auto-managed.
        assert_eq!(local_port_from_ws_url("ws://192.168.1.50:5580/giap"), None);
        assert_eq!(local_port_from_ws_url("ws://matter.local:5580/giap"), None);
        // Malformed / portless.
        assert_eq!(local_port_from_ws_url("http://127.0.0.1:5580"), None);
        assert_eq!(local_port_from_ws_url("ws://127.0.0.1/giap"), None);
    }

    #[test]
    fn controller_paths_are_nested_under_the_data_dir() {
        let data = Path::new("/var/lib/giap");
        assert_eq!(app_dir(data), Path::new("/var/lib/giap/matter-server/app"));
        assert_eq!(
            storage_dir(data),
            Path::new("/var/lib/giap/matter-server/storage-js")
        );
        // Under the data dir so the commissioned fabric survives restarts.
        assert!(storage_dir(data).starts_with(data));
    }

    #[tokio::test]
    async fn is_running_detects_a_live_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_running(port).await, "a bound port must read as running");

        drop(listener);

        assert!(
            a_port_that_reads_as_free().await.is_some(),
            "no unbound port read as free in {PORT_ATTEMPTS} attempts, so is_running \
             reports every port as running -- GIAP would never spawn a controller"
        );
    }

    /// Fresh ports to try before concluding `is_running` is broken, not unlucky.
    const PORT_ATTEMPTS: usize = 8;

    /// An unbound port `is_running` calls free; retried because sibling tests may take it.
    async fn a_port_that_reads_as_free() -> Option<u16> {
        for _ in 0..PORT_ATTEMPTS {
            let free = {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let p = l.local_addr().unwrap().port();
                drop(l);
                p
            };
            if !is_running(free).await {
                return Some(free);
            }
        }
        None
    }

    #[test]
    fn the_stderr_tail_keeps_the_end_not_the_beginning() {
        let text = (1..=50)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tail(&text, 3), "line 48\nline 49\nline 50");

        // Fewer lines than asked for is not an error.
        assert_eq!(tail("only one", 5), "only one");
        assert_eq!(tail("", 5), "");
    }

    #[tokio::test]
    async fn a_listener_that_is_not_ours_is_named_rather_than_adopted() {
        // A silent TCP listener: anything on the port that is not a giap-matter controller.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });

        let url = format!("ws://127.0.0.1:{port}/giap");
        assert!(
            matches!(probe_controller(port, &url).await, Occupant::Foreign(_)),
            "a listener that cannot speak the protocol must not be adopted"
        );

        let error = ensure_running(
            Path::new("/nonexistent"),
            port,
            Duration::from_millis(1),
            &MatterNotifier::disabled(),
            &url,
            false,
        )
        .await
        .expect_err("adopting it would leave Matter permanently broken");

        let message = error.to_string();
        assert!(message.contains("already in use"), "got: {message}");
        // The fix must be in the message; the user did nothing to cause this.
        assert!(message.contains("lsof"), "got: {message}");
        assert!(message.contains("different port"), "got: {message}");
    }

    #[tokio::test]
    async fn a_free_port_reads_as_free() {
        let free = a_port_that_reads_as_free()
            .await
            .unwrap_or_else(|| panic!("no unbound port read as free in {PORT_ATTEMPTS} attempts"));
        assert_eq!(
            probe_controller(free, &format!("ws://127.0.0.1:{free}/giap")).await,
            Occupant::Free
        );
    }

    #[tokio::test]
    async fn a_reused_pid_belonging_to_something_else_is_not_killed() {
        // This test process is certainly alive and certainly not a controller.
        let me = std::process::id();
        assert_eq!(
            pid_is_our_controller(me, Path::new("/var/lib/giap")).await,
            Some(false),
            "would have killed an unrelated live process"
        );
    }

    #[tokio::test]
    async fn a_dead_pid_reads_as_gone() {
        // Above any system's pid maximum, so never live.
        assert_eq!(
            pid_is_our_controller(4_294_967_294, Path::new("/var/lib/giap")).await,
            Some(false)
        );
    }

    #[test]
    fn the_pid_record_and_entrypoint_are_per_data_dir() {
        let a = Path::new("/var/lib/giap-a");
        let b = Path::new("/var/lib/giap-b");
        assert_ne!(pidfile(a), pidfile(b));
        assert_ne!(entrypoint(a), entrypoint(b));
        assert!(pidfile(a).starts_with(a));
    }

    #[tokio::test]
    async fn a_missing_or_junk_pidfile_is_harmless() {
        let dir = std::env::temp_dir().join(format!("giap-pid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(controller_dir(&dir)).unwrap();

        reap_orphan(&dir).await; // no file at all

        std::fs::write(pidfile(&dir), "not-a-pid").unwrap();
        reap_orphan(&dir).await;

        std::fs::write(pidfile(&dir), "").unwrap();
        reap_orphan(&dir).await;

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_source_change_changes_the_fingerprint() {
        let dir = std::env::temp_dir().join(format!("giap-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("package-lock.json"), "{}").unwrap();
        std::fs::write(dir.join("src/server.ts"), "// v1").unwrap();

        let before = fingerprint(&dir).unwrap();

        std::fs::write(dir.join("src/server.ts"), "// v2").unwrap();
        let after = fingerprint(&dir).unwrap();

        assert_ne!(
            before.sources, after.sources,
            "a source edit went unnoticed"
        );
        assert_eq!(before.deps, after.deps, "dependencies did not change");

        // Deps are fingerprinted apart, so a source edit does not force a reinstall.
        std::fs::write(dir.join("package-lock.json"), r#"{"x":1}"#).unwrap();
        assert_ne!(fingerprint(&dir).unwrap().deps, after.deps);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_renamed_file_changes_the_fingerprint() {
        // Hashing contents alone would call a rename no change at all.
        let dir = std::env::temp_dir().join(format!("giap-fp2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("package-lock.json"), "{}").unwrap();
        std::fs::write(dir.join("src/a.ts"), "same").unwrap();
        let before = fingerprint(&dir).unwrap();

        std::fs::remove_file(dir.join("src/a.ts")).unwrap();
        std::fs::write(dir.join("src/b.ts"), "same").unwrap();
        assert_ne!(before.sources, fingerprint(&dir).unwrap().sources);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refreshing_sources_keeps_node_modules() {
        let base = std::env::temp_dir().join(format!("giap-refresh-{}", std::process::id()));
        let (src, dst) = (base.join("src"), base.join("dst"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::create_dir_all(dst.join("node_modules/@matter")).unwrap();
        std::fs::create_dir_all(dst.join("src")).unwrap();

        std::fs::write(src.join("package.json"), "{}").unwrap();
        std::fs::write(src.join("src/server.ts"), "// new").unwrap();
        std::fs::write(dst.join("src/server.ts"), "// old").unwrap();
        std::fs::write(dst.join("node_modules/@matter/keep.js"), "x").unwrap();

        refresh_sources(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("src/server.ts")).unwrap(),
            "// new",
            "the source was not refreshed"
        );
        assert!(
            dst.join("node_modules/@matter/keep.js").is_file(),
            "node_modules was destroyed; the refresh would cost a reinstall"
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_install_copy_leaves_node_modules_behind() {
        let tmp = std::env::temp_dir().join(format!("giap-copy-{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = std::fs::remove_dir_all(&tmp);

        std::fs::create_dir_all(src.join("node_modules/@matter")).unwrap();
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::create_dir_all(src.join("test")).unwrap();
        std::fs::write(src.join("package.json"), "{}").unwrap();
        std::fs::write(src.join("src/server.ts"), "// entry").unwrap();
        std::fs::write(src.join("node_modules/@matter/big.js"), "x").unwrap();

        copy_tree(&src, &dst).unwrap();

        assert!(dst.join("package.json").is_file());
        assert!(
            dst.join("src/server.ts").is_file(),
            "the entrypoint must travel"
        );
        assert!(
            !dst.join("node_modules").exists(),
            "node_modules was copied"
        );
        assert!(!dst.join("test").exists(), "tests are not a runtime input");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn the_shipped_controller_is_findable_from_the_source_tree() {
        // Checks the CARGO_MANIFEST_DIR-relative fallback still finds the real directory.
        let found = source_dir().expect("matter-server/ must be findable in the repo");
        assert!(found.join("package.json").is_file());
        assert!(found.join("src/server.ts").is_file());
    }
}
