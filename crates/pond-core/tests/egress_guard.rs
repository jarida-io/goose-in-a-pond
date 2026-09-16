//! PAI-2 P5 guard: no source file sends an HTTP request without being classified.
//!
//! The programme's rule is "every use of `reqwest` implies a `record_egress`
//! call". Taken literally that guard fails for 8 of the 11 `reqwest` crates on
//! the day it lands, and most of what it flags is a health probe against a model
//! server on 127.0.0.1. A guard that reports loopback traffic as egress gets
//! switched off within a week, so this one forces a CLASSIFICATION, and checks
//! each of the three answers rather than trusting it:
//!
//! * `EGRESS_TRACKED` - the file reaches the shared tracker. Checked by symbol,
//!   so deleting the `record_egress`/`check_egress` call fails the build.
//! * `LOOPBACK_ONLY` - the file only ever talks to loopback. Checked by reading
//!   every URL literal in the file: a non-loopback destination that is not on
//!   the entry's `non_target_urls` list fails the build, so an exemption cannot
//!   quietly grow a third-party host.
//! * `UNGATED_SENDERS` - real egress P5 did not reach. Enumerated, each with the
//!   phase that removes it, under a cap that only ever moves down.
//!
//! The failure mode of a source-scanning test is matching nothing and reporting
//! success, and this programme has four recorded vacuous-test incidents. So the
//! scan asserts floors on what it found, asserts the three lists PARTITION the
//! senders (an unlisted sender fails; a listed non-sender fails as a stale
//! exemption, same polarity as `public_router_and_allowlist_agree`), and asserts
//! every crate that depends on `reqwest` owns at least one classified file --
//! which is the doc's crate-level rule, kept, in the only form it can hold.
//!
//! Why a runtime walk and not `include_str!` like the route guards: those parse
//! ONE known file. This has to see a file that does not exist yet, which is the
//! entire point, and `include_str!` cannot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Below this the walk has broken, not the tree shrunk. 363 files today.
const MIN_FILES_SCANNED: usize = 300;
/// Below this the send detector has broken, not the code moved. 18 today.
const MIN_SENDERS: usize = 15;
/// P6a gated five of P5's six; P6b gated the last one, `pond-api/src/routes.rs`.
/// This number only ever goes down, and it is now at the floor: every file in
/// the workspace that this guard sees sending HTTP either reaches the tracker
/// or is loopback-only with a checked reason.
const MAX_UNGATED: usize = 0;

/// Any of these in a file's production source means it reaches the tracker.
///
/// CALL FORMS, with the opening paren -- not bare symbols. A review after P6a
/// showed the bare-symbol version was satisfied by COMMENT PROSE: every real
/// gate could be deleted from `vision_encoder.rs` and from `main.rs` while this
/// guard stayed green, because one of P6a's own explanatory comments mentioned
/// `record_egress`. Five of the ten tracked files were vulnerable that way, and
/// two of the five were made vulnerable by comments P6a itself added.
///
/// This is the same defect P6a found in its own ORDER guard, by mutation, and
/// fixed only there. The lesson generalises and the fix has to: any source-text
/// guard in this repo that greps a bare symbol name has it. Matching a call
/// form is half the fix; [`strip_line_comments`] is the other half, because
/// `// see check_egress(...)` defeats the call form too.
const TRACKER_SYMBOLS: &[&str] = &[
    "record_egress(",
    "check_egress(",
    "egress::begin(",
    "traced_send(",
    "traced_get(",
];

