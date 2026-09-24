//! PAI-1 P9's identity half is wired, and the device id it is wired from can
//! only have come from the pond.
//!
//! This is the mirror image of `device_profile_rung_is_not_wired_yet.rs`, whose
//! three claims are now all retired: that file asserted an absence and failed
//! the day a caller landed. This asserts a presence, and a provenance.
//!
//! # Why source tripwires rather than only behaviour
//!
//! The behaviour is tested where it is pure — `pond_core::security::domain::
//! proven_device` for what each rung resolves, `device_rung_resolves_a_member.rs`
//! next door for the chain through real SQLite. Neither can see the two things
//! that would silently switch this rung off or turn it dangerous:
//!
//! * **Off:** a `Handshake` implementor that does not answer `caller_for_token`
//!   returns `Ok(None)`, so no request carries a device, so the strongest rung
//!   is unreachable — and nothing errors. `dead_code` cannot warn about it: the
//!   method is a `pub` trait item with a default body.
//! * **Dangerous:** `Principal::device_id` filled from a header, a body field or
//!   a query parameter. `IdentificationSource::PairedDevice` outranks face and
//!   explicit identification, so a client-asserted device id would outrank every
//!   proof the pond can make — the same argument that put the pairing
//!   attribution on the CODE and not on the pairing request.
//!
//! Both are one line, in a file the behavioural tests do not read.
//!
//! `include_str!` creates no dependency edge and needs no link, so this runs in
//! CI's fast pass — `ci.yml` has no `cargo test -p pond-server` and no
//! `cargo test -p pond-api` in the fast set for this crate's purposes, and a
//! guard placed next to `main.rs` would never fire on a pull request (PAI-2 P3's
//! lesson).

use std::path::{Path, PathBuf};

const MIDDLEWARE: &str = include_str!("../../pond-api/src/middleware/mod.rs");
const ROUTES: &str = include_str!("../../pond-api/src/routes.rs");
const POLICY: &str = include_str!("../../pond-core/src/security/ports/policy.rs");
const HANDSHAKE_PORT: &str = include_str!("../../pond-core/src/security/ports/handshake.rs");

/// The workspace's `crates/` directory, for the tests that walk every file
/// rather than reading a named one. A guard that names today's files cannot see
/// the implementor added tomorrow, which is the assertion-window failure this
/// programme keeps re-learning.
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
                // `target/` under a crate would be another checkout's build
                // artifacts, never source.
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

/// Everything before the `#[cfg(test)] mod …`, i.e. the part of a file that
/// ships. Test doubles legitimately construct principals with devices on them;
/// production has exactly one place that may.
///
/// Cutting at the first `#[cfg(test)]` of any kind is wrong and was wrong here
/// on the first run: `middleware/mod.rs` has a `#[cfg(test)] fn` two hundred
/// lines above `auth_middleware`, so that rule sliced the auth path itself out
/// of "production" and every assertion below passed by reading nothing.
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

/// Whether a path is test code rather than shipped code.
fn is_test_path(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == "tests" || c.as_os_str() == "benches")
}

/// Guard against the guard. If an `include_str!` stops pointing at the file this
/// test believes it points at, every assertion below passes by matching nothing
/// — four of this programme's recorded vacuous-test incidents were that shape.
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

/// `Principal::with_device` has exactly ONE production call site, it is the auth
/// middleware, and its argument is the handshake lookup's own answer.
///
/// This is the assertion about an ARGUMENT rather than a presence, and it is the
/// one that matters most: `.with_device(header_value)` compiles, wires, streams
/// and looks exactly like this line in a diff. A second call site anywhere else
/// is the same defect with more steps.
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
    // The ARGUMENT, exactly, and not merely "mentions the right variable
    // somewhere". `.with_device(asserted.unwrap_or(caller.device_id))` contains
    // `caller.device_id` and is the whole defect; that mutation was run, and
    // this is the assertion that was rewritten because of it.
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

/// The middleware reads a device from the token lookup and from nothing else.
///
/// The previous test says `with_device`'s argument is right; this one says no
/// second spelling of "the device" got into the auth path at all — an
/// `X-Device-Id` header parsed into a local and threaded through would satisfy
/// the argument check by passing a variable that is not `caller.device_id`, but
/// this catches the parse.
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

/// The dev loopback bypass returns before a token is read, so it carries no
/// device — and must not acquire one.
///
/// Getting this wrong is the failure worth naming: resolving a loopback caller
/// to "the device that paired most recently" would make every local request
/// speak as whoever last paired a phone, on a bypass whose whole point is that
/// nobody authenticated.
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

/// `Principal` has no other way to acquire a device: the three constructors all
/// start it at `None`, and nothing else assigns the field.
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

