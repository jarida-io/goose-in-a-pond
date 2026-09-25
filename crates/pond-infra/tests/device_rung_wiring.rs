//! The device rung is wired, and a principal's device id can only come from the pond's token.
//! `include_str!` adds no dependency edge, so this runs in CI's fast pass (no pond-server tests).

use std::path::{Path, PathBuf};

const MIDDLEWARE: &str = include_str!("../../pond-api/src/middleware/mod.rs");
const ROUTES: &str = include_str!("../../pond-api/src/routes.rs");
const POLICY: &str = include_str!("../../pond-core/src/security/ports/policy.rs");
const HANDSHAKE_PORT: &str = include_str!("../../pond-core/src/security/ports/handshake.rs");

/// The workspace `crates/` dir, walked so a guard also sees files added after it was written.
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/pond-infra always has a parent")
        .to_path_buf()
}

fn rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // A `target/` under a crate is build output, never source.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(body) = std::fs::read_to_string(&path) {
                    out.push((path, body));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&crates_dir(), &mut out);
    out
}

/// The shipped part of a file: everything before its `#[cfg(test)] mod`. Not the first
/// `#[cfg(test)]`: `middleware/mod.rs` has a `#[cfg(test)] fn` above `auth_middleware`.
fn production_slice(src: &str) -> &str {
    let mut from = 0usize;
    while let Some(at) = src[from..].find("#[cfg(test)]") {
        let start = from + at;
        let after = &src[start + "#[cfg(test)]".len()..];
        if after.trim_start().starts_with("mod ") {
            return &src[..start];
        }
        from = start + 1;
    }
    src
}

fn is_test_path(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == "tests" || c.as_os_str() == "benches")
}

#[test]
fn the_files_this_test_reads_are_the_ones_it_thinks_they_are() {
    assert!(
        MIDDLEWARE.contains("pub async fn auth_middleware"),
        "MIDDLEWARE is not pond-api/src/middleware/mod.rs, or the auth middleware has moved"
    );
    assert!(
        ROUTES.contains("async fn resolve_turn_scope"),
        "ROUTES is not pond-api/src/routes.rs, or the turn resolver has moved"
    );
    assert!(
        POLICY.contains("pub struct Principal"),
        "POLICY is not the file Principal lives in"
    );
    assert!(
        HANDSHAKE_PORT.contains("pub trait Handshake"),
        "HANDSHAKE_PORT is not the Handshake port"
    );
    let sources = rust_sources();
    assert!(
        sources.len() > 200,
        "the source walk found only {} files, which is not this workspace -- every walking \
         assertion below would pass by matching nothing",
        sources.len()
    );
    assert!(
        sources
            .iter()
            .any(|(p, _)| p.ends_with("pond-infra/src/sqlite_handshake.rs")),
        "the source walk did not find sqlite_handshake.rs, so it cannot see the one adapter \
         that matters"
    );
}

// ── The device's provenance ────────────────────────────────────────────────

/// One production `with_device` call: the auth middleware's, fed the token lookup's answer.
#[test]
fn the_device_on_a_principal_can_only_have_come_from_the_token() {
    let mut sites: Vec<(PathBuf, String)> = Vec::new();
    for (path, body) in rust_sources() {
        if is_test_path(&path) {
            continue;
        }
        for line in production_slice(&body).lines() {
            if line.contains(".with_device(") {
                sites.push((path.clone(), line.trim().to_string()));
            }
        }
    }

    assert_eq!(
        sites.len(),
        1,
        "`with_device` is called {} times in production source, not once: {sites:#?}. \
         The device id feeds `IdentificationSource::PairedDevice`, which outranks face and \
         explicit identification, so every place it can be set is a place a client-supplied \
         value could outrank every proof the pond can make. There is one door: the auth \
         middleware, from `Handshake::caller_for_token`.",
        sites.len()
    );

    let (path, line) = &sites[0];
    assert!(
        path.ends_with("pond-api/src/middleware/mod.rs"),
        "the only `with_device` call is in {path:?}, not the auth middleware. Outside the auth \
         layer there is no token to have proved anything, so whatever that argument is, the pond \
         did not issue it."
    );
    // Match the argument exactly: `asserted.unwrap_or(caller.device_id)` also contains it.
    let argument = line
        .split_once(".with_device(")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once(')'))
        .map(|(arg, _)| arg.trim())
        .unwrap_or("");
    assert_eq!(
        argument, "caller.device_id",
        "the auth middleware sets the device from `{argument}` rather than from the token \
         lookup's own `caller.device_id`, in `{line}`. Anything else -- a header, a body field, \
         a query parameter, or a fallback chain that prefers one -- is a device id the CLIENT \
         chose, and PairedDevice outranks every rung below it."
    );
    assert!(
        line.contains("Principal::token("),
        "the device is attached to something other than a token principal in `{line}`. Loopback \
         and internal principals presented no token, so there is no device the pond issued them."
    );
}