/// Files whose outbound calls reach the shared egress tracker.
///
/// NECESSARY BUT NOT SUFFICIENT, and the phase that filled this list out said so
/// out loud: [`egress_tracked_files_reach_the_tracker`] looks for ONE tracker
/// symbol per FILE. `pond-hf-cache/src/lib.rs` has two senders and
/// `pond-api/src/routes.rs` has sixteen, so gating one of them would turn this
/// guard green while the rest still phone out. Each multi-sender file therefore
/// carries a behavioural test of its own -- for the HF cache, the three
/// `head_redirect_*` / `get_redirect_*` / `a_permitted_redirect_chain_*` tests
/// in its own `mod tests`, one per site plus a vacuity control; for `routes.rs`,
/// which is by far the worst case, `crates/pond-api/tests/egress_offline_routes.rs`,
/// which pairs every `.send()` in the file with a gate of its own and drives
/// five of them over real HTTP with `Offline` installed.
const EGRESS_TRACKED: &[&str] = &[
    "crates/pond-adapters-goose/src/extension_manager.rs",
    "crates/pond-api/src/routes.rs",
    "crates/pond-adapters-goose/src/vision_encoder.rs",
    // PAI-8's first connector. check_egress before the send and record_egress
    // after, per the weather template -- so an offline pond refuses to ask a
    // third party about the household's day, and every request is in the feed.
    "crates/pond-adapters-caldav/src/lib.rs",
    "crates/pond-adapters-weather/src/lib.rs",
    "crates/pond-hf-cache/src/lib.rs",
    "crates/pond-infra/src/fcm_push_relay.rs",
    "crates/pond-mcp-server/src/http.rs",
    "crates/pond-server/src/main.rs",
    "crates/pond-server/src/model_download.rs",
    "crates/pond-server/src/schedule_executors.rs",
];

/// A file that sends, but only ever to loopback.
struct Exempt {
    file: &'static str,
    /// Why this is not egress. Read by a human, in a review.
    ///
    /// Never read by code, deliberately: the value of writing it down is that a
    /// reviewer sees the justification next to the exemption. Marked rather than
    /// left to warn, so the warning list stays a list of things to fix.
    #[allow(dead_code, reason = "documentation for a human reviewer, not an input")]
    reason: &'static str,
    /// Non-loopback URLs the file contains that are NOT request targets --
    /// install instructions, catalogue entries other code fetches. Every entry
    /// must still be present in the file, so a stale one fails.
    non_target_urls: &'static [&'static str],
}

const LOOPBACK_ONLY: &[Exempt] = &[
    Exempt {
        file: "crates/pond-adapters-llamafile/src/lib.rs",
        reason: "talks to the llamafile server on 127.0.0.1:8080",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-adapters-ollama/src/lib.rs",
        reason: "talks to the Ollama daemon on localhost:11434",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-agent/src/ollama_provider.rs",
        reason: "quarantined PondAgent loop (Q2-05); talks to localhost:11434",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-adapters-mistralrs/src/provider.rs",
        reason: "talks to a local mistral.rs server on 127.0.0.1:9002 (Mac-only checkpoint)",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-infra/src/ollama_provider.rs",
        reason: "talks to the Ollama daemon on localhost:11434",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-server/src/llamafile_process.rs",
        reason: "health-probes the llamafile child process on 127.0.0.1",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-server/src/ollama_process.rs",
        reason: "health-probes the Ollama daemon on 127.0.0.1",
        // Printed in a "how to install Ollama" error message; never fetched.
        non_target_urls: &["https://ollama.com/install.sh"],
    },
    Exempt {
        file: "crates/pond-server/src/composite_model_catalog_provider.rs",
        reason: "its only requests are GET localhost:11434/api/tags and \
                 POST localhost:11434/api/show",
        // Catalogue rows. The download that fetches them is model_download.rs.
        non_target_urls: &[
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/",
            "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/voices/",
            "https://huggingface.co/Mozilla/",
            "https://huggingface.co/{}/resolve/main/{}",
        ],
    },
];

/// Real egress that P5 did NOT gate, with the phase that fixes it.
///
/// This list is the honest scope of `network_mode = "offline"`: these calls
/// still leave the machine. It exists instead of a silent gap, and the cap
/// above is what stops it becoming a parking lot.
///
/// EMPTY since PAI-2 P6b, and `MAX_UNGATED` is 0. Keep the list -- an empty one
/// with a zero cap is the statement "there is no known ungated sender", which is
/// a claim the partition test re-proves on every run. Deleting it would let the
/// next unclassified sender be classified by adding an entry rather than by
/// gating the call.
const UNGATED_SENDERS: &[(&str, &str)] = &[];

