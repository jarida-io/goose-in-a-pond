//! Pictures for a report, from the pages it cites: a Wikipedia article's lead image with its
//! licence and author, else the preview image a page names for itself. The screens never load an
//! image from another site, so no site learns who reads the report; the pond fetches each one,
//! keeps a bounded copy, and serves it with its credit.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Preview images remembered at most, newest kept.
const MAX_PREVIEWS: usize = 512;
/// Pages a list has asked to fetch a picture for at most; past it, none until a restart.
const MAX_LIST_FETCHES: usize = 512;
const MAX_PAGE_CHARS: usize = 2_048;

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
    /// The first picture already kept for `pages`, without reaching any site: a list asks this.
    async fn kept(&self, _pages: &[String]) -> Option<ReportImage> {
        None
    }
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

/// Where a tool's or a read's own words name the page it came from.
const PAGE_MARKERS: &[&str] = &["Address: ", "Source: ", "\"source_url\":\""];

/// The web pages a message names, in order and once each: the links an answer cites, the address
/// a page read reports, and the source a lookup ends with. A stored tool row is JSON, so an
/// address also ends at a quote or an escape's backslash.
pub fn pages_named(text: &str, limit: usize) -> Vec<String> {
    let ends_address = |c: char| c.is_whitespace() || matches!(c, '"' | '\\' | '<' | '>');
    let mut found: Vec<(usize, String)> = Vec::new();
    for (at, _) in text.match_indices("](http") {
        let rest = &text[at + 2..];
        let end = rest
            .find(|c: char| c == ')' || ends_address(c))
            .unwrap_or(rest.len());
        found.push((at, rest[..end].to_string()));
    }
    for marker in PAGE_MARKERS {
        for (at, _) in text.match_indices(marker) {
            let rest = &text[at + marker.len()..];
            let end = rest.find(ends_address).unwrap_or(rest.len());
            found.push((at, rest[..end].to_string()));
        }
    }
    found.sort_by_key(|(at, _)| *at);
    let mut pages: Vec<String> = Vec::new();
    for (_, page) in found {
        let usable = page.len() <= MAX_PAGE_CHARS
            && (page.starts_with("https://") || page.starts_with("http://"));
        if usable && !pages.contains(&page) {
            pages.push(page);
        }
        if pages.len() == limit {
            break;
        }
    }
    pages
}

static LIST_FETCHES: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Whether a list may fetch `page`'s picture now: once per page per run, so a page with no
/// picture is not asked for on every list.
pub fn first_list_fetch(page: &str) -> bool {
    let mut asked = LIST_FETCHES.lock().unwrap_or_else(|e| e.into_inner());
    let asked = asked.get_or_insert_with(HashSet::new);
    if asked.len() >= MAX_LIST_FETCHES || asked.contains(page) {
        return false;
    }
    asked.insert(page.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_named_takes_cited_links_and_read_addresses_in_order_once() {
        let text = "Page: Playboi Carti\nAddress: https://en.wikipedia.org/wiki/Playboi_Carti\n\
                    He is a rapper ([Wikipedia](https://en.wikipedia.org/wiki/Playboi_Carti)) \
                    signed to [AWGE](https://awge.com/about \"AWGE\") and [x](ftp://nope).";
        assert_eq!(
            pages_named(text, 3),
            vec![
                "https://en.wikipedia.org/wiki/Playboi_Carti".to_string(),
                "https://awge.com/about".to_string(),
            ]
        );
        assert_eq!(pages_named(text, 1).len(), 1);
        assert!(pages_named("No pages here.", 3).is_empty());
    }

    #[test]
    fn pages_named_reads_a_stored_tool_row() {
        let row = r#"{"tool_call_id":"x","content":"Robert John Downey Jr. is an actor.\n\nSource: https://en.wikipedia.org/wiki/Robert_Downey_Jr.","arguments":"{}"}"#;
        assert_eq!(
            pages_named(row, 3),
            vec!["https://en.wikipedia.org/wiki/Robert_Downey_Jr.".to_string()]
        );
        let read = r#"{"content":"Page: Fares\nAddress: https://example.com/p\nlines","card":{"data":{"source_url":"https://example.com/q"}}}"#;
        assert_eq!(
            pages_named(read, 3),
            vec![
                "https://example.com/p".to_string(),
                "https://example.com/q".to_string()
            ]
        );
    }

    #[test]
    fn a_list_fetches_a_page_once() {
        let page = "https://example.com/only-once-for-this-test";
        assert!(first_list_fetch(page));
        assert!(!first_list_fetch(page));
    }
}
