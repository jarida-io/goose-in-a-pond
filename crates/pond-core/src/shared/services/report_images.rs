//! Pictures for a report, from the pages it cites: a Wikipedia article's lead image with its
//! licence and author, else the preview image a page names for itself. The screens never load an
//! image from another site, so no site learns who reads the report; the pond fetches each one,
//! keeps a bounded copy, and serves it with its credit.

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Preview images remembered at most, newest kept.
const MAX_PREVIEWS: usize = 512;

/// One picture a report shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportImage {
    /// The name the pond serves it under.
    pub key: String,
    /// The cited page it belongs to.
    pub page: String,
    /// The line under the picture: its author and licence, or the site it came from.
    pub credit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Where the licence is stated in full, for a Wikimedia file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_page: Option<String>,
}

/// Driven port: finds, keeps and serves a report's pictures.
#[async_trait]
pub trait ReportImages: Send + Sync {
    /// Pictures for `pages`, in their order, at most `limit`; a page with none is passed over.
    async fn for_pages(&self, pages: &[String], limit: usize) -> Vec<ReportImage>;
    /// A kept picture's bytes and media type.
    async fn read(&self, key: &str) -> Option<(Vec<u8>, String)>;
}

static INSTALLED: OnceLock<Arc<dyn ReportImages>> = OnceLock::new();

/// Installed once at startup by the binary that can fetch; a pond without it shows no pictures.
pub fn install(images: Arc<dyn ReportImages>) {
    let _ = INSTALLED.set(images);
}

pub fn installed() -> Option<Arc<dyn ReportImages>> {
    INSTALLED.get().cloned()
}

static PREVIEWS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Notes the preview image a page read names for itself, so its report can show it later.
pub fn note_preview(page: &str, image: &str) {
    let mut previews = PREVIEWS.lock().unwrap_or_else(|e| e.into_inner());
    previews.retain(|(p, _)| p != page);
    previews.push((page.to_string(), image.to_string()));
    let excess = previews.len().saturating_sub(MAX_PREVIEWS);
    previews.drain(..excess);
}

pub fn preview_of(page: &str) -> Option<String> {
    let previews = PREVIEWS.lock().unwrap_or_else(|e| e.into_inner());
    previews
        .iter()
        .rev()
        .find(|(p, _)| p == page)
        .map(|(_, image)| image.clone())
}

/// Whether `key` is one the pond made: hex only, so a request can't name a path.
pub fn is_key(key: &str) -> bool {
    (16..=64).contains(&key.len()) && key.chars().all(|c| c.is_ascii_hexdigit())
}
