//! The household's files in the data directory, which a factory reset empties. Only paths under
//! the data directory are touched, and no symlink is followed out of it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Folders that hold only the household's things. Each is emptied, not removed: the writers that
/// fill them keep running across the reset.
const FOLDERS: &[&[&str]] = &[
    &["attachments"],
    // Camera event pictures.
    &["snapshots"],
    // The MCP memory server's store.
    &["memory"],
    // The chosen microphone and speaker, which carry the household's device names.
    &["audio"],
    &["report-images"],
    &["news-audio"],
    // Saved prompt states: the conversations they were computed from, in another form.
    &["engine", "data", "prompt-cache"],
    &["engine", "data", "litert-lm", "kv-snapshots"],
    // The engine's request logs, which hold whole prompts.
    &["engine", "state", "logs"],
];

/// Files at the top of the data directory: the scheduler's tasks and their runs.
const FILES: &[&str] = &["schedules.json", "schedule_runs.json"];

/// Prefixes of files at the top: schedule files the scheduler set aside as unreadable.
const FILE_PREFIXES: &[&str] = &["schedules.json.unreadable-"];

/// Where the engine keeps its own copy of every conversation.
pub const ENGINE_SESSIONS: &[&str] = &["engine", "data", "sessions", "sessions.db"];

/// Empties every household folder and removes the household's top-level files.
pub fn erase(data_dir: &Path) -> Result<()> {
    for folder in FOLDERS {
        let Some(path) = beneath(data_dir, folder) else {
            continue;
        };
        empty(&path).with_context(|| format!("emptying {}", path.display()))?;
    }
    for entry in
        fs::read_dir(data_dir).with_context(|| format!("reading {}", data_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let household = FILES.contains(&name.as_ref())
            || FILE_PREFIXES.iter().any(|prefix| name.starts_with(prefix));
        if household && !entry.file_type()?.is_dir() {
            remove_file(&entry.path())?;
        }
    }
    Ok(())
}

/// `segments` under `data_dir`, or `None` when something on the way is missing, is not a folder,
/// or is a symlink: what a link points at is not the data directory's to erase.
pub fn beneath(data_dir: &Path, segments: &[&str]) -> Option<PathBuf> {
    let mut at = data_dir.to_path_buf();
    for segment in segments {
        at.push(segment);
        let meta = fs::symlink_metadata(&at).ok()?;
        if meta.file_type().is_symlink() {
            tracing::warn!(
                path = %at.display(),
                "factory reset: left a link out of the data directory, and what it points at, alone"
            );
            return None;
        }
    }
    Some(at)
}

/// Removes everything inside `dir`, keeping `dir` itself. A link inside is removed, never followed.
fn empty(dir: &Path) -> Result<()> {
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            // Unlinks symlinks inside the tree rather than following them.
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .or_else(already_gone)
        .with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<()> {
    fs::remove_file(path)
        .or_else(already_gone)
        .with_context(|| format!("removing {}", path.display()))
}

fn already_gone(error: io::Error) -> io::Result<()> {
    match error.kind() {
        io::ErrorKind::NotFound => Ok(()),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn the_households_files_go_and_the_ponds_own_stay() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let household = [
            "attachments/session-1/photo.jpg",
            "snapshots/front-door.jpg",
            "memory/facts.json",
            "audio/devices-input.json",
            "report-images/chart.png",
            "news-audio/digest.wav",
            "engine/data/prompt-cache/suspended/task-1",
            "engine/data/litert-lm/kv-snapshots/s1",
            "engine/state/logs/llm_request.0.jsonl",
            "schedules.json",
            "schedule_runs.json",
            "schedules.json.unreadable-20261101T000000.000000Z",
        ];
        let pond = [
            "secrets/master.key",
            "models/gemma.gguf",
            "hf_cache/models--x/blob",
            "engine/data/models/whisper.bin",
            "engine/data/litert-lm/lib/libLiteRt.so",
            "engine/config/config.yaml",
            "tls/identity.pem",
            "prompts/system.md",
            "logs/pond.log",
            ".runtime_host_credential",
        ];
        for path in household.iter().chain(pond.iter()) {
            write(data, path);
        }

        erase(data).unwrap();

        for path in household {
            assert!(!data.join(path).exists(), "{path} is the household's");
        }
        for path in pond {
            assert!(data.join(path).exists(), "{path} is the pond's own");
        }
        for folder in ["attachments", "snapshots", "engine/state/logs"] {
            assert!(
                data.join(folder).is_dir(),
                "{folder} is emptied, not removed"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn nothing_outside_the_data_directory_is_erased() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let data = tmp.path();
        write(outside.path(), "attachments/kept.jpg");
        write(outside.path(), "picture.jpg");
        std::os::unix::fs::symlink(outside.path().join("attachments"), data.join("attachments"))
            .unwrap();
        fs::create_dir_all(data.join("snapshots")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("picture.jpg"),
            data.join("snapshots/linked.jpg"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), data.join("engine")).unwrap();
        write(outside.path(), "state/logs/request.jsonl");

        erase(data).unwrap();

        assert!(outside.path().join("attachments/kept.jpg").exists());
        assert!(outside.path().join("picture.jpg").exists());
        assert!(outside.path().join("state/logs/request.jsonl").exists());
        assert!(
            !data.join("snapshots/linked.jpg").exists(),
            "the link itself goes"
        );
    }

    #[test]
    fn a_fresh_data_directory_erases_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        erase(tmp.path()).unwrap();
    }
}
