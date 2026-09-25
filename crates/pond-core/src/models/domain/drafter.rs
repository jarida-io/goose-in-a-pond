//! Which helper model a chat model needs for speculative decoding.
//!
//! Speculative decoding runs a second, tiny model beside the chat model: it
//! proposes tokens, the chat model verifies them, and several can be accepted
//! per forward pass. Measured on a Jetson Orin Nano, that takes a real turn
//! from 31 to 49 tok/s.
//!
//! This mapping lives in the domain because two layers need it and neither
//! should own it: the server fetches the file, and the local-inference adapter
//! decides whether to point the engine at it. A copy in each would drift, and
//! the failure mode of a drifted copy is a drafter that downloads and is never
//! used.

// use std::sync::atomic::{AtomicBool, Ordering};

/// The drafter that pairs with a given chat model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrafterSpec {
    /// Registry id, and the value `ModelSettings::draft_model` refers to.
    pub id: &'static str,
    pub repo: &'static str,
    pub filename: &'static str,
    pub approx_mb: u64,
}

/// The MTP drafter for `chat_model`, if one exists.
///
/// Matched on the model FAMILY rather than the full id, because one family
/// appears under many spellings -- `gemma-4-E2B-it`, `gemma-4-E2B-it-qat`,
/// `gemma-4-E2B-it-qat-UD-Q4_K_XL` -- while a drafter is tied to the
/// architecture and not to the quantisation.
///
/// The pairing is not interchangeable: an E4B drafter has a different hidden
/// size and cannot draft for an E2B target.
pub fn drafter_for(chat_model: &str) -> Option<DrafterSpec> {
    let m = chat_model.to_ascii_lowercase();
    if !m.contains("gemma-4") && !m.contains("gemma4") {
        return None;
    }
    // Gemma 4's drafters ship at the root of the same unsloth repositories the
    // quantised weights come from.
    if m.contains("e2b") {
        Some(DrafterSpec {
            id: "mtp-gemma-4-E2B-it",
            repo: "unsloth/gemma-4-E2B-it-qat-GGUF",
            filename: "mtp-gemma-4-E2B-it.gguf",
            approx_mb: 57,
        })
    } else if m.contains("e4b") {
        Some(DrafterSpec {
            id: "mtp-gemma-4-E4B-it",
            repo: "unsloth/gemma-4-E4B-it-qat-GGUF",
            filename: "mtp-gemma-4-E4B-it.gguf",
            approx_mb: 57,
        })
    } else {
        None
    }
}

/// Where the drafter's weights live under a pond's data directory.
pub fn drafter_path(data_dir: &std::path::Path, spec: &DrafterSpec) -> std::path::PathBuf {
    data_dir.join("models").join("gguf").join(spec.filename)
}

