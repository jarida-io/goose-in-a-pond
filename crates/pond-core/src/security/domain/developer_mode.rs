//! Developer mode, which only a Jarida test Pond has: a debug switch that lets the logs show what
//! the household said and the members' names.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared by the routes that flip it and the log scrubber that obeys it. Every start begins off,
/// so a restart never leaves a Pond logging content.
#[derive(Clone, Debug, Default)]
pub struct DebugSwitch(Arc<AtomicBool>);

impl DebugSwitch {
    pub fn is_on(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, on: bool) {
        self.0.store(on, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_switch_is_off_and_every_clone_sees_a_change() {
        let switch = DebugSwitch::default();
        let scrubber_side = switch.clone();
        assert!(!scrubber_side.is_on());

        switch.set(true);
        assert!(scrubber_side.is_on());

        switch.set(false);
        assert!(!scrubber_side.is_on());
    }
}
