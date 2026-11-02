//! The speculative-decoding (MTP) drafter that pairs with a chat model.
//!
//! Speculation runs on CUDA builds only: on the Orin it measured 2.1-2.3x decode in process,
//! where verifying a few drafts costs about one decode; Metal measured 0.64-0.94x on live turns.

/// The drafter that pairs with a given chat model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrafterSpec {
    /// Registry id, and the value `ModelSettings::draft_model` refers to.
    pub id: &'static str,
    pub repo: &'static str,
    pub filename: &'static str,
    pub approx_mb: u64,
    pub size_bytes: u64,
    pub sha256: &'static str,
}

impl DrafterSpec {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.repo, self.filename
        )
    }
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
            size_bytes: 59_235_648,
            sha256: "586f2460b909008640981ec34060aa864e03c144fbabfb3173c4335087e4aae0",
        })
    } else if m.contains("e4b") {
        Some(DrafterSpec {
            id: "mtp-gemma-4-E4B-it",
            repo: "unsloth/gemma-4-E4B-it-qat-GGUF",
            filename: "mtp-gemma-4-E4B-it.gguf",
            approx_mb: 57,
            size_bytes: 59_678_016,
            sha256: "423074e537504b4f9ec5eafed5c639fac82c96631626efccacdd3c4039b20605",
        })
    } else {
        None
    }
}

/// Pinned drafter matching `repo` and `filename`, if any.
pub fn drafter_pin(repo: &str, filename: &str) -> Option<DrafterSpec> {
    for model in ["gemma-4-E2B-it", "gemma-4-E4B-it"] {
        if let Some(spec) = drafter_for(model) {
            if spec.repo.eq_ignore_ascii_case(repo) && spec.filename.eq_ignore_ascii_case(filename)
            {
                return Some(spec);
            }
        }
    }
    None
}

/// Where the drafter's weights live under a pond's data directory.
pub fn drafter_path(data_dir: &std::path::Path, spec: &DrafterSpec) -> std::path::PathBuf {
    data_dir.join("models").join("gguf").join(spec.filename)
}

/// Whether the first bytes of a file are a GGUF of the `gemma4-assistant` architecture, the
/// only drafter format the engine attaches.
pub fn is_drafter_head(head: &[u8]) -> bool {
    head.starts_with(b"GGUF") && head.windows(16).any(|w| w == b"gemma4-assistant")
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

    #[test]
    fn only_a_gemma4_assistant_gguf_is_a_drafter() {
        let mut head = b"GGUF\x03\x00\x00\x00".to_vec();
        head.extend_from_slice(b"general.architecture\x08\x00\x00\x00gemma4-assistant");
        assert!(is_drafter_head(&head));
        assert!(!is_drafter_head(b"GGUF general.architecture gemma4"));
        assert!(!is_drafter_head(b"PK\x03\x04gemma4-assistant"));
    }

    #[test]
    fn drafter_pin_matches_and_pins_identity() {
        let e2b =
            drafter_pin("unsloth/gemma-4-E2B-it-qat-GGUF", "mtp-gemma-4-E2B-it.gguf").unwrap();
        assert_eq!(e2b.id, "mtp-gemma-4-E2B-it");
        assert_eq!(e2b.size_bytes, 59_235_648);
        assert_eq!(
            e2b.sha256,
            "586f2460b909008640981ec34060aa864e03c144fbabfb3173c4335087e4aae0"
        );

        let e4b =
            drafter_pin("unsloth/gemma-4-E4B-it-qat-GGUF", "mtp-gemma-4-E4B-it.gguf").unwrap();
        assert_eq!(e4b.id, "mtp-gemma-4-E4B-it");
        assert_eq!(e4b.size_bytes, 59_678_016);
        assert_eq!(
            e4b.sha256,
            "423074e537504b4f9ec5eafed5c639fac82c96631626efccacdd3c4039b20605"
        );

        assert!(drafter_pin("unsloth/unknown", "mtp-gemma-4-E2B-it.gguf").is_none());
    }
}