/// Also catches a device header parsed into a local, which the argument check can't see.
#[test]
fn the_auth_middleware_never_reads_a_device_from_the_client() {
    let production = production_slice(MIDDLEWARE);
    let mentions: Vec<&str> = production
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("device_id") && !line.starts_with("//"))
        .collect();
    assert!(
        !mentions.is_empty(),
        "the auth middleware mentions no device at all, so PAI-1 P9's identity half is not \
         wired and every turn falls through to explicit/face/guest as it did before"
    );
    for line in &mentions {
        assert!(
            line.contains("caller.device_id"),
            "the auth middleware derives a device from `{line}`. The only permitted source is \
             `Handshake::caller_for_token`, which reads the token THIS POND issued at pairing."
        );
    }
    for forbidden in [
        "x-device-id",
        "X-Device-Id",
        "X-DEVICE-ID",
        "\"device\"",
        "device_id=",
    ] {
        assert!(
            !production.contains(forbidden),
            "the auth middleware contains {forbidden:?}, which is a client-controlled spelling \
             of a device id"
        );
    }
}

/// The bypass authenticates nobody, so a "last-paired device" fallback would forge a member.
#[test]
fn the_loopback_bypass_carries_no_device() {
    let production = production_slice(MIDDLEWARE);
    let from = production
        .find("if dev_allow_loopback()")
        .expect("the dev loopback bypass has moved or been renamed; this guard is blind");
    let to = production
        .find("let token = extract_bearer_token")
        .expect("the bearer-token extraction has moved; the slice below is not the bypass");
    assert!(
        from < to,
        "the loopback bypass no longer precedes token extraction, so this slice is not the \
         bypass block"
    );
    let bypass = &production[from..to];
    assert!(
        bypass.contains("Principal::loopback()"),
        "the bypass block does not insert a loopback principal; this slice is not what it \
         claims to be and the absence below proves nothing"
    );
    assert!(
        !bypass.contains("with_device"),
        "the dev loopback bypass attaches a device. It returns before any token is read, so \
         whatever it attached was not proved by the pond -- and PairedDevice outranks every \
         other rung, so every local request would speak as that device's member."
    );
    assert!(
        !bypass.contains("caller_for_token"),
        "the dev loopback bypass looks a caller up. There is no token on this path to look one \
         up by."
    );
}

#[test]
fn every_principal_starts_with_no_device() {
    let production = production_slice(POLICY);
    assert_eq!(
        production.matches("device_id: None,").count(),
        3,
        "the three `Principal` constructors (loopback, internal, token) must each start \
         `device_id` at None. A constructor that starts it anywhere else hands a device to a \
         caller that presented no token."
    );
    assert!(
        !production.contains("device_id: Some("),
        "something in the policy port constructs a Principal with a device already on it"
    );
}

// ── The rung is reachable ──────────────────────────────────────────────────

