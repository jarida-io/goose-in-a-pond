//! Each digest's recording, kept on disk once made, so Listen synthesises a digest only once.

use std::path::{Path, PathBuf};

/// Where `digest_id`'s recording lives under the data directory. An id with anything but
/// letters, digits and dashes is reduced to them, so an id can never name another path.
pub fn path_for(data_dir: &Path, digest_id: &str) -> PathBuf {
    let safe: String = digest_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    data_dir.join("news-audio").join(format!("{safe}.wav"))
}

pub async fn read(path: &Path) -> Option<Vec<u8>> {
    tokio::fs::read(path)
        .await
        .ok()
        .filter(|wav| !wav.is_empty())
}

/// Keeps the recording; written beside it first and moved in, so a reader never sees half of one.
pub async fn write(path: &Path, wav: &[u8]) {
    let Some(dir) = path.parent() else {
        return;
    };
    let partial = path.with_extension("wav.part");
    let kept = async {
        tokio::fs::create_dir_all(dir).await?;
        tokio::fs::write(&partial, wav).await?;
        tokio::fs::rename(&partial, path).await
    }
    .await;
    if let Err(e) = kept {
        tracing::warn!(error = %e, "a digest's recording could not be kept");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_recording_is_kept_under_its_digest_and_an_odd_id_stays_inside() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path(), "../../etc/passwd");
        assert_eq!(path, dir.path().join("news-audio").join("etcpasswd.wav"));
        let path = path_for(dir.path(), "3f2c-9a");
        assert_eq!(read(&path).await, None);
        write(&path, b"RIFF").await;
        assert_eq!(read(&path).await, Some(b"RIFF".to_vec()));
    }
}
