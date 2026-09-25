//! HF cache cleanup and disk usage for `/api/v1/models/{cleanup,disk-usage}`.
//! Deletes only inside `hf_cache` and the `models/**` mirror: unreferenced, unprotected blobs,
//! `.incomplete` files past `STALE_INCOMPLETE_AGE`, and snapshot dirs of only dead symlinks.

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::fs;

/// Age past which a `*.incomplete` resume file may be swept.
pub const STALE_INCOMPLETE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// One blob removed during a cleanup sweep.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemovedBlob {
    /// Absolute path of the deleted file.
    pub path: String,
    /// Inferred category — coarse, used for UI grouping only.
    pub category: String,
    /// Size in bytes of the deleted file.
    pub bytes: u64,
}

/// Response body for `POST /api/v1/models/cleanup`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CleanupReport {
    /// Total bytes reclaimed across all `removed` entries.
    pub reclaimed_bytes: u64,
    /// One entry per deleted blob / stale resume file / snapshot dir.
    pub removed: Vec<RemovedBlob>,
}

/// Response body for `GET /api/v1/models/disk-usage`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DiskUsage {
    /// Sum of every value in `by_category`.
    pub total_bytes: u64,
    /// Per-category bytes resolved via symlinks under `models/<cat>`.
    pub by_category: std::collections::BTreeMap<String, u64>,
    /// Sum of every real blob byte under `hf_cache/hub/*/blobs/*` (excludes `.incomplete`).
    pub hf_cache_bytes: u64,
    /// Sum of every `.incomplete` resume file currently on disk.
    pub incomplete_bytes: u64,
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Sweep `{data_dir}/hf_cache`; `protected_names` (role-assigned files) survive even unreferenced.
pub async fn run_cleanup(
    data_dir: &Path,
    protected_names: &HashSet<String>,
) -> anyhow::Result<CleanupReport> {
    run_cleanup_with_threshold(data_dir, protected_names, STALE_INCOMPLETE_AGE).await
}

/// [`run_cleanup`] with a custom stale-`.incomplete` threshold (`ZERO` sweeps every one).
pub async fn run_cleanup_with_threshold(
    data_dir: &Path,
    protected_names: &HashSet<String>,
    stale_threshold: Duration,
) -> anyhow::Result<CleanupReport> {
    let hub_dir = data_dir.join("hf_cache").join("hub");
    if fs::metadata(&hub_dir).await.is_err() {
        return Ok(CleanupReport::default());
    }

    let referenced = collect_referenced_blobs(data_dir).await;
    let snapshot_basenames = collect_snapshot_basenames(&hub_dir).await;

    let mut report = CleanupReport::default();

    // ── Pass 1: unreferenced blobs ───────────────────────────────────────────
    let mut repo_dirs = fs::read_dir(&hub_dir).await?;
    while let Ok(Some(repo)) = repo_dirs.next_entry().await {
        let blobs_dir = repo.path().join("blobs");
        if fs::metadata(&blobs_dir).await.is_err() {
            continue;
        }
        let mut entries = match fs::read_dir(&blobs_dir).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // `.incomplete` files handled in pass 2.
            if name.ends_with(".incomplete") || name.ends_with(".lock") {
                continue;
            }
            let canonical = fs::canonicalize(&path)
                .await
                .unwrap_or_else(|_| path.clone());
            if referenced.contains(&canonical) {
                continue;
            }
            // Spare blobs a snapshot links under a protected basename.
            if let Some(basenames) = snapshot_basenames.get(&canonical) {
                if basenames.iter().any(|b| protected_names.contains(b)) {
                    continue;
                }
            }
            // Also spare a blob whose own name is protected (assignment with no surviving symlink).
            if protected_names.contains(&name) {
                continue;
            }
            let bytes = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            if let Err(e) = fs::remove_file(&path).await {
                tracing::warn!(path = %path.display(), error = %e, "cleanup: failed to remove blob");
                continue;
            }
            report.reclaimed_bytes += bytes;
            report.removed.push(RemovedBlob {
                path: path.display().to_string(),
                category: infer_category(&path),
                bytes,
            });
        }
    }

    // ── Pass 2: stale `.incomplete` resume files (> 24h) ─────────────────────
    let mut repo_dirs = fs::read_dir(&hub_dir).await?;
    while let Ok(Some(repo)) = repo_dirs.next_entry().await {
        let blobs_dir = repo.path().join("blobs");
        if fs::metadata(&blobs_dir).await.is_err() {
            continue;
        }
        let mut entries = match fs::read_dir(&blobs_dir).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if !name.ends_with(".incomplete") {
                continue;
            }
            let meta = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = SystemTime::now()
                .duration_since(mtime)
                .unwrap_or(Duration::ZERO);
            if age < stale_threshold {
                continue;
            }
            let bytes = meta.len();
            if let Err(e) = fs::remove_file(&path).await {
                tracing::warn!(path = %path.display(), error = %e, "cleanup: stale incomplete remove failed");
                continue;
            }
            report.reclaimed_bytes += bytes;
            report.removed.push(RemovedBlob {
                path: path.display().to_string(),
                category: infer_category(&path),
                bytes,
            });
        }
    }

    // ── Pass 3: orphaned snapshot dirs (all symlinks broken) ─────────────────
    let mut repo_dirs = fs::read_dir(&hub_dir).await?;
    while let Ok(Some(repo)) = repo_dirs.next_entry().await {
        let snapshots_dir = repo.path().join("snapshots");
        if fs::metadata(&snapshots_dir).await.is_err() {
            continue;
        }
        let mut commit_dirs = match fs::read_dir(&snapshots_dir).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        while let Ok(Some(commit)) = commit_dirs.next_entry().await {
            let commit_path = commit.path();
            if !commit_path.is_dir() {
                continue;
            }
            if is_fully_orphaned(&commit_path).await {
                if let Err(e) = fs::remove_dir_all(&commit_path).await {
                    tracing::warn!(path = %commit_path.display(), error = %e, "cleanup: snapshot dir remove failed");
                    continue;
                }
                report.removed.push(RemovedBlob {
                    path: commit_path.display().to_string(),
                    category: infer_category(&commit_path),
                    bytes: 0,
                });
            }
        }
    }

    Ok(report)
}

