//! Attaches a speculative-decoding drafter to the engine registry, downloading it if missing.

use goose::providers::local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelStorage, ModelSettings,
};
use pond_core::models::domain::drafter::{drafter_for, drafter_path, is_drafter_head, DrafterSpec};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static IN_FLIGHT: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());

/// Drafted tokens per step when the registry names none; a household's own setting is kept.
const PREFERRED_DRAFT_DEPTH: i32 = 2;

/// Register `model_name`'s drafter in `data_dir` and point the row(s) at it; returns the drafter's id.
/// If absent on disk, a background fetch is spawned and subsequent turns will attach it.
pub fn attach_drafter(data_dir: &Path, model_name: &str) -> Option<String> {
    let spec = drafter_for(model_name)?;
    let path = drafter_path(data_dir, &spec);
    let present = path.exists() && file_is_drafter(&path);

    if !present {
        clear_draft_model(model_name, spec.id);
        spawn_drafter_download(data_dir, &spec, model_name);
        return None;
    }

    if register_and_attach(data_dir, &path, &spec, model_name) {
        Some(spec.id.to_string())
    } else {
        None
    }
}

fn clear_draft_model(model_name: &str, drafter_id: &str) {
    let mut registry = match get_registry().lock() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("registry lock poisoned, not clearing drafter: {e}");
            return;
        }
    };
    if let Some(mut settings) = registry.get_model(model_name).map(|e| e.settings.clone()) {
        if settings.draft_model.as_deref() == Some(drafter_id) {
            settings.draft_model = None;
            let _ = registry.update_model_settings(model_name, settings);
        }
    }
}

fn register_and_attach(
    _data_dir: &Path,
    path: &Path,
    spec: &DrafterSpec,
    model_name: &str,
) -> bool {
    let mut registry = match get_registry().lock() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("registry lock poisoned, not attaching the drafter: {e}");
            return false;
        }
    };

    if registry
        .get_model(spec.id)
        .is_none_or(|e| e.local_path != path)
    {
        let size_bytes = std::fs::metadata(path)
            .map(|m| m.len())
            .unwrap_or(spec.size_bytes);
        let entry = LocalModelEntry {
            id: spec.id.to_string(),
            repo_id: spec.repo.to_string(),
            filename: spec.filename.to_string(),
            quantization: String::new(),
            local_path: path.to_path_buf(),
            source_url: spec.url(),
            backend_id: None,
            storage: LocalModelStorage::ManualPath,
            // Defaults on purpose: a drafter's context is built from the target's settings.
            settings: ModelSettings::default(),
            size_bytes,
            mmproj_path: None,
            mmproj_source_url: None,
            mmproj_size_bytes: 0,
            mmproj_checked: false,
            shard_files: vec![],
        };
        let _ = registry.remove_model(spec.id);
        if let Err(e) = registry.add_model(entry) {
            tracing::warn!("could not register the MTP drafter: {e}");
            return false;
        }
        tracing::info!(drafter = spec.id, "MTP drafter registered");
    }

    let target_path = registry.get_model(model_name).map(|e| e.local_path.clone());
    let matching_ids: Vec<String> = registry
        .list_models()
        .iter()
        .filter(|e| e.id == model_name || target_path.as_ref().is_some_and(|p| *p == e.local_path))
        .map(|e| e.id.clone())
        .collect();

    let mut attached = false;
    for id in matching_ids {
        if let Some(mut settings) = registry.get_model(&id).map(|e| e.settings.clone()) {
            // Depth 2 decoded about 10% faster than the engine's 4 on the Orin at the pond's own
            // sampling (temperature 0.8): deeper drafts are rejected more often than they pay.
            if settings.draft_n_max.is_none() {
                settings.draft_n_max = Some(PREFERRED_DRAFT_DEPTH);
                let _ = registry.update_model_settings(&id, settings.clone());
            }
            if settings.draft_model.as_deref() != Some(spec.id) {
                settings.draft_model = Some(spec.id.to_string());
                match registry.update_model_settings(&id, settings) {
                    Ok(()) => {
                        tracing::info!(
                            model = %id,
                            drafter = spec.id,
                            "speculative decoding attached"
                        );
                        attached = true;
                    }
                    Err(e) => {
                        tracing::warn!("could not point '{id}' at drafter {}: {e}", spec.id);
                    }
                }
            } else {
                attached = true;
            }
        }
    }
    attached
}

