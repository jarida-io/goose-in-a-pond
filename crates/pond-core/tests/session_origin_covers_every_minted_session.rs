//! Every production file that mints a `sessions` row must have a decided [`SessionOrigin`].
//! `SessionOrigin::of` is a prefix deny-list (a person's id has no shape), so this is its
//! audit: an unlisted minter would be published as a person arriving. Per file, not per call.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pond_core::shared::domain::session_activity::{SessionOrigin, POND_AUTHORED_SESSION_PREFIXES};

/// The call form, not the bare symbol, which also appears in the port and in prose.
const MINT_CALL: &str = ".create_session(";

/// Below this the walk has broken, not the tree shrunk.
const MIN_FILES_SCANNED: usize = 300;

/// Production files that mint session rows, and what their rows mean.
/// On failure, check the new entry's reason against [`POND_AUTHORED_SESSION_PREFIXES`].
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

/// The file minus every `#[cfg(test)]` item (as in `egress_guard.rs`): test fixtures mint too.
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
        // A single-line item has no block: drop just it, or the search eats up to the next brace.
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

/// Minting files not in [`KNOWN_MINTERS`]; separate from the walk so tests can feed it fakes.
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
            // Tests, examples and benches never run inside the server the observer watches.
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

/// Vacuity control: without this, a broken detector passes the test above by seeing nothing.
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

/// Vacuity control on fake sources, since a clean tree can't show the guard failing.
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

    // ... and not once the minting moves into `#[cfg(test)]`, so the slicer does its job.
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