// -- the scan -----------------------------------------------------------------

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
/// Test code has to come out: several of GIAP's unit tests are `--ignored`
/// live-hardware tests that call real APIs with a bare `client.get(url).send()`,
/// and scanning those would classify `knowledge.rs` and `finance.rs` as
/// unclassified senders when their production paths go through `traced_get`.
///
/// This was originally written as "everything before the first `#[cfg(test)]`",
/// which is what the convention looks like -- and mutation-testing this guard is
/// what showed the convention is not a rule. Appending a real sender BELOW a
/// trailing `mod tests` left the guard green, and 11 files in this tree already
/// carry more than one `#[cfg(test)]`, so the truncation was discarding
/// production code between them. Removing the items is the same intent without
/// the blind spot.
///
/// Line-based rather than brace-counting on purpose: a `format!("{{")` inside a
/// test would desynchronise a brace counter, whereas rustfmt guarantees the
/// closing brace of an item sits at the item's own indentation.
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
        // A single-line item (`#[cfg(test)] mod tests;`, `#[cfg(test)] const X
        // = ...;`) has no block to close; drop just the item it annotates, or
        // the search below would eat every line up to the next item's brace.
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

/// The source with every `//` line comment removed, string literals intact.
///
/// Used ONLY by [`egress_tracked_files_reach_the_tracker`]. `urls_in` keeps
/// running on the uncommented source on purpose: the `LOOPBACK_ONLY` entries'
/// `non_target_urls` allowances name install instructions and catalogue entries
/// that live in comments, and stripping them would report every one as stale.
///
/// String-aware because `"https://…"` contains `//`. Without the `in_string`
/// track, stripping would eat the rest of any line holding a URL literal --
/// including a `check_egress(` sitting after it -- and this guard would start
/// failing on correctly gated code. Raw strings (`r"…"`, `r#"…"#`) are handled
/// incidentally: the opening and closing quotes are still quotes. Char literals
/// are NOT tracked, deliberately -- `'a` lifetimes are indistinguishable from an
/// unterminated char literal without a real lexer, and the cost of being wrong
/// here is a false FAILURE, which someone reads, not a false pass.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut cut = line.len();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1, // skip the escaped byte
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

/// `.send()` with empty parens is the `reqwest::RequestBuilder` form; a channel
/// send always carries a message, so this does not collide with `tx.send(msg)`.
fn sends_http(prod: &str) -> bool {
    prod.contains(".send()") || prod.contains("reqwest::get(")
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "localhost"
        || h == "::1"
        || h == "[::1]"
        || h.starts_with("127.")
        || h.ends_with(".localhost")
}

