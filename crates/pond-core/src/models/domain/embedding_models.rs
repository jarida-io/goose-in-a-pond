//! The embedding models the pond offers for the Memory role. A GGUF row is a pinned file the models
//! pipeline downloads into `models/embedding`; fastembed fetches its own weights.

use super::curated::FilePin;
use super::model_record::{ModelCategory, ModelRecord};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingEngine {
    Fastembed,
    LlamaCpp,
}

/// One GGUF at one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GgufFile {
    pub repo: &'static str,
    /// Pinned commit, so a re-upload can't swap the bytes under stored vectors.
    pub revision: &'static str,
    pub filename: &'static str,
    pub size_bytes: u64,
    /// The file's sha256: its LFS oid on Hugging Face.
    pub sha256: &'static str,
}

impl GgufFile {
    /// The file at its pinned revision.
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, self.filename
        )
    }

    pub fn size_mb(&self) -> u64 {
        self.size_bytes / MIB
    }

    pub fn pin(&self) -> FilePin {
        FilePin {
            size_bytes: self.size_bytes,
            sha256: self.sha256,
        }
    }
}

/// Where a model's weights come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weights {
    /// fastembed downloads and caches its own ONNX weights on first use; the sizes are approximate.
    Fastembed {
        download_mb: u64,
        ram_estimate_mb: u64,
    },
    /// A pinned GGUF that llama.cpp loads.
    Gguf(GgufFile),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingModel {
    /// The catalogue name: what the household picks and `active_embedding_model` stores.
    pub name: &'static str,
    /// Stamped on every vector the model makes; vectors under another id are another space.
    pub model_id: &'static str,
    /// The width of a stored vector.
    pub dims: usize,
    pub summary: &'static str,
    pub weights: Weights,
}

impl EmbeddingModel {
    pub fn engine(&self) -> EmbeddingEngine {
        match self.weights {
            Weights::Fastembed { .. } => EmbeddingEngine::Fastembed,
            Weights::Gguf(_) => EmbeddingEngine::LlamaCpp,
        }
    }

    pub fn gguf(&self) -> Option<&GgufFile> {
        match &self.weights {
            Weights::Gguf(file) => Some(file),
            Weights::Fastembed { .. } => None,
        }
    }

    /// `"embedding/{name}"`.
    pub fn id(&self) -> String {
        ModelRecord::id_for(&ModelCategory::Embedding, self.name)
    }

    pub fn size_mb(&self) -> u64 {
        match self.weights {
            Weights::Fastembed { download_mb, .. } => download_mb,
            Weights::Gguf(file) => file.size_mb(),
        }
    }

    pub fn ram_estimate_mb(&self) -> u64 {
        match self.weights {
            Weights::Fastembed {
                ram_estimate_mb, ..
            } => ram_estimate_mb,
            // Weights + 25% for the run, the catalogue's own rule of thumb.
            Weights::Gguf(file) => file.size_mb() + file.size_mb() / 4,
        }
    }

    /// The catalogue row: a GGUF row downloads its pinned file, a fastembed row names no file.
    pub fn record(&self) -> ModelRecord {
        let file = self.gguf();
        ModelRecord {
            id: self.id(),
            category: ModelCategory::Embedding,
            name: self.name.to_string(),
            filename: file.map(|f| f.filename.to_string()),
            description: self.summary.to_string(),
            size_mb: self.size_mb(),
            url: file.map(GgufFile::url),
            hf_id: None,
            ram_estimate_mb: Some(self.ram_estimate_mb()),
            recommended_role: Some("embedding".to_string()),
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        }
    }
}

pub const NOMIC_EMBED_TEXT_V1_5_Q8_0: GgufFile = GgufFile {
    repo: "nomic-ai/nomic-embed-text-v1.5-GGUF",
    revision: "0188c9bf409793f810680a5a431e7b899c46104c",
    filename: "nomic-embed-text-v1.5.Q8_0.gguf",
    size_bytes: 146_146_432,
    sha256: "3e24342164b3d94991ba9692fdc0dd08e3fd7362e0aacc396a9a5c54a544c3b7",
};

pub const BGE_BASE_EN_V1_5_Q8_0: GgufFile = GgufFile {
    repo: "CompendiumLabs/bge-base-en-v1.5-gguf",
    revision: "24914efdfa9ee54e815c3fcaa78a617031251c5c",
    filename: "bge-base-en-v1.5-q8_0.gguf",
    size_bytes: 117_974_304,
    sha256: "ad1afe72cd6654a558667a3db10878b049a75bfd72912e1dabb91310d671173c",
};

pub const ALL_MINILM_L6_V2: EmbeddingModel = EmbeddingModel {
    name: "all-MiniLM-L6-v2",
    model_id: "all-MiniLM-L6-v2",
    dims: 384,
    summary: "all-MiniLM-L6-v2 — fast, lightweight sentence embeddings. 23 MB, 384 dimensions. \
              Best for domain classification and semantic search.",
    weights: Weights::Fastembed {
        download_mb: 23,
        ram_estimate_mb: 50,
    },
};

pub const BGE_SMALL_EN_V1_5: EmbeddingModel = EmbeddingModel {
    name: "bge-small-en-v1.5",
    model_id: "bge-small-en-v1.5",
    dims: 384,
    summary: "BGE Small EN v1.5 — high-quality English embedding model. 33 MB, 384 dimensions.",
    weights: Weights::Fastembed {
        download_mb: 33,
        ram_estimate_mb: 70,
    },
};