/// Walk the data directory and produce a per-category disk-usage report.
pub async fn collect_disk_usage(data_dir: &Path) -> anyhow::Result<DiskUsage> {
    let mut out = DiskUsage::default();

    // ── Per-category totals (symlinks resolve to target file size) ──────────
    let models_root = data_dir.join("models");
    if fs::metadata(&models_root).await.is_ok() {
        for cat in &["gguf", "whisper", "tts", "embedding", "llamafile", "llm"] {
            // "whisper" is virtual (models/ggml-*.bin); "llm" holds llamafile binaries.
            match *cat {
                "whisper" => {
                    let bytes = sum_files_matching(&models_root, |n| {
                        n.starts_with("ggml-") && n.ends_with(".bin")
                    })
                    .await;
                    out.by_category.insert("whisper".to_string(), bytes);
                }
                "llamafile" => {
                    let llm_dir = models_root.join("llm");
                    let bytes = sum_tree_bytes(&llm_dir).await;
                    out.by_category.insert("llamafile".to_string(), bytes);
                }
                "llm" => {} // folded into "llamafile"
                cat => {
                    let dir = models_root.join(cat);
                    let bytes = sum_tree_bytes(&dir).await;
                    out.by_category.insert(cat.to_string(), bytes);
                }
            }
        }
    }
    out.total_bytes = out.by_category.values().sum();

    // ── HF cache bytes + incomplete bytes ────────────────────────────────────
    let hub_dir = data_dir.join("hf_cache").join("hub");
    if fs::metadata(&hub_dir).await.is_ok() {
        let mut repo_dirs = fs::read_dir(&hub_dir).await?;
        while let Ok(Some(repo)) = repo_dirs.next_entry().await {
            let blobs_dir = repo.path().join("blobs");
            if fs::metadata(&blobs_dir).await.is_err() {
                continue;
            }
            let mut entries = match fs::read_dir(&blobs_dir).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = match entry.file_name().into_string() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let bytes = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                if name.ends_with(".incomplete") {
                    out.incomplete_bytes += bytes;
                } else if !name.ends_with(".lock") {
                    out.hf_cache_bytes += bytes;
                }
            }
        }
    }

    Ok(out)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Canonical targets of every symlink under `data_dir/models/**`.
async fn collect_referenced_blobs(data_dir: &Path) -> HashSet<PathBuf> {
    let mut referenced = HashSet::new();
    let models_root = data_dir.join("models");
    if fs::metadata(&models_root).await.is_err() {
        return referenced;
    }
    walk_collect_symlink_targets(&models_root, &mut referenced).await;
    referenced
}

/// Collect canonical targets of symlinks under `dir`, recursively (`models/` is shallow).
fn walk_collect_symlink_targets<'a>(
    dir: &'a Path,
    out: &'a mut HashSet<PathBuf>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = match fs::read_dir(dir).await {
            Ok(d) => d,
            Err(_) => return,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let file_type = match entry.file_type().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                if let Ok(canon) = fs::canonicalize(&path).await {
                    out.insert(canon);
                }
            } else if file_type.is_dir() {
                walk_collect_symlink_targets(&path, out).await;
            }
        }
    })
}

