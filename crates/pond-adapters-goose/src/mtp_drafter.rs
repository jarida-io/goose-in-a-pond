// NOT COMPILED. `lib.rs` leaves this module out of the build since speculative decoding was taken
// out of the llama.cpp engine on 2026-09-24 (goose 743649d98). Kept, unchanged, for when it
// returns: it needs pond-core's speculation gate and `draft_reconcile_plan` back (commented out in
// `models/domain/drafter.rs`), and its callers in `goose_agent.rs` and `pond-server` restored.

//! Drafter registration, and the speculation switch, for the live serving path.
//!
//! Speculative decoding needs a second, small model beside the chat model, and
//! the engine finds it by name through the registry — so a drafter file with no
//! registry row is invisible however correctly it was downloaded.
//!
//! This lives here, next to [`crate::vision_encoder`], because this is the path
//! that actually serves: `GooseAdapter` builds `LocalInferenceProvider`
//! directly and never goes through `LocalInferenceLlmAdapter`, which feeds the
//! quarantined PondAgent loop. Registering from there looks right, compiles,
//! and never runs.
//!
//! # The switch
//!
//! `Settings::speculative_decoding_enabled` turns the drafter off and on without a restart. The
//! engine decides whether to attach a drafter once, when it loads a model, from the registry
//! row it resolves, so applying the switch is three steps, in this order: point or clear
//! `draft_model` on the rows (pond-core's `draft_reconcile_plan`, applied in one save), evict
//! the loaded slot so the next turn reloads, and remember that it was applied. The last step is
//! [`SpeculationLedger`], and it is a ledger rather than a flag because an evict can silently
//! do nothing: a slot that is still LOADING is skipped, and that load finishes with the old
//! decision.

use crate::registry_rows::{self, RegistryRows};
use goose::providers::local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelRegistry, LocalModelStorage, ModelSettings,
};
use pond_core::models::domain::drafter::{
    draft_reconcile_plan, drafter_for, drafter_path, speculation_enabled, DraftChange, DraftRowView,
};
use pond_core::shared::services::egress;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Put this model's MTP drafter in the registry, if its weights are on disk and the speculation
/// switch is on.
///
/// Returns the registry id when speculation is available. Called on every
/// provider build rather than once, which is what makes the arrangement
/// self-correcting: a drafter downloaded after boot is picked up without a
/// restart, and one that has been deleted stops being referenced instead of
/// failing the next context creation.
pub fn ensure_drafter_registered(data_dir: &Path, model_name: &str) -> Option<String> {
    ensure_drafter_registered_with(data_dir, model_name, speculation_enabled())
}