/// `caller_for_token` defaults to `Ok(None)`, so a missed override silently disables the rung.
#[test]
fn every_handshake_adapter_answers_who_a_token_belongs_to() {
    // (type name, why it legitimately cannot answer)
    const ALLOWED_SILENT: [(&str, &str); 3] = [
        (
            "RejectingHandshake",
            "a pond-api test fixture that refuses every verify, so it issues no token and has \
             no device to name",
        ),
        (
            "MockHandshake",
            "in-memory test double with no session_tokens table; used only by pond-api's \
             integration tests, never wired by pond-server",
        ),
        (
            "AcceptingHandshake",
            "a pond-api test fixture that accepts every token without issuing one, so it has \
             no device to name",
        ),
    ];

    let mut found: Vec<(String, bool, PathBuf)> = Vec::new();
    for (path, body) in rust_sources() {
        let mut from = 0usize;
        while let Some(at) = body[from..].find("impl Handshake for ") {
            let start = from + at;
            let header = body[start..].lines().next().unwrap_or("");
            let name = header
                .trim_start_matches("impl Handshake for ")
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            // Impls sit at column 0 here, so the first bare `}` line closes this one.
            let rest = &body[start..];
            let end = rest.find("\n}\n").map(|e| e + 3).unwrap_or(rest.len());
            let block = &rest[..end];
            found.push((name, block.contains("fn caller_for_token"), path.clone()));
            from = start + end;
        }
    }

    assert!(
        !found.is_empty(),
        "no `impl Handshake for` was found anywhere in the workspace, so this guard is blind"
    );

    // Vacuity control: the walker can see a positive case.
    let sqlite = found
        .iter()
        .find(|(name, _, _)| name == "SqliteHandshakeAdapter")
        .expect("SqliteHandshakeAdapter no longer implements Handshake, or the walker is broken");
    assert!(
        sqlite.1,
        "SqliteHandshakeAdapter does not override `caller_for_token`, so every request on every \
         real pond carries no device, so `IdentificationSource::PairedDevice` -- the strongest \
         rung of identity_resolution::resolve -- is unreachable. Nothing errors; the pond simply \
         stops knowing whose phone it is talking to."
    );

    for (name, answers, path) in &found {
        if *answers {
            continue;
        }
        let excuse = ALLOWED_SILENT.iter().find(|(allowed, _)| allowed == name);
        assert!(
            excuse.is_some(),
            "`{name}` (in {path:?}) implements Handshake and does not answer \
             `caller_for_token`, so every request it authenticates carries no device and PAI-1 \
             P9's rung is silently off for it. Override it, or add it to ALLOWED_SILENT with the \
             reason it cannot."
        );
    }

    // A stale entry would pre-excuse whatever later takes that name.
    for (allowed, _) in ALLOWED_SILENT {
        assert!(
            found.iter().any(|(name, _, _)| name == allowed),
            "ALLOWED_SILENT names `{allowed}`, which implements Handshake nowhere in the \
             workspace. Remove the entry."
        );
    }
}

/// A separate `client_id_for_token` could disagree, and would hide a lost device override.
#[test]
fn the_client_id_lookup_rides_on_the_caller_lookup() {
    assert!(
        HANDSHAKE_PORT.contains("self\n            .caller_for_token(token)")
            || HANDSHAKE_PORT.contains("self.caller_for_token(token)"),
        "`client_id_for_token` no longer delegates to `caller_for_token`. Two independent \
         lookups over the same row can disagree, and an adapter that loses the device override \
         would keep answering client ids -- so nothing would notice PAI-1 P9 had gone inert."
    );
    for (path, body) in rust_sources() {
        if is_test_path(&path) || path.ends_with("security/ports/handshake.rs") {
            continue;
        }
        assert!(
            !production_slice(&body).contains("fn client_id_for_token"),
            "{path:?} overrides `client_id_for_token`. The port derives it; an override is a \
             second answer that can disagree with `caller_for_token` and that survives the \
             device lookup being deleted."
        );
    }
}

// ── The turn is threaded ───────────────────────────────────────────────────

/// The device arrives as a `ProvenDevice`, which only the auth layer can produce.
#[test]
fn the_turn_resolver_is_fed_from_the_device_rung() {
    assert!(
        ROUTES.contains("paired_device_profile: device_rung.profile_id(),"),
        "`resolve_turn_scope` no longer feeds `paired_device_profile` from the device rung. \
         PAI-1's strongest rung is unreachable again and every turn falls through to \
         explicit/face/guest."
    );
    assert!(
        !ROUTES.contains("paired_device_profile: None"),
        "something in routes.rs still hardcodes `paired_device_profile: None`, so at least one \
         turn resolves as if no device had ever been paired"
    );
    assert!(
        ROUTES.contains("device: &ProvenDevice,"),
        "`resolve_turn_scope` no longer takes a `ProvenDevice`. A `&str` there can be satisfied \
         by a header, and PairedDevice outranks every proof the pond can make."
    );
}

#[test]
fn every_turn_resolution_is_handed_a_device() {
    // Exclude the definition by its `async fn ` prefix: shape filters also drop wrapped calls.
    let mut definitions = 0usize;
    let mut call_sites: Vec<String> = Vec::new();
    for (at, _) in ROUTES.match_indices("resolve_turn_scope(") {
        if ROUTES[..at].ends_with("async fn ") {
            definitions += 1;
            continue;
        }
        let window_end = (at + 120).min(ROUTES.len());
        let window = ROUTES[at..window_end]
            .split(").await")
            .next()
            .unwrap_or("")
            .to_string();
        call_sites.push(window);
    }

    assert_eq!(
        definitions, 1,
        "expected exactly one `async fn resolve_turn_scope`; found {definitions}. Two \
         resolvers is two answers to whose turn it is, and the deeper one wins silently."
    );
    assert!(
        call_sites.len() >= 4,
        "only {} call sites of `resolve_turn_scope` were found in routes.rs, which is fewer \
         than the handlers that resolve a turn -- this guard has gone blind: {call_sites:#?}",
        call_sites.len()
    );
    for call in &call_sites {
        // Exact third argument: `&ProvenDevice::none()` compiles and silently turns the rung off.
        let third = call
            .split_once('(')
            .map(|(_, args)| args)
            .unwrap_or("")
            .split(',')
            .nth(2)
            .map(|arg| arg.trim().trim_start_matches('&'))
            .unwrap_or("");
        assert_eq!(
            third, "device",
            "`{call}` resolves a turn with `{third}` where the request's own device belongs. \
             A locally-constructed `ProvenDevice` names nothing the pond proved, so that route \
             cannot identify a speaker by their paired phone however the rest of this is wired."
        );
    }
}

