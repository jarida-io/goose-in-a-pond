//! The model's context window as the inference engine last reported it, for tools that size
//! what they hand a helper to the room it actually has.

use std::sync::atomic::{AtomicUsize, Ordering};

static WINDOW_TOKENS: AtomicUsize = AtomicUsize::new(0);

/// Called with each turn's reported window; a model switch can change it.
pub fn record(tokens: usize) {
    WINDOW_TOKENS.store(tokens, Ordering::Relaxed);
}

/// `None` until a turn has reported one.
pub fn tokens() -> Option<usize> {
    match WINDOW_TOKENS.load(Ordering::Relaxed) {
        0 => None,
        tokens => Some(tokens),
    }
}
