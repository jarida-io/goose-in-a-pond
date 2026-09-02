//! Local controller lifecycle — GIAP installs and runs the controller itself.
//!
//! The controller is a Node process: `matter-server/`, shipped beside the
//! binary, running matter.js. Requiring the user to hand-build a runtime before
//! a single bulb works is the wrong first-run experience for an appliance, so
//! when Matter is enabled and nothing is serving the configured port, GIAP sets
//! one up:
//!
//! 1. probe the port — if a controller is already there (the user runs their
//!    own, or a previous Pond left one up), use it and change nothing;
//! 2. otherwise copy the controller into the data dir and `npm ci` its pinned
//!    dependencies there;
//! 3. spawn it with its storage inside the data dir, so the commissioned fabric
//!    (and every paired device) survives restarts and upgrades;
//! 4. wait for the port to accept connections before the adapter connects.
//!
//! Only loopback URLs are auto-started: a remote `matter_ws_url` is someone
//! else's controller and GIAP must not try to manage it.
//!
//! # Why the app is copied rather than run in place
//!
//! `npm ci` writes `node_modules/` next to the `package.json` it reads, and the
//! asset root may be a read-only install directory or an app bundle. Copying the
//! sources into `<data_dir>/matter-server/app/` puts the dependency tree
//! somewhere writable, and lets Node resolve `node_modules` as a plain sibling of
//! the entrypoint — no `NODE_PATH`, no ESM resolution games.

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

/// The controller GIAP started, if any. Shared rather than owned outright
/// because two places have to agree on which process is current: the reconciler
/// kills it on teardown, and the reconnect supervisor replaces it when it finds
/// the process dead. `None` means GIAP did not start one — the user runs their
/// own controller, or Matter is off.
pub type SharedServerChild = Arc<AsyncMutex<Option<Child>>>;

/// matter.js 0.17's own engine range, verbatim: `>=20.19.0 <22.0.0 || >=22.13.0`.
///
/// It is a range with a HOLE in it, not a floor, and that is the whole point of
/// spelling it out here: Node 22.0 through 22.12 satisfies "20.19 or newer" and
/// does NOT satisfy matter.js. Treated as a floor, GIAP installed a controller
/// onto a Node it cannot run on, and the failure arrived as whatever the runtime
/// happened to throw first rather than as "this Node is not supported".
///
/// `MIN_NODE` is still the number the guidance quotes, because it is the oldest
/// Node that works and "install 20.19+" is the sentence a person can act on.
pub const MIN_NODE: (u32, u32) = (20, 19);

/// The excluded range: 22.0 up to, but not including, 22.13.
const EXCLUDED_NODE: ((u32, u32), (u32, u32)) = ((22, 0), (22, 13));

/// How many lines of the controller's stderr to keep.
///
/// A child that dies before it is ready writes its reason only to stderr, and
/// with the output going to a file nobody reads, the most GIAP could say was
/// that the port never opened. The tail is attached to the readiness-timeout
/// error so the reason travels with the failure. Twenty lines is enough for a
/// Node stack trace without holding a log in memory.
const STDERR_TAIL_LINES: usize = 20;