#[test]
fn handlers_take_the_device_from_the_principal() {
    assert!(
        ROUTES.contains("ProvenDevice::from_principal(principal)"),
        "routes.rs no longer builds a ProvenDevice from a Principal; whatever it builds one \
         from now is the thing to argue about"
    );
    let readers = ROUTES.matches("proven_device(principal.as_ref())").count();
    assert!(
        readers >= 5,
        "only {readers} handlers read the device off their principal. `resolve_turn_scope` has \
         five threading points (chat, chat_stream, run_recipe, agent_chat_stream, and the two \
         proposal routes through `proposal_caller`); a handler that stopped taking a Principal \
         resolves every turn as if no device existed."
    );
}

#[test]
fn no_request_dto_on_a_turn_route_has_a_device_field() {
    for dto in [
        "struct ChatRequest {",
        "struct RunRecipeRequest {",
        "struct DecideProposalRequest {",
        "struct ProposalCallerQuery {",
    ] {
        let at = ROUTES
            .find(dto)
            .unwrap_or_else(|| panic!("{dto} has been renamed or removed; this guard is blind"));
        let rest = &ROUTES[at..];
        let end = rest
            .find("\n}")
            .unwrap_or_else(|| panic!("{dto} has no closing brace at column 0"));
        let body = &rest[..end];
        assert!(
            !body.contains("device"),
            "{dto} has grown a device field:\n{body}\nA device id in a request body is the \
             client's own word for itself, and PairedDevice outranks face and explicit \
             identification -- it would outrank every proof the pond can make."
        );
    }
}

// ── The assembly point, which nothing else can reach ───────────────────────

/// Overlaps the literal check above, but survives rustfmt reflows and covers every file.
#[test]
fn the_only_thing_that_can_fill_the_paired_device_rung_is_the_device_rung() {
    let mut sites: Vec<(PathBuf, String, String)> = Vec::new();
    for (path, body) in rust_sources() {
        if is_test_path(&path) {
            continue;
        }
        for line in production_slice(&body).lines() {
            let line = line.trim();
            // The field DECLARATION, not a site that fills it.
            if !line.starts_with("paired_device_profile:") || line.ends_with("Option<&'a str>,") {
                continue;
            }
            let argument = line
                .split_once("paired_device_profile:")
                .map(|(_, rest)| rest.trim().trim_end_matches(',').trim())
                .unwrap_or("")
                .to_string();
            sites.push((path.clone(), line.to_string(), argument));
        }
    }

    // Vacuity control: only `None` sites would pass the check below trivially.
    let from_a_device: Vec<&(PathBuf, String, String)> = sites
        .iter()
        .filter(|(_, _, argument)| argument != "None")
        .collect();
    assert!(
        !from_a_device.is_empty(),
        "nothing in production fills `paired_device_profile` from a device any more. PAI-1 P9's \
         identity half is inert again and the strongest rung of the resolver is unreachable."
    );

    // Nothing may follow `.profile_id()`: any `.or(..)` lets the top rung answer on no evidence.
    let bad: Vec<&(PathBuf, String, String)> = from_a_device
        .iter()
        .copied()
        .filter(|(_, _, argument)| !argument.ends_with(".profile_id()"))
        .collect();
    assert!(
        bad.is_empty(),
        "a production site fills the paired-device rung with something other than a bare \
         `DeviceRung::profile_id()`: {bad:#?}. Only `DeviceRung::Member` may answer `Some` -- no \
         device, an unattributed device and an unreadable attribution store must all fall \
         through to explicit, then face, then guest. `IdentificationSource::PairedDevice` \
         outranks every one of them, so a default here outranks every proof the pond can make."
    );

    assert!(
        from_a_device
            .iter()
            .any(|(path, _, _)| path.ends_with("pond-api/src/routes.rs")),
        "no site in routes.rs fills the rung from a device; `resolve_turn_scope` is where a \
         turn's scope is decided, so if it is not here it is not on the turn path: {sites:#?}"
    );
}
