//! Factory prompt-template descriptions are spelled only in `prompts.rs`.
//! Copies drift and overwrite each other on reseed; a source scan is needed because `upsert`
//! takes a plain `String` that no type can police.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pond_core::prompts::{builtin_template_content, BUILTIN_PROMPT_TEMPLATES};

/// The one file allowed to spell a factory description.
const HOME: &str = "crates/pond-core/src/prompts.rs";

/// Below this the walk has broken, not the tree shrunk.
const FLOOR_FILES: usize = 300;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

/// The file minus every `#[cfg(test)]` item (same as `egress_guard.rs`'s).
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

/// The source minus `//` comments, string literals intact; comments may quote descriptions.
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

/// (files scanned, files whose production source contains `needle`)
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

    let mut hits = BTreeSet::new();
    for rel in &files {
        let Ok(src) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        if strip_line_comments(&production_source(&src)).contains(needle) {
            hits.insert(rel.clone());
        }
    }
    (files.len(), hits)
}

#[test]
fn no_second_copy_of_a_factory_description() {
    assert!(
        !BUILTIN_PROMPT_TEMPLATES.is_empty(),
        "the built-in table is empty -- this guard would certify nothing"
    );

    for &(name, _, description) in BUILTIN_PROMPT_TEMPLATES {
        let (files, hits) = scan(description);
        assert!(
            files >= FLOOR_FILES,
            "walked only {files} files (floor {FLOOR_FILES}) -- the walk is broken, \
             so an empty result proves nothing"
        );
        assert!(
            hits.contains(HOME),
            "'{name}': its description is not in {HOME}. Either the table moved or \
             this guard's needle has rotted; found in {hits:?}"
        );
        assert_eq!(
            hits.len(),
            1,
            "'{name}': its description is spelled in {} files, not just {HOME}: {hits:?}\n\
             Every writer must read BUILTIN_PROMPT_TEMPLATES. A second literal is how \
             `pond prompts reset` and the boot reseed came to disagree.",
            hits.len()
        );
    }
}

/// An unresolvable row would seed at boot but 404 on reset.
#[test]
fn every_row_resolves_through_the_public_lookup() {
    for &(name, content, description) in BUILTIN_PROMPT_TEMPLATES {
        let found = builtin_template_content(name)
            .unwrap_or_else(|| panic!("'{name}' is in the table but does not resolve"));
        assert_eq!(found.0, content, "'{name}': lookup returned other content");
        assert_eq!(
            found.1, description,
            "'{name}': lookup returned another description"
        );
    }
    assert!(
        builtin_template_content("definitely-not-a-builtin").is_none(),
        "an unknown name must not resolve, or `prompts reset` would invent a template"
    );
}

/// `Settings.prompt_style` stores these names; losing one strands users already on it.
#[test]
fn the_four_shipped_styles_are_all_present() {
    let names: BTreeSet<&str> = BUILTIN_PROMPT_TEMPLATES
        .iter()
        .map(|&(n, _, _)| n)
        .collect();
    for expected in ["balanced", "concise", "technical", "warm"] {
        assert!(
            names.contains(expected),
            "style '{expected}' is gone from the built-in table; installs already \
             carry it in Settings.prompt_style and would fall back to balanced"
        );
    }
}