/// Parse `"v20.19.4"` into `(20, 19)`. Pure so the version gate is testable
/// without an interpreter.
pub fn parse_node_version(output: &str) -> Option<(u32, u32)> {
    let version = output.trim().trim_start_matches(['v', 'V']);
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Does this Node satisfy matter.js's engine range?
///
/// Not `version >= MIN_NODE`: see [`MIN_NODE`] for the hole that comparison
/// misses. Tuple ordering still does the work, twice.
pub fn meets_min_node(version: (u32, u32)) -> bool {
    if version < MIN_NODE {
        return false;
    }
    let (excluded_from, excluded_until) = EXCLUDED_NODE;
    !(version >= excluded_from && version < excluded_until)
}

/// Why this Node will not do, in a sentence a person can act on.
///
/// The excluded range needs its own wording: "Node 22.5 is on PATH but the
/// controller needs 20.19+" reads as a contradiction, and a user who checks
/// their version against that sentence concludes GIAP is broken.
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

/// The loopback port to auto-start for, or `None` when the URL points at another
/// host — GIAP only manages a controller it runs itself.
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
/// Where the running controller's pid is recorded, so a controller that outlived
/// its Pond can be found and reaped on the next start.
fn pidfile(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("controller.pid")
}

/// Records the lockfile the installed tree was built from, so an upgrade that
/// changes dependencies reinstalls and one that does not is a no-op.
fn install_marker(data_dir: &Path) -> PathBuf {
    app_dir(data_dir).join(".giap-install")
}
/// The fabric store — commissioned nodes live here, so it must be stable.
pub fn storage_dir(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("storage-js")
}

/// Is something accepting connections on the controller port?
///
/// A bare TCP probe, and only used to wait for a controller GIAP has just
/// spawned — where what is listening is not in question. Deciding whether to
/// ADOPT a listener is [`probe_controller`]'s job, because "something answers"
/// and "our controller answers" are different questions, and conflating them let
/// any process holding the port be adopted forever.
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

/// How long the adoption probe waits. Loopback, so a controller that is up
/// answers in milliseconds; the bound only stops a wedged listener from stalling
/// startup.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Find out what is on `port` by speaking to it.
///
/// The TCP probe alone is not enough to decide whether to reuse a listener, and
/// getting that wrong is not a small matter. A different server holding the port
/// answers TCP, so it was adopted; it does not serve `/giap`, so every connection
/// then failed; and because it had been "reused", GIAP never started a controller
/// of its own. Permanently broken, and the log said only that it was reusing a
/// controller and then could not reach it.
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
///
/// Named and specific because the fix is specific, and because the situation is
/// one an upgrade creates rather than anything the user did wrong.
fn port_is_taken(port: u16, why: &str) -> anyhow::Error {
    anyhow!(
        "port {port} is already in use by something that is not a {PROTOCOL_NAME} controller: \
         {why}. Usually that is another Matter controller, or one left running from an earlier \
         release. Stop it (`lsof -nP -iTCP:{port} -sTCP:LISTEN` names the process), or point the \
         Matter controller address at a different port."
    )
}

/// Where the controller sources are shipped. `GIAP_ASSET_ROOT` and the exe's
/// neighbours, matching how `extensions/` is found.
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

/// Directories never worth copying into the install.
///
/// `node_modules` is the one that matters: in a dev checkout the source tree has
/// one, and `npm ci` deletes and rebuilds it anyway — so copying it is a hundred
/// megabytes of work to produce something immediately thrown away. The others
/// are simply not runtime inputs.
const NOT_COPIED: &[&str] = &["node_modules", "test", ".git"];

/// Replace the installed sources while leaving `node_modules` where it is.
///
/// `copy_tree` clears the destination first, which would take the dependency
/// tree with it — the whole point of this path is not to pay for that again.
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

/// What the installed tree was built from: its dependencies, and its sources.
///
/// TWO fingerprints, because they answer different questions and have very
/// different costs. Dependencies change rarely and cost minutes (`npm ci`);
/// sources change with every release and cost a file copy.
///
/// The marker used to be the lockfile alone, which silently made every
/// source-only change a no-op: a controller fix would ship in the binary, the
/// installed copy under the data dir would keep running the old code, and
/// nothing anywhere would say so. That is how a fixed bug comes back on the one
/// machine that already had the software.
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

/// Hash `bytes` into a short hex string.
///
/// `DefaultHasher` rather than a cryptographic digest: this detects change, it
/// does not defend against a forged one, and the alternative was a new
/// dependency for something a std hasher does adequately.
fn digest(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Every file under `dir`, hashed with its relative path so a rename counts as
/// a change. Sorted, so the result does not depend on directory order.
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

/// Install the controller into the data dir. Idempotent: if the installed tree
/// was built from the lockfile that ships now, this is a no-op.
/// Turn what node printed into something a person can act on.
///
/// A crashing controller prints a JavaScript stack trace, and passing that through
/// verbatim asks the reader to parse a loader backtrace to find out that a directory
/// needs reinstalling. The failures worth naming are the ones with a specific remedy;
/// anything else keeps node's own words, because an unrecognised fault said plainly
/// is better than a guess said confidently.
fn explain_startup_failure(stderr: &str, app: &Path) -> String {
    if stderr.contains("ERR_MODULE_NOT_FOUND") {
        // Which module is missing decides whether the dependencies or the sources
        // are the incomplete half, and the remedy is the same either way.
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

/// Is this install actually runnable?
///
/// The marker records what was *asked* for, not what survived. A cancelled or raced
/// `npm ci` leaves a tree that satisfies every check above -- marker current,
/// `node_modules` present -- and still cannot start, because the loader needs `tsx`
/// and the entry file. Checking the two things node will reach for turns a silent
/// corrupt install into one clear failure at setup, instead of a stack trace on
/// every boot from then on.
fn install_is_runnable(app: &Path) -> bool {
    app.join("src/server.ts").is_file() && app.join("node_modules/tsx").is_dir()
}

/// Serialises installs.
///
/// `ensure_installed` is reached from the reconciler, from the revive path, and
/// from a plain start, and nothing stopped two of them running at once. They race
/// destructively rather than merely wastefully: one clears the tree while the other
/// is halfway through `npm ci` into it, and the survivor then writes the "installed"
/// marker over a half-built install that every later start trusts. Observed as a
/// controller that had 7 of its 51 packages and reported `Cannot find package 'tsx'`
/// on every boot, from a marker claiming the install was complete.
///
/// Held across the whole install, `npm ci` included, so the second caller waits and
/// then finds the marker already current rather than redoing it.
static INSTALL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn ensure_installed(data_dir: &Path, notifier: &MatterNotifier) -> Result<()> {
    // Waiting here is the point: whoever holds this may be about to make the very
    // install this call would otherwise start in parallel with it.
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

    // Sources changed but dependencies did not — by far the common case for an
    // upgrade. Refreshing the code is a file copy; reinstalling `node_modules`
    // for it would be minutes of work to arrive at the same tree.
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

    // `kill_on_drop` because this future is cancellable: the reconciler races
    // `connect()` against a settings change, so toggling Matter off mid-install
    // drops us here. Without it the child keeps running after the runtime has
    // reported `Disabled`, keeps writing into the tree, and survives process
    // exit -- `shutdown()` never sees it. Re-enabling before it finishes then
    // finds a half-written tree and starts a second install into it.
    //
    // `npm ci` is the multi-minute part, so it is where a cancellation almost
    // always lands.
    let output = Command::new("npm")
        .args(["ci", "--omit=dev", "--no-audit", "--no-fund"])
        .current_dir(&app)
        .kill_on_drop(true)
        .output()
        .await
        .context("running npm ci — is npm on PATH?")?;

    if !output.status.success() {
        // npm's own diagnosis, rather than a status code. This used to go
        // nowhere at all: the install inherited stdio, so a failure on a
        // headless Pond left "the port never opened" as the only symptom.
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "installing the Matter controller failed ({}). npm said:\n{}",
            output.status,
            tail(&stderr, STDERR_TAIL_LINES)
        ));
    }

    // Before the marker, not after: a marker written over an install that cannot
    // run is the thing that made this survive restarts.
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

    /// What this actually looked like: node's loader backtrace, surfaced verbatim,
    /// asking the reader to work out from `package_json_reader:301` that a directory
    /// needed reinstalling.
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
        // And none of node's plumbing.
        assert!(!explained.contains("package_json_reader"), "{explained}");
    }

    #[test]
    fn a_taken_port_is_named_as_one() {
        let explained =
            explain_startup_failure("Error: listen EADDRINUSE :::5580", Path::new("/x/app"));
        assert!(explained.contains("port is already taken"), "{explained}");
    }

    /// An unrecognised fault is passed through rather than guessed at: node's own
    /// words are worth more than a confident wrong explanation.
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

    /// The check that would have caught the half-built install at setup instead of
    /// on every boot from then on.
    #[test]
    fn an_install_missing_its_loader_is_not_runnable() {
        // Same shape as the other filesystem tests here: a named directory under
        // the process id, cleaned up at the end.
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

/// The last `lines` lines of `text`.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A bounded ring of the controller's most recent stderr lines.
type StderrTail = Arc<Mutex<VecDeque<String>>>;

/// Spawn the controller. The child is `kill_on_drop`, so holding the handle ties
/// its lifetime to pond-server: drop it and the controller goes away too.
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
        // Piped rather than sent to a file nobody reads. The controller writes
        // structured NDJSON here, so the relay below can re-emit each record at
        // the level it names instead of flattening everything to one.
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

    // Nothing should reach stdout — the controller keeps it clean deliberately —
    // so anything that does is unexpected and worth seeing rather than dropping.
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

/// Re-emit one controller stderr line into `tracing`.
///
/// Structured records keep their level and their fields; anything else — a Node
/// stack trace, matter.js's own output — is relayed at debug, where it is
/// available when someone goes looking without filling the log by default.
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

/// Classify the pid in the pidfile, reading its command line via `ps` (portable
/// across macOS and Linux):
///
///   * `Some(true)`  — alive, and still our controller: safe to kill.
///   * `Some(false)` — `ps` ran and reported no such process, or a live but
///     UNRELATED one (the pid was reused). Never kill; clear the file.
///   * `None`        — `ps` could not be run, so liveness is indeterminate. The
///     caller must not treat this as dead, or a real orphan loses the only
///     record of itself.
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
            // The entrypoint path is unique to this data dir, so two Ponds on
            // one machine cannot reap each other's controllers.
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

/// Kill a controller left behind by a Pond that did not exit cleanly.
///
/// The graceful path already kills the controller (`stop_controller`, on the
/// signal handler), and `kill_on_drop` covers an unwinding exit. Neither fires
/// on `SIGKILL`, a panic under `panic = "abort"`, or an OOM kill. In practice
/// the controller usually dies anyway — its stderr is a pipe to the Pond, so the
/// next line it writes fails — but that is luck, not design: an idle controller
/// with nothing to say survives, and being reachable it would then be ADOPTED by
/// the next start and never owned by anyone, since nothing holds its handle.
///
/// So it is reaped rather than adopted. "When the Pond dies, everything dies
/// with it" is only true if something enforces it on the way back up.
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
            // Keep the file: it is the only record of a controller that may
            // still be holding the port, and a later start can retry.
            tracing::warn!(
                pid,
                "matter: could not determine whether the recorded controller is alive; \
                 keeping the pidfile so a later start can retry"
            );
        }
    }
}

