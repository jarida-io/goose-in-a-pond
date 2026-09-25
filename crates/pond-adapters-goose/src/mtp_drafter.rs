//! Registers speculative-decoding drafters, which the engine finds only via the registry. Must
//! live on this serving path: `GooseAdapter` never goes through `LocalInferenceLlmAdapter`.

use goose::providers::local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelRegistry, LocalModelStorage, ModelSettings,
};
use pond_core::models::domain::drafter::{drafter_for, drafter_path};
use std::path::Path;

/// Register this model's MTP drafter if its weights exist; returns its id. Called on every
/// provider build, so drafters added or deleted after boot are handled without a restart.
pub fn ensure_drafter_registered(data_dir: &Path, model_name: &str) -> Option<String> {
    let spec = drafter_for(model_name)?;
    let path = drafter_path(data_dir, &spec);
    if !path.exists() {
        return None;
    }

    let mut registry = match get_registry().lock() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("registry lock poisoned, not registering the drafter: {e}");
            return None;
        }
    };
    // Already registered: still point this model at it. Startup and the provider build pass
    // different spellings, and only the provider build's canonical stem is read.
    if registry
        .get_model(spec.id)
        .is_some_and(|e| e.local_path == path)
    {
        point_target_at_drafter(&mut registry, model_name, spec.id);
        return Some(spec.id.to_string());
    }

    let entry = LocalModelEntry {
        id: spec.id.to_string(),
        repo_id: spec.repo.to_string(),
        filename: spec.filename.to_string(),
        quantization: String::new(),
        local_path: path,
        source_url: format!(
            "https://huggingface.co/{}/resolve/main/{}",
            spec.repo, spec.filename
        ),
        backend_id: None,
        storage: LocalModelStorage::ManualPath,
        // Defaults on purpose: a drafter's context is built from the TARGET's settings.
        settings: ModelSettings::default(),
        size_bytes: 0,
        mmproj_path: None,
        mmproj_source_url: None,
        mmproj_size_bytes: 0,
        mmproj_checked: false,
        shard_files: vec![],
    };
    if let Err(e) = registry.add_model(entry) {
        tracing::warn!("could not register the MTP drafter: {e}");
        return None;
    }
    tracing::info!(drafter = spec.id, "MTP drafter registered");
    point_target_at_drafter(&mut registry, model_name, spec.id);
    Some(spec.id.to_string())
}

/// Set `draft_model` on the row the ENGINE resolves (the canonical stem). Read-modify-write:
/// `update_model_settings` replaces the whole `ModelSettings`.
fn point_target_at_drafter(
    registry: &mut impl std::ops::DerefMut<Target = LocalModelRegistry>,
    model_id: &str,
    drafter_id: &str,
) {
    let Some(mut settings) = registry.get_model(model_id).map(|e| e.settings.clone()) else {
        return;
    };
    if settings.draft_model.as_deref() == Some(drafter_id) {
        return;
    }
    settings.draft_model = Some(drafter_id.to_string());
    match registry.update_model_settings(model_id, settings) {
        Ok(()) => tracing::info!(
            model = model_id,
            drafter = drafter_id,
            "speculation enabled"
        ),
        Err(e) => tracing::warn!("could not point '{model_id}' at its drafter: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_with_no_drafter_registers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_drafter_registered(tmp.path(), "Llama-3.2-3B-Instruct").is_none());
    }

    #[test]
    fn a_drafter_that_is_not_on_disk_registers_nothing() {
        // A row for a missing file would fail the next context creation.
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_drafter_registered(tmp.path(), "gemma-4-E2B-it-qat").is_none());
    }
}