/// [`ensure_drafter_registered`] with the switch passed in, for a caller holding this turn's
/// settings row: the process-global gate may already have been moved by a concurrent PUT.
///
/// Off, it touches nothing. It takes the registry lock itself, so a caller must never hold a
/// registry guard across it.
pub fn ensure_drafter_registered_with(
    data_dir: &Path,
    model_name: &str,
    enabled: bool,
) -> Option<String> {
    if !enabled {
        return None;
    }
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
    // Registering and pointing are separate steps, and the early return has to
    // skip only the first. This function is called twice with two different
    // spellings of the same model -- once from startup with the settings
    // string, once from the provider build with the canonical stem the engine
    // resolves -- and returning here on "already registered" meant the second
    // call, the only one holding the id that matters, never ran.
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
        // Defaults on purpose: a drafter is never a chat model, so nothing
        // reads its context size, tool mode or thinking flag. Its context is
        // built from the TARGET's settings, beside the target's own.
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

/// Set `draft_model` on the row the ENGINE resolves.
///
/// `apply_jetson_settings` stamps the model id as spelled in settings
/// (`gemma-4-E2B-it-qat-UD-Q4_K_XL`), while the engine loads the canonical stem
/// (`gemma-4-E2B-it-qat`) that `register_gguf_model` returns. Those are two rows
/// in one registry, and a `draft_model` written to the first is never read. This
/// is called with the canonical key, because that is what the caller in
/// `goose_agent` has in hand.
///
/// Read-modify-write rather than a fresh block: `update_model_settings` replaces
/// the whole `ModelSettings`, so constructing one here would drop whatever else
/// the row is carrying.
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

/// Make every row agree with the switch: OFF clears `draft_model` registry-wide, ON points every
/// row naming `target` at `drafter_id` (or clears them when there is no usable drafter).
///
/// One snapshot, one plan (pond-core's), one save. Returns the changes applied; empty means the
/// registry already agreed (which does not mean the loaded slot does: see [`SpeculationLedger`]).
pub fn reconcile_drafter(
    rows: &dyn RegistryRows,
    target: &Path,
    drafter_id: Option<&str>,
    enabled: bool,
) -> Vec<DraftChange> {
    let snapshot = rows.snapshot();
    let views: Vec<DraftRowView<'_>> = snapshot
        .iter()
        .map(|r| DraftRowView {
            id: &r.id,
            resolved_path: &r.resolved_path,
            draft_model: r.draft_model.as_deref(),
        })
        .collect();
    let target = registry_rows::resolve(target);
    let plan = draft_reconcile_plan(&views, &target, drafter_id, enabled);
    if plan.is_empty() {
        return plan;
    }
    let by_id: HashMap<&str, &Option<String>> = plan
        .iter()
        .map(|c| (c.id.as_str(), &c.draft_model))
        .collect();
    rows.edit(&mut |entry| match by_id.get(entry.id.as_str()) {
        Some(want) if entry.settings.draft_model != **want => {
            entry.settings.draft_model = (*want).clone();
            true
        }
        _ => false,
    });
    plan
}

/// Whether a file is a drafter the engine can load: GGUF, and the `gemma4-assistant`
/// architecture. The same check `ensure_mtp_drafter` makes at startup, so the runtime fetch
/// cannot install something the startup path would refuse.
pub fn is_loadable_drafter(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; 16 * 1024];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    head.truncate(n);
    head.starts_with(b"GGUF") && head.windows(16).any(|w| w == b"gemma4-assistant")
}

/// Fetch this model's drafter through pond-hf-cache and link it where the engine looks.
///
/// For the switch turned ON at runtime with the drafter not on disk. Same transport as the
/// vision encoder (resumable, per-hop egress gating, a mode change mid-transfer pauses it) and
/// the same validation as startup. A regular file already at the destination is never
/// overwritten: a loadable one is kept, anything else is renamed aside first.
pub async fn fetch_drafter(data_dir: &Path, chat_model: &str) -> anyhow::Result<PathBuf> {
    let spec =
        drafter_for(chat_model).ok_or_else(|| anyhow::anyhow!("{chat_model} has no drafter"))?;
    let dest = drafter_path(data_dir, &spec);
    if std::env::var(crate::vision_encoder::PROVISIONING_OPT_OUT_ENV).as_deref() == Ok("1") {
        anyhow::bail!(
            "{}=1; not downloading the drafter",
            crate::vision_encoder::PROVISIONING_OPT_OUT_ENV
        );
    }
    let cache = pond_hf_cache::HfCache::new(data_dir);
    let client = pond_hf_cache::build_redirect_aware_client(cache.token())?;
    let repo = cache.repo(spec.repo);
    let blob = repo
        .file(spec.filename)
        .download_to_blob(&client, cache.token(), |_, _| {
            egress::egress_verdict("huggingface.co", egress::network_mode()).is_ok()
        })
        .await?;
    if !is_loadable_drafter(&blob) {
        let aside = blob.with_file_name(format!(
            "{}.invalid",
            blob.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("drafter")
        ));
        let _ = std::fs::rename(&blob, &aside);
        anyhow::bail!(
            "the downloaded drafter is not a loadable gemma4-assistant GGUF; set aside at {}",
            aside.display()
        );
    }
    match pond_hf_cache::link_blob(&blob, &dest).await {
        Ok(()) => {}
        Err(e) if e.downcast_ref::<pond_hf_cache::DestNotALink>().is_some() => {
            if is_loadable_drafter(&dest) {
                return Ok(dest);
            }
            let aside = dest.with_file_name(format!("{}.invalid", spec.filename));
            std::fs::rename(&dest, &aside)?;
            pond_hf_cache::link_blob(&blob, &dest).await?;
        }
        Err(e) => return Err(e),
    }
    Ok(dest)
}

