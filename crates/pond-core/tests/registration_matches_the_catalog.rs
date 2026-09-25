//! The builtin registration list and the tool-group catalog must describe the same set.
//! An uncatalogued builtin counts as a user-added MCP server: never narrowed, even for guests.
//! Parses source: registering needs the goose submodule, and `REGISTERED_EXTENSIONS` is a
//! `OnceLock`. In pond-core because CI only runs pond-core's tests.

use pond_core::mcp::domain::tool_group::{ORCHESTRATOR_EXTENSION, TOOLKIT_EXTENSION, TOOL_GROUPS};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // crates/pond-core -> repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn registration_source() -> String {
    let path = workspace_root().join("crates/pond-adapters-goose/src/giap_registration.rs");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    // Line comments out, so neither commented-out code nor prose can satisfy this guard.
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `register_builtin_extension(...)` call's first argument, resolved to its name.
/// An unresolvable argument panics rather than being dropped: `giap-toolkit` uses a const.
fn registered_extensions_from_source() -> Vec<String> {
    let src = registration_source();
    let mut names = Vec::new();
    let mut rest = src.as_str();
    const CALL: &str = "register_builtin_extension(";
    while let Some(i) = rest.find(CALL) {
        rest = &rest[i + CALL.len()..];
        // The first argument runs to the first comma at paren depth 0.
        let mut depth = 0usize;
        let mut end = rest.len();
        for (j, c) in rest.char_indices() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' if depth > 0 => depth -= 1,
                ')' if depth == 0 => {
                    end = j;
                    break;
                }
                ',' if depth == 0 => {
                    end = j;
                    break;
                }
                _ => {}
            }
        }
        let arg = rest[..end].trim();
        names.push(resolve_argument(arg));
        rest = &rest[end..];
    }
    names
}

fn resolve_argument(arg: &str) -> String {
    if let Some(literal) = arg.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return literal.to_string();
    }
    // A path to one of this crate's consts, matched on its last segment.
    let last = arg.rsplit("::").next().unwrap_or(arg).trim();
    match last {
        "TOOLKIT_EXTENSION" => TOOLKIT_EXTENSION.to_string(),
        "ORCHESTRATOR_EXTENSION" => ORCHESTRATOR_EXTENSION.to_string(),
        _ => panic!(
            "giap_registration.rs registers an extension under `{arg}`, which this test cannot \
             resolve to a name. Do NOT delete the call from the parser's view and do not make \
             the parser skip it -- an unresolved registration is exactly how the extension count \
             was recorded wrong twice. Add the const to `resolve_argument`."
        ),
    }
}

/// Vacuity control with a deliberately loose bound; the set equality below pins the rest.
#[test]
fn the_parser_actually_finds_registrations() {
    let found = registered_extensions_from_source();
    assert!(
        found.len() > 10,
        "the registration parser found only {} extension(s) in giap_registration.rs. That is a \
         broken parser, not a shrunken registry -- every other assertion in this file would pass \
         vacuously.",
        found.len()
    );
    assert!(
        found.iter().any(|n| n == TOOLKIT_EXTENSION),
        "the parser did not resolve the one registration that goes through a const. That is the \
         exact blind spot that made this programme's extension count wrong twice."
    );
    assert!(
        found.iter().all(|n| n.starts_with("giap-")),
        "resolved a registration to something that is not a giap extension name: {found:?}"
    );
}

/// A catalogued group nothing registers is a dead end the model can still be offered.
#[test]
fn every_registered_extension_is_in_the_catalog_and_the_reverse() {
    let registered: BTreeSet<String> = registered_extensions_from_source().into_iter().collect();
    let catalogued: BTreeSet<String> = TOOL_GROUPS
        .iter()
        .map(|g| g.extension.to_string())
        .collect();

    let uncatalogued: Vec<&String> = registered.difference(&catalogued).collect();
    assert!(
        uncatalogued.is_empty(),
        "registered but not in TOOL_GROUPS: {uncatalogued:?}. An uncatalogued extension is \
         treated as a user-added MCP server, which selection never narrows and neither denylist \
         can remove."
    );

    let unregistered: Vec<&String> = catalogued.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "in TOOL_GROUPS but never registered: {unregistered:?}. The scorer can select it and \
         giap-toolkit can be asked to enable it, and it does not exist."
    );
}

/// Pinned here because `AGENTS.md` is untracked; update its sentence in the same change.
#[test]
fn the_extension_count_is_pinned() {
    const CLAIMED: usize = 11;

    // The registration count, which the test above ties to the catalog's.
    let registered = registered_extensions_from_source().len();
    assert_eq!(
        CLAIMED, registered,
        "this test claims {CLAIMED} builtin extensions; giap_registration.rs has {registered} \
         `register_builtin_extension(` call sites. Count the CALL SITES -- the import line has \
         no open paren, so nothing is subtracted from the grep, and two call sites pass a const \
         rather than a string literal."
    );
}

