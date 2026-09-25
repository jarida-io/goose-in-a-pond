//! Locates GIAP's bundled `extensions/` tree, so bundled-script MCP entries don't depend on
//! the launch cwd (an MCP child resolves relative paths against the cwd it inherits).

use std::path::{Path, PathBuf};

/// Env override naming the directory that contains `extensions/`, for uninferable layouts.
pub const ASSET_ROOT_ENV: &str = "GIAP_ASSET_ROOT";

/// The first candidate holding `extensions/` (env, build repo, exe dir, cwd), else the cwd.
pub fn resolve() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();

    for candidate in candidates() {
        if has_extensions_dir(&candidate) {
            // Canonical, since it is shown and persisted (not `crates/pond-server/../..`).
            return candidate.canonicalize().unwrap_or(candidate);
        }
    }

    tracing::warn!(
        cwd = %cwd.display(),
        "no bundled extensions/ directory found; falling back to the current \
         working directory. Extensions that launch a bundled script will fail \
         to start — set {ASSET_ROOT_ENV} to the directory containing extensions/",
    );
    cwd
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Some(env_root) = std::env::var_os(ASSET_ROOT_ENV) {
        out.push(PathBuf::from(env_root));
    }

    // Compile-time repo root; exists only on the machine that built the binary.
    out.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            out.push(exe_dir.to_path_buf());
            // macOS app bundle: Contents/MacOS/pond-server -> Contents/Resources
            out.push(exe_dir.join("../Resources"));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd);
    }

    out
}

fn has_extensions_dir(root: &Path) -> bool {
    root.join("extensions").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_extensions_dir_detects_the_bundled_tree() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(
            has_extensions_dir(&repo_root),
            "the repo root should contain extensions/"
        );
        assert!(!has_extensions_dir(Path::new("/nonexistent-giap-root")));
    }

    #[test]
    fn resolve_prefers_the_environment_override() {
        // Env is process-global: assert the ordering rather than mutate it under parallel tests.
        let first = candidates().into_iter().next().expect("a candidate");
        match std::env::var_os(ASSET_ROOT_ENV) {
            Some(env_root) => assert_eq!(first, PathBuf::from(env_root)),
            None => assert_eq!(
                first,
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
            ),
        }
    }

    #[test]
    fn resolve_finds_a_root_holding_the_extensions_tree() {
        let root = resolve();
        assert!(
            has_extensions_dir(&root),
            "resolve() returned {} which has no extensions/ dir",
            root.display()
        );
    }

    #[test]
    fn resolve_returns_a_canonical_path() {
        let root = resolve();
        assert!(
            !root.components().any(|c| c.as_os_str() == ".."),
            "resolve() returned an uncanonicalized path: {}",
            root.display()
        );
    }
}
