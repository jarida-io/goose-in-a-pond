//! What the host can say about this run of the Pond: its boot id and uptime, whether the previous
//! run stopped gracefully (the run marker), and why a stop was asked for (the stop intent). The
//! decisions made from them are pond-core's (`network::domain::pond_status`); these are the files.
//!
//! - `<data_dir>/.pond_running` holds the boot id the current run started on. It is written at
//!   start and removed at the end of the graceful stop, so finding one at start means the previous
//!   run never got there.
//! - `<data_dir>/.stop_intent` holds one word, `restart`, `update` or `shutdown`, written by
//!   whatever asks the Pond to stop and read once by the stop that follows.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use pond_core::network::domain::pond_status::{boot_id, PreviousRun, StopReason};

const RUN_MARKER: &str = ".pond_running";
const STOP_INTENT: &str = ".stop_intent";

/// The most a marker or an intent file is read; both hold one short line.
const SMALL_FILE: u64 = 256;

/// How long a stop intent stays believed. One older than this was left by a stop that never
/// happened, and says nothing about the stop under way.
const INTENT_FRESHNESS: Duration = Duration::from_secs(10 * 60);

/// The host's kernel boot id, where it has one (Linux).
pub fn read_boot_id() -> Option<String> {
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    boot_id(&raw)
}

/// How long the host has been up, from its monotonic clock (Linux `/proc/uptime`).
pub fn read_uptime_seconds() -> Option<u64> {
    uptime_seconds(&std::fs::read_to_string("/proc/uptime").ok()?)
}

fn uptime_seconds(raw: &str) -> Option<u64> {
    let seconds: f64 = raw.split_whitespace().next()?.parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds as u64)
}

/// Starts this run: reads what the previous run left, writes this run's marker on
/// `current_boot`, and forgets any stop intent a stop that never happened left behind.
pub fn begin(data_dir: &Path, current_boot: Option<&str>) -> Result<PreviousRun> {
    let marker = data_dir.join(RUN_MARKER);
    let left = read_small(&marker)?;
    let previous = PreviousRun::from_marker(left.as_deref(), current_boot);
    write_synced(&marker, current_boot.unwrap_or(""))?;
    match std::fs::remove_file(data_dir.join(STOP_INTENT)) {
        Ok(()) => tracing::info!(
            target: "giap::trace",
            kind = "stop_intent_stale",
            "a stop intent from before this start was forgotten"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("could not clear the stop intent"),
    }
    Ok(previous)
}

/// Ends this run gracefully: the next start finds no marker and calls this stop clean.
pub fn end(data_dir: &Path) -> Result<()> {
    match std::fs::remove_file(data_dir.join(RUN_MARKER)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).context("could not remove the run marker")
        }
        _ => Ok(()),
    }
}

/// Records why the Pond is about to be stopped, for the stop that follows to report.
pub fn record_stop_intent(data_dir: &Path, reason: StopReason) -> Result<()> {
    write_synced(&data_dir.join(STOP_INTENT), reason.as_str())
}

/// Why this stop was asked for, if something recorded it in the last ten minutes. The intent is
/// removed, so it is never read twice.
pub fn take_stop_intent(data_dir: &Path) -> Option<StopReason> {
    let path = data_dir.join(STOP_INTENT);
    let fresh = std::fs::symlink_metadata(&path)
        .ok()
        .filter(|meta| meta.is_file())
        .and_then(|meta| meta.modified().ok())
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age <= INTENT_FRESHNESS);
    let reason = read_small(&path)
        .ok()
        .flatten()
        .and_then(|text| StopReason::from_intent(&text));
    if let Err(error) = std::fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(kind = "stop_intent_unremovable", %error, "could not remove the stop intent");
        }
    }
    reason.filter(|_| fresh)
}

/// A small regular file's text, or `None` when it does not exist. A symlink or anything larger
/// than [`SMALL_FILE`] is refused.
fn read_small(path: &Path) -> Result<Option<String>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("{}", path.display())),
    };
    anyhow::ensure!(
        meta.is_file() && meta.len() <= SMALL_FILE,
        "{} is not a small regular file",
        path.display()
    );
    Ok(Some(std::fs::read_to_string(path)?))
}

/// Writes `text` atomically and durably, so a power cut leaves the old file or the new one.
fn write_synced(path: &Path, text: &str) -> Result<()> {
    let directory = path.parent().context("a run file needs a directory")?;
    let name = path.file_name().context("a run file needs a name")?;
    let staged = directory.join(format!(
        "{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = (|| {
        let mut file = std::fs::File::create(&staged)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&staged, path)?;
        std::fs::File::open(directory)?.sync_all()
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written.with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::network::domain::pond_status::PreviousShutdown;

    const BOOT_A: &str = "0b5e1c2a-1111-4222-8333-944455556666";
    const BOOT_B: &str = "7f00aa10-2222-4333-8444-a55566667777";

    #[test]
    fn a_run_that_ends_gracefully_leaves_a_clean_start_and_one_that_does_not_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let first = begin(dir.path(), Some(BOOT_A)).unwrap();
        assert_eq!(
            first.shutdown,
            PreviousShutdown::Clean,
            "a first start is clean"
        );
        end(dir.path()).unwrap();
        assert_eq!(
            begin(dir.path(), Some(BOOT_A)).unwrap().shutdown,
            PreviousShutdown::Clean
        );

        // No end: the power went. The next start is on another boot.
        let after_cut = begin(dir.path(), Some(BOOT_B)).unwrap();
        assert!(after_cut.lost_power());

        // A crash on the same boot is unclean, but not a power cut.
        let after_crash = begin(dir.path(), Some(BOOT_B)).unwrap();
        assert_eq!(after_crash.shutdown, PreviousShutdown::Unclean);
        assert!(!after_crash.lost_power());
        end(dir.path()).unwrap();
        end(dir.path()).unwrap();
    }

    #[test]
    fn a_stop_intent_is_read_once_and_a_stale_one_is_forgotten_at_start() {
        let dir = tempfile::tempdir().unwrap();
        record_stop_intent(dir.path(), StopReason::Update).unwrap();
        assert_eq!(take_stop_intent(dir.path()), Some(StopReason::Update));
        assert_eq!(take_stop_intent(dir.path()), None);

        record_stop_intent(dir.path(), StopReason::Restart).unwrap();
        begin(dir.path(), None).unwrap();
        assert_eq!(take_stop_intent(dir.path()), None);

        std::fs::write(dir.path().join(STOP_INTENT), "nap").unwrap();
        assert_eq!(take_stop_intent(dir.path()), None);
        assert!(!dir.path().join(STOP_INTENT).exists());
    }

    #[test]
    fn an_oversized_marker_is_refused_rather_than_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RUN_MARKER), "x".repeat(1024)).unwrap();
        assert!(begin(dir.path(), Some(BOOT_A)).is_err());
    }

    #[test]
    fn uptime_is_the_first_field_in_whole_seconds() {
        assert_eq!(uptime_seconds("512.77 1880.12\n"), Some(512));
        assert_eq!(uptime_seconds(""), None);
        assert_eq!(uptime_seconds("-1 0"), None);
    }
}
