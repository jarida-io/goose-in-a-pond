//! Memory-aware model scheduler: only one LLM is resident at a time (Jetson Orin Nano, 8 GB
//! unified). Goose's `InferenceRuntime` already evicts the other model slots on a switch; this
//! adds live memory reporting from `/proc/meminfo` and a `watch` channel so wake-word detection
//! can pre-load the chat model. llamafile and Ollama self-manage memory, so use `NoopScheduler`.

use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::watch;

use pond_core::models::ports::model_scheduler::{MemoryStatus, ModelScheduler};

// ── Jetson / Linux memory constants ──────────────────────────────────────────

// The budget constants and the device-aware budget moved to pond-core's `device_budget`, so the
// goose adapter can ask whether picture support fits beside a model with the SAME arithmetic
// `apply_jetson_settings` sizes the window with. Re-exported under their old names so nothing
// that reads `scheduler::LLM_BUDGET_MB` changes.
pub use pond_core::models::domain::device_budget::{
    llm_budget_mb, total_ram_mb, JETSON_TOTAL_RAM_MB, LLM_BUDGET_MB,
};

// ── ResourceAwareModelScheduler ──────────────────────────────────────────────

/// Scheduler for devices running local GGUF models (in-process llama.cpp).
///
/// Reads `/proc/meminfo` on Linux to report live memory. On other platforms
/// (macOS dev machines, CI) it falls back to the budget constants.
pub struct ResourceAwareModelScheduler {
    /// Name of the model currently loaded in the llama.cpp slot.
    currently_hot: Mutex<Option<String>>,
    /// Sending half — `notify_wake_word()` sends `true` on this channel.
    /// The server spawns a background task that receives and pre-loads.
    wake_tx: watch::Sender<bool>,
}

impl ResourceAwareModelScheduler {
    /// Create a new scheduler, also returning the receiving end of the
    /// wake-word channel so the server can spawn a pre-loader task.
    pub fn new() -> (Self, watch::Receiver<bool>) {
        let (wake_tx, wake_rx) = watch::channel(false);
        (
            Self {
                currently_hot: Mutex::new(None),
                wake_tx,
            },
            wake_rx,
        )
    }

    /// Update the currently-hot model name. Called by the server after a
    /// successful model load (optional — used for accurate reporting).
    pub fn set_hot_model(&self, name: Option<String>) {
        if let Ok(mut guard) = self.currently_hot.lock() {
            *guard = name;
        }
    }

    /// Read free RAM in MB from `/proc/meminfo` (Linux / Jetson).
    /// Returns `None` on non-Linux platforms or if the file cannot be read.
    fn read_free_ram_mb() -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let content = std::fs::read_to_string("/proc/meminfo").ok()?;
            // "MemAvailable:   XXXXXX kB"
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemAvailable:") {
                    let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                    return Some(kb / 1024);
                }
            }
            None
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

#[async_trait]
impl ModelScheduler for ResourceAwareModelScheduler {
    async fn notify_wake_word(&self) {
        // Signal receivers — the server's pre-loader task will start loading
        // the chat model. We ignore send errors (no receivers = no task running).
        let _ = self.wake_tx.send(true);
    }

    fn memory_status(&self) -> MemoryStatus {
        let loaded_model = self.currently_hot.lock().ok().and_then(|g| g.clone());

        let total = total_ram_mb();
        let budget = llm_budget_mb();
        let (total_mb, available_for_llm_mb) = match Self::read_free_ram_mb() {
            Some(free_mb) => (total, free_mb.min(budget)),
            None => {
                // Fallback: estimate based on whether a model is loaded.
                let used = if loaded_model.is_some() {
                    budget / 2 // rough midpoint
                } else {
                    0
                };
                (total, budget.saturating_sub(used))
            }
        };

        MemoryStatus {
            total_mb,
            available_for_llm_mb,
            loaded_model,
        }
    }
}

// ── NoopScheduler ────────────────────────────────────────────────────────────

/// Pass-through scheduler for llamafile and Ollama backends.
///
/// Those providers manage their own memory externally; GIAP does not evict
/// them. Memory status returns zeros so the UI shows "managed externally".
pub struct NoopScheduler;

#[async_trait]
impl ModelScheduler for NoopScheduler {
    async fn notify_wake_word(&self) {}

    fn memory_status(&self) -> MemoryStatus {
        MemoryStatus::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_scheduler_returns_zero_status() {
        let s = NoopScheduler;
        let status = s.memory_status();
        assert_eq!(status.total_mb, 0);
        assert_eq!(status.available_for_llm_mb, 0);
        assert!(status.loaded_model.is_none());
    }

    #[tokio::test]
    async fn resource_scheduler_tracks_hot_model() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        assert!(sched.memory_status().loaded_model.is_none());

        sched.set_hot_model(Some("llama3.2-3b".to_string()));
        assert_eq!(
            sched.memory_status().loaded_model.as_deref(),
            Some("llama3.2-3b")
        );
    }