fn spawn_drafter_download(data_dir: &Path, spec: &DrafterSpec, model_name: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let claimed = {
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        in_flight.insert(spec.id)
    };
    if !claimed {
        return;
    }

    let data_dir = data_dir.to_path_buf();
    let spec = spec.clone();
    let model_name = model_name.to_string();

    handle.spawn(async move {
        struct FlightGuard(&'static str);
        impl Drop for FlightGuard {
            fn drop(&mut self) {
                if let Ok(mut set) = IN_FLIGHT.lock() {
                    set.remove(self.0);
                }
            }
        }
        let _guard = FlightGuard(spec.id);
        match download_drafter(&data_dir, &spec).await {
            Ok(path) => {
                tracing::info!(
                    drafter = spec.id,
                    path = %path.display(),
                    "MTP drafter downloaded and verified"
                );
                register_and_attach(&data_dir, &path, &spec, &model_name);
            }
            Err(e) => {
                tracing::warn!(
                    drafter = spec.id,
                    error = %format!("{e:#}"),
                    "could not download MTP drafter"
                );
            }
        }
    });
}

async fn download_drafter(data_dir: &Path, spec: &DrafterSpec) -> anyhow::Result<PathBuf> {
    let cache = pond_hf_cache::HfCache::new(data_dir);
    let token = cache.token().map(String::from);
    let client = pond_hf_cache::build_redirect_aware_client(token.as_deref())?;
    let repo = cache
        .repo(spec.repo.to_string())
        .with_revision("main".to_string());
    let fetch = repo
        .file(spec.filename.to_string())
        .expect_size(spec.size_bytes)
        .expect_etag(spec.sha256);

    let blob = fetch
        .download_to_blob(&client, token.as_deref(), |_| true)
        .await?;
    let dest = drafter_path(data_dir, spec);

    if let Err(e) = pond_hf_cache::link_blob(&blob, &dest).await {
        if e.downcast_ref::<pond_hf_cache::DestNotALink>().is_some() {
            let _ = tokio::fs::remove_file(&dest).await;
            pond_hf_cache::link_blob(&blob, &dest).await?;
        } else {
            return Err(e);
        }
    }

    if !file_is_drafter(&dest) {
        anyhow::bail!("downloaded file is not a valid gemma4-assistant drafter");
    }
    Ok(dest)
}

/// The first 16 KiB of `path` name the `gemma4-assistant` architecture.
fn file_is_drafter(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; 16 * 1024];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    head.truncate(n);
    is_drafter_head(&head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_drafter_file(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut head = b"GGUF\x03\x00\x00\x00".to_vec();
        head.extend_from_slice(b"general.architecture\x08\x00\x00\x00gemma4-assistant");
        std::fs::write(path, head).unwrap();
    }

    #[test]
    fn attach_drafter_registers_and_attaches_on_disk_drafter() {
        let tmp = tempdir().unwrap();
        let spec = drafter_for("gemma-4-E2B-it").unwrap();
        let path = drafter_path(tmp.path(), &spec);
        make_drafter_file(&path);

        let target_model = "gemma-4-E2B-it-qat-UD-Q4_K_XL";
        {
            let mut reg = get_registry().lock().unwrap();
            let _ = reg.remove_model(target_model);
            let _ = reg.remove_model(spec.id);
            let entry = LocalModelEntry {
                id: target_model.to_string(),
                repo_id: format!("local/{target_model}"),
                filename: format!("{target_model}.gguf"),
                quantization: String::new(),
                local_path: tmp.path().join(format!("{target_model}.gguf")),
                source_url: String::new(),
                backend_id: None,
                storage: LocalModelStorage::ManualPath,
                settings: ModelSettings::default(),
                size_bytes: 1000,
                mmproj_path: None,
                mmproj_source_url: None,
                mmproj_size_bytes: 0,
                mmproj_checked: false,
                shard_files: vec![],
            };
            reg.add_model(entry).unwrap();
        }

        let attached = attach_drafter(tmp.path(), target_model);
        assert_eq!(attached.as_deref(), Some(spec.id));

        {
            let reg = get_registry().lock().unwrap();
            let drafter_entry = reg.get_model(spec.id).expect("drafter registered");
            assert_eq!(drafter_entry.source_url, spec.url());
            assert_eq!(drafter_entry.local_path, path);

            let model_entry = reg.get_model(target_model).expect("target model exists");
            assert_eq!(model_entry.settings.draft_model.as_deref(), Some(spec.id));
        }

        std::fs::remove_file(&path).unwrap();
        let attached_after_delete = attach_drafter(tmp.path(), target_model);
        assert!(attached_after_delete.is_none());

        {
            let reg = get_registry().lock().unwrap();
            let model_entry = reg.get_model(target_model).expect("target model exists");
            assert!(model_entry.settings.draft_model.is_none());
        }
    }
}
