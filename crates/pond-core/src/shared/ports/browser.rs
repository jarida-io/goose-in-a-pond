//! The headless browser the pond reads script-built pages with: whether one is there, whether
//! a page could be rendered now, and downloading the pond's own.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::Serialize;

/// What a download is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Install {
    #[default]
    Idle,
    Downloading {
        done_bytes: u64,
        total_bytes: Option<u64>,
    },
    Unpacking,
    Failed {
        why: String,
    },
}

/// What the settings screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// The household's switch.
    pub enabled: bool,
    /// The browser a render would use, if any.
    pub browser: Option<String>,
    /// Whether that browser is the pond's own download.
    pub own: bool,
    pub free_mb: Option<u64>,
    pub needs_free_mb: u64,
    /// Whether a page could be rendered right now.
    pub ready: bool,
    /// Why not, in a word: `off`, `no_browser`, `low_memory`.
    pub reason: Option<&'static str>,
    /// Whether a browser can be downloaded for this machine.
    pub can_download: bool,
    pub install: Install,
}

#[async_trait]
pub trait Browser: Send + Sync {
    fn status(&self) -> Status;
    /// Download the pond's own browser; the household asked for it.
    async fn install(&self) -> Result<(), String>;
    /// Remove the pond's own browser; one installed on the machine is not touched.
    async fn uninstall(&self) -> bool;
}

static BROWSER: OnceLock<Arc<dyn Browser>> = OnceLock::new();

/// Install the process's browser once, at startup; later calls are ignored.
pub fn install(browser: Arc<dyn Browser>) {
    let _ = BROWSER.set(browser);
}

pub fn installed() -> Option<Arc<dyn Browser>> {
    BROWSER.get().cloned()
}
