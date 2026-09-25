//! Stored reasoning (`session_thinking`) is shown to the user and never replayed to the model.
//! Only listed files may call `get_thinking_for_session`: a source scan, since every
//! prompt-building path can reach the port, and replaying costs the answer's decode reserve.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The call form: the bare symbol also appears in prose.
const READ_CALL: &str = "get_thinking_for_session(";
/// Writing isn't the hazard, but any prompt path touching this table is a warning sign.
const WRITE_CALL: &str = "add_thinking(";

/// Below this the walk has broken, not the tree shrunk.
const FLOOR_FILES: usize = 300;

/// The only files allowed to READ stored reasoning; the port and its impl hold the call form.
/// The one real caller is `routes.rs`'s `get_session_messages`, which reaches no model.
const READERS_ALLOWED: &[&str] = &[
    "crates/pond-core/src/user_data/ports/session_storage.rs",
    "crates/pond-infra/src/sqlite_session_storage.rs",
    "crates/pond-api/src/routes.rs",
];

/// The only files allowed to WRITE stored reasoning; `chat.rs` owns turn persistence.
const WRITERS_ALLOWED: &[&str] = &[
    "crates/pond-core/src/user_data/ports/session_storage.rs",
    "crates/pond-infra/src/sqlite_session_storage.rs",
    "crates/pond-core/src/shared/services/chat.rs",
];

/// Files that must contain the read call, so a rename can't empty the scan unnoticed.
const READERS_REQUIRED: &[&str] = &[
    "crates/pond-core/src/user_data/ports/session_storage.rs",
    "crates/pond-api/src/routes.rs",
];

/// Directories whose files build a prompt; a hit here is reported with its reason.
const PROMPT_BUILDING_PATHS: &[(&str, &str)] = &[
    (
        "models/services/context/",
        "the context pipeline -- trimming, compaction and budgeting all feed \
         the next prompt",
    ),
    (
        "shared/services/session_summary.rs",
        "the rolling summariser, whose output is prepended to every subsequent \
         turn",
    ),
    (
        "prompt_builder",
        "the prompt builder -- everything it assembles is sent to the model",
    ),
    (
        "prompts.rs",
        "the system-prompt templates, rendered into every turn",
    ),
    (
        "turn_trimmer",
        "the turn trimmer, which decides what survives into the next prompt",
    ),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

/// The file minus every `#[cfg(test)]` item, as in `egress_guard.rs`.
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

/// The source minus `//` comments, string literals intact; the port's docs name the call.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut cut = line.len();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name == "target" || name == "tests" || name.starts_with('.') {
                continue;
            }
            collect_rs(&path, root, out);
        } else if name.ends_with(".rs") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// (files scanned, files containing `needle` in production, comment-free source)
fn scan(needle: &str) -> (usize, BTreeSet<String>) {
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
        files.len() >= FLOOR_FILES,
        "scanned only {} .rs files under crates/ (floor {FLOOR_FILES}) -- the \
         walk has broken, not the tree shrunk. A guard that matches nothing \
         reports success.",
        files.len()
    );

    let mut hits = BTreeSet::new();
    for rel in &files {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
        let code = strip_line_comments(&production_source(&src));
        if code.contains(needle) {
            hits.insert(rel.clone());
        }
    }
    (files.len(), hits)
}

fn describe_prompt_path(file: &str) -> Option<&'static str> {
    PROMPT_BUILDING_PATHS
        .iter()
        .find(|(frag, _)| file.contains(frag))
        .map(|(_, why)| *why)
}

// -- the assertions -----------------------------------------------------------

