//! The house speaker the news reads a digest on, installed by the binary that has one, so a
//! route can ask for a reading without the API knowing the audio stack.

use std::sync::{Arc, OnceLock};

use crate::models::ports::announce::Announce;

static INSTALLED: OnceLock<Arc<dyn Announce>> = OnceLock::new();

/// Installed once at startup; a pond without a speaker answers that it has none.
pub fn install(speaker: Arc<dyn Announce>) {
    let _ = INSTALLED.set(speaker);
}

pub fn installed() -> Option<Arc<dyn Announce>> {
    INSTALLED.get().cloned()
}
