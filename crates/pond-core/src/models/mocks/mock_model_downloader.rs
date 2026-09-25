use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::models::ports::model_downloader::ModelDownloader;

/// Spy `ModelDownloader` that records calls and optionally creates placeholder files.
pub struct MockModelDownloader {
    /// All (url, dest) pairs that were successfully downloaded.
    pub downloads: Arc<RwLock<Vec<(String, PathBuf)>>>,
    /// When true, writes an empty placeholder file at `dest` on each download call.
    write_placeholder: bool,
}

impl MockModelDownloader {
    pub fn new(write_placeholder: bool) -> Self {
        Self {
            downloads: Arc::new(RwLock::new(Vec::new())),
            write_placeholder,
        }
    }

    pub async fn was_downloaded(&self, url: &str) -> bool {
        self.downloads.read().await.iter().any(|(u, _)| u == url)
    }

    pub async fn was_downloaded_any(&self) -> bool {
        !self.downloads.read().await.is_empty()
    }

    pub async fn download_count(&self) -> usize {
        self.downloads.read().await.len()
    }
}

#[async_trait]
impl ModelDownloader for MockModelDownloader {
    async fn download(&self, url: &str, dest: &Path, _size_hint_mb: u64) -> Result<()> {
        if dest.exists() {
            return Ok(());
        }
        if self.write_placeholder {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(dest, b"placeholder").await?;
        }
        self.downloads
            .write()
            .await
            .push((url.to_string(), dest.to_path_buf()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn records_download_url() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("model.gguf");
        let dl = MockModelDownloader::new(false);
        dl.download("https://example.com/model.gguf", &dest, 0)
            .await
            .unwrap();
        assert!(dl.was_downloaded("https://example.com/model.gguf").await);
        assert!(dl.was_downloaded_any().await);
    }

    #[tokio::test]
    async fn writes_placeholder_when_requested() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("model.gguf");
        let dl = MockModelDownloader::new(true);
        dl.download("https://example.com/model.gguf", &dest, 100)
            .await
            .unwrap();
        assert!(dest.exists(), "placeholder file should be created");
    }

    #[tokio::test]
    async fn skips_if_file_already_exists() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("model.gguf");
        tokio::fs::write(&dest, b"existing").await.unwrap();

        let dl = MockModelDownloader::new(true);
        dl.download("https://example.com/model.gguf", &dest, 0)
            .await
            .unwrap();
        assert_eq!(dl.download_count().await, 0);
    }
}
