//! Every HTTP-sending source file must be classified, and each class is checked.
//! `EGRESS_TRACKED` calls the tracker, `LOOPBACK_ONLY` has every URL literal checked, and
//! `UNGATED_SENDERS` is capped. The lists must partition the senders, with floors against
//! a vacuous scan. A runtime walk, not `include_str!`, so it sees files that don't exist yet.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Below this the walk has broken, not the tree shrunk. 363 files today.
const MIN_FILES_SCANNED: usize = 300;
/// Below this the send detector has broken, not the code moved. 18 today.
const MIN_SENDERS: usize = 15;
/// Only ever goes down; at 0, every sender reaches the tracker or is checked loopback-only.
const MAX_UNGATED: usize = 0;

/// Call forms, not bare symbols, matched after [`strip_line_comments`]: prose isn't a gate.
const TRACKER_SYMBOLS: &[&str] = &[
    "record_egress(",
    "check_egress(",
    "egress::begin(",
    "traced_send(",
    "traced_get(",
];

/// Files that reach the egress tracker; checked per FILE, not per send.
/// Multi-sender files carry their own tests: `pond-hf-cache`'s, `egress_offline_routes.rs`.
const EGRESS_TRACKED: &[&str] = &[
    "crates/pond-adapters-goose/src/extension_manager.rs",
    "crates/pond-api/src/routes.rs",
    "crates/pond-adapters-goose/src/vision_encoder.rs",
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
    /// Why this is not egress, for a human reviewer.
    #[allow(dead_code, reason = "documentation for a human reviewer, not an input")]
    reason: &'static str,
    /// Non-loopback URLs in the file that aren't request targets; each must still be present.
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

/// Known senders that bypass `network_mode = "offline"`, with what fixes each.
/// Keep it even empty: with the zero cap it states "no known ungated sender".
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

/// The file minus every `#[cfg(test)]` item; production code can sit after one.
/// Line-based, not brace-counting: rustfmt puts an item's closing brace at its own indent.
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

/// Strips `//` comments outside string literals (URLs contain `//`; char literals untracked).
/// Not for `urls_in`: `non_target_urls` live in comments.
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

/// Empty-paren `.send()` is reqwest's; a channel send always takes a message.
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

#[test]
fn egress_tracked_files_reach_the_tracker() {
    let (_, senders) = scan();
    let mut broken = Vec::new();
    for f in EGRESS_TRACKED {
        let Some(prod) = senders.get(*f) else {
            continue; // the partition test owns this case
        };
        // Comments stripped: prose that mentions a gate is not a gate.
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

/// Catches senders the file-level detector can't see (`Client::execute`, `reqwest::blocking`).
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
        // A dependency line, not prose like pond-adapters-whisper's note on the removed dep.
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

/// `main.rs` production code, comments stripped, split at each column-0 `fn` / `async fn`.
/// Stripped because a comment quoting `Command::new("curl")` would read as a download.
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

/// Asserts ORDER: the process-global `network_mode` defaults to `Open` until installed.
/// Lives in pond-core because CI never runs pond-server's tests.
#[test]
fn every_entry_point_installs_the_gate_before_it_downloads() {
    let chunks = main_rs_fn_chunks();

    // Every way a `main.rs` function reaches the network with a large transfer.
    const DOWNLOAD_CALLS: &[&str] = &[
        "    ensure_onnx_runtime();",
        "model_download::download_file(",
        "download_and_extract_ort(",
    ];

    /// Helpers, not entry points (callers own the install); named so each exemption is audited.
    const DOWNLOAD_HELPERS: &[&str] = &["ensure_onnx_runtime", "download_and_extract_ort"];

    let mut callers = 0usize;
    for chunk in &chunks {
        // The install must precede the EARLIEST download of any kind.
        let Some(call_at) = DOWNLOAD_CALLS.iter().filter_map(|c| chunk.find(c)).min() else {
            continue;
        };

        let name = chunk.lines().next().unwrap_or("<unknown>");
        if DOWNLOAD_HELPERS.iter().any(|h| name.starts_with(h)) {
            continue;
        }
        callers += 1;
        // The qualified call form, not the bare symbol, so a mention can't count as an install.
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

    // Vacuity control; raise the count only once the new entry point installs the mode first.
    assert_eq!(
        callers, 4,
        "expected the 4 downloading entry points (run_setup, run_server, \
         run_chat, run_models); found {callers}. If an entry point was added or \
         removed, update this number after checking the new one installs the \
         mode first. If it dropped to 0 the detector has broken, not the code."
    );

    // The ORT fetch's own gate: nothing else here can see a curl subprocess.
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

/// ORDER again: `build_goose_backend` reaches the network as soon as it is built.
#[test]
fn every_entry_point_installs_the_gate_before_it_builds_an_agent() {
    let chunks = main_rs_fn_chunks();

    // Takes its settings as arguments; its callers own the install.
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

        // The qualified call form: a mere mention is not an install.
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

    // Vacuity control; raise the count only once the new entry point installs the mode first.
    assert_eq!(
        callers, 3,
        "expected the 3 agent-building entry points (run_server, run_chat, \
         run_agent_cmd); found {callers}. If one was added or removed, update \
         this number after checking the new one installs the mode first. If it \
         dropped to 0 the detector has broken, not the code."
    );
}