// Speculative decoding was taken out of the llama.cpp engine on 2026-09-24 (goose 743649d98), so
// the switch and its registry reconcile are commented out rather than deleted; restore them
// together if it returns.
// // ── The speculation switch ──────────────────────────────────────────────────
//
// /// Default on: an install that has never touched the setting keeps speculative decoding, and
// /// `Settings::speculative_decoding_enabled` itself defaults to true.
// static SPECULATION_ENABLED: AtomicBool = AtomicBool::new(true);
//
// /// Apply the current `Settings::speculative_decoding_enabled`.
// ///
// /// A process global, like the mic gate, because the code that stamps `draft_model` into the
// /// registry (`apply_jetson_settings`, `ensure_drafter_registered`) has no access to settings.
// /// It must be written before any of them runs: at each entry point's start, and in PUT
// /// /settings before the handler rebuilds a provider, or the rebuild re-stamps the drafter the
// /// user just turned off.
// pub fn set_speculation_enabled(enabled: bool) {
//     let previous = SPECULATION_ENABLED.swap(enabled, Ordering::SeqCst);
//     if previous != enabled {
//         tracing::info!(
//             speculative_decoding_enabled = enabled,
//             "speculative decoding switched {}; it applies when the model next loads",
//             if enabled { "on" } else { "off" }
//         );
//     }
// }
//
// /// Whether a drafter may be attached.
// pub fn speculation_enabled() -> bool {
//     SPECULATION_ENABLED.load(Ordering::SeqCst)
// }
//
// /// One registry row, as the reconcile needs to see it.
// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
// pub struct DraftRowView<'a> {
//     pub id: &'a str,
//     /// The row's `local_path`, resolved through symlinks.
//     pub resolved_path: &'a std::path::Path,
//     /// Its `settings.draft_model`.
//     pub draft_model: Option<&'a str>,
// }
//
// /// Set one row's `settings.draft_model` to this value.
// #[derive(Debug, Clone, PartialEq, Eq)]
// pub struct DraftChange {
//     pub id: String,
//     pub draft_model: Option<String>,
// }
//
// /// The `draft_model` changes that make the registry agree with the switch.
// ///
// /// OFF clears `draft_model` on EVERY row that has one, registry-wide, not only the rows naming
// /// the target: one GGUF is registered under three ids on the Mac, rows for inactive models keep
// /// values persisted by earlier boots, and any of them can cold-load the shared slot with the
// /// drafter attached. ON points every row resolving to `target_path` at `drafter_id`; with no
// /// usable drafter (`None`: this model has none, or its file is not on disk) those rows are
// /// cleared instead, as `apply_jetson_settings` has always done for a deleted drafter, so a
// /// cold load is never handed a path that is not there.
// ///
// /// Empty when nothing changes, which is the adapter's signal not to save and not to evict.
// pub fn draft_reconcile_plan(
//     rows: &[DraftRowView<'_>],
//     target_path: &std::path::Path,
//     drafter_id: Option<&str>,
//     enabled: bool,
// ) -> Vec<DraftChange> {
//     rows.iter()
//         .filter_map(|r| {
//             let want = if !enabled {
//                 None
//             } else if r.resolved_path == target_path {
//                 drafter_id
//             } else {
//                 // ON leaves rows for other files alone.
//                 return None;
//             };
//             (r.draft_model != want).then(|| DraftChange {
//                 id: r.id.to_string(),
//                 draft_model: want.map(str::to_string),
//             })
//         })
//         .collect()
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_decides_the_drafter_not_the_quantisation() {
        for id in [
            "gemma-4-E2B-it",
            "gemma-4-E2B-it-qat",
            "gemma-4-E2B-it-qat-UD-Q4_K_XL",
            "GEMMA-4-e2b-IT",
        ] {
            assert_eq!(drafter_for(id).unwrap().id, "mtp-gemma-4-E2B-it", "{id}");
        }
        assert_eq!(
            drafter_for("gemma-4-E4B-it-qat-UD-Q4_K_XL").unwrap().id,
            "mtp-gemma-4-E4B-it"
        );
    }

    #[test]
    fn a_model_with_no_drafter_gets_none() {
        for id in [
            "Llama-3.2-3B-Instruct",
            "gemma-4-12b-it",
            "granite-4.1-3b",
            "gemma-4-E5B-it",
        ] {
            assert!(drafter_for(id).is_none(), "{id}");
        }
    }

    #[test]
    fn the_two_families_never_share_a_drafter() {
        let e2b = drafter_for("gemma-4-E2B-it").unwrap();
        let e4b = drafter_for("gemma-4-E4B-it").unwrap();
        assert_ne!(e2b.id, e4b.id);
        assert_ne!(e2b.filename, e4b.filename);
    }

    // /// The switch defaults on. Only the default is asserted: flipping a process global here
    // /// would race every other test in the binary that reads it.
    // #[test]
    // fn speculation_defaults_on() {
    //     assert!(speculation_enabled());
    // }
    //
    // use std::path::Path;
    //
    // const E2B: &str = "mtp-gemma-4-E2B-it";
    //
    // /// The Mac's registry shape: one GGUF under three ids, another model's rows carrying a
    // /// persisted drafter from an earlier boot, and the drafter's own row.
    // fn rows<'a>() -> Vec<DraftRowView<'a>> {
    //     let e2b = Path::new("/blobs/e2b");
    //     let e4b = Path::new("/blobs/e4b");
    //     vec![
    //         DraftRowView {
    //             id: "gemma-4-E2B-it-Q4_K_M",
    //             resolved_path: e2b,
    //             draft_model: Some(E2B),
    //         },
    //         DraftRowView {
    //             id: "gemma-4-E2B-it",
    //             resolved_path: e2b,
    //             draft_model: None,
    //         },
    //         DraftRowView {
    //             id: "gemma-4-E2B-it:Q4_K_M",
    //             resolved_path: e2b,
    //             draft_model: Some(E2B),
    //         },
    //         DraftRowView {
    //             id: "gemma-4-E4B-it-qat",
    //             resolved_path: e4b,
    //             draft_model: Some("mtp-gemma-4-E4B-it"),
    //         },
    //         DraftRowView {
    //             id: E2B,
    //             resolved_path: Path::new("/blobs/mtp"),
    //             draft_model: None,
    //         },
    //     ]
    // }
    //
    // fn ids(plan: &[DraftChange]) -> Vec<&str> {
    //     plan.iter().map(|c| c.id.as_str()).collect()
    // }
    //
    // #[test]
    // fn off_clears_every_row_that_has_a_drafter_registry_wide() {
    //     let plan = draft_reconcile_plan(&rows(), Path::new("/blobs/e2b"), Some(E2B), false);
    //     assert_eq!(
    //         ids(&plan),
    //         [
    //             "gemma-4-E2B-it-Q4_K_M",
    //             "gemma-4-E2B-it:Q4_K_M",
    //             "gemma-4-E4B-it-qat"
    //         ],
    //         "an inactive model's persisted drafter can still cold-load the shared slot"
    //     );
    //     assert!(plan.iter().all(|c| c.draft_model.is_none()));
    // }
    //
    // #[test]
    // fn on_points_every_row_naming_the_target_and_touches_no_other() {
    //     let plan = draft_reconcile_plan(&rows(), Path::new("/blobs/e2b"), Some(E2B), true);
    //     assert_eq!(
    //         plan,
    //         vec![DraftChange {
    //             id: "gemma-4-E2B-it".into(),
    //             draft_model: Some(E2B.into())
    //         }],
    //         "the rows already pointed, the E4B rows and the drafter's own row are left alone"
    //     );
    // }
    //
    // #[test]
    // fn on_with_no_usable_drafter_clears_the_target_rows_only() {
    //     let plan = draft_reconcile_plan(&rows(), Path::new("/blobs/e2b"), None, true);
    //     assert_eq!(
    //         ids(&plan),
    //         ["gemma-4-E2B-it-Q4_K_M", "gemma-4-E2B-it:Q4_K_M"]
    //     );
    // }
    //
    // /// Applying a plan and planning again yields nothing: the adapter's cue not to save or evict.
    // #[test]
    // fn a_reconciled_registry_plans_nothing() {
    //     for enabled in [true, false] {
    //         let target = Path::new("/blobs/e2b");
    //         let mut owned: Vec<(String, std::path::PathBuf, Option<String>)> = rows()
    //             .iter()
    //             .map(|r| {
    //                 (
    //                     r.id.to_string(),
    //                     r.resolved_path.to_path_buf(),
    //                     r.draft_model.map(str::to_string),
    //                 )
    //             })
    //             .collect();
    //         let view = |o: &[(String, std::path::PathBuf, Option<String>)]| -> Vec<DraftChange> {
    //             let v: Vec<DraftRowView> = o
    //                 .iter()
    //                 .map(|(id, p, d)| DraftRowView {
    //                     id,
    //                     resolved_path: p,
    //                     draft_model: d.as_deref(),
    //                 })
    //                 .collect();
    //             draft_reconcile_plan(&v, target, Some(E2B), enabled)
    //         };
    //         for change in view(&owned) {
    //             let row = owned.iter_mut().find(|r| r.0 == change.id).unwrap();
    //             row.2 = change.draft_model;
    //         }
    //         assert!(view(&owned).is_empty(), "enabled={enabled}");
    //     }
    //     assert!(draft_reconcile_plan(&[], Path::new("/x"), Some(E2B), false).is_empty());
    // }
}
