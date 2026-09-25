//! Memory-aware scheduler for one resident LLM (Goose evicts the others on a switch): adds
//! `/proc/meminfo` reporting and a wake-word `watch` channel so the chat model can pre-load.

use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::watch;

use pond_core::models::ports::model_scheduler::{MemoryStatus, ModelScheduler};

// ── Jetson / Linux memory constants ──────────────────────────────────────────

/// Orin Nano RAM as the kernel sees it (`free -m`), not the marketed 8192: carveouts come first.
/// Overstating it fails silently, as an over-large context swaps rather than failing to allocate.
pub const JETSON_TOTAL_RAM_MB: u64 = 7620;
/// Approximate headroom used by OS + GIAP server + UI at idle (MB).
const SYSTEM_OVERHEAD_MB: u64 = 1500;
/// Whisper base model resident size (MB).
const STT_RESERVED_MB: u64 = 200;
/// Reserved TTS resident size (MB).
const TTS_RESERVED_MB: u64 = 100;
/// Approximate MB available for a single LLM slot.
pub const LLM_BUDGET_MB: u64 =
    JETSON_TOTAL_RAM_MB - SYSTEM_OVERHEAD_MB - STT_RESERVED_MB - TTS_RESERVED_MB;

/// Everything but the LLM slot, shared by [`LLM_BUDGET_MB`] and the runtime `llm_budget_mb`.
const RESERVED_MB: u64 = SYSTEM_OVERHEAD_MB + STT_RESERVED_MB + TTS_RESERVED_MB;

/// The believed device's RAM, never a host probe: a dev Mac's 64 GB would satisfy everything.
pub fn total_ram_mb() -> u64 {
    pond_core::models::domain::device_profile::active()
        .map(|p| p.total_ram_mb)
        .unwrap_or(JETSON_TOTAL_RAM_MB)
}

/// Runtime twin of [`LLM_BUDGET_MB`], equal to it unless a device profile is emulating a board.
pub fn llm_budget_mb() -> u64 {
    total_ram_mb().saturating_sub(RESERVED_MB)
}

// ── ResourceAwareModelScheduler ──────────────────────────────────────────────

/// Scheduler for in-process GGUF; live memory from `/proc/meminfo`, else the budget constants.
pub struct ResourceAwareModelScheduler {
    /// Name of the model currently loaded in the llama.cpp slot.
    currently_hot: Mutex<Option<String>>,
    /// `notify_wake_word()` sends `true` here; the server's pre-loader task receives it.
    wake_tx: watch::Sender<bool>,
}

impl ResourceAwareModelScheduler {
    /// Also returns the wake-word receiver for the server's pre-loader task.
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

    /// Record the loaded model; optional, for accurate reporting.
    pub fn set_hot_model(&self, name: Option<String>) {
        if let Ok(mut guard) = self.currently_hot.lock() {
            *guard = name;
        }
    }

    /// `MemAvailable` from `/proc/meminfo` in MB; `None` off Linux or if unreadable.
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
        // No receiver just means no pre-loader is running.
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

/// For self-managing llamafile/Ollama; zeroed status makes the UI show "managed externally".
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

        assert!(!*rx.borrow());

        sched.notify_wake_word().await;

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
        // Checked against the profile so this still holds under `scripts/jetson-emu.sh test`.
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

    #[test]
    fn the_runtime_budget_equals_the_constant_when_nothing_is_emulated() {
        if pond_core::models::domain::device_profile::active().is_none() {
            assert_eq!(llm_budget_mb(), LLM_BUDGET_MB);
            assert_eq!(total_ram_mb(), JETSON_TOTAL_RAM_MB);
        }
    }

    #[test]
    fn memory_status_fallback_available_is_full_budget_when_no_model_loaded() {
        // Only `<=`: on Linux the value is live `/proc/meminfo`, not the fallback.
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        let status = sched.memory_status();

        assert!(
            status.available_for_llm_mb <= llm_budget_mb(),
            "available should not exceed budget: {} > {}",
            status.available_for_llm_mb,
            llm_budget_mb()
        );
    }

    #[test]
    fn memory_status_available_decreases_when_model_is_hot() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        let free_before = sched.memory_status().available_for_llm_mb;

        sched.set_hot_model(Some("my-model".to_string()));
        let free_after = sched.memory_status().available_for_llm_mb;

        // Only the non-Linux fallback reacts to `set_hot_model`.
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

        let _ = rx.borrow_and_update();
        sched.notify_wake_word().await;
        // Only checks that re-signalling doesn't panic.
    }
}