/// Every `impl Handshake for` either answers `caller_for_token` or is on the
/// allowlist below, with a reason.
///
/// The port defaults this method to `Ok(None)` deliberately — the default
/// narrows, so a forgotten override loses a capability rather than granting one
/// — but a forgotten override on the SQLite adapter would make PAI-1 P9 inert
/// on every real pond while the whole workspace stayed green. That is the exact
/// shape this programme has shipped three times.
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
            // Impl blocks in this workspace are at column 0, so the first
            // line that is exactly `}` closes this one.
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

    // A stale allowlist entry is a hole: it excuses a name nothing implements,
    // and the day something does, it is excused before anyone looks.
    for (allowed, _) in ALLOWED_SILENT {
        assert!(
            found.iter().any(|(name, _, _)| name == allowed),
            "ALLOWED_SILENT names `{allowed}`, which implements Handshake nowhere in the \
             workspace. Remove the entry."
        );
    }
}

/// `client_id_for_token` is derived from `caller_for_token` and not implemented
/// beside it.
///
/// This is what makes the default above survivable. The two lookups answer
/// questions about the same `session_tokens` row; implemented separately they
/// can drift into disagreeing about which client a token belongs to, and — more
/// to the point — an adapter that drops the `caller_for_token` override would
/// keep answering client ids and lose only the device, which is the silent half.
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

/// `resolve_turn_scope` feeds the resolver from the device rung, and takes the
/// device as a type only the auth layer can produce.
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

/// Every call site passes one. A handler that quietly passes
/// `ProvenDevice::none()` because it was the shortest way to compile is a turn
/// that cannot identify its speaker — and it is invisible.
#[test]
fn every_turn_resolution_is_handed_a_device() {
    // The definition's own `(` matches too. It is excluded by what precedes it
    // -- `async fn ` -- rather than by the shape of the line, because the
    // definition's arguments are one per line and every shape-based filter for
    // that also filters out a wrapped call.
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
        // The THIRD argument, exactly, and not "the call mentions a device
        // somewhere". `&ProvenDevice::none()` is a legal third argument, is the
        // shortest way to make a handler compile, and switches the rung off for
        // that route with nothing to see in a diff. That mutation was run.
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

/// The handler-side halves: each route reads the device from its `Principal`,
/// and there is one function that does it.
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

/// No client-facing request body or query carries a device. This is the
/// structural half of "a device id supplied by the client is ignored": ignoring
/// it is not a check somebody remembered to write, there is nowhere to put it.
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

/// No production site fills the paired-device rung with anything but a bare
/// `DeviceRung::profile_id()`.
///
/// **This overlaps `the_turn_resolver_is_fed_from_the_device_rung` above and is
/// not a replacement for it.** Worth saying plainly, because the first version
/// of this comment claimed the seam was uncovered and that was wrong: the
/// mutation `.profile_id().or(Some("liz".to_string()))` fails that test too. I
/// had run it against `device_rung_resolves_a_member.rs` alone -- which does
/// leave the seam open, since it reproduces `resolve_turn_scope` "minus the
/// `AppState` this crate cannot build" and so tests every PIECE and not the
/// function that assembles them -- and mis-attributed the gap.
///
/// What this adds over the literal check is two things. It is a property rather
/// than a string, so rustfmt reflowing that line cannot turn the guard off; and
/// it covers EVERY file, so a second site added somewhere else is caught, which
/// a `ROUTES.contains` cannot see. The vacuity control is the other half: with
/// only `None` sites left it fails rather than passing, so un-wiring the rung
/// is not a way to satisfy it.
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

    // Vacuity control: if nothing fills the rung from a device any more, PAI-1
    // P9's identity half has been un-wired and this test would otherwise pass
    // by finding only `None`s.
    let from_a_device: Vec<&(PathBuf, String, String)> = sites
        .iter()
        .filter(|(_, _, argument)| argument != "None")
        .collect();
    assert!(
        !from_a_device.is_empty(),
        "nothing in production fills `paired_device_profile` from a device any more. PAI-1 P9's \
         identity half is inert again and the strongest rung of the resolver is unreachable."
    );

    // The property. Every site either declines the rung outright, or hands it a
    // `DeviceRung`'s own answer with NOTHING appended: `.or(..)`, `.or_else(..)`
    // and `.unwrap_or(..)` all compile, all look like this line in a diff, and
    // all mean the strongest rung answers `Some` when the pond knows nothing.
    //
    // This exists because the behavioural suite next door cannot see it. That
    // file says in its own doc comment that it reproduces `resolve_turn_scope`
    // "minus the `AppState` this crate cannot build", so it tests every PIECE
    // and not the function that assembles them -- and the mutation
    // `device_rung.profile_id().or(Some("liz".to_string()))` was applied to
    // `routes.rs` with all eight of its tests staying green. That is a pond
    // where an unreadable attribution store silently makes everybody Liz.
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

    // And the assembly point is where it should be.
    assert!(
        from_a_device
            .iter()
            .any(|(path, _, _)| path.ends_with("pond-api/src/routes.rs")),
        "no site in routes.rs fills the rung from a device; `resolve_turn_scope` is where a \
         turn's scope is decided, so if it is not here it is not on the turn path: {sites:#?}"
    );
}
