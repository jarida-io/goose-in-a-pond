//! Server startup helpers that are also reachable from integration tests.
//!
//! Exposing these through the crate's library target (lib.rs) lets integration
//! tests in `tests/` import them without duplicating code.

use std::sync::Arc;

use pond_core::domain::model_record::ModelCategory;
use pond_core::ports::model_downloader::ModelDownloader;
use pond_core::ports::model_repository::ModelRepository;
use pond_core::ports::model_storage::ModelStorage;

/// Background task: download any role-assigned model whose file is missing from disk.
///
/// Called once at server startup (via `tokio::spawn`) after `sync_assignments_to_settings`.
/// Runs non-blocking so the HTTP server is available immediately while models download.
///
/// # Logic
/// 1. Read all current role → model_id assignments from the repository.
/// 2. For each assignment fetch the `ModelRecord`.
/// 3. Skip models with no local file (`Ollama`, `TtsHttp`).
/// 4. If the file is present on disk but `downloaded` flag is false → fix the DB flag.
/// 5. If the file is absent → download it; on success flip `downloaded = true`.
///
/// Returns the number of downloads triggered (useful for tests and status logs).
pub async fn auto_download_assigned_models(
    repo:       Arc<dyn ModelRepository + Send + Sync>,
    storage:    Arc<dyn ModelStorage + Send + Sync>,
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
        if matches!(record.category, ModelCategory::Ollama | ModelCategory::TtsHttp) {
            continue;
        }

        let on_disk = storage.is_present(&record);

        // DB drift: file on disk but flag is false — correct without re-downloading.
        if on_disk && !record.downloaded {
            if let Err(e) = repo.set_downloaded(&record.id, true).await {
                tracing::warn!("auto_download: failed to fix DB flag for '{}': {e}", record.id);
            } else {
                tracing::info!(
                    "auto_download: corrected downloaded flag for '{}' (file already on disk)",
                    record.id
                );
            }
            continue;
        }

        // File present and flag correct — nothing to do.
        if on_disk {
            continue;
        }

        // Sibling-quantisation detection: the catalog row points at e.g.
        // `gemma-4-E4B-it-Q4_K_S.gguf` but the user already downloaded
        // `gemma-4-E4B-it-Q4_K_M.gguf` (same model, different quant).
        // Treat that as "good enough" — flip the DB flag and skip the
        // download so we don't trip the gated-HF 401 over a cosmetic
        // quantisation difference.
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

        // File absent — need to download.
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
                tracing::warn!("auto_download: no local path for '{}' — skipping", record.id);
                continue;
            }
        };

        tracing::info!(
            "auto_download: '{}' assigned to role '{}' but not on disk — downloading…",
            record.id, a.role
        );

        // Ensure parent directory exists.
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

/// Look in `target`'s parent directory for a file that's the same model as
/// `target` but with a different quantisation suffix. Returns `Some(path)`
/// when exactly one such sibling exists.
///
/// "Same model" is detected by stripping the trailing `-Qxxxx` token (e.g.
/// `-Q4_K_S`, `-Q4_K_M`, `-Q5_0`) from the basename and looking for any
/// other GGUF file in the directory whose basename starts with that prefix.
///
/// Why: HF gates the Gemma-4 GGUF repo behind a license + auth token. A
/// user who already manually downloaded one quantisation (Q4_K_M) and
/// pointed the catalog at another (Q4_K_S) would otherwise hit a noisy
/// `401 Unauthorized` warning on every server boot — even though they have
/// a perfectly usable copy of the model on disk.
async fn find_sibling_quantisation(target: &std::path::Path) -> Option<std::path::PathBuf> {
    let parent = target.parent()?;
    let target_stem = target.file_stem()?.to_str()?;

    // Strip a trailing `-Q...` quantisation suffix to get the model prefix.
    // If no such suffix exists we don't have a useful prefix to match on.
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
        // Match: starts with `<prefix>-Q` and ends with `.gguf`. The `-Q`
        // requirement avoids accidentally matching unrelated files that
        // happen to share a common name fragment.
        if name.starts_with(&format!("{prefix}-Q")) && name.ends_with(".gguf") {
            hits.push(path);
        }
    }

    if hits.len() == 1 {
        Some(hits.into_iter().next().unwrap())
    } else {
        // Zero hits: nothing to do; let auto-download try the URL.
        // Multiple hits: ambiguous — let the user resolve it manually
        // rather than silently picking the wrong quantisation.
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
