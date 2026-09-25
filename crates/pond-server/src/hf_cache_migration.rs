//! One-shot move of flat model files into the HF cache as `giap-local/{stem}` blobs, leaving
//! symlinks behind. `{cache_root}/.migrated` makes it run once; no-op off unix.

use std::path::{Path, PathBuf};

use pond_hf_cache::HfCache;

/// Summary of one migration pass.
#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    /// Total regular files inspected.
    pub scanned: usize,
    /// Files successfully moved into the cache and symlinked.
    pub migrated: usize,
    /// Files skipped because they were already symlinks.
    pub skipped_symlinks: usize,
    /// Per-file failures (`"{path}: {err}"`) — non-fatal.
    pub errors: Vec<String>,
}

impl MigrationReport {
    /// True if the migration changed nothing on disk.
    pub fn is_empty(&self) -> bool {
        self.scanned == 0 && self.migrated == 0 && self.skipped_symlinks == 0
    }
}

/// Path to the on-disk marker that records a completed migration pass.
fn marker_path(data_dir: &Path) -> PathBuf {
    HfCache::new(data_dir).root().join(".migrated")
}

/// Run the migration once (later calls are no-ops); per-file failures go to `errors`.
pub async fn migrate_flat_files_to_blobs(data_dir: &Path) -> anyhow::Result<MigrationReport> {
    let marker = marker_path(data_dir);
    if tokio::fs::metadata(&marker).await.is_ok() {
        return Ok(MigrationReport::default());
    }

    #[cfg(unix)]
    let report = unix_migrate(data_dir).await;

    #[cfg(not(unix))]
    let report = {
        let _ = data_dir;
        MigrationReport::default()
    };

    // Write the marker even after per-file failures: never re-walk the filesystem every boot.
    if let Some(parent) = marker.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    if let Err(e) = tokio::fs::write(&marker, b"1").await {
        return Err(anyhow::anyhow!(
            "write migration marker {}: {e}",
            marker.display()
        ));
    }
    Ok(report)
}

#[cfg(unix)]
async fn unix_migrate(data_dir: &Path) -> MigrationReport {
    let mut report = MigrationReport::default();
    let models_root = data_dir.join("models");
    if tokio::fs::metadata(&models_root).await.is_err() {
        return report;
    }

    // ── Each target dir, with whether to recurse one level ──────────────
    let targets: &[(PathBuf, bool)] = &[
        (models_root.join("gguf"), false),
        (models_root.clone(), false), // ggml-*.bin sit at models root
        (models_root.join("llm"), false),
        (models_root.join("tts"), false),
        (models_root.join("embedding"), true),
    ];

    for (dir, recurse) in targets {
        if let Err(e) = walk_dir(dir, *recurse, &mut report, data_dir).await {
            report.errors.push(format!("{}: {e}", dir.display()));
        }
    }
    report
}

/// File-extension allowlist per target. `None` means "any regular file".
#[cfg(unix)]
fn file_filter(dir: &Path, root: &Path) -> Box<dyn Fn(&Path) -> bool + Send> {
    if dir == root {
        // Whisper ggml-*.bin only — don't touch random files at models root.
        Box::new(|p: &Path| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
                .unwrap_or(false)
        })
    } else if dir.ends_with("gguf") {
        Box::new(|p: &Path| p.extension().and_then(|e| e.to_str()) == Some("gguf"))
    } else if dir.ends_with("llm") {
        Box::new(|p: &Path| p.extension().and_then(|e| e.to_str()) == Some("llamafile"))
    } else if dir.ends_with("tts") {
        Box::new(|p: &Path| {
            let name = match p.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => return false,
            };
            name.ends_with(".onnx") || name.ends_with(".onnx.json")
        })
    } else {
        // embedding/ — accept any regular file.
        Box::new(|_p: &Path| true)
    }
}

