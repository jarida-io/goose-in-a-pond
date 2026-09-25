//! Puts Node on PATH at startup: a GUI launch (macOS launchd) gets a bare PATH, hiding nvm or
//! Homebrew Node from the Matter controller and from `npx` MCP extensions spawned by the client.

use std::path::PathBuf;

/// Plausible Node install dirs, most specific first.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        // nvm's `default` alias is not a directory, so take the highest installed version.
        for base in [".nvm/versions/node", ".local/share/fnm/node-versions"] {
            let root = home.join(base);
            if let Ok(entries) = std::fs::read_dir(&root) {
                let mut versions: Vec<PathBuf> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                // Lexicographic; fine for v20-v24, and Matter rejects a too-old pick.
                versions.sort();
                dirs.extend(
                    versions
                        .into_iter()
                        .rev()
                        .flat_map(|v| [v.join("bin"), v.join("installation/bin")].into_iter()),
                );
            }
        }
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".asdf/shims"));
        dirs.push(home.join(".local/bin"));
    }

    // Homebrew (Apple silicon, then Intel), then distro locations.
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));

    dirs
}

fn holds_node(dir: &std::path::Path) -> bool {
    dir.join("node").is_file()
}

fn node_dir_in(path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?).find(|dir| holds_node(dir))
}

/// Dir to prepend so Node is reachable under `path`; `None` if it already is or none exists.
fn dir_to_prepend(path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if node_dir_in(path).is_some() {
        return None;
    }
    candidate_dirs().into_iter().find(|dir| holds_node(dir))
}

/// Prepends a Node dir to `PATH` if needed and returns it. Calls [`std::env::set_var`], so
/// call it once at the top of `main`, before any thread or child is spawned.
pub fn ensure_node_on_path() -> Option<PathBuf> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let Some(found) = dir_to_prepend(Some(&current)) else {
        tracing::debug!("node: already reachable, or nowhere to be found");
        return None;
    };

    let mut dirs = vec![found.clone()];
    dirs.extend(std::env::split_paths(&current));
    let joined = std::env::join_paths(dirs).ok()?;
    std::env::set_var("PATH", joined);

    tracing::info!(
        target: "giap::trace",
        kind = "node_path_extended",
        dir = %found.display(),
        "node was not on PATH; added the directory it is in so Matter and the \
         stdio extensions can find it"
    );
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_without_node_is_not_offered() {
        let empty = std::env::temp_dir().join(format!("giap-nodepath-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!holds_node(&empty));

        std::fs::write(empty.join("node"), "#!/bin/sh\n").unwrap();
        assert!(
            holds_node(&empty),
            "a directory with a node in it qualifies"
        );

        std::fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn the_candidate_list_covers_the_managers_people_actually_use() {
        let dirs = candidate_dirs();
        let rendered: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        let joined = rendered.join(" ");

        for expected in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            assert!(joined.contains(expected), "missing {expected} in {joined}");
        }
        if std::env::var_os("HOME").is_some() {
            for expected in [".volta/bin", ".asdf/shims"] {
                assert!(joined.contains(expected), "missing {expected}");
            }
        }
    }

    #[test]
    fn a_launchd_path_gets_node_prepended() {
        let bare = std::ffi::OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");

        // Nothing to assert where Node is already in a system location.
        let Some(found) = dir_to_prepend(Some(&bare)) else {
            return;
        };
        assert!(
            holds_node(&found),
            "offered {} as a node directory",
            found.display()
        );
        assert!(
            !bare
                .to_string_lossy()
                .contains(&found.display().to_string()),
            "prepended a directory that was already on the PATH"
        );
    }

    #[test]
    fn a_path_that_already_reaches_node_is_left_alone() {
        let Some(dir) = node_dir_in(std::env::var_os("PATH").as_deref()) else {
            return; // no Node here
        };
        let just_that = std::ffi::OsString::from(dir.display().to_string());
        assert!(dir_to_prepend(Some(&just_that)).is_none());
    }
}