/// Blob canonical path → basenames of the snapshot symlinks targeting it (for name protection).
async fn collect_snapshot_basenames(
    hub_dir: &Path,
) -> std::collections::HashMap<PathBuf, HashSet<String>> {
    let mut out: std::collections::HashMap<PathBuf, HashSet<String>> =
        std::collections::HashMap::new();
    let mut repo_dirs = match fs::read_dir(hub_dir).await {
        Ok(d) => d,
        Err(_) => return out,
    };
    while let Ok(Some(repo)) = repo_dirs.next_entry().await {
        let snapshots_dir = repo.path().join("snapshots");
        if fs::metadata(&snapshots_dir).await.is_err() {
            continue;
        }
        let mut commit_dirs = match fs::read_dir(&snapshots_dir).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        while let Ok(Some(commit)) = commit_dirs.next_entry().await {
            let mut entries = match fs::read_dir(commit.path()).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            while let Ok(Some(file)) = entries.next_entry().await {
                let path = file.path();
                let basename = match file.file_name().into_string() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                if let Ok(canon) = fs::canonicalize(&path).await {
                    out.entry(canon).or_default().insert(basename);
                }
            }
        }
    }
    out
}

/// True if every entry in `dir` is a broken symlink; false for an empty dir.
async fn is_fully_orphaned(dir: &Path) -> bool {
    let mut entries = match fs::read_dir(dir).await {
        Ok(d) => d,
        Err(_) => return false,
    };
    let mut any_entry = false;
    while let Ok(Some(entry)) = entries.next_entry().await {
        any_entry = true;
        let path = entry.path();
        // Symlink whose target exists → not orphaned.
        if fs::metadata(&path).await.is_ok() {
            return false;
        }
    }
    any_entry
}

/// Total bytes under `dir`, following symlinks; 0 when `dir` is absent.
fn sum_tree_bytes<'a>(
    dir: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + 'a>> {
    Box::pin(async move {
        let mut total = 0u64;
        let mut entries = match fs::read_dir(dir).await {
            Ok(d) => d,
            Err(_) => return 0,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let file_type = match entry.file_type().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                total = total.saturating_add(sum_tree_bytes(&path).await);
            } else {
                if let Ok(m) = fs::metadata(&path).await {
                    total = total.saturating_add(m.len());
                }
            }
        }
        total
    })
}

/// Bytes of top-level files in `dir` whose name matches `predicate`.
async fn sum_files_matching(dir: &Path, predicate: impl Fn(&str) -> bool) -> u64 {
    let mut total = 0u64;
    let mut entries = match fs::read_dir(dir).await {
        Ok(d) => d,
        Err(_) => return 0,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if !predicate(&name) {
            continue;
        }
        if let Ok(m) = fs::metadata(entry.path()).await {
            total = total.saturating_add(m.len());
        }
    }
    total
}

