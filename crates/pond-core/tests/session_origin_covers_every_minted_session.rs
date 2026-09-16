//! Guard: every place that mints a row in `pond_system.db`'s `sessions` table
//! is a place whose origin [`SessionOrigin`] has been decided (PAI-7 P1).
//!
//! `SessionOrigin::of` is a deny-list -- `POND_AUTHORED_SESSION_PREFIXES`, one
//! entry, the scheduler's `sched-` -- because a person's session id has no
//! shape to require: it is whatever client opened the conversation chose. A
//! deny-list of today's prefixes is worth exactly as much as the audit behind
//! it, and an audit written into a comment rots in a week. This is that audit,
//! executable.
//!
//! What it catches: a new background feature that creates its own session and
//! does not say so. The session-activity observer publishes every unclassified
//! session as a person arriving, and PAI-7 exists to decide whether to
//! interrupt a *person* -- presence fabricated from the pond's own work is
//! worse than no presence signal at all, because P4's reviewer acts on it with
//! confidence.
//!
//! What it does not catch: a second call added inside a file that already
//! mints. The unit of classification is the file, because that is the unit
//! whose reason is written down.
//!
//! A runtime walk rather than `include_str!`, for the same reason as
//! `egress_guard.rs`: it has to see a file that does not exist yet.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pond_core::shared::domain::session_activity::{SessionOrigin, POND_AUTHORED_SESSION_PREFIXES};

/// The call form. Not the bare symbol: `create_session` appears in the port
/// definition, in doc comments, and in this file's own prose, and a guard
/// satisfied by comment prose is the recorded failure shape this tree has hit
/// most often.
const MINT_CALL: &str = ".create_session(";

/// Below this the walk has broken, not the tree shrunk.
const MIN_FILES_SCANNED: usize = 300;

/// Every production file that mints a session row, and what its rows mean.
///
/// Paths are workspace-relative with forward slashes. The reason is the point
/// of the entry: it is what a reader checks against
/// [`POND_AUTHORED_SESSION_PREFIXES`] when this test fails.
const KNOWN_MINTERS: &[(&str, &str)] = &[
    (
        "crates/pond-api/src/routes.rs",
        "an HTTP request opened it; the id is the caller's or a fresh UUID -- a person",
    ),
    (
        "crates/pond-server/src/main.rs",
        "the terminal voice/chat CLI's own session -- a person, in another process",
    ),
    (
        "crates/pond-server/src/schedule_executors.rs",
        "MACHINE: `sched-{task_id}-{unix_ts}`, one per schedule fire and per rule \
         AgentPrompt action. Covered by POND_AUTHORED_SESSION_PREFIXES.",
    ),
    (
        "crates/pond-adapters-goose/src/goose_agent.rs",
        "goose's own `session_manager`, which writes goose's sessions.db -- a different \
         store that the observer never reads",
    ),
    (
        "crates/pond-adapters-goose/src/extension_manager.rs",
        "the same reason as `goose_agent.rs` above -- this is the SAME goose \
         `SessionManager`, and the call simply moved here. It mints one row, the \
         `giap-extensions` session that MCP extensions are added to, resolved by name \
         and re-created only when that row is missing. It lands in goose's sessions.db, \
         which the observer never reads: every activity-watcher path is fed \
         `SqliteSessionStorage::new(db.system.clone())` (pond-server/src/main.rs), not \
         the forwarding `session_adapter`. So it cannot fabricate presence -- and its \
         id is goose's `YYYYMMDD_n`, which would look human if it ever were read.",
    ),
    (
        "crates/pond-adapters-goose/src/session_adapter.rs",
        "the same reason as `goose_agent.rs` above, one layer down: this is a \
         `SessionStorage` impl that FORWARDS to goose's `session_manager`, so every id \
         it mints lands in goose's sessions.db. It also never chooses an id -- \
         `create_session` takes one from its caller, and that caller is already \
         classified here. The observer reads `pond_system.db` and cannot see any of it.",
    ),
];

