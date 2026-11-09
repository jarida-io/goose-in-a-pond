//! Puts the right Node on PATH at startup, for the Matter controller and the `npx` MCP extensions the
//! client spawns. A GUI launch (macOS launchd) gets a bare PATH, hiding nvm or Homebrew Node; and a
//! service can find an old Node first, like the Jetson's apt Node 12 in /usr/bin. For that one, the
//! Node `giap.sh node` recorded goes ahead of it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Plausible Node install dirs, most specific first.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // nvm's `default` alias is not a directory, so take the highest installed version.
        for base in [".nvm/versions/node", ".local/share/fnm/node-versions"] {
            let root = home.join(base);
            if let Ok(entries) = std::fs::read_dir(&root) {
                let mut versions: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                // Lexicographic; fine for v20-v24, and Matter rejects a too-old pick.
                versions.sort();
                dirs.extend(
                    versions
                        .into_iter()
                        .rev()
                        .flat_map(|v| [v.join("bin"), v.join("installation/bin")].into_iter()),
                );
            }
        }
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".asdf/shims"));
        dirs.push(home.join(".local/bin"));
    }

    // Homebrew (Apple silicon, then Intel), then distro locations.
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));

    dirs
}

fn holds_node(dir: &Path) -> bool {
    dir.join("node").is_file()
}

fn node_dir_in(path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?).find(|dir| holds_node(dir))
}

// ── the Node `giap.sh node` recorded ─────────────────────────────────────────

/// Where `giap.sh node` records the Node to use (`node_record_file` in scripts/lib/node-setup.sh):
/// `$GIAP_NODE_HOME/.path`, else `~/.giap/node/.path`.
fn record_file_in(giap_node_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let node_home = match giap_node_home.filter(|h| !h.is_empty()) {
        Some(node_home) => PathBuf::from(node_home),
        None => PathBuf::from(home.filter(|h| !h.is_empty())?)
            .join(".giap")
            .join("node"),
    };
    Some(node_home.join(".path"))
}

fn record_file() -> Option<PathBuf> {
    record_file_in(std::env::var_os("GIAP_NODE_HOME"), std::env::var_os("HOME"))
}

/// The directory on the record's first line, if it is absolute and holds a `node`. A relative one is
/// refused: it would resolve against whatever directory the pond was started in.
fn recorded_dir(record: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(record).ok()?;
    let dir = PathBuf::from(text.lines().next()?);
    (dir.is_absolute() && holds_node(&dir)).then_some(dir)
}

// ── the range ────────────────────────────────────────────────────────────────

/// A version's numbers, read the way scripts/lib/node-setup.sh reads them: one leading `v`, a major
/// that must be digits, and a minor that is 0 when it is missing or not digits.
fn major_minor(version: &str) -> Option<(u32, u32)> {
    fn digits(s: &str) -> Option<u32> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    let major = digits(parts.next()?)?;
    Some((major, parts.next().and_then(digits).unwrap_or(0)))
}

/// The range `giap.sh node` records a Node for, `^22.13.0 || ^24.0.0 || >=26.0.0` (ESLint 10 and
/// vitest; the odd-numbered 23 and 25 are out). It is `node_version_ok` in
/// scripts/lib/node-setup.sh, and a test runs both.
fn inside_range(version: &str) -> bool {
    match major_minor(version) {
        Some((22, minor)) => minor >= 13,
        Some((major, _)) => major == 24 || major >= 26,
        None => false,
    }
}

/// How long `node --version` may take: a version manager's shim can stall, and startup must not.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// What `<dir>/node --version` says, if it runs, exits 0 within `timeout` and says a version. Asked
/// of the binary, as the script does, not read off its path.
fn version_in(dir: &Path, timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(dir.join("node"))
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut said = String::new();
    child.stdout.take()?.read_to_string(&mut said).ok()?;
    let said = said.trim();
    (status.success() && major_minor(said).is_some()).then(|| said.to_string())
}

// ── the decision ─────────────────────────────────────────────────────────────

/// Why a directory went first on PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// There was no `node` on PATH.
    NoneOnPath,
    /// The `node` on PATH is outside the range; this is the version it gave, if it gave one.
    OutsideRange { on_path: Option<String> },
}

