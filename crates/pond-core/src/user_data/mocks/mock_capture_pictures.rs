//! In-memory [`CapturePictures`] that names and refuses files as the disk adapter does.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::user_data::ports::capture_pictures::{mime_type_of, picture_file_name, CapturePictures};

#[derive(Default)]
pub struct InMemoryCapturePictures {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl InMemoryCapturePictures {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many pictures are kept.
    pub fn count(&self) -> usize {
        self.files.lock().unwrap().len()
    }
}

#[async_trait]
impl CapturePictures for InMemoryCapturePictures {
    async fn save(&self, capture_id: &str, mime_type: &str, bytes: &[u8]) -> Result<String> {
        let file = picture_file_name(capture_id, mime_type)
            .context("not a picture type or capture id the pond keeps")?;
        self.files
            .lock()
            .unwrap()
            .insert(file.clone(), bytes.to_vec());
        Ok(file)
    }

    async fn read(&self, file: &str) -> Result<Option<(Vec<u8>, String)>> {
        let mime = mime_type_of(file).context("not a capture picture's file name")?;
        let files = self.files.lock().unwrap();
        Ok(files
            .get(file)
            .map(|bytes| (bytes.clone(), mime.to_string())))
    }

    async fn delete(&self, file: &str) -> Result<()> {
        mime_type_of(file).context("not a capture picture's file name")?;
        self.files.lock().unwrap().remove(file);
        Ok(())
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
}