// ── Applying the switch ─────────────────────────────────────────────────────

/// What decides the drafter a loaded model has: which model, the switch, and whether the file
/// is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftTuple {
    /// The canonical registry key the engine loads the model under.
    pub key: String,
    pub enabled: bool,
    pub present: bool,
}

/// What a turn must do before it streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerStep {
    /// The loaded slot already reflects this tuple.
    Nothing,
    /// The tuple changed: reconcile the rows (and evict if that changed anything).
    Reconcile,
    /// A reconcile is applied to the rows but not yet to a loaded slot: evict again.
    EvictAgain { epoch: u64 },
}

/// Which switch position the engine's LOADED slot reflects.
///
/// Every reconcile is followed by an evict, whether or not it changed a row: other writers move
/// `draft_model` too (a PUT's provider rebuild re-stamps it from the gate before this adapter's
/// next turn runs), so "the rows already agree" says nothing about what the loaded slot was
/// built from. The evict is free when nothing is loaded, which covers the first turn of a
/// process and a model switch.
///
/// An evict only acts on a slot that is Loaded; one still Loading (the boot prewarm on the Orin
/// spends 10-20 s there, a quick OFF then ON lands inside the first reload) is skipped, and that
/// load completes with the decision it resolved before the reconcile. So a reconcile is only
/// PENDING until one of two things proves a load happened after it: the evict reports it
/// emptied a slot, or a later turn of this adapter reports a cold load (`model_load_ms`). Until
/// then every local turn evicts again before it streams.
#[derive(Debug, Default)]
pub struct SpeculationLedger {
    applied: Option<DraftTuple>,
    pending: Option<(DraftTuple, u64)>,
    epoch: u64,
    /// A runtime drafter fetch is running.
    fetching: bool,
}

impl SpeculationLedger {
    pub fn step(&self, tuple: &DraftTuple) -> LedgerStep {
        match &self.pending {
            Some((pending, epoch)) if pending == tuple => LedgerStep::EvictAgain { epoch: *epoch },
            Some(_) => LedgerStep::Reconcile,
            None if self.applied.as_ref() == Some(tuple) => LedgerStep::Nothing,
            None => LedgerStep::Reconcile,
        }
    }

    /// The rows now agree with `tuple`; the loaded slot does not yet. Returns the epoch to evict
    /// under.
    pub fn reconciled(&mut self, tuple: DraftTuple) -> u64 {
        self.epoch += 1;
        self.pending = Some((tuple, self.epoch));
        self.epoch
    }

    /// An evict under `epoch` returned. Only `evicted == true` proves anything.
    pub fn evicted(&mut self, epoch: u64, evicted: bool) {
        if evicted {
            self.settle(epoch);
        }
    }

    /// A turn that evicted under `epoch` then reported a cold load, which resolved the rows as
    /// they stood after the reconcile.
    pub fn loaded(&mut self, epoch: u64) {
        self.settle(epoch);
    }

