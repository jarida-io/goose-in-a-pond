//! Points goose's LiteRT-LM backend at an installed library before any provider is built, and says
//! whether the backend will find one at all. It only ever sets `GOOSE_LITERT_LIB_DIR`, only when
//! that is unset, and downloads nothing.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The variable goose's `litert` backend searches first.
const LIB_DIR_ENV: &str = "GOOSE_LITERT_LIB_DIR";
/// Beside the executable: where the app bundle carries the library, and where the backend looks
/// when the variable is unset.
const BUNDLE_SUBDIR: &str = "litert-lm";
/// Overrides `~/.giap/litert-lm`, as it does for `scripts/lib/litert-setup.sh`.
const HOME_ENV: &str = "GIAP_LITERT_HOME";
/// goose's state root, which `pin_goose_state_under` sets before this module runs.
const GOOSE_ROOT_ENV: &str = "GOOSE_PATH_ROOT";
/// Under goose's state root: the data directory's `litert-lm/lib`, the backend's last place to look.
const GOOSE_DATA_SUBDIR: &str = "data/litert-lm/lib";
/// `metal` runs a package this finds on a Mac on LiteRT's Metal accelerator, as the desktop app
/// does; otherwise it runs on WebGPU, as the Orin does. See [`metal_view`].
const ACCELERATOR_ENV: &str = "GIAP_LITERT_ACCELERATOR";
/// What a package needs only to run on WebGPU: the accelerator, its sampler, and Dawn, which only
/// those two load. `LITERT_DESKTOP_LEAVES_OUT` in `scripts/lib/litert-setup.sh` names the same.
const WEBGPU_ONLY: &[&str] = &[
    "libLiteRtWebGpuAccelerator.dylib",
    "libLiteRtTopKWebGpuSampler.dylib",
    "libwebgpu_dawn.dylib",
];
/// Appended to a package directory's name to name its [`metal_view`].
const METAL_VIEW_SUFFIX: &str = ".metal";

#[cfg(target_os = "macos")]
const LIB_FILE: &str = "liblitert-lm.dylib";
#[cfg(windows)]
const LIB_FILE: &str = "litert-lm.dll";
#[cfg(not(any(target_os = "macos", windows)))]
const LIB_FILE: &str = "liblitert-lm.so";

/// Which library the backend loads. An explicit `GOOSE_LITERT_LIB_DIR` wins. A library in
/// `litert-lm/` beside the executable is the one an app was built and verified with, so it is
/// left to the backend, which finds it there; otherwise the variable is set to the newest
/// package under `<data_dir>/lib/litert-lm/` (a device), then to the one
/// `~/.giap/litert-lm/.path-<platform>` records (a development Mac). Nothing found leaves it unset.
///
/// On a Mac the app carries only the Metal accelerator, so it runs on Metal. A package this finds
/// carries both and runs on WebGPU, the Orin's accelerator, so tests on a Mac reflect the Orin;
/// `GIAP_LITERT_ACCELERATOR=metal` runs it on Metal instead.
///
/// Returns whether the backend will find a library in any of the places it searches, which decides
/// whether the catalogue offers LiteRT-LM models at all.
pub fn ensure_library_dir(data_dir: &Path) -> bool {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    if let Some(dir) = env_dir(LIB_DIR_ENV) {
        tracing::info!(
            dir = %dir.display(),
            "LiteRT-LM library directory from {LIB_DIR_ENV}"
        );
    } else {
        let bundled = exe_dir.as_deref().map(|dir| dir.join(BUNDLE_SUBDIR));
        match choose(bundled.as_deref(), data_dir, record_file().as_deref()) {
            Choice::Bundled(dir) => tracing::info!(
                dir = %dir.display(),
                "LiteRT-LM library beside the executable; left to the litert backend"
            ),
            Choice::Found(dir, from) => {
                let dir = if wants_metal() {
                    metal_view(&dir).unwrap_or(dir)
                } else {
                    dir
                };
                tracing::info!(dir = %dir.display(), from, "LiteRT-LM library directory");
                pond_core::shared::services::process_env::set(LIB_DIR_ENV, &dir);
            }
            Choice::Nothing => tracing::info!(
                "no LiteRT-LM library beside the executable, in the data directory or recorded; \
                 the litert backend falls back to its own search"
            ),
        }
    }
    let goose_root = env_dir(GOOSE_ROOT_ENV).filter(|root| root.is_absolute());
    let searched = backend_search_dirs(
        env_dir(LIB_DIR_ENV).as_deref(),
        exe_dir.as_deref(),
        goose_root.as_deref(),
    );
    let found = library_in(&searched);
    if !found {
        let searched = searched
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        // A warning only for a pond that has LiteRT-LM models to run; most never miss the library.
        if has_litert_models(data_dir) {
            tracing::warn!(
                searched,
                "no LiteRT-LM library where the litert backend looks, so this pond's LiteRT-LM \
                 models cannot run until one is installed (`giap.sh litert build`) and the pond \
                 restarts"
            );
        } else {
            tracing::info!(
                searched,
                "no LiteRT-LM library where the litert backend looks; LiteRT-LM models are listed \
                 as unavailable until one is installed (`giap.sh litert build`) and the pond \
                 restarts"
            );
        }
    }
    found
}

