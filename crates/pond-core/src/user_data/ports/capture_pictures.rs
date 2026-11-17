//! Where the pictures of a member's captures are kept: one file each, under a name the pond
//! gives it. A capture's row keeps that name.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// The picture types a capture may be, each with the extension its file is kept under.
const KEPT_TYPES: [(&str, &str); 3] = [
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/webp", "webp"),
];
const MAX_ID_CHARS: usize = 64;

/// Letters, digits and dashes only, so an id can never name another path.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_CHARS
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The file name a capture's picture is kept under, its id and its type's extension; `None`
/// for a type the pond does not keep or an id that is not plain.
pub fn picture_file_name(capture_id: &str, mime_type: &str) -> Option<String> {
    let kind = mime_type.trim().to_ascii_lowercase();
    let (_, extension) = KEPT_TYPES.iter().find(|(mime, _)| *mime == kind)?;
    is_plain_id(capture_id).then(|| format!("{capture_id}.{extension}"))
}

/// The type of a kept picture, read from its file name; `None` for a name
/// [`picture_file_name`] never gives, such as one with a path separator.
pub fn mime_type_of(file: &str) -> Option<&'static str> {
    let (id, extension) = file.rsplit_once('.')?;
    let (mime, _) = KEPT_TYPES.iter().find(|(_, kept)| *kept == extension)?;
    is_plain_id(id).then_some(*mime)
}

/// A kept picture's file name and when it was last written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptPicture {
    pub file: String,
    pub saved_at: DateTime<Utc>,
}

/// Every picture is one member's; the caller checks whose before it reads or deletes one.
#[async_trait]
pub trait CapturePictures: Send + Sync {
    /// Keeps a capture's picture and returns the file name its row should hold.
    async fn save(&self, capture_id: &str, mime_type: &str, bytes: &[u8]) -> Result<String>;

    /// The picture's bytes and type, or `None` once it is gone; a name `save` never gave is
    /// refused.
    async fn read(&self, file: &str) -> Result<Option<(Vec<u8>, String)>>;

    /// Deletes the picture, where one already gone is no error and a name `save` never gave is
    /// refused.
    async fn delete(&self, file: &str) -> Result<()>;

    /// Every picture kept, in no order, so the ones no capture names any more can be found.
    async fn kept(&self) -> Result<Vec<KeptPicture>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_named_by_its_capture_and_its_type_alone() {
        assert_eq!(
            picture_file_name("3f2c-9a", "image/jpeg").as_deref(),
            Some("3f2c-9a.jpg")
        );
        assert_eq!(
            picture_file_name("3f2c-9a", " Image/PNG ").as_deref(),
            Some("3f2c-9a.png")
        );
        assert_eq!(
            picture_file_name("3f2c-9a", "image/webp").as_deref(),
            Some("3f2c-9a.webp")
        );
        assert_eq!(picture_file_name("3f2c-9a", "image/gif"), None);
        assert_eq!(picture_file_name("3f2c-9a", "image/svg+xml"), None);
        for id in ["", "../etc", "a/b", "a\\b", "a.b", &"a".repeat(65)] {
            assert_eq!(picture_file_name(id, "image/jpeg"), None, "{id}");
        }
    }

    #[test]
    fn only_a_name_the_pond_gave_reads_as_a_picture() {
        assert_eq!(mime_type_of("3f2c-9a.jpg"), Some("image/jpeg"));
        assert_eq!(mime_type_of("3f2c-9a.webp"), Some("image/webp"));
        for name in [
            "3f2c-9a",
            "3f2c-9a.gif",
            "../secrets.jpg",
            "captures/3f2c-9a.jpg",
            "a\\b.png",
            ".jpg",
            "a.b.jpg",
        ] {
            assert_eq!(mime_type_of(name), None, "{name}");
        }
    }
}