/// Every `http://` / `https://` literal in `src`, as (full literal, host).
fn urls_in(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for scheme in ["https://", "http://"] {
        let mut from = 0usize;
        while let Some(i) = src[from..].find(scheme) {
            let at = from + i;
            let rest = &src[at..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '`' || c == ')')
                .unwrap_or(rest.len());
            let full = rest[..end].to_string();
            let after = &full[scheme.len()..];
            let host = after
                .split(['/', ':'])
                .next()
                .unwrap_or_default()
                .to_string();
            out.push((full, host));
            from = at + scheme.len();
        }
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
            // `tests/` holds integration tests, which are tests by definition.
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

/// (all scanned files, sender files) -- relative, forward-slashed, sorted.
fn scan() -> (Vec<String>, BTreeMap<String, String>) {
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
        "scanned only {} .rs files under crates/ (floor {MIN_FILES_SCANNED}) -- \
         the walk has broken, not the tree shrunk",
        files.len()
    );

    let mut senders = BTreeMap::new();
    for rel in &files {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
        let prod = production_source(&src);
        if sends_http(&prod) {
            senders.insert(rel.clone(), prod);
        }
    }

    assert!(
        senders.len() >= MIN_SENDERS,
        "found only {} HTTP-sending files (floor {MIN_SENDERS}) -- the detector \
         has broken. A guard that matches nothing reports success.",
        senders.len()
    );

    (files, senders)
}

// -- the assertions -----------------------------------------------------------

/// Every sender is in exactly one list, and every list entry is a sender.
#[test]
fn every_http_sender_is_classified_exactly_once() {
    let (_, senders) = scan();

    let mut listed: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for f in EGRESS_TRACKED {
        listed.entry(f).or_default().push("EGRESS_TRACKED");
    }
    for e in LOOPBACK_ONLY {
        listed.entry(e.file).or_default().push("LOOPBACK_ONLY");
    }
    for (f, _) in UNGATED_SENDERS {
        listed.entry(f).or_default().push("UNGATED_SENDERS");
    }

    let duplicated: Vec<String> = listed
        .iter()
        .filter(|(_, l)| l.len() > 1)
        .map(|(f, l)| format!("{f} in {l:?}"))
        .collect();
    assert!(
        duplicated.is_empty(),
        "a file must be in exactly one list:\n  {}",
        duplicated.join("\n  ")
    );

    let listed_set: BTreeSet<&str> = listed.keys().copied().collect();
    let sender_set: BTreeSet<&str> = senders.keys().map(|s| s.as_str()).collect();

    let unclassified: Vec<&&str> = sender_set.difference(&listed_set).collect();
    assert!(
        unclassified.is_empty(),
        "these files send HTTP and are in no list:\n  {:?}\n\
         Decide: does it reach the tracker (EGRESS_TRACKED), is it loopback-only \
         (LOOPBACK_ONLY, with a reason), or is it ungated egress (UNGATED_SENDERS, \
         which is capped at {MAX_UNGATED} and shrinking)?",
        unclassified
    );

    let stale: Vec<&&str> = listed_set.difference(&sender_set).collect();
    assert!(
        stale.is_empty(),
        "these listed files no longer send HTTP -- an exemption that outlives its \
         reason:\n  {:?}\nDelete the entry.",
        stale
    );
}

/// A tracked file that stops calling the tracker fails the build.
#[test]
fn egress_tracked_files_reach_the_tracker() {
    let (_, senders) = scan();
    let mut broken = Vec::new();
    for f in EGRESS_TRACKED {
        let Some(prod) = senders.get(*f) else {
            continue; // the partition test owns this case
        };
        // Comments stripped: prose that MENTIONS a gate is not a gate. See
        // TRACKER_SYMBOLS for the mutation that proved this necessary.
        let code = strip_line_comments(prod);
        if !TRACKER_SYMBOLS.iter().any(|s| code.contains(s)) {
            broken.push(*f);
        }
    }
    assert!(
        broken.is_empty(),
        "these files are listed EGRESS_TRACKED but CALL none of \
         {TRACKER_SYMBOLS:?} in code (comments do not count):\n  {:?}",
        broken
    );
}

/// A loopback exemption that grows a third-party destination fails the build.
///
/// This is the half that makes the exemption list safe to have. Without it,
/// "it only talks to Ollama" is a claim in a comment.
#[test]
fn loopback_exemptions_contain_no_third_party_url() {
    let (_, senders) = scan();
    let mut offenders = Vec::new();
    let mut stale_allowances = Vec::new();

    for e in LOOPBACK_ONLY {
        let Some(prod) = senders.get(e.file) else {
            continue; // the partition test owns this case
        };
        for (full, host) in urls_in(prod) {
            if is_loopback_host(&host) {
                continue;
            }
            if e.non_target_urls.iter().any(|a| full.starts_with(a)) {
                continue;
            }
            offenders.push(format!("{} -> {full}", e.file));
        }
        for allowed in e.non_target_urls {
            if !prod.contains(allowed) {
                stale_allowances.push(format!("{} no longer contains {allowed}", e.file));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these LOOPBACK_ONLY files contain a non-loopback destination:\n  {}\n\
         Either route the call through the egress tracker and move the file to \
         EGRESS_TRACKED, or -- if the URL is genuinely never fetched here -- add \
         it to that entry's non_target_urls with a reason.",
        offenders.join("\n  ")
    );
    assert!(
        stale_allowances.is_empty(),
        "stale non_target_urls entries (the URL is gone, the exemption is not):\n  {}",
        stale_allowances.join("\n  ")
    );
}

/// The ungated list only shrinks.
#[test]
fn ungated_egress_is_capped_and_shrinking() {
    assert!(
        UNGATED_SENDERS.len() <= MAX_UNGATED,
        "UNGATED_SENDERS has {} entries, cap {MAX_UNGATED}. This list is the honest \
         scope of network_mode=\"offline\": every file on it still phones out. \
         Gate the new sender instead of raising the cap.",
        UNGATED_SENDERS.len()
    );
    for (file, why) in UNGATED_SENDERS {
        assert!(
            why.contains("PAI"),
            "{file} must name the phase that removes it, not just describe itself"
        );
    }
}

/// The doc's crate-level rule, in the only form that can hold: every crate that
/// depends on `reqwest` owns at least one classified file.
///
/// This is what catches a crate that starts sending through a form the file-level
/// detector does not know -- `Client::execute`, `reqwest::blocking`, a wrapper.
#[test]
fn every_reqwest_crate_owns_a_classified_sender() {
    let root = workspace_root();
    let (_, senders) = scan();

    let sender_crates: BTreeSet<String> = senders
        .keys()
        .filter_map(|p| p.split('/').nth(1).map(str::to_string))
        .collect();

    let mut reqwest_crates = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ readable") {
        let entry = entry.expect("readable dir entry");
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        // A dependency line, not the prose in pond-adapters-whisper's manifest
        // recording that the dep was REMOVED.
        let declares = text.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("reqwest") && l.contains('=')
        });
        if declares {
            reqwest_crates.insert(entry.file_name().to_string_lossy().to_string());
        }
    }

    assert!(
        reqwest_crates.len() >= 10,
        "found only {} crates depending on reqwest -- the manifest scan has broken",
        reqwest_crates.len()
    );

    let unaccounted: Vec<&String> = reqwest_crates.difference(&sender_crates).collect();
    assert!(
        unaccounted.is_empty(),
        "these crates depend on reqwest but own no file this guard sees sending:\n  {:?}\n\
         Either the crate no longer needs reqwest (drop the dep), or it sends \
         through a form the detector misses (Client::execute, reqwest::blocking, \
         a wrapper) -- teach `sends_http` about it.",
        unaccounted
    );
}