    fn settle(&mut self, epoch: u64) {
        if let Some((tuple, pending_epoch)) = self.pending.take() {
            if pending_epoch == epoch {
                self.applied = Some(tuple);
            } else {
                self.pending = Some((tuple, pending_epoch));
            }
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Claim the runtime drafter fetch. `false` when one is already running.
    pub fn begin_fetch(&mut self) -> bool {
        !std::mem::replace(&mut self.fetching, true)
    }

    pub fn end_fetch(&mut self) {
        self.fetching = false;
    }
}

/// Where `GOOSE_LOCAL_DRAFT_MODEL` is set, if it is. It outranks the registry row, and so the
/// switch: the engine reads it whenever a row has no `draft_model`.
pub fn draft_override_source() -> Option<&'static str> {
    let set =
        goose::providers::local_inference::config_resolver::string_param("GOOSE_LOCAL_DRAFT_MODEL")
            .ok()
            .flatten()
            .is_some_and(|v| !v.trim().is_empty());
    if !set {
        return None;
    }
    Some(
        if std::env::var("GOOSE_LOCAL_DRAFT_MODEL").is_ok_and(|v| !v.trim().is_empty()) {
            "the environment"
        } else {
            "goose's config.yaml"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry_rows::{test_entry, MemoryRows};

    #[test]
    fn a_model_with_no_drafter_registers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_drafter_registered(tmp.path(), "Llama-3.2-3B-Instruct").is_none());
    }

    #[test]
    fn a_drafter_that_is_not_on_disk_registers_nothing() {
        // The registry row is what the engine resolves, so writing one for a
        // file that is not there would turn a missing download into a failed
        // context creation on the next turn.
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_drafter_registered(tmp.path(), "gemma-4-E2B-it-qat").is_none());
    }

