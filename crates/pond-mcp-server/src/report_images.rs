//! The pond's own copies of report pictures, on disk: fetched once per cited page through the
//! tracked, public-only client, bounded in size and kind, and served from here so no screen loads
//! an image from another site.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use pond_core::shared::services::report_images::{is_key, preview_of, ReportImage, ReportImages};
use pond_core::shared::services::wiki_images::{
    article_of, credited_image, image_info_query, lead_image_name, lead_image_query,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::web_search;

/// One picture at most: a phone card's copy, not an original.
const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// All kept pictures at most; the oldest go first.
const MAX_STORE_BYTES: u64 = 200 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// Kinds a screen can draw safely; SVG is left out, since it can carry script.
const IMAGE_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
const MAX_CREDIT_CHARS: usize = 160;

#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    image: ReportImage,
    content_type: String,
}

pub struct DiskReportImages {
    dir: PathBuf,
}

impl DiskReportImages {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn paths(&self, key: &str) -> (PathBuf, PathBuf) {
        (
            self.dir.join(format!("{key}.img")),
            self.dir.join(format!("{key}.json")),
        )
    }

    async fn stored_at(&self, key: &str) -> Option<Stored> {
        let (_, meta) = self.paths(key);
        let text = tokio::fs::read_to_string(meta).await.ok()?;
        serde_json::from_str(&text).ok()
    }

    async fn for_page(&self, page: &str) -> Option<ReportImage> {
        let key = key_of(page);
        if let Some(stored) = self.stored_at(&key).await {
            return Some(stored.image);
        }
        let found = match article_of(page) {
            Some((lang, title)) => wikipedia_image(&lang, &title).await,
            None => preview_of(page).map(|url| Found {
                url,
                credit: format!("Image from {}", web_search::domain_of(page)),
                licence: None,
                caption: None,
                file_page: None,
            }),
        }?;
        let (bytes, content_type) = fetch_image(&found.url).await?;
        let image = ReportImage {
            key: key.clone(),
            page: page.to_string(),
            credit: clip(&found.credit, MAX_CREDIT_CHARS),
            licence: found.licence,
            caption: found.caption,
            file_page: found.file_page,
        };
        let (img, meta) = self.paths(&key);
        tokio::fs::create_dir_all(&self.dir).await.ok()?;
        tokio::fs::write(&img, &bytes).await.ok()?;
        let stored = Stored {
            image: image.clone(),
            content_type,
        };
        tokio::fs::write(&meta, serde_json::to_vec(&stored).ok()?)
            .await
            .ok()?;
        prune(&self.dir, MAX_STORE_BYTES).await;
        tracing::info!(target: "giap::trace", kind = "report_image_kept", bytes = bytes.len());
        Some(image)
    }
}

#[async_trait]
impl ReportImages for DiskReportImages {
    async fn for_pages(&self, pages: &[String], limit: usize) -> Vec<ReportImage> {
        let tries = pages
            .iter()
            .take(limit.saturating_mul(2))
            .map(|p| self.for_page(p));
        futures::future::join_all(tries)
            .await
            .into_iter()
            .flatten()
            .take(limit)
            .collect()
    }

    async fn kept(&self, pages: &[String]) -> Option<ReportImage> {
        for page in pages {
            if let Some(stored) = self.stored_at(&key_of(page)).await {
                return Some(stored.image);
            }
        }
        None
    }

    async fn read(&self, key: &str) -> Option<(Vec<u8>, String)> {
        if !is_key(key) {
            return None;
        }
        let stored = self.stored_at(key).await?;
        let (img, _) = self.paths(key);
        let bytes = tokio::fs::read(img).await.ok()?;
        Some((bytes, stored.content_type))
    }
}

struct Found {
    url: String,
    credit: String,
    licence: Option<String>,
    caption: Option<String>,
    file_page: Option<String>,
}

