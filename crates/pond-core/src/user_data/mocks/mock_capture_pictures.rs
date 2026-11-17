//! In-memory [`CapturePictures`] that names and refuses files as the disk adapter does.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::ports::capture_pictures::{
    mime_type_of, picture_file_name, CapturePictures, KeptPicture,
};

struct Kept {
    bytes: Vec<u8>,
    saved_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct InMemoryCapturePictures {
    files: Mutex<HashMap<String, Kept>>,
}

impl InMemoryCapturePictures {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many pictures are kept.
    pub fn count(&self) -> usize {
        self.files.lock().unwrap().len()
    }

    /// Makes a kept picture as old as one written at `saved_at`; `false` when there is none.
    pub fn backdate(&self, file: &str, saved_at: DateTime<Utc>) -> bool {
        match self.files.lock().unwrap().get_mut(file) {
            Some(kept) => {
                kept.saved_at = saved_at;
                true
            }
            None => false,
        }
    }
}

#[async_trait]
impl CapturePictures for InMemoryCapturePictures {
    async fn save(&self, capture_id: &str, mime_type: &str, bytes: &[u8]) -> Result<String> {
        let file = picture_file_name(capture_id, mime_type)
            .context("not a picture type or capture id the pond keeps")?;
        let kept = Kept {
            bytes: bytes.to_vec(),
            saved_at: Utc::now(),
        };
        self.files.lock().unwrap().insert(file.clone(), kept);
        Ok(file)
    }

    async fn read(&self, file: &str) -> Result<Option<(Vec<u8>, String)>> {
        let mime = mime_type_of(file).context("not a capture picture's file name")?;
        let files = self.files.lock().unwrap();
        Ok(files
            .get(file)
            .map(|kept| (kept.bytes.clone(), mime.to_string())))
    }

    async fn delete(&self, file: &str) -> Result<()> {
        mime_type_of(file).context("not a capture picture's file name")?;
        self.files.lock().unwrap().remove(file);
        Ok(())
    }

    async fn kept(&self) -> Result<Vec<KeptPicture>> {
        let files = self.files.lock().unwrap();
        Ok(files
            .iter()
            .map(|(file, kept)| KeptPicture {
                file: file.clone(),
                saved_at: kept.saved_at,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_picture_is_kept_read_and_deleted_by_the_name_it_was_given() {
        let pictures = InMemoryCapturePictures::new();
        let file = pictures.save("c1", "image/png", b"png").await.unwrap();
        assert_eq!(file, "c1.png");
        assert_eq!(
            pictures.read(&file).await.unwrap(),
            Some((b"png".to_vec(), "image/png".to_string()))
        );
        pictures.delete(&file).await.unwrap();
        pictures.delete(&file).await.unwrap();
        assert_eq!(pictures.read(&file).await.unwrap(), None);
        assert_eq!(pictures.count(), 0);
        assert!(pictures.save("c1", "image/gif", b"gif").await.is_err());
        assert!(pictures.read("../c1.png").await.is_err());
        assert!(pictures.delete("a/c1.png").await.is_err());
    }

    #[tokio::test]
    async fn every_kept_picture_is_listed_with_when_it_was_written() {
        let pictures = InMemoryCapturePictures::new();
        assert!(pictures.kept().await.unwrap().is_empty());
        let file = pictures.save("c1", "image/png", b"png").await.unwrap();
        let long_ago = Utc::now() - chrono::Duration::days(3);
        assert!(pictures.backdate(&file, long_ago));
        assert!(!pictures.backdate("c2.png", long_ago));
        assert_eq!(
            pictures.kept().await.unwrap(),
            [KeptPicture {
                file,
                saved_at: long_ago
            }]
        );
    }
}
