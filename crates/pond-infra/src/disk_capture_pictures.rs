//! Capture pictures on disk, under the data directory: one file each, in a directory only the
//! pond's own user can open.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

use chrono::{DateTime, Utc};

use pond_core::user_data::ports::capture_pictures::{
    mime_type_of, picture_file_name, CapturePictures, KeptPicture,
};

/// The directory under the data directory.
const DIR: &str = "captures";

pub struct DiskCapturePictures {
    dir: PathBuf,
}

impl DiskCapturePictures {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The store in a pond's data directory.
    pub fn under(data_dir: &Path) -> Self {
        Self::new(data_dir.join(DIR))
    }

    /// Where a picture is, for a name `save` gave; any other name is refused.
    fn path_of(&self, file: &str) -> Result<(PathBuf, &'static str)> {
        let mime = mime_type_of(file).context("not a capture picture's file name")?;
        Ok((self.dir.join(file), mime))
    }

    async fn ensure_dir(&self) -> Result<()> {
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(&self.dir)
            .await
            .with_context(|| format!("creating {}", self.dir.display()))?;
        // A directory that was already there keeps the mode it had, so it is set again.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
                .await
                .with_context(|| format!("closing {} to other users", self.dir.display()))?;
        }
        Ok(())
    }
}

#[async_trait]
impl CapturePictures for DiskCapturePictures {
    async fn save(&self, capture_id: &str, mime_type: &str, bytes: &[u8]) -> Result<String> {
        let file = picture_file_name(capture_id, mime_type)
            .context("not a picture type or capture id the pond keeps")?;
        self.ensure_dir().await?;
        let path = self.dir.join(&file);
        // Written beside it and moved in, so a reader never sees half a picture.
        let partial = self.dir.join(format!("{file}.part"));
        let _ = tokio::fs::remove_file(&partial).await;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let written = async {
            let mut picture = options.open(&partial).await?;
            picture.write_all(bytes).await?;
            picture.sync_all().await?;
            drop(picture);
            tokio::fs::rename(&partial, &path).await
        }
        .await;
        if let Err(e) = written {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(e).with_context(|| format!("writing {}", path.display()));
        }
        Ok(file)
    }

    async fn read(&self, file: &str) -> Result<Option<(Vec<u8>, String)>> {
        let (path, mime) = self.path_of(file)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some((bytes, mime.to_string()))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    async fn delete(&self, file: &str) -> Result<()> {
        let (path, _) = self.path_of(file)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
        }
    }

    async fn kept(&self) -> Result<Vec<KeptPicture>> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("listing {}", self.dir.display())),
        };
        let mut kept = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let Some(file) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // Only names `save` gives: a half-written `.part` is never listed.
            if mime_type_of(&file).is_none() {
                continue;
            }
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let saved_at: DateTime<Utc> = meta
                .modified()
                .map(DateTime::from)
                .unwrap_or_else(|_| Utc::now());
            kept.push(KeptPicture { file, saved_at });
        }
        Ok(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_picture_is_kept_under_its_captures_id_and_read_back_until_deleted() {
        let data_dir = tempfile::tempdir().unwrap();
        let pictures = DiskCapturePictures::under(data_dir.path());
        let file = pictures
            .save("3f2c-9a", "image/webp", b"RIFFwebp")
            .await
            .unwrap();
        assert_eq!(file, "3f2c-9a.webp");
        assert!(data_dir.path().join("captures").join(&file).is_file());
        assert_eq!(
            pictures.read(&file).await.unwrap(),
            Some((b"RIFFwebp".to_vec(), "image/webp".to_string()))
        );

        pictures
            .save("3f2c-9a", "image/webp", b"again")
            .await
            .unwrap();
        assert_eq!(pictures.read(&file).await.unwrap().unwrap().0, b"again");
        let left: Vec<_> = std::fs::read_dir(data_dir.path().join("captures"))
            .unwrap()
            .collect();
        assert_eq!(left.len(), 1, "no partial file is left behind");

        pictures.delete(&file).await.unwrap();
        assert_eq!(pictures.read(&file).await.unwrap(), None);
        pictures.delete(&file).await.unwrap();
    }

    #[tokio::test]
    async fn only_the_pictures_it_kept_are_listed() {
        let data_dir = tempfile::tempdir().unwrap();
        let pictures = DiskCapturePictures::under(data_dir.path());
        assert!(
            pictures.kept().await.unwrap().is_empty(),
            "no directory yet"
        );
        let file = pictures.save("c1", "image/jpeg", b"jpeg").await.unwrap();
        let dir = data_dir.path().join("captures");
        std::fs::write(dir.join("c2.jpg.part"), b"half").unwrap();
        std::fs::write(dir.join("notes.txt"), b"not a picture").unwrap();
        let kept = pictures.kept().await.unwrap();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].file, file);
        assert!(Utc::now() - kept[0].saved_at < chrono::Duration::minutes(1));
    }

    #[tokio::test]
    async fn a_name_the_pond_never_gave_is_refused_whole() {
        let data_dir = tempfile::tempdir().unwrap();
        std::fs::write(data_dir.path().join("secrets.jpg"), b"not a capture").unwrap();
        let pictures = DiskCapturePictures::under(data_dir.path());
        pictures.save("c1", "image/jpeg", b"jpeg").await.unwrap();
        for name in [
            "../secrets.jpg",
            "/etc/passwd",
            "a/c1.jpg",
            "a\\c1.jpg",
            "c1",
        ] {
            assert!(pictures.read(name).await.is_err(), "{name}");
            assert!(pictures.delete(name).await.is_err(), "{name}");
        }
        assert!(data_dir.path().join("secrets.jpg").exists());
        assert!(pictures.save("../c2", "image/jpeg", b"x").await.is_err());
        assert!(pictures.save("c2", "image/gif", b"x").await.is_err());
        assert!(pictures
            .save("c2", "image/jpeg; ../../x", b"x")
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_the_ponds_own_user_can_open_the_directory_or_a_picture() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        let data_dir = tempfile::tempdir().unwrap();
        let dir = data_dir.path().join("captures");
        let pictures = DiskCapturePictures::under(data_dir.path());
        let file = pictures.save("c1", "image/png", b"png").await.unwrap();
        assert_eq!((mode(&dir), mode(&dir.join(&file))), (0o700, 0o600));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = pictures.save("c2", "image/png", b"png").await.unwrap();
        assert_eq!(
            (mode(&dir), mode(&dir.join(&file))),
            (0o700, 0o600),
            "a directory left open is closed again"
        );
    }
}