// ── The third list in the family ───────────────────────────────────────────

/// `dispatcher.rs`'s routed prefixes, as extension names.
/// Read from `prefix: PREFIX_X` uses, not declarations: `PREFIX_AUDIT` is declared but unrouted.
fn dispatcher_routed_extensions() -> BTreeSet<String> {
    let path = workspace_root().join("crates/pond-mcp-server/src/dispatcher.rs");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let code: String = src
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // const PREFIX_WEATHER: &str = "giap-weather__";
    let mut by_const: std::collections::BTreeMap<String, String> = Default::default();
    for line in code.lines() {
        let Some(rest) = line.trim().strip_prefix("const PREFIX_") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once(':') else {
            continue;
        };
        let Some(open) = tail.find('"') else { continue };
        let Some(close) = tail[open + 1..].find('"') else {
            continue;
        };
        let value = &tail[open + 1..open + 1 + close];
        by_const.insert(
            format!("PREFIX_{}", name.trim()),
            value.trim_end_matches("__").to_string(),
        );
    }
    assert!(
        by_const.len() >= 6,
        "found {} PREFIX_* consts in dispatcher.rs — the parser is broken, so an \
         empty result would prove nothing",
        by_const.len()
    );

    let mut routed = BTreeSet::new();
    for (at, _) in code.match_indices("prefix: PREFIX_") {
        let tail = &code[at + "prefix: ".len()..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        let ident = &tail[..end];
        let ext = by_const.get(ident).unwrap_or_else(|| {
            panic!("dispatcher.rs routes `{ident}`, which is not a PREFIX_* const it declares")
        });
        routed.insert(ext.clone());
    }
    routed
}

/// Extensions the direct dispatcher deliberately doesn't route, each with its reason.
const DISPATCHER_EXCLUSIONS: &[(&str, &str)] = &[
    (
        "giap-sensors",
        "needs the SensorStorage installed by pond-server's init_sensor_deps",
    ),
    (
        "giap-context",
        "needs the context deps AND a resolvable caller; these routes carry no \
         engine session, so scope_for could only ever refuse",
    ),
    (
        "giap-toolkit",
        "widens a SESSION's tool selection, and these routes have no session",
    ),
    (
        "giap-orchestrator",
        "delegation needs a live turn authority, which only a chat turn publishes",
    ),
];

/// `dispatcher.rs` is a second live dispatch path (the direct `/api/v1` tool routes).
/// Its safety is `DIRECT_DISPATCH_ALLOWLIST`; this only checks the lists agree on what exists.
#[test]
fn the_dispatcher_routes_a_documented_subset_of_the_registered_extensions() {
    let registered: BTreeSet<String> = registered_extensions_from_source().into_iter().collect();
    let routed = dispatcher_routed_extensions();
    let excluded: BTreeSet<String> = DISPATCHER_EXCLUSIONS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();

    assert!(
        !routed.is_empty(),
        "the dispatcher routes nothing — parser broken"
    );

    // 1. Nothing routed that isn't registered: a phantom prefix never matches a real tool.
    for ext in &routed {
        assert!(
            registered.contains(ext),
            "dispatcher.rs routes '{ext}', which giap_registration.rs does not register"
        );
    }

    // 2. Nothing both excluded and routed.
    for ext in &excluded {
        assert!(
            !routed.contains(ext),
            "'{ext}' is on DISPATCHER_EXCLUSIONS and is routed anyway"
        );
    }

    // 3. Every registered extension is routed, or excluded WITH a reason.
    let unexplained: Vec<&String> = registered
        .iter()
        .filter(|e| !routed.contains(*e) && !excluded.contains(*e))
        .collect();
    assert!(
        unexplained.is_empty(),
        "registered but neither routed by dispatcher.rs nor listed in \
         DISPATCHER_EXCLUSIONS with a reason: {unexplained:?}\n\
         Route it, or say why it cannot be — a silent omission is \
         indistinguishable from a forgotten one, which is how this drifted."
    );

    // 4. No exclusion names an extension that no longer exists; stale entries hide drift.
    for (ext, reason) in DISPATCHER_EXCLUSIONS {
        assert!(
            registered.contains(&ext.to_string()),
            "DISPATCHER_EXCLUSIONS names '{ext}', which is not registered at all"
        );
        assert!(
            !reason.trim().is_empty(),
            "'{ext}' is excluded with no reason"
        );
    }
}