#[cfg(unix)]
#[allow(clippy::needless_pass_by_value)]
async fn walk_dir(
    dir: &Path,
    recurse_one_level: bool,
    report: &mut MigrationReport,
    data_dir: &Path,
) -> anyhow::Result<()> {
    let models_root = data_dir.join("models");
    let accept = file_filter(dir, &models_root);

    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();

        // symlink_metadata: detect existing symlinks instead of following them.
        let meta = match tokio::fs::symlink_metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                report.errors.push(format!("{}: {e}", path.display()));
                continue;
            }
        };

        if meta.file_type().is_symlink() {
            // Count only accepted symlinks, so stray dotfiles stay out of the report.
            if accept(&path) {
                report.skipped_symlinks += 1;
            }
            continue;
        }

        if meta.file_type().is_dir() {
            if recurse_one_level {
                // One-level recurse: walk children but do NOT recurse again.
                if let Err(e) = Box::pin(walk_dir(&path, false, report, data_dir)).await {
                    report.errors.push(format!("{}: {e}", path.display()));
                }
            }
            continue;
        }

        if !meta.file_type().is_file() {
            continue;
        }
        if !accept(&path) {
            continue;
        }

        report.scanned += 1;
        if let Err(e) = migrate_one(&path, meta.len(), data_dir).await {
            report.errors.push(format!("{}: {e}", path.display()));
            continue;
        }
        report.migrated += 1;
    }
    Ok(())
}

/// Migrate a single flat file: hash, move into cache blob path, symlink back.
#[cfg(unix)]
async fn migrate_one(flat_path: &Path, file_size: u64, data_dir: &Path) -> anyhow::Result<()> {
    let file_name = flat_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("non-utf8 file name"))?
        .to_string();

    let etag = sha256_first_40(flat_path).await?;
    let stem = file_name
        .split('.')
        .next()
        .unwrap_or(&file_name)
        .to_string();
    let cache = HfCache::new(data_dir);
    let repo = cache.repo(format!("giap-local/{stem}"));
    let blob_path = repo.blob_path(&etag);

    let blobs_dir = blob_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("blob path has no parent"))?;
    tokio::fs::create_dir_all(blobs_dir).await?;

    // If a matching blob already exists, drop the flat file (it's redundant).
    match tokio::fs::metadata(&blob_path).await {
        Ok(meta) if meta.len() == file_size => {
            tokio::fs::remove_file(flat_path).await?;
        }
        _ => {
            // Try a same-fs rename first; fall back to copy + remove if cross-fs.
            if tokio::fs::rename(flat_path, &blob_path).await.is_err() {
                tokio::fs::copy(flat_path, &blob_path).await?;
                tokio::fs::remove_file(flat_path).await?;
            }
        }
    }

    let blob_owned = blob_path.clone();
    let flat_owned = flat_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::os::unix::fs::symlink(&blob_owned, &flat_owned).map_err(|e| {
            anyhow::anyhow!(
                "symlink {} -> {}: {e}",
                flat_owned.display(),
                blob_owned.display()
            )
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("symlink task panicked: {e}"))??;
    Ok(())
}

/// First 40 hex chars of `path`'s sha256, HF etag length; hashed on a blocking thread.
#[cfg(unix)]
async fn sha256_first_40(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let path = path.to_path_buf();
    let hex = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let mut f = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let digest = hasher.finalize();
        let mut s = String::with_capacity(64);
        for b in digest.iter() {
            use std::fmt::Write as _;
            let _ = write!(&mut s, "{b:02x}");
        }
        Ok(s)
    })
    .await
    .map_err(|e| anyhow::anyhow!("hash task panicked: {e}"))??;
    Ok(hex.chars().take(40).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_empty_by_default() {
        let r = MigrationReport::default();
        assert!(r.is_empty());
    }

    #[test]
    fn report_is_not_empty_after_scan() {
        let r = MigrationReport {
            scanned: 1,
            ..Default::default()
        };
        assert!(!r.is_empty());
    }
}