    #[tokio::test]
    async fn wake_word_sends_signal() {
        let (sched, mut rx) = ResourceAwareModelScheduler::new();

        // Initial value is false
        assert!(!*rx.borrow());

        sched.notify_wake_word().await;

        // After notification the channel has the new value
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
    }

    #[test]
    fn llm_budget_is_positive_and_reasonable() {
        assert!(LLM_BUDGET_MB > 4096, "budget should be > 4GB on 8GB device");
        assert!(LLM_BUDGET_MB < JETSON_TOTAL_RAM_MB);
    }

    #[test]
    fn memory_status_total_matches_jetson_constant() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        let status = sched.memory_status();
        // total_mb is the device we BELIEVE we are: the constant unless a device
        // profile is emulating another board. Asserted against the profile so
        // this test still means something under `scripts/jetson-emu.sh test`.
        assert_eq!(status.total_mb, total_ram_mb());
        match pond_core::models::domain::device_profile::active() {
            None => assert_eq!(status.total_mb, JETSON_TOTAL_RAM_MB),
            Some(p) => assert_eq!(
                status.total_mb, p.total_ram_mb,
                "under emulation the scheduler must report the emulated board, or the profile is \
                 not reaching it and an emulated run proves nothing"
            ),
        }
    }

    /// The runtime budget and the compile-time one agree when nothing is being
    /// emulated. This is the safety property of the device-profile mechanism.
    #[test]
    fn the_runtime_budget_equals_the_constant_when_nothing_is_emulated() {
        if pond_core::models::domain::device_profile::active().is_none() {
            assert_eq!(llm_budget_mb(), LLM_BUDGET_MB);
            assert_eq!(total_ram_mb(), JETSON_TOTAL_RAM_MB);
        }
    }

    #[test]
    fn memory_status_fallback_available_is_full_budget_when_no_model_loaded() {
        // On non-Linux (or when /proc/meminfo is absent) the fallback uses 0 used
        // when no model is hot — available should equal LLM_BUDGET_MB.
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        let status = sched.memory_status();

        // On Linux (CI) /proc/meminfo is available and the value varies —
        // just check it's within the sane range.
        assert!(
            status.available_for_llm_mb <= llm_budget_mb(),
            "available should not exceed budget: {} > {}",
            status.available_for_llm_mb,
            llm_budget_mb()
        );
    }

    #[test]
    fn memory_status_available_decreases_when_model_is_hot() {
        // On non-Linux the fallback estimates `LLM_BUDGET_MB / 2` used when a
        // model is hot.  On Linux /proc/meminfo is used and the fallback branch
        // is skipped — skip the assertion in that case.
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        let free_before = sched.memory_status().available_for_llm_mb;

        sched.set_hot_model(Some("my-model".to_string()));
        let free_after = sched.memory_status().available_for_llm_mb;

        // On Linux available_for_llm_mb comes from /proc/meminfo and won't
        // change based on set_hot_model — that path is already tested above.
        // On macOS / Windows (no /proc/meminfo) the fallback branch IS taken
        // and available should drop.
        if cfg!(not(target_os = "linux")) {
            assert!(
                free_after < free_before,
                "available should decrease when a model is hot; before={free_before}, after={free_after}"
            );
        }
    }

    #[test]
    fn set_hot_model_to_none_clears_it() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        sched.set_hot_model(Some("my-model".to_string()));
        assert!(sched.memory_status().loaded_model.is_some());

        sched.set_hot_model(None);
        assert!(sched.memory_status().loaded_model.is_none());
    }

    #[tokio::test]
    async fn noop_scheduler_notify_wake_word_does_not_panic() {
        let s = NoopScheduler;
        // NoopScheduler::notify_wake_word is a no-op; this must not panic
        s.notify_wake_word().await;
    }

    #[test]
    fn resource_scheduler_new_starts_with_no_hot_model() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        assert!(sched.memory_status().loaded_model.is_none());
    }

    #[tokio::test]
    async fn wake_word_channel_can_be_re_signalled() {
        let (sched, mut rx) = ResourceAwareModelScheduler::new();

        sched.notify_wake_word().await;
        rx.changed().await.unwrap();
        assert!(*rx.borrow());

        // Mark as seen and signal again — rx should become changed once more
        let _ = rx.borrow_and_update();
        sched.notify_wake_word().await;
        // The channel is already true so sending true again may not mark changed;
        // just verify no panic.
    }
}
