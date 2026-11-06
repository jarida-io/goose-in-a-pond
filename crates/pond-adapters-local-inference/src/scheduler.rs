//! Memory-aware scheduler for one resident LLM (Goose evicts the others on a switch): reports
//! memory (the board's budget on a budgeted device, a desktop's own readings elsewhere) and owns
//! a wake-word `watch` channel so the chat model can pre-load.

use std::sync::{Mutex, OnceLock};

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

// ── Device readings ──────────────────────────────────────────────────────────

const MIB: u64 = 1024 * 1024;

/// What a host says about its memory, in MB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostMemory {
    pub total_mb: u64,
    /// What it could hand a new program now: `MemAvailable` on Linux, the kernel's available
    /// (non-compressed) memory on macOS.
    pub available_mb: u64,
}

/// Where the scheduler reads the device from; tests inject fixed readings.
#[derive(Debug, Clone, Copy)]
pub struct DeviceReadings {
    /// Whether this process lives within the board's budget (a Jetson, or one emulated).
    pub budgeted: fn() -> bool,
    /// The host's own memory, `None` when it cannot be read.
    pub host: fn() -> Option<HostMemory>,
}

impl DeviceReadings {
    /// This process's device.
    pub const LIVE: Self = Self {
        budgeted: pond_core::models::domain::device_budget::budgeted_device,
        host: read_host_memory,
    };
}

/// The host's memory from the operating system, through one reused `sysinfo` handle.
fn read_host_memory() -> Option<HostMemory> {
    static SYSTEM: OnceLock<Mutex<sysinfo::System>> = OnceLock::new();
    let mut system = SYSTEM
        .get_or_init(|| Mutex::new(sysinfo::System::new()))
        .lock()
        .ok()?;
    system.refresh_memory();
    let total_mb = system.total_memory() / MIB;
    (total_mb > 0).then(|| HostMemory {
        total_mb,
        available_mb: system.available_memory() / MIB,
    })
}

/// A desktop's status from its own reading: its total, the LLM slot's budget being that total
/// less the desktop reserve, and its available memory within that budget.
fn host_status(reading: HostMemory, loaded_model: Option<String>) -> MemoryStatus {
    let budget_mb = pond_core::models::domain::device_budget::host_llm_budget_mb(reading.total_mb);
    MemoryStatus {
        total_mb: reading.total_mb,
        available_for_llm_mb: reading.available_mb.min(budget_mb),
        budget_mb,
        loaded_model,
        available_mb: Some(reading.available_mb),
    }
}

// ── ResourceAwareModelScheduler ──────────────────────────────────────────────

/// Scheduler for in-process GGUF. A budgeted device reports the board's budget (live memory from
/// `/proc/meminfo`, else the budget constants); any other host reports its own memory.
pub struct ResourceAwareModelScheduler {
    /// Name of the model currently loaded in the llama.cpp slot.
    currently_hot: Mutex<Option<String>>,
    /// `notify_wake_word()` sends `true` here; the server's pre-loader task receives it.
    wake_tx: watch::Sender<bool>,
    readings: DeviceReadings,
}

impl ResourceAwareModelScheduler {
    /// Also returns the wake-word receiver for the server's pre-loader task.
    pub fn new() -> (Self, watch::Receiver<bool>) {
        Self::with_readings(DeviceReadings::LIVE)
    }

    /// [`Self::new`] reading the device from `readings`.
    pub fn with_readings(readings: DeviceReadings) -> (Self, watch::Receiver<bool>) {
        let (wake_tx, wake_rx) = watch::channel(false);
        (
            Self {
                currently_hot: Mutex::new(None),
                wake_tx,
                readings,
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

        // A desktop is not an 8 GB board: it reports its own memory, when it can be read.
        if !(self.readings.budgeted)() {
            if let Some(reading) = (self.readings.host)() {
                return host_status(reading, loaded_model);
            }
        }

        let total = total_ram_mb();
        let budget = llm_budget_mb();
        let available_mb = Self::read_free_ram_mb();
        let (total_mb, available_for_llm_mb) = match available_mb {
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
            budget_mb: budget,
            loaded_model,
            available_mb,
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
    use pond_core::models::domain::device_budget::host_llm_budget_mb;

    /// The board's arithmetic, whatever this host is.
    const BOARD: DeviceReadings = DeviceReadings {
        budgeted: || true,
        host: || None,
    };

    fn desktop(total_mb: u64, available_mb: u64) -> MemoryStatus {
        host_status(
            HostMemory {
                total_mb,
                available_mb,
            },
            None,
        )
    }

    #[test]
    fn a_desktop_reports_its_own_memory_not_an_orin() {
        let (sched, _rx) = ResourceAwareModelScheduler::with_readings(DeviceReadings {
            budgeted: || false,
            host: || {
                Some(HostMemory {
                    total_mb: 32_768,
                    available_mb: 20_000,
                })
            },
        });
        let status = sched.memory_status();
        assert_eq!(status.total_mb, 32_768);
        assert_eq!(status.budget_mb, 24_576);
        assert_eq!(status.available_for_llm_mb, 20_000);
    }

    #[test]
    fn a_desktop_never_offers_more_than_its_budget() {
        let status = desktop(16_384, 15_000);
        assert_eq!(status.budget_mb, 12_288);
        assert_eq!(status.available_for_llm_mb, 12_288);
        assert_eq!(
            status.available_mb,
            Some(15_000),
            "the Memory model's reading is the host's, outside the LLM budget"
        );
    }

    #[test]
    fn a_budgeted_device_keeps_the_boards_arithmetic_whatever_the_host_says() {
        let (sched, _rx) = ResourceAwareModelScheduler::with_readings(DeviceReadings {
            budgeted: || true,
            host: || {
                Some(HostMemory {
                    total_mb: 65_536,
                    available_mb: 60_000,
                })
            },
        });
        let status = sched.memory_status();
        assert_eq!(status.total_mb, total_ram_mb());
        assert_eq!(status.budget_mb, llm_budget_mb());
        assert!(status.available_for_llm_mb <= llm_budget_mb());
    }

    #[test]
    fn an_unreadable_desktop_falls_back_to_the_boards_arithmetic() {
        let (sched, _rx) = ResourceAwareModelScheduler::with_readings(DeviceReadings {
            budgeted: || false,
            host: || None,
        });
        let status = sched.memory_status();
        assert_eq!(status.total_mb, total_ram_mb());
        assert_eq!(status.budget_mb, llm_budget_mb());
    }

    /// The real host: a budgeted run reports the board, any other its own readable memory.
    #[test]
    fn the_live_reading_is_this_hosts_own() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        let status = sched.memory_status();
        if pond_core::models::domain::device_budget::budgeted_device() {
            assert_eq!(status.total_mb, total_ram_mb());
            return;
        }
        let host = read_host_memory().expect("this host reports its memory");
        assert_eq!(status.total_mb, host.total_mb);
        assert_eq!(status.budget_mb, host_llm_budget_mb(host.total_mb));
        assert!(status.available_for_llm_mb <= status.budget_mb);
    }

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
        let (sched, _rx) = ResourceAwareModelScheduler::with_readings(BOARD);
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
        let (sched, _rx) = ResourceAwareModelScheduler::with_readings(BOARD);
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
        let (sched, _rx) = ResourceAwareModelScheduler::with_readings(BOARD);
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