    /// Off, registration does nothing at all, even with the drafter on disk: the rows are the
    /// reconcile's to clear, and a register-and-point here would undo it.
    #[test]
    fn the_switch_off_registers_and_points_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = drafter_for("gemma-4-E2B-it").unwrap();
        let path = drafter_path(tmp.path(), &spec);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"GGUF").unwrap();
        assert!(ensure_drafter_registered_with(tmp.path(), "gemma-4-E2B-it", false).is_none());
    }

    fn tuple(enabled: bool) -> DraftTuple {
        DraftTuple {
            key: "gemma-4-E2B-it".into(),
            enabled,
            present: true,
        }
    }

    /// Even a reconcile that changed no row is only pending: a PUT's provider rebuild can have
    /// re-stamped the rows already, while the loaded slot still carries the old drafter.
    #[test]
    fn a_reconcile_is_pending_until_a_load_proves_it() {
        let mut ledger = SpeculationLedger::default();
        assert_eq!(ledger.step(&tuple(true)), LedgerStep::Reconcile);
        let epoch = ledger.reconciled(tuple(true));
        assert!(ledger.is_pending());
        ledger.evicted(epoch, true);
        assert_eq!(ledger.step(&tuple(true)), LedgerStep::Nothing);
    }

    /// The pending-evict guard: an evict that emptied nothing (the slot was still loading)
    /// leaves the switch pending, and every later turn evicts again until one does.
    #[test]
    fn an_evict_that_emptied_nothing_keeps_the_switch_pending() {
        let mut ledger = SpeculationLedger::default();
        let first = ledger.reconciled(tuple(true));
        ledger.evicted(first, true);

        assert_eq!(ledger.step(&tuple(false)), LedgerStep::Reconcile);
        let epoch = ledger.reconciled(tuple(false));
        ledger.evicted(epoch, false);
        assert!(ledger.is_pending());
        assert_eq!(ledger.step(&tuple(false)), LedgerStep::EvictAgain { epoch });

        ledger.evicted(epoch, true);
        assert!(!ledger.is_pending());
        assert_eq!(ledger.step(&tuple(false)), LedgerStep::Nothing);
    }

    /// Nothing was loaded (the evict had nothing to empty): the turn's own cold load is the
    /// proof, and only for the epoch it evicted under.
    #[test]
    fn a_cold_load_after_the_reconcile_applies_it_and_an_older_one_does_not() {
        let mut ledger = SpeculationLedger::default();
        let first = ledger.reconciled(tuple(false));
        ledger.evicted(first, false);
        // The switch moves again before anything loaded.
        assert_eq!(ledger.step(&tuple(true)), LedgerStep::Reconcile);
        let second = ledger.reconciled(tuple(true));
        ledger.loaded(first);
        assert!(
            ledger.is_pending(),
            "a load for an earlier epoch proves nothing about this one"
        );
        ledger.loaded(second);
        assert_eq!(ledger.step(&tuple(true)), LedgerStep::Nothing);
    }

    #[test]
    fn only_one_runtime_fetch_runs_at_a_time() {
        let mut ledger = SpeculationLedger::default();
        assert!(ledger.begin_fetch());
        assert!(!ledger.begin_fetch());
        ledger.end_fetch();
        assert!(ledger.begin_fetch());
    }

    /// OFF clears every row that has a drafter, registry-wide, in one save; ON points only the
    /// rows naming the target.
    #[test]
    fn the_reconcile_applies_the_plan_to_every_row_in_one_save() {
        let tmp = tempfile::tempdir().unwrap();
        let e2b = tmp.path().join("e2b.gguf");
        let e4b = tmp.path().join("e4b.gguf");
        std::fs::write(&e2b, b"x").unwrap();
        std::fs::write(&e4b, b"y").unwrap();
        let link = tmp.path().join("alias.gguf");
        std::os::unix::fs::symlink(&e2b, &link).unwrap();
        let with_draft = |id: &str, path: &Path, draft: Option<&str>| {
            let mut e = test_entry(id, path);
            e.settings.draft_model = draft.map(str::to_string);
            e
        };
        let rows = MemoryRows::with(vec![
            with_draft("gemma-4-E2B-it", &e2b, Some("mtp-gemma-4-E2B-it")),
            with_draft("gemma-4-E2B-it-Q4_K_M", &link, None),
            with_draft("gemma-4-E4B-it-qat", &e4b, Some("mtp-gemma-4-E4B-it")),
        ]);

        let on = reconcile_drafter(&rows, &e2b, Some("mtp-gemma-4-E2B-it"), true);
        assert_eq!(
            on.len(),
            1,
            "only the unpointed spelling of the target changes"
        );
        assert_eq!(
            rows.get("gemma-4-E2B-it-Q4_K_M")
                .settings
                .draft_model
                .as_deref(),
            Some("mtp-gemma-4-E2B-it"),
            "a symlinked spelling of the same file is the same slot"
        );
        assert_eq!(
            rows.get("gemma-4-E4B-it-qat")
                .settings
                .draft_model
                .as_deref(),
            Some("mtp-gemma-4-E4B-it"),
            "ON leaves other files alone"
        );

        let off = reconcile_drafter(&rows, &e2b, Some("mtp-gemma-4-E2B-it"), false);
        assert_eq!(off.len(), 3);
        for id in [
            "gemma-4-E2B-it",
            "gemma-4-E2B-it-Q4_K_M",
            "gemma-4-E4B-it-qat",
        ] {
            assert_eq!(rows.get(id).settings.draft_model, None, "{id}");
        }
        assert_eq!(
            rows.saves.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one save per reconcile, however many rows"
        );
        assert!(reconcile_drafter(&rows, &e2b, None, false).is_empty());
        assert_eq!(rows.saves.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn only_a_gemma4_assistant_gguf_counts_as_a_drafter() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.gguf");
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        bytes.extend_from_slice(b"gemma4-assistant");
        std::fs::write(&good, &bytes).unwrap();
        assert!(is_loadable_drafter(&good));
        let chat = tmp.path().join("chat.gguf");
        std::fs::write(&chat, b"GGUF gemma4 weights").unwrap();
        assert!(!is_loadable_drafter(&chat));
        assert!(!is_loadable_drafter(&tmp.path().join("absent.gguf")));
    }
}