/// A directory put first on PATH, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepended {
    pub dir: PathBuf,
    /// The one `giap.sh node` recorded, whose node said `version`; otherwise an install found by
    /// looking, which was not asked.
    pub recorded: bool,
    pub version: Option<String>,
    pub reason: Reason,
}

/// What to put first on PATH, if anything. With a Node recorded by `giap.sh node`, the `node` on PATH
/// stays only if it is inside the range; otherwise the recorded one goes first, if it is inside the
/// range itself. That is the order `node_use_repo` in scripts/lib/node-setup.sh takes, so the pond
/// runs on the Node giap.sh and its doctor run on. With no `node` on PATH at all and no usable record,
/// the first install found. Anything else leaves PATH as it is; without a record no version is asked.
fn plan(
    path: Option<&OsStr>,
    recorded: Option<PathBuf>,
    installs: impl FnOnce() -> Vec<PathBuf>,
    ask_version: impl Fn(&Path) -> Option<String>,
) -> Option<Prepended> {
    let on_path = node_dir_in(path);
    if let Some(dir) = recorded.filter(|dir| on_path.as_ref() != Some(dir)) {
        let said = on_path.as_deref().and_then(&ask_version);
        if !said.as_deref().is_some_and(inside_range) {
            if let Some(version) = ask_version(&dir).filter(|v| inside_range(v)) {
                let reason = match on_path {
                    Some(_) => Reason::OutsideRange { on_path: said },
                    None => Reason::NoneOnPath,
                };
                return Some(Prepended {
                    dir,
                    recorded: true,
                    version: Some(version),
                    reason,
                });
            }
        }
    }
    if on_path.is_some() {
        return None;
    }
    installs()
        .into_iter()
        .find(|dir| holds_node(dir))
        .map(|dir| Prepended {
            dir,
            recorded: false,
            version: None,
            reason: Reason::NoneOnPath,
        })
}

/// What [`ensure_node_on_path`] did, for [`log_decision`]: it runs before logging is set up.
static DECISION: OnceLock<Prepended> = OnceLock::new();

/// Puts a Node dir first on `PATH` if [`plan`] says so, and returns it. Calls
/// [`std::env::set_var`], so call it once at the top of `main`, before any thread is spawned; it may
/// run `node --version` and wait for it, which starts none.
pub fn ensure_node_on_path() -> Option<Prepended> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let recorded = record_file().and_then(|record| recorded_dir(&record));
    let decision = plan(Some(&current), recorded, candidate_dirs, |dir| {
        version_in(dir, PROBE_TIMEOUT)
    })?;

    let mut dirs = vec![decision.dir.clone()];
    dirs.extend(std::env::split_paths(&current));
    let joined = std::env::join_paths(dirs).ok()?;
    pond_core::shared::services::process_env::set("PATH", joined);
    let _ = DECISION.set(decision.clone());
    Some(decision)
}

