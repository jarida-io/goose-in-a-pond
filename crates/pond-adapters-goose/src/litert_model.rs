//! LiteRT-LM chat models in goose's registry: one row per `.litertlm` file, run by the `litert`
//! backend under the `local` provider. The chat path and the side-call adapter both register
//! through here, so they can't disagree about a row.

use goose::providers::local_inference::local_model_registry::{
    get_registry, LiteRtSettings, LocalModelEntry, LocalModelStorage, ModelSettings,
    ToolCallingMode,
};
use pond_core::models::domain::device_budget;
use pond_core::models::domain::engine::LITERT_BACKEND_ID;
use pond_core::models::domain::litert::{self, EngineOptions, Overrides};
use std::path::Path;

/// Before any LiteRT-LM engine loads: on the budgeted device, compile only the 128-token prefill
/// chunk (see [`litert::DEVICE_PREFILL_SIGNATURES`]), unless the environment already sets it.
/// Process-wide in the library, so it is set here once rather than per registry row; llama.cpp
/// never reads it.
pub fn apply_device_environment() {
    let current = std::env::var(litert::PREFILL_SIGNATURES_ENV).ok();
    let budgeted = device_budget::budgeted_device();
    match litert::prefill_signatures(budgeted, current.as_deref()) {
        Some(value) => {
            pond_core::shared::services::process_env::set(litert::PREFILL_SIGNATURES_ENV, value);
            tracing::info!(
                target: "giap::trace",
                kind = "litert_device_environment",
                prefill_signatures = value,
                "LiteRT-LM compiles only the 128-token prefill chunk on this device"
            );
        }
        None if budgeted => tracing::info!(
            target: "giap::trace",
            kind = "litert_device_environment",
            prefill_signatures = current.as_deref().unwrap_or_default(),
            "LiteRT-LM prefill chunk sizes left as the environment sets them"
        ),
        None => {}
    }
}

/// A LiteRT-LM row's settings: the engine options, and nothing only llama.cpp reads.
pub fn registry_settings(options: &EngineOptions) -> ModelSettings {
    ModelSettings {
        context_size: Some(options.context_tokens),
        tool_calling: ToolCallingMode::ForceNative,
        vision_capable: false,
        litert: Some(LiteRtSettings {
            backend: Some(options.execution.as_str().to_string()),
            speculative_decoding: Some(options.speculative_decoding),
        }),
        ..ModelSettings::default()
    }
}

/// Registers or repairs `model`'s row; returns its registry id, which is the model id itself.
/// A repaired row keeps its stamped tuning, and a row that is already right is not rewritten.
pub fn register(model: &str, data_dir: &Path) -> String {
    let local_path = litert::model_path(data_dir, model);
    if !local_path.exists() {
        tracing::warn!(
            model,
            path = %local_path.display(),
            "LiteRT-LM model file not found; the local provider will fail to load it"
        );
    }
    let fresh = registry_settings(&litert::engine_options(
        model,
        device_budget::budgeted_device(),
        Overrides::from_env(),
    ));
    match get_registry().lock() {
        Ok(mut registry) => {
            let planned = planned_entry(registry.get_model(model), model, &local_path, &fresh);
            if let Some(entry) = planned {
                match registry.add_model(entry) {
                    Ok(()) => tracing::info!(
                        model,
                        path = %local_path.display(),
                        "Registered LiteRT-LM model in the local registry"
                    ),
                    Err(e) => tracing::warn!(model, "Could not register LiteRT-LM model: {e}"),
                }
            }
        }
        Err(e) => tracing::warn!("local model registry lock poisoned: {e}"),
    }
    model.to_string()
}

/// The row [`register`] writes, or `None` when `existing` already is that row.
fn planned_entry(
    existing: Option<&LocalModelEntry>,
    id: &str,
    local_path: &Path,
    fresh: &ModelSettings,
) -> Option<LocalModelEntry> {
    let filename = local_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| id.to_string());
    let mut entry = existing.cloned().unwrap_or_else(|| LocalModelEntry {
        id: id.to_string(),
        repo_id: format!("local/{id}"),
        filename: filename.clone(),
        quantization: String::new(),
        local_path: local_path.to_path_buf(),
        source_url: String::new(),
        backend_id: None,
        storage: LocalModelStorage::ManualPath,
        settings: fresh.clone(),
        size_bytes: 0,
        mmproj_path: None,
        mmproj_source_url: None,
        mmproj_size_bytes: 0,
        mmproj_checked: false,
        shard_files: vec![],
    });
    entry.filename = filename;
    entry.local_path = local_path.to_path_buf();
    // Entry level: a settings stamp replaces the whole `ModelSettings`, its `backend_id` too.
    entry.backend_id = Some(LITERT_BACKEND_ID.to_string());
    // GIAP owns the file; goose must not delete it as managed storage.
    entry.storage = LocalModelStorage::ManualPath;
    entry.mmproj_path = None;
    entry.mmproj_source_url = None;
    entry.mmproj_size_bytes = 0;
    entry.shard_files.clear();
    let settings = &mut entry.settings;
    settings.tool_calling = ToolCallingMode::ForceNative;
    settings.vision_capable = false;
    settings.mmproj_size_bytes = 0;
    if settings.litert.is_none() {
        settings.litert = fresh.litert.clone();
    }
    if settings.context_size.is_none() {
        settings.context_size = fresh.context_size;
    }

    let unchanged = existing.is_some_and(|old| same_row(old, &entry));
    (!unchanged).then_some(entry)
}

