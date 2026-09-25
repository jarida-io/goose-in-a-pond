//! The speculative-decoding drafter each chat model needs, shared by the server that fetches it
//! and the adapter that uses it: a drifted copy downloads a drafter that is never used.

/// The drafter that pairs with a given chat model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrafterSpec {
    /// Registry id, and the value `ModelSettings::draft_model` refers to.
    pub id: &'static str,
    pub repo: &'static str,
    pub filename: &'static str,
    pub approx_mb: u64,
}

/// The MTP drafter for `chat_model`, matched on family and size, not quantisation. Sizes don't
/// mix: an E4B drafter has a different hidden size and cannot draft for an E2B target.
pub fn drafter_for(chat_model: &str) -> Option<DrafterSpec> {
    let m = chat_model.to_ascii_lowercase();
    if !m.contains("gemma-4") && !m.contains("gemma4") {
        return None;
    }
    // Gemma 4 drafters ship at the root of the unsloth repos the quantised weights come from.
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
}