/// Ensure a controller is reachable on `port`, installing and starting one if
/// needed. Returns the child handle when GIAP started it (the caller must keep
/// it alive), or `None` when an existing controller was reused.
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
        // Not ours, so it must not be adopted — and spawning onto the port would
        // only fail to bind, with a worse message than this one.
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
        // BLE is optional, and asking for it must never cost the controller.
        //
        // On macOS the OS KILLS a process that touches CoreBluetooth without an
        // `NSBluetoothAlwaysUsageDescription` in its bundle's Info.plist -- SIGKILL,
        // from TCC, with the reason only in a crash report. The controller cannot
        // catch that, so `ble.ts`'s try/catch does not help: the process is simply
        // gone. Left alone, the supervisor would respawn it and it would be killed
        // again, forever, and Matter would be unusable BECAUSE a transport was
        // switched on.
        //
        // So the second attempt drops it. IP-only is the behaviour every install had
        // before BLE existed, and it is strictly better than a crash loop.
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
///
/// Split out of [`ensure_running`] so the BLE fallback can run it twice without
/// repeating the readiness loop or the stderr reporting.
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

    // The reason, not a pointer to where the reason might be. A controller that
    // dies during startup writes why to stderr and nowhere else, and "see the
    // log file" was as far as this could go when that file went unread.
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