/// Field-for-field, through serde: `LocalModelEntry` has no `PartialEq`.
fn same_row(a: &LocalModelEntry, b: &LocalModelEntry) -> bool {
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::litert::Execution;
    use std::path::PathBuf;

    const ID: &str = "gemma-4-E2B-it.litertlm";

    fn path() -> PathBuf {
        PathBuf::from("/pond/models/litertlm").join(ID)
    }

    fn fresh(context_tokens: u32) -> ModelSettings {
        registry_settings(&EngineOptions {
            context_tokens,
            execution: Execution::Gpu,
            speculative_decoding: true,
        })
    }

    #[test]
    fn a_new_row_is_a_litert_row_the_engine_routes_by_entry() {
        let entry = planned_entry(None, ID, &path(), &fresh(16384)).expect("a new row");
        assert_eq!(entry.id, ID);
        assert_eq!(entry.filename, ID);
        assert_eq!(entry.local_path, path());
        assert_eq!(entry.backend_id.as_deref(), Some("litert"));
        assert_eq!(entry.storage, LocalModelStorage::ManualPath);
        assert_eq!(entry.settings.tool_calling, ToolCallingMode::ForceNative);
        assert!(!entry.settings.vision_capable);
        assert!(entry.mmproj_path.is_none());
        assert_eq!(entry.settings.context_size, Some(16384));
        let block = entry.settings.litert.expect("the litert block");
        assert_eq!(block.backend.as_deref(), Some("gpu"));
        assert_eq!(block.speculative_decoding, Some(true));
        assert!(
            entry.settings.n_gpu_layers.is_none() && entry.settings.type_k.is_none(),
            "no llama.cpp knob belongs on a LiteRT-LM row"
        );
    }

    #[test]
    fn registering_a_row_that_is_already_right_writes_nothing() {
        let first = planned_entry(None, ID, &path(), &fresh(16384)).unwrap();
        assert!(planned_entry(Some(&first), ID, &path(), &fresh(16384)).is_none());
    }

    /// The stamp owns the window: a repair keeps it even where the fresh default differs.
    #[test]
    fn a_repair_keeps_the_stamped_tuning() {
        let mut stamped = planned_entry(None, ID, &path(), &fresh(4096)).unwrap();
        stamped.backend_id = None;
        stamped.local_path = PathBuf::from("/old/place").join(ID);
        let repaired = planned_entry(Some(&stamped), ID, &path(), &fresh(16384)).expect("repair");
        assert_eq!(repaired.backend_id.as_deref(), Some("litert"));
        assert_eq!(repaired.local_path, path());
        assert_eq!(repaired.settings.context_size, Some(4096));
    }

    /// A llama.cpp stamp replaces the whole settings block; the next registration puts the
    /// LiteRT-LM one back rather than leaving the engine on its CPU default.
    #[test]
    fn a_wiped_litert_block_is_filled_in() {
        let mut wiped = planned_entry(None, ID, &path(), &fresh(16384)).unwrap();
        wiped.settings = ModelSettings {
            n_gpu_layers: Some(99),
            ..ModelSettings::default()
        };
        let repaired = planned_entry(Some(&wiped), ID, &path(), &fresh(16384)).expect("repair");
        assert_eq!(
            repaired.settings.litert.and_then(|l| l.backend).as_deref(),
            Some("gpu")
        );
        assert_eq!(repaired.settings.context_size, Some(16384));
        assert_eq!(repaired.settings.tool_calling, ToolCallingMode::ForceNative);
    }

    #[test]
    fn a_row_never_carries_picture_support() {
        let mut row = planned_entry(None, ID, &path(), &fresh(16384)).unwrap();
        row.mmproj_path = Some(PathBuf::from(
            "/pond/models/mmproj/gemma-4-e2b-it/mmproj.gguf",
        ));
        row.settings.vision_capable = true;
        let repaired = planned_entry(Some(&row), ID, &path(), &fresh(16384)).expect("repair");
        assert!(repaired.mmproj_path.is_none());
        assert!(!repaired.settings.vision_capable);
    }
}