/// The gate must be INSTALLED before anything downloads, on every entry point.
///
/// PAI-2 P6a found two ways this claim was false while every other test in this
/// file was green, and neither is visible to a scan that only asks "does the
/// file mention a tracker symbol".
///
/// 1. `set_network_mode` had exactly ONE call site, inside `run_server`. The
///    mode is a process-global that defaults to `Open`, so `pond chat` -- which
///    is also the terminal voice loop -- and `pond setup` ran with the setting
///    unread. Every gate they inherited evaluated against a default nobody had
///    chosen.
/// 2. Inside `run_server`, `ensure_onnx_runtime()` -- which downloads ~100 MB
///    from github.com by shelling out to `curl` -- ran 40-odd lines BEFORE the
///    mode was installed. Gating it there would have been a mechanism that
///    cannot fire, which this programme already has two of.
///
/// So this asserts ORDER, not presence. Presence is what was already true.
///
/// A review after P6a found a THIRD way, and it was this guard's own detector
/// that hid it. The detector asked "which functions call `ensure_onnx_runtime()`"
/// and its vacuity control pinned that answer at three -- so `run_models`
/// (`pond models download`), which calls `model_download::download_file` twice
/// and installs no mode at all, was not merely missed but LOCKED OUT of the
/// question. The gate P6a added inside `download_file` was inert there, and a
/// stored `network_mode = "offline"` permitted a full model download: a privacy
/// control failing OPEN. The detector now asks "which functions DOWNLOAD",
/// which is the question the test's name always claimed to be asking.
///
/// Source-text and not a runtime check because there is nothing to call: the
/// defect is where a statement sits in a 3,000-line `async fn`. It lives in
/// `pond-core` rather than beside `main.rs` because CI has no
/// `cargo test -p pond-server` -- a guard there never fires on a PR. Same
/// reasoning, and same shape, as
/// `pond-infra/tests/redaction_chokepoints_are_wired.rs`.
/// `main.rs` split into one chunk per top-level `fn` / `async fn`, comments
/// stripped, `#[cfg(test)]` items removed.
///
/// Comments have to go for the whole scan. `download_and_extract_ort`'s own
/// explanatory comment contains the literal `Command::new("curl")`, ~13 lines
/// ABOVE the `check_egress` call it is explaining, so an ORDER assertion would
/// read the prose as the download and report the gate as too late. Same lesson
/// as TRACKER_SYMBOLS, applied before it bites.
///
/// Top-level items start at column 0, so this splits without brace-counting.
/// The marker is re-prepended so each chunk still carries the `fn` line it came
/// from -- which is what lets the assertions name the offending function.
fn main_rs_fn_chunks() -> Vec<String> {
    let main_rs = workspace_root().join("crates/pond-server/src/main.rs");
    let src = std::fs::read_to_string(&main_rs)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", main_rs.display()));
    let prod = strip_line_comments(&production_source(&src));
    prod.split("\nfn ")
        .flat_map(|c| c.split("\nasync fn "))
        .map(|c| c.to_string())
        .collect()
}