#[test]
fn stored_reasoning_is_read_only_by_the_history_handler() {
    let (_, hits) = scan(READ_CALL);

    // Vacuity control first: with the known callers gone, the scan proves nothing.
    for required in READERS_REQUIRED {
        assert!(
            hits.contains(*required),
            "`{READ_CALL}` no longer appears in the production source of \
             {required}. Either the method was renamed -- in which case this \
             guard is matching a string that no longer exists and is certifying \
             an empty result -- or the history handler stopped replaying stored \
             reasoning to the UI. Update {READ_CALL} here, or delete the \
             feature properly. Files that did match: {hits:?}"
        );
    }

    let allowed: BTreeSet<&str> = READERS_ALLOWED.iter().copied().collect();
    let unexpected: Vec<String> = hits
        .iter()
        .filter(|f| !allowed.contains(f.as_str()))
        .map(|f| match describe_prompt_path(f) {
            Some(why) => format!("{f}\n      ^ this is {why}"),
            None => format!("{f}"),
        })
        .collect();

    assert!(
        unexpected.is_empty(),
        "stored reasoning text is being read outside the history handler:\n    \
         {}\n\n  `session_thinking` holds passages the model produced while \
         working out an answer -- superseded, unreviewed, and frequently wrong. \
         PAI-5's third invariant is that they are NEVER replayed into a prompt. \
         If this call is on a path that builds model input, the prompt now \
         carries the model's own discarded scratch work, which costs decode \
         tokens out of the same reserve the answer comes from and reintroduces \
         conclusions the turn already rejected.\n\n  If a new UI read genuinely \
         needs this, add the file to READERS_ALLOWED in this test AND say in \
         the commit message why it cannot reach a model.",
        unexpected.join("\n    ")
    );
}

#[test]
fn stored_reasoning_is_written_only_by_the_persistence_owner() {
    let (_, hits) = scan(WRITE_CALL);

    assert!(
        hits.contains("crates/pond-core/src/shared/services/chat.rs"),
        "`{WRITE_CALL}` no longer appears in ChatService. Nothing writes \
         reasoning text any more, and the read side, the migration and the \
         setting are all still in the tree describing a feature that does not \
         happen. Files that did match: {hits:?}"
    );

    let allowed: BTreeSet<&str> = WRITERS_ALLOWED.iter().copied().collect();
    let unexpected: Vec<&String> = hits
        .iter()
        .filter(|f| !allowed.contains(f.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "reasoning text is being written outside ChatService: {unexpected:?}. \
         ChatService is the sole owner of turn persistence -- a handler that \
         writes its own rows cannot key them to the assistant message id, which \
         ChatService mints, and will be silently dropped by the next refactor \
         exactly as the inline persistence blocks were."
    );
}

/// Asserts the setting itself reaches the builder in both handlers, not just that it's named.
#[test]
fn both_stream_handlers_hand_the_users_choice_to_the_persistence_owner() {
    let root = workspace_root();
    let src = std::fs::read_to_string(root.join("crates/pond-api/src/routes.rs"))
        .expect("routes.rs readable");
    let code = strip_line_comments(&production_source(&src));

    let with_thinking = code.matches(".with_thinking(").count();
    assert_eq!(
        with_thinking, 2,
        "expected exactly 2 `.with_thinking(` calls in routes.rs (one per \
         streaming chat handler); found {with_thinking}. Fewer means a handler \
         builds a ChatService that silently defaults to NOT persisting -- which \
         is the safe direction but means the user's setting does nothing on \
         that route. More means a third stream handler appeared and this guard \
         has not been told which."
    );

    // The argument, not just the call: `.with_thinking(true)` would ignore the setting.
    assert!(
        code.contains(".with_thinking(settings.persist_thinking)"),
        "the `/chat/stream` handler must pass `settings.persist_thinking` to \
         `.with_thinking(`, not a literal. A hardcoded `true` keeps every \
         reasoning passage the model has ever produced regardless of what the \
         user chose."
    );
    assert!(
        code.contains(".with_thinking(persist_thinking)")
            && code.contains("|s| s.persist_thinking"),
        "the `/agent/chat/stream` handler must read `persist_thinking` from the \
         settings repository and pass it through. This route reads no other \
         setting, so it is the one where a hardcoded literal would be easiest \
         to write and hardest to notice."
    );

    // The gate is inside `record_thinking`; uncalled, `persist_thinking = true` does nothing.
    let recorded = code.matches("record_thinking(").count();
    assert_eq!(
        recorded, 2,
        "expected exactly 2 `record_thinking(` calls in routes.rs (one per \
         `AgentStreamEvent::Thinking` arm); found {recorded}. Zero means the \
         thinking frames stream to the browser and are dropped, which is the \
         behaviour P6 exists to change, and every other assertion in this file \
         would still pass."
    );
}