/// Whether a `.litertlm` model has been downloaded into this pond.
fn has_litert_models(data_dir: &Path) -> bool {
    std::fs::read_dir(pond_core::models::domain::litert::models_dir(data_dir))
        .map(|entries| {
            entries.flatten().any(|entry| {
                pond_core::models::domain::litert::is_litert_model(
                    &entry.file_name().to_string_lossy(),
                )
            })
        })
        .unwrap_or(false)
}

/// A non-empty directory variable.
fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// The directories goose's `litert` backend searches, in its order (`library_dirs` in
/// `goose-local-inference/src/litert/ffi.rs`): `GOOSE_LITERT_LIB_DIR`, `litert-lm/` beside the
/// executable, the executable's own directory, then goose's data directory. That last one is known
/// here only under an absolute `GOOSE_PATH_ROOT`; without one goose uses its own app directory,
/// which pond-server never installs into.
fn backend_search_dirs(
    lib_dir: Option<&Path>,
    exe_dir: Option<&Path>,
    goose_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = lib_dir.map(Path::to_path_buf).into_iter().collect();
    if let Some(exe_dir) = exe_dir {
        dirs.push(exe_dir.join(BUNDLE_SUBDIR));
        dirs.push(exe_dir.to_path_buf());
    }
    dirs.extend(goose_root.map(|root| root.join(GOOSE_DATA_SUBDIR)));
    dirs
}

/// Whether any of `dirs` holds the library, as the backend's own search decides it.
fn library_in(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| dir.join(LIB_FILE).is_file())
}

/// `GIAP_LITERT_ACCELERATOR=metal`, on a Mac.
fn wants_metal() -> bool {
    cfg!(target_os = "macos")
        && std::env::var(ACCELERATOR_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("metal"))
}

/// A sibling of the package `dir`, `<dir>.metal`, holding hard links to its files except
/// [`WEBGPU_ONLY`]: the file set the desktop app carries. `None` when the package has no WebGPU
/// accelerator or the view cannot be made, and the package is used as it is.
///
/// LiteRT-LM never names a runtime library directory, so LiteRT opens its GPU accelerator by bare
/// file name and the loader finds it beside libLiteRt; on macOS it tries WebGPU before Metal and
/// keeps the first (`gpu_registry.cc`), so a package carrying both runs WebGPU over Metal. On an
/// M4 with E2B, Metal measured warm turns at 129-142 ms to the first token against 155-192, a
/// 128-token prefill chunk 84-92 ms against 111-143, and decode about 13% faster. Hard links, not
/// symbolic ones: the loader resolves a symbolic link to the package and finds WebGPU there again.
fn metal_view(dir: &Path) -> Option<PathBuf> {
    if !WEBGPU_ONLY.iter().any(|f| dir.join(f).is_file()) {
        return None;
    }
    let mut name = dir.file_name()?.to_os_string();
    name.push(METAL_VIEW_SUFFIX);
    let view = dir.with_file_name(name);
    let make = || -> std::io::Result<()> {
        std::fs::create_dir_all(&view)?;
        let mut wanted = std::collections::HashSet::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let file = entry.file_name();
            if !entry.file_type()?.is_file() || WEBGPU_ONLY.iter().any(|f| file == *f) {
                continue;
            }
            let link = view.join(&file);
            if !same_file(&entry.path(), &link) {
                let _ = std::fs::remove_file(&link);
                std::fs::hard_link(entry.path(), &link)?;
            }
            wanted.insert(file);
        }
        for entry in std::fs::read_dir(&view)? {
            let entry = entry?;
            if !wanted.contains(&entry.file_name()) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    };
    match make() {
        Ok(()) => Some(view),
        Err(error) => {
            tracing::warn!(%error, view = %view.display(), "could not prepare the Metal view of the LiteRT-LM package; it runs on WebGPU");
            None
        }
    }
}

/// Whether `a` and `b` are the same file (one inode), so an existing link is current.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

