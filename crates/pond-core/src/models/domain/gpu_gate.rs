//! Keeps ONNX Runtime from setting up a GPU session while ggml computes in this process. Its CUDA
//! provider calls `cudaDeviceSynchronize` during setup, which is illegal while llama.cpp captures
//! a CUDA graph, and llama.cpp aborts the process on the broken capture.

use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// Compute runs side by side and a setup runs alone. A waiting setup holds nothing back, so a turn
/// never queues behind one; the model just stays on the CPU a little longer.
#[derive(Default)]
pub struct GpuGate {
    state: Mutex<State>,
    changed: Condvar,
    reopened: Notify,
}

#[derive(Default)]
struct State {
    computing: usize,
    setting_up: bool,
}

/// Held for one stretch of ggml compute: a generation, a transcription.
#[must_use]
pub struct ComputeGuard(&'static GpuGate);

/// Held while ONNX Runtime creates or builds a GPU session.
#[must_use]
pub struct SetupGuard(&'static GpuGate);

/// A [`ComputeGuard`] for a call that can be dropped before its decode ends: a stream the caller
/// lets go, a lane job cancelled when the household speaks. Such a decode stops only at its next
/// token, after any prefill, so an unfinished lease holds the gate a while; that delays nothing
/// but a later ONNX setup.
#[must_use]
pub struct ComputeLease(Option<ComputeGuard>);

const UNFINISHED_LEASE_HOLD: Duration = Duration::from_secs(60);

// Process-wide because the adapters that compute and the one that sets up never meet.
static GATE: OnceLock<GpuGate> = OnceLock::new();

pub fn gpu_gate() -> &'static GpuGate {
    GATE.get_or_init(GpuGate::default)
}

impl GpuGate {
    pub async fn compute(&'static self) -> ComputeGuard {
        let started = Instant::now();
        loop {
            let reopened = self.reopened.notified();
            let mut reopened = std::pin::pin!(reopened);
            // Registered before the check, so a setup ending in between still wakes this.
            reopened.as_mut().enable();
            if let Some(guard) = self.try_compute() {
                note_compute_wait(started);
                return guard;
            }
            reopened.await;
        }
    }

    pub async fn lease(&'static self) -> ComputeLease {
        ComputeLease(Some(self.compute().await))
    }

    /// For a blocking thread: it parks the thread while a setup runs.
    pub fn compute_blocking(&'static self) -> ComputeGuard {
        let started = Instant::now();
        let mut state = self.lock();
        while state.setting_up {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.computing += 1;
        drop(state);
        note_compute_wait(started);
        ComputeGuard(self)
    }

    /// Waits until nothing computes, then holds new compute back until the guard drops.
    pub fn setup_blocking(&'static self) -> SetupGuard {
        let mut state = self.lock();
        while state.computing > 0 || state.setting_up {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.setting_up = true;
        SetupGuard(self)
    }

    fn try_compute(&'static self) -> Option<ComputeGuard> {
        let mut state = self.lock();
        if state.setting_up {
            return None;
        }
        state.computing += 1;
        Some(ComputeGuard(self))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for ComputeGuard {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.computing -= 1;
        if state.computing == 0 {
            self.0.changed.notify_all();
        }
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        self.0.lock().setting_up = false;
        self.0.changed.notify_all();
        self.0.reopened.notify_waiters();
    }
}

impl ComputeLease {
    /// The decode has ended; the gate is released now.
    pub fn finished(mut self) {
        self.0 = None;
    }
}

impl Drop for ComputeLease {
    fn drop(&mut self) {
        let Some(guard) = self.0.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep(UNFINISHED_LEASE_HOLD).await;
                drop(guard);
            });
        }
    }
}

fn note_compute_wait(started: Instant) {
    let waited = started.elapsed();
    if waited >= Duration::from_millis(10) {
        tracing::info!(
            target: "giap::trace",
            kind = "gpu_compute_waited",
            waited_ms = waited.as_millis() as u64,
            "a decode waited for an ONNX model's GPU setup"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    const SETTLE: Duration = Duration::from_millis(100);
    const PATIENCE: Duration = Duration::from_secs(5);

    fn gate() -> &'static GpuGate {
        Box::leak(Box::default())
    }

    #[test]
    fn a_setup_waits_for_compute_then_holds_new_compute_back() {
        let gate = gate();
        let decoding = gate.compute_blocking();
        let (set_up, setups) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        thread::spawn(move || {
            let guard = gate.setup_blocking();
            set_up.send(()).unwrap();
            released.recv().unwrap();
            drop(guard);
        });
        assert!(
            setups.recv_timeout(SETTLE).is_err(),
            "a setup beside a running decode is what aborted the pond"
        );
        drop(decoding);
        setups
            .recv_timeout(PATIENCE)
            .expect("the setup runs once the decode ends");

        let (computed, computes) = mpsc::channel();
        thread::spawn(move || {
            let _guard = gate.compute_blocking();
            computed.send(()).unwrap();
        });
        assert!(
            computes.recv_timeout(SETTLE).is_err(),
            "a decode must not start during a setup"
        );
        release.send(()).unwrap();
        computes
            .recv_timeout(PATIENCE)
            .expect("the decode runs once the setup ends");
    }

    #[test]
    fn a_waiting_setup_never_holds_a_turn_back() {
        let gate = gate();
        let first = gate.compute_blocking();
        let (set_up, setups) = mpsc::channel();
        thread::spawn(move || {
            let _guard = gate.setup_blocking();
            set_up.send(()).unwrap();
        });
        thread::sleep(SETTLE);
        let (computed, computes) = mpsc::channel();
        thread::spawn(move || {
            let guard = gate.compute_blocking();
            computed.send(()).unwrap();
            thread::sleep(SETTLE);
            drop(guard);
        });
        computes
            .recv_timeout(PATIENCE)
            .expect("a second decode starts beside the first while the setup only waits");
        drop(first);
        setups
            .recv_timeout(PATIENCE)
            .expect("the setup runs once both decodes end");
    }

    fn computing(gate: &GpuGate) -> usize {
        gate.lock().computing
    }

    #[tokio::test(start_paused = true)]
    async fn a_lease_dropped_before_its_decode_ends_holds_the_gate_a_while() {
        let gate = gate();
        gate.lease().await.finished();
        assert_eq!(computing(gate), 0, "a finished decode releases at once");

        drop(gate.lease().await);
        tokio::task::yield_now().await;
        assert_eq!(
            computing(gate),
            1,
            "a cancelled decode still runs to its next token, after any prefill"
        );
        tokio::time::sleep(UNFINISHED_LEASE_HOLD + SETTLE).await;
        assert_eq!(computing(gate), 0);
    }

    #[tokio::test]
    async fn an_async_decode_waits_out_a_setup() {
        let gate = gate();
        let setup = gate.setup_blocking();
        let decode = tokio::spawn(async move {
            let _guard = gate.compute().await;
        });
        tokio::time::sleep(SETTLE).await;
        assert!(
            !decode.is_finished(),
            "a decode must not start during a setup"
        );
        drop(setup);
        tokio::time::timeout(PATIENCE, decode)
            .await
            .expect("the decode runs once the setup ends")
            .unwrap();
        let _next_setup = gate.setup_blocking();
    }
}
