//! One voice session per user (not per data dir: the mic is what's contended). A `flock`, so
//! a dead holder never leaves it stale; the PID written inside only names the holder.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use fs2::FileExt;

/// Held for the lifetime of a voice session; releases on drop.
pub struct VoiceLock {
    file: File,
    path: PathBuf,
}

fn lock_path() -> PathBuf {
    // Windows has no uid, but its temp dir is already per-user.
    #[cfg(unix)]
    let name = format!("giap-voice-{}.lock", unsafe { libc::getuid() });
    #[cfg(not(unix))]
    let name = "giap-voice.lock".to_string();

    std::env::temp_dir().join(name)
}

impl VoiceLock {
    /// Take the voice lock, or fail with a message naming the holder and what to do.
    pub fn acquire() -> Result<Self> {
        let path = lock_path();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| anyhow!("cannot open the voice lock at {}: {e}", path.display()))?;

        if file.try_lock_exclusive().is_err() {
            // PID for the message only; best effort, it may be empty or partial.
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            let holder = holder.trim();

            let who = if holder.is_empty() {
                "another voice session".to_string()
            } else {
                format!("another voice session (pid {holder})")
            };

            return Err(anyhow!(
                "{who} is already running on this device.\n\
                 \n\
                 Voice needs sole use of the microphone and speaker. Two sessions \
                 answer in different voices and take the audio device from each \
                 other.\n\
                 \n\
                 Stop the other session first{}.",
                if holder.is_empty() {
                    String::new()
                } else {
                    format!(" — `kill {holder}`")
                }
            ));
        }

        // Record who holds it, for the next process's refusal message.
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;

        tracing::debug!("voice lock acquired at {}", path.display());
        Ok(Self { file, path })
    }
}

impl Drop for VoiceLock {
    fn drop(&mut self) {
        // Best effort: the kernel releases the lock at exit anyway; unlinking is just tidiness.
        let _ = FileExt::unlock(&self.file);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock is process-wide, so parallel tests would race and pass vacuously.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn the_lock_is_independent_of_the_data_directory() {
        let before = lock_path();
        std::env::set_var("POND_DATA_DIR", "/tmp/some-other-profile");
        let after = lock_path();
        std::env::remove_var("POND_DATA_DIR");
        assert_eq!(
            before, after,
            "the device is the resource, not the database"
        );
    }

    /// Two accounts on one machine have separate audio sessions.
    #[cfg(unix)]
    #[test]
    fn the_lock_is_scoped_to_the_user() {
        let name = lock_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let uid = unsafe { libc::getuid() };
        assert!(name.contains(&uid.to_string()), "{name} is not user-scoped");
    }

    #[test]
    fn a_second_session_is_refused_and_names_the_holder() {
        let _serial = serial();
        let first = VoiceLock::acquire().expect("nothing else should hold the lock in a test run");

        let err = VoiceLock::acquire()
            .err()
            .expect("a second session must be refused")
            .to_string();

        assert!(
            err.contains(&std::process::id().to_string()),
            "the refusal must name the holding process: {err}"
        );
        assert!(
            err.contains("microphone"),
            "the refusal must say why, not just no: {err}"
        );

        drop(first);
    }

    #[test]
    fn releasing_lets_the_next_session_start() {
        let _serial = serial();
        let first = VoiceLock::acquire().expect("nothing else should hold the lock in a test run");
        drop(first);

        let second = VoiceLock::acquire().expect("the lock must be free once the holder has gone");
        drop(second);
    }
}