pub const NOMIC_EMBED_TEXT_V1_5: EmbeddingModel = EmbeddingModel {
    name: "nomic-embed-text-v1.5",
    model_id: "nomic-embed-text-v1.5",
    dims: 768,
    summary: "Nomic Embed Text v1.5 — 8-bit GGUF run by llama.cpp. 768 dimensions, tuned for \
              search.",
    weights: Weights::Gguf(NOMIC_EMBED_TEXT_V1_5_Q8_0),
};

pub const BGE_BASE_EN_V1_5: EmbeddingModel = EmbeddingModel {
    name: "bge-base-en-v1.5",
    model_id: "bge-base-en-v1.5",
    dims: 768,
    summary: "BGE Base EN v1.5 — 8-bit GGUF run by llama.cpp. 768 dimensions, English.",
    weights: Weights::Gguf(BGE_BASE_EN_V1_5_Q8_0),
};

/// Every embedding model, in the order the catalogue lists them.
pub const EMBEDDING_MODELS: &[EmbeddingModel] = &[
    ALL_MINILM_L6_V2,
    BGE_SMALL_EN_V1_5,
    NOMIC_EMBED_TEXT_V1_5,
    BGE_BASE_EN_V1_5,
];

/// The model named `name` in the catalogue.
pub fn find(name: &str) -> Option<&'static EmbeddingModel> {
    EMBEDDING_MODELS.iter().find(|m| m.name == name)
}

/// The GGUF a Hugging Face file is, when it is one at its pinned revision.
pub fn pinned(repo: &str, revision: &str, filename: &str) -> Option<&'static GgufFile> {
    EMBEDDING_MODELS
        .iter()
        .filter_map(EmbeddingModel::gguf)
        .find(|f| f.repo == repo && f.revision == revision && f.filename == filename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::curated;
    use crate::models::domain::taxonomy::Acquisition;
    use std::collections::HashSet;

    fn hex(s: &str, n: usize) -> bool {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn names_and_model_ids_are_unique() {
        let names: HashSet<_> = EMBEDDING_MODELS.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), EMBEDDING_MODELS.len());
        let ids: HashSet<_> = EMBEDDING_MODELS.iter().map(|m| m.model_id).collect();
        assert_eq!(ids.len(), EMBEDDING_MODELS.len());
    }

    #[test]
    fn every_gguf_row_is_pinned_to_one_upload() {
        let ggufs: Vec<_> = EMBEDDING_MODELS.iter().filter_map(|m| m.gguf()).collect();
        assert_eq!(ggufs.len(), 2, "nomic and bge-base");
        for f in ggufs {
            assert!(hex(f.revision, 40), "{}: revision", f.filename);
            assert!(hex(f.sha256, 64), "{}: sha256", f.filename);
            assert!(f.size_bytes > 0, "{}", f.filename);
            assert!(f.filename.ends_with(".gguf"), "{}", f.filename);
            assert_eq!(
                f.url(),
                format!(
                    "https://huggingface.co/{}/resolve/{}/{}",
                    f.repo, f.revision, f.filename
                )
            );
        }
    }

    /// Both download paths enforce size and hash through this one lookup.
    #[test]
    fn the_download_pin_lookup_finds_every_gguf_row() {
        for f in EMBEDDING_MODELS.iter().filter_map(|m| m.gguf()) {
            assert_eq!(
                curated::file_pin(f.repo, f.revision, f.filename),
                Some(f.pin())
            );
            assert_eq!(pinned(f.repo, f.revision, f.filename), Some(f));
            assert_eq!(pinned(f.repo, "main", f.filename), None);
        }
    }

    /// A 384-wide GGUF would be indistinguishable from fastembed's vectors in a mixed store.
    #[test]
    fn the_engine_follows_the_weights_and_widths_stay_apart() {
        for m in EMBEDDING_MODELS {
            match m.engine() {
                EmbeddingEngine::Fastembed => {
                    assert!(m.gguf().is_none(), "{}", m.name);
                    assert_eq!(m.dims, 384, "{}", m.name);
                }
                EmbeddingEngine::LlamaCpp => {
                    assert!(m.gguf().is_some(), "{}", m.name);
                    assert_ne!(m.dims, 384, "{}", m.name);
                }
            }
        }
    }

    #[test]
    fn gguf_rows_download_and_fastembed_rows_stay_external() {
        for m in EMBEDDING_MODELS {
            let row = m.record();
            assert_eq!(row.id, format!("embedding/{}", m.name));
            assert_eq!(row.category, ModelCategory::Embedding);
            assert_eq!(row.size_mb, m.size_mb());
            match m.gguf() {
                Some(f) => {
                    assert_eq!(row.filename.as_deref(), Some(f.filename));
                    assert_eq!(row.url, Some(f.url()));
                    assert_eq!(Acquisition::of(&row, false), Acquisition::Download);
                }
                None => {
                    assert_eq!(row.filename, None);
                    assert_eq!(row.url, None);
                    assert_eq!(Acquisition::of(&row, false), Acquisition::External);
                }
            }
        }
        assert_eq!(find("nomic-embed-text-v1.5").unwrap().size_mb(), 139);
    }

    /// Settings and role assignments already store these names.
    #[test]
    fn the_names_are_the_ones_ponds_already_store() {
        let names: Vec<_> = EMBEDDING_MODELS.iter().map(|m| m.name).collect();
        assert_eq!(
            names,
            [
                "all-MiniLM-L6-v2",
                "bge-small-en-v1.5",
                "nomic-embed-text-v1.5",
                "bge-base-en-v1.5"
            ]
        );
    }
}