async fn json_of(url: &str) -> Option<Value> {
    let client = web_search::page_client()?;
    let response = crate::http::traced_get_with(client, url, |b| b.timeout(FETCH_TIMEOUT))
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

async fn wikipedia_image(lang: &str, title: &str) -> Option<Found> {
    let name = lead_image_name(&json_of(&lead_image_query(lang, title)).await?)?;
    let image = credited_image(&json_of(&image_info_query(lang, &name)).await?)?;
    let credit = match &image.author {
        Some(author) => format!("{author}, {}", image.licence),
        None => image.licence.clone(),
    };
    Some(Found {
        url: image.url,
        credit,
        licence: Some(image.licence),
        caption: image.caption,
        file_page: Some(image.file_page),
    })
}

/// The picture's bytes and kind, if it is a public image of an allowed kind within the cap.
async fn fetch_image(url: &str) -> Option<(Vec<u8>, String)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    pond_net::public_only::check_target(&parsed).ok()?;
    let client = web_search::page_client()?;
    let response = crate::http::traced_get_with(client, url, |b| {
        b.timeout(FETCH_TIMEOUT).header(
            reqwest::header::ACCEPT,
            "image/jpeg,image/png,image/webp,image/gif",
        )
    })
    .await
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?
        .split(';')
        .next()?
        .trim()
        .to_ascii_lowercase();
    if !IMAGE_TYPES.contains(&content_type.as_str()) {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_IMAGE_BYTES as u64)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    (bytes.len() <= MAX_IMAGE_BYTES).then(|| (bytes.to_vec(), content_type))
}

/// Removes the oldest pictures until the store is within `max_bytes`.
async fn prune(dir: &Path, max_bytes: u64) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let mut images = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "img") {
            if let Ok(meta) = entry.metadata().await {
                let at = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                images.push((at, meta.len(), path));
            }
        }
    }
    let mut total: u64 = images.iter().map(|(_, n, _)| n).sum();
    images.sort_by_key(|(at, _, _)| *at);
    for (_, size, path) in images {
        if total <= max_bytes {
            break;
        }
        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(path.with_extension("json")).await;
        total = total.saturating_sub(size);
    }
}

/// A page's key: two FNV-1a passes as 32 hex digits, stable across builds, unlike std's hasher.
pub fn key_of(page: &str) -> String {
    let fnv = |seed: u64| {
        page.bytes().fold(seed, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    format!(
        "{:016x}{:016x}",
        fnv(0xcbf2_9ce4_8422_2325),
        fnv(0x8422_2325_cbf2_9ce4)
    )
}

fn clip(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => format!("{}...", text[..at].trim_end()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system's temp, removed by the test that made it.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("giap-report-images-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_pages_key_is_stable_hex_the_route_accepts() {
        let key = key_of("https://en.wikipedia.org/wiki/Fort_Jesus");
        assert_eq!(key, key_of("https://en.wikipedia.org/wiki/Fort_Jesus"));
        assert_ne!(key, key_of("https://en.wikipedia.org/wiki/Mombasa"));
        assert!(is_key(&key), "{key}");
        assert!(!is_key("../../etc/passwd"));
    }

    #[tokio::test]
    async fn a_kept_picture_is_read_back_and_an_unknown_key_is_not() {
        let dir = scratch("read");
        let store = DiskReportImages::new(dir.clone());
        let key = key_of("https://cars.example/used/2");
        let (img, meta) = store.paths(&key);
        std::fs::write(&img, b"\xff\xd8jpeg").unwrap();
        let stored = Stored {
            image: ReportImage {
                key: key.clone(),
                page: "https://cars.example/used/2".into(),
                credit: "Image from cars.example".into(),
                licence: None,
                caption: None,
                file_page: None,
            },
            content_type: "image/jpeg".into(),
        };
        std::fs::write(&meta, serde_json::to_vec(&stored).unwrap()).unwrap();
        let (bytes, kind) = store.read(&key).await.unwrap();
        assert_eq!(
            (bytes.as_slice(), kind.as_str()),
            (&b"\xff\xd8jpeg"[..], "image/jpeg")
        );
        let found = store
            .for_pages(&["https://cars.example/used/2".to_string()], 3)
            .await;
        assert_eq!(found.len(), 1, "a kept page's picture needs no fetch");
        assert!(store
            .read("0123456789abcdef0123456789abcdef")
            .await
            .is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_oldest_pictures_go_first_past_the_cap() {
        let dir = scratch("prune");
        for (name, size) in [("a", 600), ("b", 600)] {
            std::fs::write(dir.join(format!("{name}.img")), vec![0u8; size]).unwrap();
            std::fs::write(dir.join(format!("{name}.json")), b"{}").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        prune(&dir, 1_000).await;
        assert!(!dir.join("a.img").exists() && !dir.join("a.json").exists());
        assert!(dir.join("b.img").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
