//! The process environment, changed safely while other threads read it.
//!
//! glibc's `setenv` grows the environment by reallocating the array it made the time before, which
//! frees the old one. A C library that calls `getenv` on another thread at that moment walks freed
//! memory. On the Jetson, ALSA did: it read `ALSA_CONFIG_PATH` while listing sound devices for the
//! speaker route, as the Goose adapter set its context knobs at start-up, and the pond died with
//! SIGSEGV in `getenv` on about one start in five (2026-11-09). Rust's own `std::env::var` takes
//! std's lock; C code reads `environ` directly and takes none.
//!
//! So once threads run, every change to the environment goes through [`set`] or [`remove`], which
//! hold [`ENV`] for writing, and a call into a C library that reads the environment on its own
//! thread (ALSA, through cpal and rodio) runs inside [`reading`]. The workspace test
//! `environment_changes_go_through_process_env` fails on a raw `set_var` or `remove_var` in
//! production code.

use std::ffi::OsStr;
use std::sync::RwLock;

/// Held for writing while the environment changes, and for reading around a C call that reads it.
static ENV: RwLock<()> = RwLock::new(());

/// Set `key` to `value` for this process, never while a guarded C call reads the environment.
pub fn set(key: &str, value: impl AsRef<OsStr>) {
    let _changing = ENV.write().unwrap_or_else(|e| e.into_inner());
    // SAFETY: every writer and every guarded C reader holds ENV, and std's own readers take
    // std's lock, so nothing walks the environment while setenv may free its array.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var(key, value)
    }
}

/// Remove `key` from this process's environment, under the same rule as [`set`].
pub fn remove(key: &str) {
    let _changing = ENV.write().unwrap_or_else(|e| e.into_inner());
    // SAFETY: as in `set`.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::remove_var(key)
    }
}

/// Run `call`, a call into C code that reads the environment, with no change to it under way.
///
/// Never nest it, and never [`set`] or [`remove`] inside it: a reader waiting behind a queued
/// writer would wait for itself.
pub fn reading<T>(call: impl FnOnce() -> T) -> T {
    let _reading = ENV.read().unwrap_or_else(|e| e.into_inner());
    call()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_remove_change_the_environment() {
        set("POND_PROCESS_ENV_TEST", "one");
        assert_eq!(std::env::var("POND_PROCESS_ENV_TEST").as_deref(), Ok("one"));
        set("POND_PROCESS_ENV_TEST", "two");
        assert_eq!(std::env::var("POND_PROCESS_ENV_TEST").as_deref(), Ok("two"));
        remove("POND_PROCESS_ENV_TEST");
        assert!(std::env::var("POND_PROCESS_ENV_TEST").is_err());
    }

    #[test]
    fn reading_returns_what_the_call_returns() {
        assert_eq!(reading(|| 41 + 1), 42);
    }

    /// Readers in C (here, libc's getenv) never see the array mid-change: many writers adding
    /// and removing variables while many guarded readers walk the environment.
    #[cfg(unix)]
    #[test]
    fn guarded_readers_survive_writers_growing_the_environment() {
        extern "C" {
            fn getenv(name: *const std::os::raw::c_char) -> *mut std::os::raw::c_char;
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let name = std::ffi::CString::new("PATH").unwrap();
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        // SAFETY: getenv on a valid C string, inside the guard.
                        let found = reading(|| unsafe { !getenv(name.as_ptr()).is_null() });
                        assert!(found);
                    }
                })
            })
            .collect();
        for round in 0..2000 {
            let key = format!("POND_PROCESS_ENV_GROW_{}", round % 64);
            set(&key, round.to_string());
            if round % 3 == 0 {
                remove(&key);
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for r in readers {
            r.join().expect("a guarded reader crashed");
        }
        for i in 0..64 {
            remove(&format!("POND_PROCESS_ENV_GROW_{i}"));
        }
    }
}