/// Logs what [`ensure_node_on_path`] did. It ran before there was a subscriber to hear it, so the
/// tracing setup calls this once one is installed.
pub fn log_decision() {
    let Some(done) = DECISION.get() else {
        return;
    };
    let dir = done.dir.display();
    let version = done.version.as_deref().unwrap_or("?");
    match &done.reason {
        Reason::OutsideRange { on_path } => tracing::info!(
            target: "giap::trace",
            kind = "node_path_recorded",
            dir = %dir,
            version,
            on_path = on_path.as_deref().unwrap_or("no version"),
            "node {} on PATH is outside the range; put node {version} from {dir}, which `giap.sh \
             node` recorded, ahead of it for Matter and the stdio extensions",
            on_path.as_deref().unwrap_or("(no version)")
        ),
        Reason::NoneOnPath if done.recorded => tracing::info!(
            target: "giap::trace",
            kind = "node_path_recorded",
            dir = %dir,
            version,
            "node was not on PATH; added node {version} from {dir}, which `giap.sh node` \
             recorded, so Matter and the stdio extensions can find it"
        ),
        Reason::NoneOnPath => tracing::info!(
            target: "giap::trace",
            kind = "node_path_extended",
            dir = %dir,
            "node was not on PATH; added the directory it is in so Matter and the stdio \
             extensions can find it"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashMap;

    /// `<root>/<name>/node`, an empty file: enough for [`holds_node`] when the version is faked.
    fn node_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("node"), "").unwrap();
        dir
    }

    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    /// A version probe answering from a table, counting how often it is asked.
    struct Probe {
        says: HashMap<PathBuf, &'static str>,
        asked: Cell<usize>,
    }

    impl Probe {
        fn new(says: &[(&Path, &'static str)]) -> Self {
            Probe {
                says: says.iter().map(|(d, v)| (d.to_path_buf(), *v)).collect(),
                asked: Cell::new(0),
            }
        }
        fn ask(&self, dir: &Path) -> Option<String> {
            self.asked.set(self.asked.get() + 1);
            self.says.get(dir).map(|v| v.to_string())
        }
    }

    #[test]
    fn a_directory_without_node_is_not_offered() {
        let empty = std::env::temp_dir().join(format!("giap-nodepath-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!holds_node(&empty));

        std::fs::write(empty.join("node"), "#!/bin/sh\n").unwrap();
        assert!(
            holds_node(&empty),
            "a directory with a node in it qualifies"
        );

        std::fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn the_candidate_list_covers_the_managers_people_actually_use() {
        let dirs = candidate_dirs();
        let rendered: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        let joined = rendered.join(" ");

        for expected in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            assert!(joined.contains(expected), "missing {expected} in {joined}");
        }
        if std::env::var_os("HOME").is_some() {
            for expected in [".volta/bin", ".asdf/shims"] {
                assert!(joined.contains(expected), "missing {expected}");
            }
        }
    }

    #[test]
    fn a_launchd_path_gets_node_prepended() {
        let bare = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");

        // Nothing to assert where Node is already in a system location.
        let Some(found) = plan(Some(&bare), None, candidate_dirs, |_| None) else {
            return;
        };
        assert!(
            holds_node(&found.dir),
            "offered {} as a node directory",
            found.dir.display()
        );
        assert!(
            !bare
                .to_string_lossy()
                .contains(&found.dir.display().to_string()),
            "prepended a directory that was already on the PATH"
        );
    }

    #[test]
    fn a_path_that_already_reaches_node_is_left_alone() {
        let Some(dir) = node_dir_in(std::env::var_os("PATH").as_deref()) else {
            return; // no Node here
        };
        let just_that = std::ffi::OsString::from(dir.display().to_string());
        assert!(plan(Some(&just_that), None, candidate_dirs, |_| None).is_none());
    }

    #[test]
    fn without_a_record_nothing_is_asked_and_a_path_with_node_stays() {
        let root = tempfile::tempdir().unwrap();
        let old = node_dir(root.path(), "old");
        let probe = Probe::new(&[(&old, "v12.22.9")]);

        let got = plan(Some(&path_of(&[&old])), None, Vec::new, |d| probe.ask(d));
        assert_eq!(got, None);
        assert_eq!(probe.asked.get(), 0, "no record, so no version to ask");
    }

    #[test]
    fn without_a_record_a_bare_path_gets_the_first_install() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let installed = node_dir(root.path(), "installed");

        let got = plan(
            Some(&path_of(&[&empty])),
            None,
            || vec![root.path().join("nothing-here"), installed.clone()],
            |_| None,
        );
        assert_eq!(
            got,
            Some(Prepended {
                dir: installed,
                recorded: false,
                version: None,
                reason: Reason::NoneOnPath,
            })
        );
    }

    #[test]
    fn an_old_node_on_path_gives_way_to_the_recorded_one() {
        // The Jetson: apt's Node 12 in /usr/bin, and a Node 22 that `giap.sh node` downloaded.
        let root = tempfile::tempdir().unwrap();
        let apt = node_dir(root.path(), "usr-bin");
        let recorded = node_dir(root.path(), "giap-node");
        let probe = Probe::new(&[(&apt, "v12.22.9"), (&recorded, "v22.20.0")]);

        let got = plan(
            Some(&path_of(&[&apt])),
            Some(recorded.clone()),
            Vec::new,
            |d| probe.ask(d),
        );
        assert_eq!(
            got,
            Some(Prepended {
                dir: recorded,
                recorded: true,
                version: Some("v22.20.0".into()),
                reason: Reason::OutsideRange {
                    on_path: Some("v12.22.9".into())
                },
            })
        );
    }

    #[test]
    fn an_odd_numbered_node_on_path_gives_way_too() {
        let root = tempfile::tempdir().unwrap();
        let brew = node_dir(root.path(), "brew");
        let recorded = node_dir(root.path(), "nvm-26");
        let probe = Probe::new(&[(&brew, "v25.8.2"), (&recorded, "v26.10.0")]);

        let got = plan(
            Some(&path_of(&[&brew])),
            Some(recorded.clone()),
            Vec::new,
            |d| probe.ask(d),
        )
        .expect("25 is outside the range");
        assert_eq!(got.dir, recorded);
    }

    #[test]
    fn a_node_on_path_inside_the_range_stays() {
        let root = tempfile::tempdir().unwrap();
        let own = node_dir(root.path(), "own");
        let recorded = node_dir(root.path(), "recorded");
        let probe = Probe::new(&[(&own, "v24.1.0"), (&recorded, "v22.20.0")]);

        let got = plan(Some(&path_of(&[&own])), Some(recorded), Vec::new, |d| {
            probe.ask(d)
        });
        assert_eq!(got, None, "the person's own Node is inside the range");
    }

    #[test]
    fn a_node_on_path_that_will_not_say_its_version_gives_way() {
        let root = tempfile::tempdir().unwrap();
        let broken = node_dir(root.path(), "broken-shim");
        let recorded = node_dir(root.path(), "recorded");
        let probe = Probe::new(&[(&recorded, "v24.3.0")]);

        let got = plan(
            Some(&path_of(&[&broken])),
            Some(recorded.clone()),
            Vec::new,
            |d| probe.ask(d),
        );
        assert_eq!(
            got.map(|p| (p.dir, p.reason)),
            Some((recorded, Reason::OutsideRange { on_path: None }))
        );
    }

    #[test]
    fn a_recorded_node_outside_the_range_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let apt = node_dir(root.path(), "usr-bin");
        let stale = node_dir(root.path(), "stale");
        let probe = Probe::new(&[(&apt, "v12.22.9"), (&stale, "v25.0.0")]);

        let got = plan(Some(&path_of(&[&apt])), Some(stale), Vec::new, |d| {
            probe.ask(d)
        });
        assert_eq!(got, None, "nothing better to put first");
    }

    #[test]
    fn a_recorded_node_goes_ahead_of_the_installs_on_a_bare_path() {
        let root = tempfile::tempdir().unwrap();
        let recorded = node_dir(root.path(), "recorded");
        let installed = node_dir(root.path(), "installed");
        let probe = Probe::new(&[(&recorded, "v22.20.0")]);

        let got = plan(
            Some(&path_of(&[root.path()])),
            Some(recorded.clone()),
            || vec![installed],
            |d| probe.ask(d),
        );
        assert_eq!(
            got,
            Some(Prepended {
                dir: recorded,
                recorded: true,
                version: Some("v22.20.0".into()),
                reason: Reason::NoneOnPath,
            })
        );
    }

    #[test]
    fn a_bare_path_with_a_bad_record_falls_back_to_the_installs() {
        let root = tempfile::tempdir().unwrap();
        let recorded = node_dir(root.path(), "recorded");
        let installed = node_dir(root.path(), "installed");
        let probe = Probe::new(&[(&recorded, "v12.22.9")]);

        let got = plan(
            Some(&path_of(&[root.path()])),
            Some(recorded),
            || vec![installed.clone()],
            |d| probe.ask(d),
        );
        assert_eq!(got.map(|p| (p.dir, p.recorded)), Some((installed, false)));
    }

    #[test]
    fn the_recorded_dir_already_first_on_path_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let recorded = node_dir(root.path(), "recorded");
        let probe = Probe::new(&[(&recorded, "v22.20.0")]);

        let got = plan(
            Some(&path_of(&[&recorded])),
            Some(recorded.clone()),
            Vec::new,
            |d| probe.ask(d),
        );
        assert_eq!(got, None);
        assert_eq!(probe.asked.get(), 0);
    }

    #[test]
    fn the_record_lives_where_the_script_writes_it() {
        assert_eq!(
            record_file_in(Some("/x/node-home".into()), Some("/home/nano".into())),
            Some(PathBuf::from("/x/node-home/.path"))
        );
        assert_eq!(
            record_file_in(None, Some("/home/nano".into())),
            Some(PathBuf::from("/home/nano/.giap/node/.path"))
        );
        assert_eq!(
            record_file_in(Some("".into()), Some("/home/nano".into())),
            Some(PathBuf::from("/home/nano/.giap/node/.path")),
            "an empty GIAP_NODE_HOME counts as unset, as it does in the script"
        );
        assert_eq!(record_file_in(None, None), None);
    }

    #[test]
    fn a_record_names_an_absolute_directory_with_node_in_it() {
        let root = tempfile::tempdir().unwrap();
        let good = node_dir(root.path(), "v22.20.0/bin");
        let record = root.path().join(".path");

        std::fs::write(&record, format!("{}\n", good.display())).unwrap();
        assert_eq!(recorded_dir(&record), Some(good.clone()));

        std::fs::write(&record, "v22.20.0/bin\n").unwrap();
        assert_eq!(recorded_dir(&record), None, "relative");

        std::fs::write(&record, format!("{}\n", root.path().display())).unwrap();
        assert_eq!(recorded_dir(&record), None, "no node in it");

        std::fs::write(&record, "").unwrap();
        assert_eq!(recorded_dir(&record), None, "empty");

        assert_eq!(recorded_dir(&root.path().join("missing")), None);
    }

    /// The script's own lists (`t_range_inside`, `t_range_outside` in node-setup.test.sh).
    const INSIDE: &[&str] = &[
        "v22.13.0", "22.13.0", "v22.13.1", "v22.99.9", "v24.0.0", "v24.11.0", "v26.0.0",
        "v26.10.0", "v27.1.0", "v30.0.0",
    ];
    const OUTSIDE: &[&str] = &[
        "v20.19.0", "v21.0.0", "v22.11.9", "v22.12.0", "v22.12.9", "v22.0.0", "v23.5.0", "v25.8.2",
        "v18.0.0", "", "garbage", "v", "22", "v12.22.9",
    ];

    #[test]
    fn the_range_is_the_repos() {
        for v in INSIDE {
            assert!(inside_range(v), "{v} should be inside");
        }
        for v in OUTSIDE {
            assert!(!inside_range(v), "[{v}] should be outside");
        }
    }

    #[test]
    fn the_range_is_the_one_the_script_checks() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/lib/node-setup.sh");
        if !script.is_file() {
            return;
        }
        let Ok(out) = std::process::Command::new("bash")
            .arg("-c")
            .arg(r#". "$1"; shift; for v in "$@"; do if node_version_ok "$v"; then echo in; else echo out; fi; done"#)
            .arg("node-setup")
            .arg(&script)
            .args(INSIDE.iter().chain(OUTSIDE))
            .output()
        else {
            return; // no bash here
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let said: Vec<&str> = text.lines().collect();
        let versions: Vec<&str> = INSIDE.iter().chain(OUTSIDE).copied().collect();
        assert_eq!(said.len(), versions.len(), "the script said: {text}");
        for (v, verdict) in versions.iter().zip(said) {
            assert_eq!(
                inside_range(v),
                verdict == "in",
                "[{v}]: the script says {verdict}, this file disagrees"
            );
        }
    }

    #[cfg(unix)]
    fn fake_node(root: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let node = dir.join("node");
        std::fs::write(&node, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn the_version_is_asked_of_the_binary() {
        let root = tempfile::tempdir().unwrap();
        let old = fake_node(root.path(), "old", "echo v12.22.9");
        assert_eq!(
            version_in(&old, PROBE_TIMEOUT),
            Some("v12.22.9".to_string())
        );

        let failing = fake_node(root.path(), "failing", "echo v22.20.0; exit 1");
        assert_eq!(version_in(&failing, PROBE_TIMEOUT), None, "it failed");

        let chatty = fake_node(root.path(), "chatty", "echo 'no version is set for node'");
        assert_eq!(version_in(&chatty, PROBE_TIMEOUT), None, "not a version");

        assert_eq!(version_in(&root.path().join("absent"), PROBE_TIMEOUT), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_node_that_hangs_is_given_up_on() {
        let root = tempfile::tempdir().unwrap();
        let stuck = fake_node(root.path(), "stuck", "exec sleep 30");
        let started = Instant::now();
        assert_eq!(version_in(&stuck, Duration::from_millis(200)), None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited {:?} for a node that never answers",
            started.elapsed()
        );
    }
}