#[test]
fn every_entry_point_installs_the_gate_before_it_downloads() {
    let chunks = main_rs_fn_chunks();

    // Every way a `main.rs` function reaches the network with a large transfer.
    const DOWNLOAD_CALLS: &[&str] = &[
        "    ensure_onnx_runtime();",
        "model_download::download_file(",
        "download_and_extract_ort(",
    ];

    /// The two download helpers themselves, which are NOT entry points.
    ///
    /// They must be named rather than inferred. Widening `DOWNLOAD_CALLS` to
    /// ask "which functions download" made `ensure_onnx_runtime`'s own body
    /// match (it calls `download_and_extract_ort`), and `download_and_extract_ort`'s
    /// chunk matches its own `fn` line at byte 0. Both are helpers: their
    /// callers own the install, and demanding one here would mean reading the
    /// settings row from a synchronous fn with no runtime. Keeping the list
    /// explicit and short is the point -- an entry point silently added here
    /// is exactly the hole `run_models` sat in, so the exemption is auditable
    /// rather than a heuristic. `download_and_extract_ort` gets its own,
    /// stricter assertion after the loop.
    const DOWNLOAD_HELPERS: &[&str] = &["ensure_onnx_runtime", "download_and_extract_ort"];

    let mut callers = 0usize;
    for chunk in &chunks {
        // The EARLIEST download in the function is the one the install has to
        // precede. Taking the first `ensure_onnx_runtime()` and ignoring an
        // earlier `download_file` would let a gap open up again.
        let Some(call_at) = DOWNLOAD_CALLS.iter().filter_map(|c| chunk.find(c)).min() else {
            continue;
        };

        let name = chunk.lines().next().unwrap_or("<unknown>");
        if DOWNLOAD_HELPERS.iter().any(|h| name.starts_with(h)) {
            continue;
        }
        callers += 1;
        // The CALL form, with its path qualifier and opening paren -- not the
        // bare symbol. Mutation-testing this guard is what forced the
        // distinction: deleting the install from `run_chat` left the guard
        // GREEN, because the comment ABOVE the deleted call still said the
        // words "set_network_mode" and a substring search cannot tell prose
        // from code. Every real call site in `main.rs` is written
        // `..::egress::set_network_mode(`; a mention in a comment is not.
        let install_at = chunk.find("egress::set_network_mode(");

        assert!(
            install_at.is_some(),
            "`{name}` downloads (one of {DOWNLOAD_CALLS:?}) but never calls \
             set_network_mode -- so the process-global is still at its `Open` \
             default and a stored `network_mode = \"offline\"` does not apply \
             here. Install the mode from the settings row before the first \
             fetch."
        );
        let install_at = install_at.unwrap();

        assert!(
            install_at < call_at,
            "`{name}` installs the egress gate at byte {install_at} but \
             downloads at byte {call_at} -- the transfer happens BEFORE the \
             setting that governs it is read, so the gate inside it can never \
             refuse. Move the download below set_network_mode."
        );
    }

    // Vacuity control. If the download helpers are renamed or the call sites
    // move, the loop above finds nothing and reports success -- the exact
    // failure shape this file's header warns about.
    //
    // FOUR, not three. The previous three counted callers of
    // `ensure_onnx_runtime()`, which is a different question from "which
    // functions download" and pinned the wrong answer: `run_models`
    // (`pond models download`) fetches a model and its config sibling through
    // `model_download::download_file` and installed no mode at all, so P6a's
    // gate inside that fn was inert there and `offline` permitted the download.
    // A privacy control failing OPEN. Raise this number only after checking the
    // new entry point installs the mode first.
    assert_eq!(
        callers, 4,
        "expected the 4 downloading entry points (run_setup, run_server, \
         run_chat, run_models); found {callers}. If an entry point was added or \
         removed, update this number after checking the new one installs the \
         mode first. If it dropped to 0 the detector has broken, not the code."
    );

    // The ORT fetch itself, which the ORDER assertions above deliberately do
    // not cover: they prove the mode is INSTALLED in time, not that the ~100 MB
    // github.com transfer is gated at all. Deleting the `check_egress` line
    // from `download_and_extract_ort` left all six tests in this file green --
    // `egress_tracked_files_reach_the_tracker` is satisfied by the unrelated
    // OAuth `egress::begin(` elsewhere in `main.rs`, and this test only ever
    // asked about ordering while its own failure message talked about "the gate
    // inside it". It is the only subprocess sender in the tree, so no
    // reqwest-shaped detector will ever see it; this is the whole of its
    // coverage.
    let ort = chunks
        .iter()
        .find(|c| c.starts_with("download_and_extract_ort("))
        .expect(
            "`fn download_and_extract_ort(` is gone from main.rs. It was the \
             only subprocess sender in the tree and the only thing gating the \
             ~100 MB ONNX Runtime fetch -- if it moved, move this assertion \
             with it rather than deleting it.",
        );
    let gate_at = ort.find("egress::check_egress(").expect(
        "`download_and_extract_ort` shells out to curl for a ~100 MB github.com \
         transfer and no longer calls check_egress. The egress guard finds \
         senders by looking for `reqwest` and is structurally blind to a \
         subprocess, so nothing else in this file can catch it: `offline` and \
         `allowlist` would both silently permit the largest single outbound \
         transfer the pond makes.",
    );
    let curl_at = ort
        .find("Command::new(\"curl\")")
        .expect("`download_and_extract_ort` no longer shells out to curl -- re-derive this test");
    assert!(
        gate_at < curl_at,
        "`download_and_extract_ort` calls check_egress at byte {gate_at} but \
         spawns curl at byte {curl_at}. A refusal after the bytes are on the \
         wire is not a refusal."
    );
}