/// Coarse category for the response only; deletion never depends on it.
fn infer_category(path: &Path) -> String {
    let s = path.display().to_string().to_ascii_lowercase();
    if s.contains("/gguf") || s.contains("--gguf") || s.ends_with(".gguf") {
        return "gguf".to_string();
    }
    if s.contains("/tts") || s.contains("piper") || s.ends_with(".onnx") {
        return "tts".to_string();
    }
    if s.contains("whisper") || s.contains("ggml-") {
        return "whisper".to_string();
    }
    if s.contains("/embedding") || s.contains("embed") {
        return "embedding".to_string();
    }
    if s.contains("llamafile") || s.contains("/llm/") {
        return "llamafile".to_string();
    }
    "gguf".to_string()
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Synthetic HF-cache blob; `repo_folder` is the full `models--…` folder name.
    fn make_blob(root: &Path, repo_folder: &str, etag: &str, body: &[u8]) -> PathBuf {
        let blobs = root
            .join("hf_cache")
            .join("hub")
            .join(repo_folder)
            .join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        let blob = blobs.join(etag);
        std::fs::write(&blob, body).unwrap();
        blob
    }

    /// Symlink `models/<sub>` → `target`, as the migration leaves on disk.
    fn link_flat(root: &Path, sub: &str, target: &Path) -> PathBuf {
        let link = root.join("models").join(sub);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        symlink(target, &link).unwrap();
        link
    }

    #[tokio::test]
    async fn cleanup_removes_unreferenced_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = make_blob(
            tmp.path(),
            "models--giap-local--orphan.gguf",
            "deadbeef",
            b"orphan blob contents",
        );

        let report = run_cleanup(tmp.path(), &HashSet::new()).await.unwrap();

        assert!(!blob.exists(), "orphan blob should be removed");
        assert_eq!(report.reclaimed_bytes, 20);
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].bytes, 20);
    }

    #[tokio::test]
    async fn cleanup_preserves_referenced_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = make_blob(
            tmp.path(),
            "models--giap-local--alive.gguf",
            "cafef00d",
            b"alive blob contents",
        );
        // Flat path under models/gguf/alive.gguf → symlink at the blob.
        link_flat(tmp.path(), "gguf/alive.gguf", &blob);

        let report = run_cleanup(tmp.path(), &HashSet::new()).await.unwrap();

        assert!(blob.exists(), "referenced blob must survive");
        assert_eq!(report.reclaimed_bytes, 0);
        assert!(report.removed.is_empty());
    }

    #[tokio::test]
    async fn cleanup_preserves_assigned_blob_even_if_unsymlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = make_blob(
            tmp.path(),
            "models--giap-local--assigned.gguf",
            "feedface",
            b"assigned blob contents",
        );
        // Snapshot link with the assigned name, no models/** link: deactivated but still assigned.
        let snap_dir = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--giap-local--assigned.gguf")
            .join("snapshots")
            .join("main");
        std::fs::create_dir_all(&snap_dir).unwrap();
        symlink(&blob, snap_dir.join("assigned.gguf")).unwrap();

        let mut protected = HashSet::new();
        protected.insert("assigned.gguf".to_string());

        let report = run_cleanup(tmp.path(), &protected).await.unwrap();
        assert!(
            blob.exists(),
            "assigned blob must survive even without flat symlink"
        );
        assert!(report.removed.is_empty());
    }

    #[tokio::test]
    async fn cleanup_removes_stale_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let blobs_dir = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--giap-local--resume.gguf")
            .join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        let stale = blobs_dir.join("abc123.incomplete");
        std::fs::write(&stale, b"partial").unwrap();

        // Zero threshold makes any `.incomplete` stale, with no filetime crate to backdate.
        let report = run_cleanup_with_threshold(tmp.path(), &HashSet::new(), Duration::ZERO)
            .await
            .unwrap();
        assert!(!stale.exists(), "stale .incomplete should be deleted");
        assert!(report.reclaimed_bytes >= 7);
    }

    #[tokio::test]
    async fn cleanup_keeps_fresh_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let blobs_dir = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--giap-local--resume.gguf")
            .join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        let fresh = blobs_dir.join("abc123.incomplete");
        std::fs::write(&fresh, b"partial").unwrap();

        // Default threshold (24h) — fresh file must survive.
        let _ = run_cleanup(tmp.path(), &HashSet::new()).await.unwrap();
        assert!(fresh.exists(), "fresh .incomplete must survive");
    }

    #[tokio::test]
    async fn cleanup_removes_orphaned_snapshot_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let snap_dir = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--giap-local--gone.gguf")
            .join("snapshots")
            .join("main");
        std::fs::create_dir_all(&snap_dir).unwrap();
        // Symlink whose target does not exist.
        symlink(
            snap_dir.join("nonexistent-blob"),
            snap_dir.join("file.gguf"),
        )
        .unwrap();

        let _ = run_cleanup(tmp.path(), &HashSet::new()).await.unwrap();
        assert!(
            !snap_dir.exists(),
            "fully-orphaned snapshot dir should be deleted"
        );
    }

    #[tokio::test]
    async fn disk_usage_reports_per_category() {
        let tmp = tempfile::tempdir().unwrap();

        // GGUF: one 5-byte symlink-to-blob.
        let gguf_blob = make_blob(tmp.path(), "models--giap-local--m.gguf", "blob1", b"AAAAA");
        link_flat(tmp.path(), "gguf/m.gguf", &gguf_blob);

        // TTS: a real .onnx file (no blob — flat file).
        let tts_dir = tmp.path().join("models").join("tts");
        std::fs::create_dir_all(&tts_dir).unwrap();
        std::fs::write(tts_dir.join("voice.onnx"), b"TTTTTTT").unwrap();

        // Whisper: a `ggml-base.bin` flat file at models root.
        let models = tmp.path().join("models");
        std::fs::write(models.join("ggml-base.bin"), b"WWW").unwrap();

        // Incomplete: counted separately.
        let blobs_dir = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--giap-local--m.gguf")
            .join("blobs");
        std::fs::write(blobs_dir.join("blob2.incomplete"), b"IIII").unwrap();

        let usage = collect_disk_usage(tmp.path()).await.unwrap();
        assert_eq!(usage.by_category.get("gguf").copied().unwrap_or(0), 5);
        assert_eq!(usage.by_category.get("tts").copied().unwrap_or(0), 7);
        assert_eq!(usage.by_category.get("whisper").copied().unwrap_or(0), 3);
        assert_eq!(usage.total_bytes, 15);
        assert_eq!(usage.incomplete_bytes, 4);
        assert_eq!(usage.hf_cache_bytes, 5); // only the real blob1, not incomplete
    }
}
