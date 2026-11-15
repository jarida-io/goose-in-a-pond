//! In-process [`FactoryResetProgress`]: the one reset this process may run at a time, and the
//! step it has reached, for a status poll and the gate that holds other requests meanwhile.

use std::sync::{Arc, Mutex, MutexGuard};

use pond_core::network::services::factory_reset::{FactoryResetProgress, FactoryResetStep};

#[derive(Default)]
pub struct InMemoryFactoryResetProgress(Mutex<State>);

#[derive(Default)]
struct State {
    running: bool,
    step: Option<FactoryResetStep>,
}

impl InMemoryFactoryResetProgress {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes the one reset slot, or `None` while another reset holds it. The slot frees when the
    /// returned run drops, however the reset ends.
    pub fn begin(self: &Arc<Self>) -> Option<FactoryResetRun> {
        let mut state = self.state();
        if state.running {
            return None;
        }
        *state = State {
            running: true,
            step: None,
        };
        Some(FactoryResetRun(self.clone()))
    }

    pub fn running(&self) -> bool {
        self.state().running
    }

    /// The step the running reset last reported, or `None` when none is running.
    pub fn current(&self) -> Option<FactoryResetStep> {
        self.state().step
    }
}

impl FactoryResetProgress for InMemoryFactoryResetProgress {
    fn report(&self, step: FactoryResetStep) {
        let mut state = self.state();
        if state.running {
            state.step = Some(step);
        }
    }
}

/// The running reset's hold on the slot.
pub struct FactoryResetRun(Arc<InMemoryFactoryResetProgress>);

impl Drop for FactoryResetRun {
    fn drop(&mut self) {
        *self.0.state() = State::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_reset_at_a_time_and_its_step_lasts_only_while_it_runs() {
        let progress = Arc::new(InMemoryFactoryResetProgress::new());
        progress.report(FactoryResetStep::Exporting);
        assert_eq!(progress.current(), None, "no reset is running");

        let run = progress.begin().expect("the slot is free");
        assert!(progress.running());
        assert!(
            progress.begin().is_none(),
            "a second reset waits for the first"
        );
        progress.report(FactoryResetStep::ErasingData);
        assert_eq!(progress.current(), Some(FactoryResetStep::ErasingData));

        drop(run);
        assert!(!progress.running());
        assert_eq!(progress.current(), None);
        assert!(
            progress.begin().is_some(),
            "the slot frees when the run ends"
        );
    }
}