/// The gate must also be installed before anything BUILDS AN AGENT.
///
/// PAI-2 P6b's companion to the download guard above, and it exists because
/// that one could not see the hole. Its detector asks "which functions
/// DOWNLOAD", and `run_agent_cmd` (`pond agent chat` / `agent tools` /
/// `agent extras`) downloads nothing -- it reads the settings row, ignores
/// `network_mode`, and hands three arms to `build_goose_backend`, which wires
/// the LLM provider, the weather adapter and the whole MCP tool surface. Every
/// gate those paths inherit then evaluated against the `Open` default nobody
/// chose, so a stored `network_mode = "offline"` did nothing on that entry
/// point. Downloading is one way to phone home; running a turn is the other,
/// and it is the common one.
///
/// Asserting ORDER rather than presence, for the same reason as the download
/// guard: `build_goose_backend` reaches the network as soon as it is built, so
/// an install below it is a mechanism that cannot fire.
#[test]
fn every_entry_point_installs_the_gate_before_it_builds_an_agent() {
    let chunks = main_rs_fn_chunks();

    // The helper itself is not an entry point: it takes the settings it needs
    // as arguments and its callers own the install. Named, not inferred --
    // `DOWNLOAD_HELPERS` above records what inferring costs.
    const BACKEND_HELPERS: &[&str] = &["build_goose_backend"];

    let mut callers = 0usize;
    for chunk in &chunks {
        let Some(build_at) = chunk.find("build_goose_backend(") else {
            continue;
        };
        let name = chunk.lines().next().unwrap_or("<unknown>");
        if BACKEND_HELPERS.iter().any(|h| name.starts_with(h)) {
            continue;
        }
        callers += 1;

        // The CALL form with its path qualifier, and comments already stripped:
        // a comment saying the words "set_network_mode" is not an install. That
        // exact mutation passed against the bare-symbol version of the download
        // guard.
        let install_at = chunk.find("egress::set_network_mode(");
        assert!(
            install_at.is_some(),
            "`{name}` builds a Goose agent -- LLM provider, weather adapter, the \
             whole MCP tool surface -- but never calls set_network_mode, so the \
             process-global is still at its `Open` default and a stored \
             `network_mode = \"offline\"` does not apply on this entry point. \
             Install the mode from the settings row before building the backend."
        );
        let install_at = install_at.unwrap();
        assert!(
            install_at < build_at,
            "`{name}` installs the egress gate at byte {install_at} but builds \
             the agent at byte {build_at}. The backend starts talking as soon as \
             it exists; an install below it can never refuse anything."
        );
    }

    // Vacuity control. If `build_goose_backend` is renamed and this loop finds
    // nothing, the test reports success -- the failure shape this file's header
    // warns about, and the one that let `run_models` sit in a hole for a phase.
    // THREE: run_server, run_chat, run_agent_cmd. Raise it only after checking
    // the new entry point installs the mode first.
    assert_eq!(
        callers, 3,
        "expected the 3 agent-building entry points (run_server, run_chat, \
         run_agent_cmd); found {callers}. If one was added or removed, update \
         this number after checking the new one installs the mode first. If it \
         dropped to 0 the detector has broken, not the code."
    );
}
