//! GIAP must never enter `LlamaBackend::init()`'s CAS: the flag is shared with Goose, which
//! panics if it loses it. Source-scanned, since reproducing needs Goose and a real model.

use std::path::Path;

/// Every `.rs` file in this crate, as (display path, contents).
fn crate_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path.display().to_string(), text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
        &mut out,
    );
    assert!(
        !out.is_empty(),
        "the source scan found nothing -- walker is broken"
    );
    out
}

/// Strip `//` comments so docs that name `LlamaBackend::init()` don't trip the scan.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_giap_code_calls_llama_backend_init() {
    let mut offenders = Vec::new();
    for (path, src) in crate_sources() {
        let code = strip_line_comments(&src);
        for (lineno, line) in code.lines().enumerate() {
            if line.contains("LlamaBackend::init") {
                offenders.push(format!("{path}:{}", lineno + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "GIAP called LlamaBackend::init() at {offenders:?}.\n\
         That enters llama-cpp-2's process-global CAS, which Goose assumes it \
         always wins -- losing it makes Goose hit `unreachable!` and panic the \
         moment a local chat model loads. Initialise the C backend directly \
         instead (see engine.rs :: get_or_init_backend) and let Goose own the flag."
    );
}

/// Dropping `LlamaBackend` resets the flag and frees the backend under Goose.
#[test]
fn the_backend_handle_is_held_strongly_and_never_freed() {
    let engine = crate_sources()
        .into_iter()
        .find(|(p, _)| p.ends_with("engine.rs"))
        .map(|(_, s)| strip_line_comments(&s))
        .expect("engine.rs not found");

    assert!(
        engine.contains("static BACKEND: OnceLock<Arc<LlamaBackend>>"),
        "the shared backend must be held as a strong Arc in a OnceLock. A Weak \
         lets the last engine drop it, which runs Drop -- resetting a flag GIAP \
         does not own and calling llama_backend_free() under Goose."
    );
    assert!(
        !engine.contains("Weak<LlamaBackend>"),
        "a Weak<LlamaBackend> is back: the backend can be freed while Goose still \
         holds it. Once the backend is shared the only sound rule is initialise \
         once, never free."
    );
}
