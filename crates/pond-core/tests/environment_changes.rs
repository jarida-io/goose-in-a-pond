//! Every change to the process environment in production code goes through
//! `pond_core::shared::services::process_env`, which C code reading the environment on another
//! thread can be guarded against. A bare `setenv` crashed the pond in ALSA's `getenv` at start-up
//! (2026-11-09; see that module).
//!
//! Each workspace crate's `src/` is read up to its first `#[cfg(test)]`, so tests, which set
//! variables of their own before threads exist, are not counted.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn environment_changes_go_through_process_env() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(&crates)
        .expect("the crates directory")
        .flatten()
    {
        rust_files(&krate.path().join("src"), &mut files);
    }
    assert!(files.len() > 100, "found only {} source files", files.len());

    let mut raw = Vec::new();
    for file in files {
        if file.ends_with("process_env.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let production = text.split("#[cfg(test)]").next().unwrap_or_default();
        for (n, line) in production.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            if code.contains("env::set_var(") || code.contains("env::remove_var(") {
                raw.push(format!("{}:{}", file.display(), n + 1));
            }
        }
    }
    assert!(
        raw.is_empty(),
        "change the environment with process_env::set or remove, not std::env directly:\n{}",
        raw.join("\n")
    );
}