/// Where the library is, by who should point the backend at it.
#[derive(Debug, PartialEq, Eq)]
enum Choice {
    /// Beside the executable: the backend finds it without being told.
    Bundled(PathBuf),
    /// Elsewhere, so `GOOSE_LITERT_LIB_DIR` must name it; with where it was found.
    Found(PathBuf, &'static str),
    Nothing,
}

fn choose(bundled: Option<&Path>, data_dir: &Path, record: Option<&Path>) -> Choice {
    if let Some(dir) = bundled.filter(|dir| dir.join(LIB_FILE).is_file()) {
        return Choice::Bundled(dir.to_path_buf());
    }
    match find_library_dir(data_dir, record) {
        Some((dir, from)) => Choice::Found(dir, from),
        None => Choice::Nothing,
    }
}

/// The library's directory and where it was found.
fn find_library_dir(data_dir: &Path, record: Option<&Path>) -> Option<(PathBuf, &'static str)> {
    newest_package(&data_dir.join("lib").join("litert-lm"))
        .map(|dir| (dir, "data directory"))
        .or_else(|| {
            record
                .and_then(recorded_dir)
                .map(|dir| (dir, "recorded package"))
        })
        .map(|(dir, from)| (std::fs::canonicalize(&dir).unwrap_or(dir), from))
}

/// The subdirectory of `root` whose library was modified last; ties go to the later name. A
/// [`metal_view`] is no package: its links share the package's time and its name sorts later.
fn newest_package(root: &Path) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .ends_with(METAL_VIEW_SUFFIX)
        {
            continue;
        }
        let dir = entry.path();
        let Ok(meta) = std::fs::metadata(dir.join(LIB_FILE)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(t, d)| (modified, &dir) > (*t, d))
        {
            newest = Some((modified, dir));
        }
    }
    newest.map(|(_, dir)| dir)
}

/// The package directory on the record's first line, if it holds the library. The line may
/// also name the library file itself.
fn recorded_dir(record: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(record).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let named = PathBuf::from(line);
    let dir = if named.is_file() {
        named.parent()?.to_path_buf()
    } else {
        named
    };
    dir.join(LIB_FILE).is_file().then_some(dir)
}