/// Both attempts failed, so BLE was not the problem. Carries both reasons: the
/// second is the real fault, and the first is what a reader would otherwise be
/// left blaming.
fn ble_and_ip_both_failed(with_ble: &anyhow::Error, without: &anyhow::Error) -> anyhow::Error {
    anyhow!(
        "{without} (it also failed with BLE enabled, which is therefore not the cause: \
         {with_ble})"
    )
}

/// What a revival attempt actually did, so the caller can tell "the controller
/// was dead and is back" from "the controller is fine, the fault is elsewhere".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revival {
    /// The URL points at another host: someone else's controller, which GIAP
    /// must never install for or spawn.
    NotLocal,
    /// A controller was already listening; nothing was installed or spawned.
    Reused,
    /// The controller was gone and a fresh one is now accepting connections.
    Restarted,
}

/// Re-run [`ensure_running`] for a controller GIAP manages itself, parking any
/// freshly spawned child in `child` so teardown still kills the process that is
/// actually running.
///
/// The reconnect supervisor calls this once reconnecting alone has stopped
/// working: a controller whose process has exited will never answer a reconnect,
/// no matter how long the loop runs. Idempotent by construction —
/// [`ensure_running`] reuses a live port — so it is safe to call repeatedly, and
/// it never puts a second controller onto a fabric that already has one.
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

    // A revival is not a first run, so it never announces setup: the user is
    // already being told the controller is unreachable.
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
        // Storing the new handle drops the dead one, which is harmless:
        // `kill_on_drop` against an already-exited process is a no-op, and
        // teardown now kills the controller that is really running.
        Some(fresh) => {
            *child.lock().await = Some(fresh);
            Ok(Revival::Restarted)
        }
        // Something is serving the port — leave the stored handle alone rather
        // than claiming ownership of a process GIAP did not start.
        None => Ok(Revival::Reused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;

    /// A remote controller is another machine's process. Revival runs on every
    /// failing URL, so this is the guard that stops GIAP installing a runtime
    /// and spawning a controller for a server it does not own.
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

    /// A controller that is still listening is reused, never restarted. The
    /// supervisor retries revival for as long as reconnects keep failing, and a
    /// second controller on a live fabric would be worse than the outage it was
    /// trying to fix.
    #[tokio::test]
    async fn revival_reuses_a_controller_that_is_still_listening() {
        // Speaks the greeting, which is what "a controller of ours" now means:
        // a bare listener would (correctly) be refused instead.
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
        // The floor is a MINOR one, which is the whole reason this is a tuple:
        // Node 20.18 satisfies "20+" and does not satisfy matter.js.
        assert!(!meets_min_node((20, 18)));
        assert!(!meets_min_node((18, 20)));
    }

    /// matter.js's engine range has a HOLE in it, and the gate treated it as a
    /// floor: `>=20.19.0 <22.0.0 || >=22.13.0` excludes 22.0 through 22.12.
    ///
    /// Node 22 is what NodeSource's `setup_22.x` installs — the very command
    /// GIAP's own guidance tells a user to run — so an early 22 is not a
    /// contrived case. It was green-lit, the controller was installed onto it,
    /// and the failure surfaced as whatever matter.js threw first.
    #[test]
    fn the_hole_in_matter_js_engine_range_is_not_a_floor() {
        assert!(!meets_min_node((22, 0)), "22.0 is excluded");
        assert!(!meets_min_node((22, 5)), "22.5 is excluded");
        assert!(!meets_min_node((22, 12)), "22.12 is the last excluded");
        assert!(meets_min_node((22, 13)), "22.13 is where support resumes");
        // And the two ends of the range are untouched.
        assert!(meets_min_node((21, 7)), "21.x is inside >=20.19 <22.0");
        assert!(meets_min_node((24, 14)));
    }

    /// A refusal a person can act on. "Needs 20.19+" against a Node 22.5 that
    /// IS 20.19-or-newer reads as a contradiction, and a user checking their
    /// version against that sentence concludes GIAP is broken.
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
        // Storage must live under the data dir so the commissioned fabric
        // survives restarts.
        assert!(storage_dir(data).starts_with(data));
    }

    #[tokio::test]
    async fn is_running_detects_a_live_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_running(port).await, "a bound port must read as running");

        drop(listener);

        // A port with nothing on it must not read as running. That is what decides
        // whether GIAP spawns its own controller: a false "running" leaves a pond
        // waiting forever for a controller nobody started.
        //
        // Asking about a port we just released is a race that cannot be won outright.
        // Several tests here bind ephemeral ports at once, and the OS is free to hand
        // ours straight to one of them between the drop and the question -- which is
        // what made this fail roughly one run in two.
        //
        // So a positive answer is retried with a fresh port rather than trusted. The
        // check stays honest: an `is_running` that answered "running" for everything
        // would exhaust every attempt and fail, which is the regression worth
        // catching. Losing a port to a sibling costs one more attempt instead of a
        // red build, and losing eight in a row is not a race any more.
        assert!(
            a_port_that_reads_as_free().await.is_some(),
            "no unbound port read as free in {PORT_ATTEMPTS} attempts, so is_running \
             reports every port as running -- GIAP would never spawn a controller"
        );
    }

    /// How many fresh ports to try before concluding `is_running` is broken
    /// rather than merely unlucky.
    const PORT_ATTEMPTS: usize = 8;

    /// A port that is bound to nothing and that `is_running` agrees is free.
    ///
    /// Asking about a port we just released is a race that cannot be won
    /// outright: several tests in this binary bind ephemeral ports at once, and
    /// the OS may hand ours straight to one of them between the drop and the
    /// question. That made these tests fail roughly one run in two.
    ///
    /// So a positive answer is retried with a fresh port rather than trusted,
    /// and `None` after every attempt is a real finding, not a shrug — an
    /// `is_running` that answered "running" for everything would exhaust the
    /// attempts and the caller asserts on that. The alternative shape, an early
    /// `return` on the first positive answer, also stops the flake but passes
    /// forever once `is_running` regresses, which is the one thing these tests
    /// exist to catch.
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
        // The reason a process died is its last words, not its first.
        let text = (1..=50)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tail(&text, 3), "line 48\nline 49\nline 50");

        // Fewer lines than asked for is not an error.
        assert_eq!(tail("only one", 5), "only one");
        assert_eq!(tail("", 5), "");
    }

    /// The regression this whole probe exists for. Another server holding the
    /// port answers TCP, so the old check adopted it; it does not serve `/giap`,
    /// so every connection then failed; and having "reused" it, GIAP never
    /// started a controller of its own. Permanently broken, with a log that said
    /// only that it was reusing a controller and then could not reach it.
    #[tokio::test]
    async fn a_listener_that_is_not_ours_is_named_rather_than_adopted() {
        // A plain TCP listener that never speaks: the shape of anything on the
        // port that is not a giap-matter controller.
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
        // The fix has to be in the message: nothing the user did caused this,
        // so nothing they know tells them how to clear it.
        assert!(message.contains("lsof"), "got: {message}");
        assert!(message.contains("different port"), "got: {message}");
    }

    #[tokio::test]
    async fn a_free_port_reads_as_free() {
        // Was `if is_running(free) { return; }`, which stopped the flake by
        // skipping the assertion — so a broken `is_running` made this test pass
        // rather than fail. `a_port_that_reads_as_free` retries instead, and
        // exhausting it is a failure.
        let free = a_port_that_reads_as_free()
            .await
            .unwrap_or_else(|| panic!("no unbound port read as free in {PORT_ATTEMPTS} attempts"));
        assert_eq!(
            probe_controller(free, &format!("ws://127.0.0.1:{free}/giap")).await,
            Occupant::Free
        );
    }

    /// A pid that is alive but is NOT our controller must never be killed. The
    /// pidfile can outlive the process it names, and the OS reuses pids — so
    /// without the command-line check, a start-up could kill an unrelated
    /// process belonging to the user.
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

    /// A pid nothing is using reads as gone, so the stale record is cleared
    /// rather than kept forever.
    #[tokio::test]
    async fn a_dead_pid_reads_as_gone() {
        // PID 1 exists, so pick something implausible instead: a pid above the
        // system maximum can never be live.
        assert_eq!(
            pid_is_our_controller(4_294_967_294, Path::new("/var/lib/giap")).await,
            Some(false)
        );
    }

    /// Reaping is keyed on the entrypoint path, which contains the data dir, so
    /// two Ponds on one machine cannot stop each other's controllers.
    #[test]
    fn the_pid_record_and_entrypoint_are_per_data_dir() {
        let a = Path::new("/var/lib/giap-a");
        let b = Path::new("/var/lib/giap-b");
        assert_ne!(pidfile(a), pidfile(b));
        assert_ne!(entrypoint(a), entrypoint(b));
        assert!(pidfile(a).starts_with(a));
    }

    /// An absent or unparseable record is simply nothing to do, not an error:
    /// this runs on every single start.
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

    /// The regression that broke a working install: the marker was the lockfile
    /// alone, so a change to the controller's SOURCES left the installed copy
    /// untouched. The fix shipped in the binary, the data dir kept running the
    /// old code, and nothing said so — which is how a fixed bug comes back on
    /// the one machine that already had the software.
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

        // And a dependency change is distinguishable from a source change, so a
        // source edit does not pay for a reinstall.
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

    /// The refresh path exists to avoid a multi-minute reinstall, so it must not
    /// take `node_modules` with it.
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
        // A dev checkout's matter-server/ has a node_modules of its own, and
        // `npm ci` deletes and rebuilds one regardless — so copying it is a lot
        // of work to produce something immediately discarded.
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
        // Guards the packaging contract from the dev side: `cargo test` runs
        // with CARGO_MANIFEST_DIR set, so this proves the repo-relative fallback
        // still points at the real directory after a move.
        let found = source_dir().expect("matter-server/ must be findable in the repo");
        assert!(found.join("package.json").is_file());
        assert!(found.join("src/server.ts").is_file());
    }
}
