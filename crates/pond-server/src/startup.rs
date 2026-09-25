//! Startup helpers, exposed via lib.rs so integration tests can call them.

use std::sync::Arc;

use pond_core::models::domain::model_record::ModelCategory;
use pond_core::models::ports::model_downloader::ModelDownloader;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::model_storage::ModelStorage;

/// Downloads each role-assigned model missing from disk and fixes stale `downloaded` flags;
/// returns how many downloads it started. Spawned after `sync_assignments_to_settings`.
pub async fn auto_download_assigned_models(
    repo: Arc<dyn ModelRepository + Send + Sync>,
    storage: Arc<dyn ModelStorage + Send + Sync>,
    downloader: Arc<dyn ModelDownloader + Send + Sync>,
) -> usize {
    let assignments = match repo.list_assignments().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("auto_download: failed to read assignments: {e}");
            return 0;
        }
    };

    let mut triggered = 0usize;

    for a in &assignments {
        let record = match repo.get_by_id(&a.model_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(
                    "auto_download: model '{}' in assignment but not in catalog — skipping",
                    a.model_id
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("auto_download: DB error fetching '{}': {e}", a.model_id);
                continue;
            }
        };

        // Server-side models need no local file — skip silently.
        if matches!(
            record.category,
            ModelCategory::Ollama | ModelCategory::TtsHttp
        ) {
            continue;
        }

        let on_disk = storage.is_present(&record);

        if on_disk && !record.downloaded {
            if let Err(e) = repo.set_downloaded(&record.id, true).await {
                tracing::warn!(
                    "auto_download: failed to fix DB flag for '{}': {e}",
                    record.id
                );
            } else {
                tracing::info!(
                    "auto_download: corrected downloaded flag for '{}' (file already on disk)",
                    record.id
                );
            }
            continue;
        }

        if on_disk {
            continue;
        }

        // A same-model sibling quant on disk counts: fetching this one may hit a gated-HF 401.
        if let Some(target) = storage.path_for(&record) {
            if let Some(sibling) = find_sibling_quantisation(&target).await {
                tracing::info!(
                    "auto_download: '{}' not present, but a same-model sibling \
                     ({}) is already on disk — using it and skipping download",
                    record.id,
                    sibling.display(),
                );
                if let Err(e) = repo.set_downloaded(&record.id, true).await {
                    tracing::warn!(
                        "auto_download: sibling found for '{}' but DB flag update failed: {e}",
                        record.id,
                    );
                }
                continue;
            }
        }

        let url = match record.url.as_deref() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => {
                tracing::warn!(
                    "auto_download: model '{}' (role '{}') has no download URL — cannot auto-download",
                    record.id, a.role
                );
                continue;
            }
        };

        let path = match storage.path_for(&record) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    "auto_download: no local path for '{}' — skipping",
                    record.id
                );
                continue;
            }
        };

        tracing::info!(
            "auto_download: '{}' assigned to role '{}' but not on disk — downloading…",
            record.id,
            a.role
        );

        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                tracing::warn!(
                    "auto_download: could not create dir {}: {e}",
                    parent.display()
                );
                continue;
            }
        }

        match downloader.download(&url, &path, record.size_mb).await {
            Ok(()) => {
                if let Err(e) = repo.set_downloaded(&record.id, true).await {
                    tracing::warn!(
                        "auto_download: downloaded '{}' but failed to update DB: {e}",
                        record.id
                    );
                } else {
                    tracing::info!("auto_download: '{}' ready at {}", record.id, path.display());
                    triggered += 1;
                }
            }
            Err(e) => {
                tracing::warn!("auto_download: failed to download '{}': {e}", record.id);
            }
        }
    }

    triggered
}

/// The unique `<prefix>-Q*.gguf` sibling of `target`: same model, different quantisation.
async fn find_sibling_quantisation(target: &std::path::Path) -> Option<std::path::PathBuf> {
    let parent = target.parent()?;
    let target_stem = target.file_stem()?.to_str()?;

    let prefix = match target_stem.rfind("-Q") {
        Some(idx) => &target_stem[..idx],
        None => return None,
    };
    if prefix.is_empty() {
        return None;
    }

    let mut rd = match tokio::fs::read_dir(parent).await {
        Ok(rd) => rd,
        Err(_) => return None,
    };

    let mut hits: Vec<std::path::PathBuf> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path == target {
            continue; // (defensive — target doesn't exist by precondition)
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Requiring `-Q` keeps unrelated files sharing the prefix from matching.
        if name.starts_with(&format!("{prefix}-Q")) && name.ends_with(".gguf") {
            hits.push(path);
        }
    }

    if hits.len() == 1 {
        Some(hits.into_iter().next().unwrap())
    } else {
        // Several hits are ambiguous; don't guess which quant the user wants.
        None
    }
}

#[cfg(test)]
mod sibling_tests {
    use super::find_sibling_quantisation;

    #[tokio::test]
    async fn finds_single_sibling_with_different_quant() {
        let dir = tempfile::tempdir().unwrap();
        let sibling = dir.path().join("gemma-4-E4B-it-Q4_K_M.gguf");
        std::fs::write(&sibling, b"x").unwrap();
        let target = dir.path().join("gemma-4-E4B-it-Q4_K_S.gguf");
        assert_eq!(find_sibling_quantisation(&target).await, Some(sibling));
    }

    #[tokio::test]
    async fn returns_none_when_no_sibling() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("unrelated-Q4_K_M.gguf"), b"x").unwrap();
        let target = dir.path().join("gemma-4-E4B-it-Q4_K_S.gguf");
        assert_eq!(find_sibling_quantisation(&target).await, None);
    }

    #[tokio::test]
    async fn returns_none_when_multiple_siblings_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gemma-4-E4B-it-Q4_K_M.gguf"), b"x").unwrap();
        std::fs::write(dir.path().join("gemma-4-E4B-it-Q5_0.gguf"), b"x").unwrap();
        let target = dir.path().join("gemma-4-E4B-it-Q4_K_S.gguf");
        assert_eq!(find_sibling_quantisation(&target).await, None);
    }

    #[tokio::test]
    async fn returns_none_when_target_has_no_quantisation_suffix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("model-extra-Q4_K_M.gguf"), b"x").unwrap();
        let target = dir.path().join("model.gguf");
        assert_eq!(find_sibling_quantisation(&target).await, None);
    }
}
