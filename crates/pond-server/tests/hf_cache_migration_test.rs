//! Tests for the one-shot flat-file → HF-cache migration.

use std::path::{Path, PathBuf};

use pond_server::hf_cache_migration::migrate_flat_files_to_blobs;
use tempfile::TempDir;

/// Build the standard `{data_dir}/models/...` layout and return helper paths.
struct Fixture {
    _tmp: TempDir,
    data_dir: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let data_dir = tmp.path().to_path_buf();
        Self {
            _tmp: tmp,
            data_dir,
        }
    }

    async fn write_file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.data_dir.join(rel);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(&path, bytes).await.unwrap();
        path
    }
}

async fn is_symlink(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[tokio::test]
async fn migration_handles_missing_dir() {
    let fx = Fixture::new().await;
    let report = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(report.scanned, 0);
    assert_eq!(report.migrated, 0);
    assert_eq!(report.skipped_symlinks, 0);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    // Marker should be written so subsequent calls are no-ops.
    let marker = fx.data_dir.join("hf_cache").join(".migrated");
    assert!(
        tokio::fs::metadata(&marker).await.is_ok(),
        "marker file should be created even when nothing was found"
    );
}

#[tokio::test]
async fn migration_idempotent() {
    let fx = Fixture::new().await;
    fx.write_file("models/gguf/foo.gguf", b"hello world").await;

    let first = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(first.scanned, 1, "first call should scan the file");
    assert_eq!(first.migrated, 1, "first call should migrate the file");

    let second = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(
        second.scanned, 0,
        "second call should return empty report (marker present)"
    );
    assert_eq!(second.migrated, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn migration_skips_symlinks() {
    let fx = Fixture::new().await;
    let real = fx
        .write_file("models/elsewhere/target.gguf", b"real bytes")
        .await;
    let link = fx.data_dir.join("models").join("gguf").join("link.gguf");
    tokio::fs::create_dir_all(link.parent().unwrap())
        .await
        .unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let report = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(
        report.skipped_symlinks, 1,
        "the pre-existing symlink should be counted as skipped"
    );
    assert!(
        is_symlink(&link).await,
        "the symlink should be untouched on disk"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn migration_moves_flat_file_to_blob_and_symlinks() {
    let fx = Fixture::new().await;
    let flat = fx
        .write_file("models/gguf/foo.gguf", b"hello world contents")
        .await;

    let report = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(report.scanned, 1);
    assert_eq!(report.migrated, 1);

    assert!(
        is_symlink(&flat).await,
        "{} should now be a symlink",
        flat.display()
    );

    let target = tokio::fs::read_link(&flat).await.unwrap();
    let target_str = target.to_string_lossy();
    assert!(
        target_str.contains("hf_cache") && target_str.contains("/blobs/"),
        "symlink should point into hf_cache/.../blobs/, got: {target_str}"
    );

    let resolved = tokio::fs::canonicalize(&flat).await.unwrap();
    let bytes = tokio::fs::read(&resolved).await.unwrap();
    assert_eq!(bytes, b"hello world contents");
}

#[cfg(unix)]
#[tokio::test]
async fn migration_picks_up_whisper_ggml_at_models_root() {
    let fx = Fixture::new().await;
    // ggml-*.bin lives at the models/ root, not in a subdir.
    let flat = fx
        .write_file("models/ggml-tiny.en.bin", b"whisper model bytes")
        .await;

    let report = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(report.scanned, 1, "should find ggml file at models root");
    assert_eq!(report.migrated, 1);
    assert!(is_symlink(&flat).await);
}

#[cfg(unix)]
#[tokio::test]
async fn migration_picks_up_tts_voice_and_config() {
    let fx = Fixture::new().await;
    let voice = fx
        .write_file("models/tts/en_US-lessac-medium.onnx", b"onnx bytes")
        .await;
    let config = fx
        .write_file("models/tts/en_US-lessac-medium.onnx.json", b"{}")
        .await;

    let report = migrate_flat_files_to_blobs(&fx.data_dir).await.expect("ok");
    assert_eq!(report.scanned, 2);
    assert_eq!(report.migrated, 2);
    assert!(is_symlink(&voice).await);
    assert!(is_symlink(&config).await);
}
