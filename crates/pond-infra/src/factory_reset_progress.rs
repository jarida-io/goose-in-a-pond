//! In-process [`FactoryResetProgress`]: the whole reset runs inside one request handler, so there
//! is nothing to persist — just a slot a concurrent status poll can read while it runs.

use std::sync::Mutex;

use pond_core::network::services::factory_reset::{FactoryResetProgress, FactoryResetStep};

#[derive(Default)]
pub struct InMemoryFactoryResetProgress(Mutex<Option<FactoryResetStep>>);

impl InMemoryFactoryResetProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// The step a reset in progress last reported, or `None` when none is running.
    pub fn current(&self) -> Option<FactoryResetStep> {
        *self.0.lock().unwrap()
    }

    /// Clears the slot, so a finished reset's last step doesn't linger for the next poll.
    pub fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
}

impl FactoryResetProgress for InMemoryFactoryResetProgress {
    fn report(&self, step: FactoryResetStep) {
        *self.0.lock().unwrap() = Some(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_overwrite_and_clear_resets_to_none() {
        let progress = InMemoryFactoryResetProgress::new();
        assert_eq!(progress.current(), None);

        progress.report(FactoryResetStep::Exporting);
        assert_eq!(progress.current(), Some(FactoryResetStep::Exporting));

        progress.report(FactoryResetStep::ErasingData);
        assert_eq!(progress.current(), Some(FactoryResetStep::ErasingData));

        progress.clear();
        assert_eq!(progress.current(), None);
    }
}