/// `~/.giap/litert-lm/.path-<platform>`, for the platforms a package is built for.
fn record_file() -> Option<PathBuf> {
    let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-arm64"
    } else {
        return None;
    };
    let home = match std::env::var_os(HOME_ENV).filter(|h| !h.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => dirs::home_dir()?.join(".giap").join("litert-lm"),
    };
    Some(home.join(format!(".path-{platform}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn package(root: &Path, name: &str, age_secs: u64) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let lib = dir.join(LIB_FILE);
        std::fs::write(&lib, b"lib").unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&lib)
            .unwrap()
            .set_modified(when)
            .unwrap();
        dir
    }

    #[test]
    fn the_newest_library_under_the_data_dir_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib").join("litert-lm");
        package(&root, "1.0.0-aaaaaaaa", 3600);
        let newer = package(&root, "1.0.0-bbbbbbbb", 60);
        std::fs::create_dir_all(root.join("1.0.1-empty")).unwrap();
        assert_eq!(newest_package(&root), Some(newer));
    }

    #[test]
    fn the_data_dir_comes_before_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = package(&tmp.path().join("lib").join("litert-lm"), "1.0.0-a", 10);
        let elsewhere = package(&tmp.path().join("home"), "macos-arm64", 10);
        let record = tmp.path().join(".path-macos-arm64");
        std::fs::write(&record, format!("{}\n", elsewhere.display())).unwrap();

        let (dir, from) = find_library_dir(tmp.path(), Some(&record)).unwrap();
        assert_eq!(dir, std::fs::canonicalize(installed).unwrap());
        assert_eq!(from, "data directory");
    }

    #[test]
    fn the_record_names_a_package_directory_or_its_library() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = package(tmp.path(), "macos-arm64", 10);
        let record = tmp.path().join(".path-macos-arm64");

        std::fs::write(&record, format!("{}\n", pkg.display())).unwrap();
        assert_eq!(recorded_dir(&record), Some(pkg.clone()));

        std::fs::write(&record, format!("{}\n", pkg.join(LIB_FILE).display())).unwrap();
        assert_eq!(recorded_dir(&record), Some(pkg.clone()));

        let (dir, from) = find_library_dir(&tmp.path().join("no-pond"), Some(&record)).unwrap();
        assert_eq!(dir, std::fs::canonicalize(&pkg).unwrap());
        assert_eq!(from, "recorded package");
    }

    /// An app must load the library it was built with, not a development package.
    #[test]
    fn the_library_beside_the_executable_comes_first() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = package(&tmp.path().join("Resources"), "litert-lm", 10);
        package(&tmp.path().join("lib").join("litert-lm"), "1.0.0-a", 10);
        let elsewhere = package(&tmp.path().join("home"), "macos-arm64", 10);
        let record = tmp.path().join(".path-macos-arm64");
        std::fs::write(&record, format!("{}\n", elsewhere.display())).unwrap();

        assert_eq!(
            choose(Some(&bundled), tmp.path(), Some(&record)),
            Choice::Bundled(bundled)
        );
        let empty = tmp.path().join("Resources-without").join("litert-lm");
        assert!(matches!(
            choose(Some(&empty), tmp.path(), Some(&record)),
            Choice::Found(_, "data directory")
        ));
        assert_eq!(
            choose(Some(&empty), &tmp.path().join("no-pond"), None),
            Choice::Nothing
        );
    }

    #[test]
    fn the_metal_view_leaves_out_webgpu_and_follows_the_package() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("macos-arm64");
        std::fs::create_dir_all(&pkg).unwrap();
        for f in [
            "liblitert-lm.dylib",
            "libLiteRt.dylib",
            "libLiteRtMetalAccelerator.dylib",
            "libLiteRtWebGpuAccelerator.dylib",
            "libLiteRtTopKWebGpuSampler.dylib",
            "libwebgpu_dawn.dylib",
        ] {
            std::fs::write(pkg.join(f), f).unwrap();
        }
        let view = metal_view(&pkg).expect("a package with WebGPU gets a view");
        assert_eq!(view, tmp.path().join("macos-arm64.metal"));
        let mut names: Vec<_> = std::fs::read_dir(&view)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "libLiteRt.dylib",
                "libLiteRtMetalAccelerator.dylib",
                "liblitert-lm.dylib"
            ]
        );
        assert!(same_file(
            &pkg.join("libLiteRt.dylib"),
            &view.join("libLiteRt.dylib")
        ));

        // A rebuilt package (new files, one gone) is followed, not served stale.
        std::fs::remove_file(pkg.join("libLiteRt.dylib")).unwrap();
        std::fs::write(pkg.join("libLiteRt.dylib"), "rebuilt").unwrap();
        std::fs::remove_file(pkg.join("libLiteRtMetalAccelerator.dylib")).unwrap();
        metal_view(&pkg).unwrap();
        assert_eq!(
            std::fs::read_to_string(view.join("libLiteRt.dylib")).unwrap(),
            "rebuilt"
        );
        assert!(!view.join("libLiteRtMetalAccelerator.dylib").exists());
    }

    /// A run asking for Metal must not leave every later run on Metal.
    #[test]
    fn a_metal_view_is_never_taken_for_the_newest_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib").join("litert-lm");
        let pkg = package(&root, "1.0.0-aaaaaaaa", 60);
        std::fs::write(pkg.join("libLiteRtWebGpuAccelerator.dylib"), "webgpu").unwrap();
        let view = metal_view(&pkg).unwrap();
        assert!(view.join(LIB_FILE).is_file());
        assert_eq!(newest_package(&root), Some(pkg));
    }

    #[test]
    fn a_package_without_webgpu_is_used_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("macos-arm64");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("liblitert-lm.dylib"), "lib").unwrap();
        assert_eq!(metal_view(&pkg), None);
    }

    #[test]
    fn a_record_without_a_library_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let record = tmp.path().join(".path-macos-arm64");
        assert_eq!(recorded_dir(&record), None, "no record file");

        std::fs::write(&record, "\n").unwrap();
        assert_eq!(recorded_dir(&record), None, "empty record");

        std::fs::write(&record, format!("{}\n", tmp.path().display())).unwrap();
        assert_eq!(
            recorded_dir(&record),
            None,
            "a directory without the library"
        );

        assert_eq!(find_library_dir(tmp.path(), Some(&record)), None);
        assert_eq!(find_library_dir(tmp.path(), None), None);
    }

    #[test]
    fn the_backend_search_follows_goose_order() {
        let dirs = backend_search_dirs(
            Some(Path::new("/lib-dir")),
            Some(Path::new("/app")),
            Some(Path::new("/pond/engine")),
        );
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/lib-dir"),
                PathBuf::from("/app/litert-lm"),
                PathBuf::from("/app"),
                PathBuf::from("/pond/engine/data/litert-lm/lib"),
            ]
        );
        assert!(backend_search_dirs(None, None, None).is_empty());
    }

    /// The catalogue's availability rests on this: a library in any searched place counts, and an
    /// empty search, as on a Jetson nothing was installed on, does not.
    #[test]
    fn the_library_counts_wherever_the_backend_would_find_it() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_dir = tmp.path().join("target").join("release");
        let goose_root = tmp.path().join("engine");
        let named = tmp.path().join("named-but-empty");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&named).unwrap();

        let searched = backend_search_dirs(Some(&named), Some(&exe_dir), Some(&goose_root));
        assert!(!library_in(&searched), "nothing installed anywhere");

        package(&goose_root.join("data").join("litert-lm"), "lib", 10);
        assert!(library_in(&searched), "goose's data directory");
        assert!(
            !library_in(&backend_search_dirs(Some(&named), Some(&exe_dir), None)),
            "goose's data directory is unknown without its root"
        );
    }
}