fn workspace_root() -> PathBuf {
    // crates/pond-core -> repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

/// The file with every `#[cfg(test)]` ITEM removed, and nothing else.
///
/// Lifted from `egress_guard.rs :: production_source`, including the blind spot
/// that guard's own mutation run found: "everything before the first
/// `#[cfg(test)]`" looks like the convention and is not one -- 11 files here
/// carry more than one, and truncating discards the production code between
/// them.
///
/// Test code has to come out or this guard is about fixtures: nearly every
/// session-storage test calls `create_session` to set one up, and classifying
/// those files would bury the two production entries that matter in a list
/// nobody reads.
fn production_source(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.trim_start().starts_with("#[cfg(test)]") {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // A single-line item has no block to close; drop just the item it
        // annotates, or the search below eats up to the next item's brace.
        let opens_block = lines
            .get(i + 1)
            .map(|l| l.trim_end().ends_with('{'))
            .unwrap_or(false);
        if !opens_block {
            i += 2;
            continue;
        }
        let closer = format!("{}}}", " ".repeat(indent));
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim_end() != closer {
            j += 1;
        }
        i = j + 1;
    }
    out
}

/// The decision, over `(path, source)` pairs: which files mint a session row
/// and are not in [`KNOWN_MINTERS`].
///
/// Split out from the walk so it can be run against sources that do not exist
/// on disk -- a guard that only ever sees a clean tree has never been shown to
/// fail.
fn unclassified_minters(sources: &[(String, String)]) -> Vec<String> {
    let known: BTreeSet<&str> = KNOWN_MINTERS.iter().map(|(path, _)| *path).collect();
    sources
        .iter()
        .filter(|(path, src)| {
            production_source(src).contains(MINT_CALL) && !known.contains(path.as_str())
        })
        .map(|(path, _)| path.clone())
        .collect()
}

fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // `tests/` holds integration tests, which are tests by definition.
            // `examples/` and `benches/` are developer harnesses run by hand:
            // nothing the server spawns is in them, so a session they create
            // exists only in whatever pond the developer pointed them at, and
            // never reaches the observer running in a serving process.
            // (`pond-adapters-goose/examples/tool_calling_test.rs` is the one
            // that made this exclusion explicit -- it was found by this guard
            // failing, which is the first evidence that the walk works.)
            if name == "target"
                || name == "tests"
                || name == "examples"
                || name == "benches"
                || name.starts_with('.')
            {
                continue;
            }
            collect_rs(&path, root, out);
        } else if name.ends_with(".rs") {
            out.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn scan() -> Vec<(String, String)> {
    let root = workspace_root();
    let crates = root.join("crates");
    assert!(
        crates.is_dir(),
        "no {} -- this guard scans the workspace and cannot run without it",
        crates.display()
    );

    let mut files = Vec::new();
    collect_rs(&crates, &root, &mut files);
    files.sort();
    assert!(
        files.len() >= MIN_FILES_SCANNED,
        "scanned only {} files, expected at least {MIN_FILES_SCANNED} -- the walk has broken, \
         not the tree shrunk",
        files.len()
    );

    files
        .into_iter()
        .filter_map(|rel| {
            std::fs::read_to_string(root.join(&rel))
                .ok()
                .map(|src| (rel, src))
        })
        .collect()
}

/// The audit itself.
#[test]
fn every_production_session_minter_has_a_decided_origin() {
    let sources = scan();
    let unclassified = unclassified_minters(&sources);
    assert!(
        unclassified.is_empty(),
        "these files mint a session row and no origin has been decided for the ids they mint: \
         {unclassified:?}\n\
         The session-activity observer (PAI-7 P1) publishes every session it cannot recognise as \
         the pond's own as a person arriving, and P4 will interrupt somebody on that. If the pond \
         creates these for its own background work, give the id a prefix in \
         POND_AUTHORED_SESSION_PREFIXES ({POND_AUTHORED_SESSION_PREFIXES:?}); if a person does, \
         add the file to KNOWN_MINTERS with the reason."
    );
}

/// Vacuity control 1: the detector still finds the site the deny-list was
/// written against. If this stops matching, the test above passes because it
/// sees nothing at all.
#[test]
fn the_scan_still_finds_the_scheduler_minting_its_sessions() {
    let sources = scan();
    let scheduler = sources
        .iter()
        .find(|(path, _)| path == "crates/pond-server/src/schedule_executors.rs")
        .expect("the scheduler's executor file is in the workspace");
    assert!(
        production_source(&scheduler.1).contains(MINT_CALL),
        "the scheduler no longer appears to mint sessions -- either the call form moved (and \
         this guard is now blind) or the scheduler stopped creating them (and \
         POND_AUTHORED_SESSION_PREFIXES may be dead)"
    );

    // And the id it mints is still classified as the pond's own.
    assert_eq!(
        SessionOrigin::of("sched-morning-summary-1700000000"),
        SessionOrigin::Machine
    );
}

/// Vacuity control 2: the decision really does report a file it has not been
/// told about. Run against sources that are not on disk, because a guard whose
/// only input is a clean tree has never been shown to fail.
#[test]
fn a_new_background_feature_that_mints_sessions_is_reported() {
    let clean = vec![
        (
            "crates/pond-server/src/schedule_executors.rs".to_string(),
            "storage.create_session(id).await".to_string(),
        ),
        (
            "crates/pond-server/src/harmless.rs".to_string(),
            "// nothing to see".to_string(),
        ),
    ];
    assert!(
        unclassified_minters(&clean).is_empty(),
        "a classified minter was reported as unclassified"
    );

    let mut with_a_newcomer = clean;
    with_a_newcomer.push((
        "crates/pond-server/src/proactive_reviewer.rs".to_string(),
        "let id = format!(\"review-{n}\");\nstorage.create_session(id).await;".to_string(),
    ));
    assert_eq!(
        unclassified_minters(&with_a_newcomer),
        vec!["crates/pond-server/src/proactive_reviewer.rs".to_string()],
        "a new file minting sessions was not reported, so this guard cannot see the regression \
         it exists for"
    );

    // ... and the same file with its minting inside `#[cfg(test)]` is not
    // reported, so the slicer is doing the work claimed for it rather than
    // matching everything.
    let mut only_in_tests = with_a_newcomer;
    only_in_tests.pop();
    only_in_tests.push((
        "crates/pond-server/src/proactive_reviewer.rs".to_string(),
        "#[cfg(test)]\nmod tests {\n    storage.create_session(id).await;\n}\n".to_string(),
    ));
    assert!(
        unclassified_minters(&only_in_tests).is_empty(),
        "a fixture in a test module was reported as a production session minter"
    );
}
