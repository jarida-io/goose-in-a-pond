//! GIAP's own short list of conversation models, each pinned to one upload so a download refuses
//! any other bytes. Names are the file stems (GGUF) and file names (LiteRT-LM) ponds already use,
//! so an existing role assignment keeps pointing at its pick.

use super::engine::Engine;
use super::model_record::{ModelCategory, ModelRecord};

const MIB: u64 = 1024 * 1024;

/// One pick at one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CuratedModel {
    /// File name in the repository and on disk.
    pub filename: &'static str,
    pub engine: Engine,
    pub repo: &'static str,
    /// Pinned commit, so a re-upload can't swap the bytes.
    pub revision: &'static str,
    pub size_bytes: u64,
    /// The file's sha256: its LFS oid on Hugging Face, never the `xetHash` beside it.
    pub sha256: &'static str,
    /// The longest context the model card states.
    pub context_length: u32,
    /// What the household reads as the model's name.
    pub title: &'static str,
    pub summary: &'static str,
    pub quantization: Option<&'static str>,
    /// The picture add-on's `dir` in the pairing table, for a pick that reads pictures with one.
    pub pictures: Option<&'static str>,
}

impl CuratedModel {
    /// The catalogue name: a GGUF's file stem, a LiteRT-LM file's whole name.
    pub fn name(&self) -> &'static str {
        match self.engine {
            Engine::LlamaCpp => self.filename.strip_suffix(".gguf").unwrap_or(self.filename),
            Engine::LiteRtLm | Engine::Ollama | Engine::Llamafile => self.filename,
        }
    }

    pub fn category(&self) -> ModelCategory {
        self.engine.category()
    }

    /// `"{category}/{name}"`.
    pub fn id(&self) -> String {
        ModelRecord::id_for(&self.category(), self.name())
    }

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
}

/// The picks, in the order the catalogue lists them.
pub const CURATED: &[CuratedModel] = &[
    CuratedModel {
        filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
        engine: Engine::LlamaCpp,
        repo: "unsloth/gemma-4-E4B-it-qat-GGUF",
        revision: "8c5a9e4fd5482e2be20fe0bf013b4c262a8f4265",
        size_bytes: 4_215_695_776,
        sha256: "df0fd4ee07072c607c29a0a1cb4f98918426cca12f45a2776bdd6ee6d09a4de3",
        context_length: 131_072,
        title: "Gemma 4 E4B",
        summary: "Gemma 4 E4B Instruct, quantisation-aware 4-bit (~4.2 GB, tool calling + \
                  thinking, reads pictures with an add-on)",
        quantization: Some("UD-Q4_K_XL"),
        pictures: Some("gemma-4-e4b-it-qat"),
    },
    CuratedModel {
        filename: "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf",
        engine: Engine::LlamaCpp,
        repo: "unsloth/gemma-4-E2B-it-qat-GGUF",
        revision: "66a399f68ddd113b06dff02fca9523e55465d11d",
        size_bytes: 2_620_370_976,
        sha256: "e531007218dfab990486a5de7676a6932d6ea8dea233d1f698d7c21cf8a16889",
        context_length: 131_072,
        title: "Gemma 4 E2B",
        summary: "Gemma 4 E2B Instruct, quantisation-aware 4-bit (~2.6 GB, tool calling + \
                  thinking, reads pictures with an add-on)",
        quantization: Some("UD-Q4_K_XL"),
        pictures: Some("gemma-4-e2b-it-qat"),
    },
    CuratedModel {
        filename: "gemma-4-E2B-it.litertlm",
        engine: Engine::LiteRtLm,
        repo: "litert-community/gemma-4-E2B-it-litert-lm",
        revision: "b3ca0d2f076785a8f4b2219ddbd2bdb99954eae1",
        size_bytes: 2_588_147_712,
        sha256: "181938105e0eefd105961417e8da75903eacda102c4fce9ce90f50b97139a63c",
        context_length: 32_768,
        title: "Gemma 4 E2B",
        summary: "Gemma 4 E2B Instruct for LiteRT-LM (~2.6 GB, tool calling + thinking, text only)",
        quantization: None,
        pictures: None,
    },
    CuratedModel {
        filename: "gemma-4-E4B-it.litertlm",
        engine: Engine::LiteRtLm,
        repo: "litert-community/gemma-4-E4B-it-litert-lm",
        revision: "2eee7ac325f20eb8c9ac1d0e972f7c84663062da",
        size_bytes: 3_659_530_240,
        sha256: "0b2a8980ce155fd97673d8e820b4d29d9c7d99b8fa6806f425d969b145bd52e0",
        context_length: 32_768,
        title: "Gemma 4 E4B",
        summary: "Gemma 4 E4B Instruct for LiteRT-LM (~3.7 GB, tool calling + thinking, text only)",
        quantization: None,
        pictures: None,
    },
];

/// What a pinned download must deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilePin {
    pub size_bytes: u64,
    pub sha256: &'static str,
}

/// The pick a Hugging Face file is, when it is one at its pinned revision.
pub fn pinned(repo: &str, revision: &str, filename: &str) -> Option<&'static CuratedModel> {
    CURATED
        .iter()
        .find(|p| p.repo == repo && p.revision == revision && p.filename == filename)
}

/// The pick named `name` in `category`.
pub fn find(category: &ModelCategory, name: &str) -> Option<&'static CuratedModel> {
    CURATED
        .iter()
        .find(|p| &p.category() == category && p.name() == name)
}

/// The pick a catalogue row is, by category and name.
pub fn for_record(record: &ModelRecord) -> Option<&'static CuratedModel> {
    find(&record.category, &record.name)
}

/// The size and hash a Hugging Face file must have: a pick, a drafter, a paired picture add-on, a
/// GGUF embedding model or a pinned Whisper.
pub fn file_pin(repo: &str, revision: &str, filename: &str) -> Option<FilePin> {
    if let Some(pick) = pinned(repo, revision, filename) {
        return Some(FilePin {
            size_bytes: pick.size_bytes,
            sha256: pick.sha256,
        });
    }
    if let Some(embedder) = super::embedding_models::pinned(repo, revision, filename) {
        return Some(embedder.pin());
    }
    if let Some(drafter) = super::drafter::drafter_pin(repo, filename) {
        return Some(FilePin {
            size_bytes: drafter.size_bytes,
            sha256: drafter.sha256,
        });
    }
    if let Some(pin) = super::voice_match_model::pinned(repo, revision, filename) {
        return Some(pin);
    }
    if let Some(pin) = super::whisper_models::pinned(repo, revision, filename) {
        return Some(pin);
    }
    super::vision_pairing::encoder_pinned(repo, revision, filename).map(|spec| FilePin {
        size_bytes: spec.size_bytes,
        sha256: spec.sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str, n: usize) -> bool {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn every_pin_is_complete() {
        for pick in CURATED {
            assert!(hex(pick.revision, 40), "{}: revision", pick.filename);
            assert!(hex(pick.sha256, 64), "{}: sha256", pick.filename);
            assert!(pick.size_bytes > 0, "{}", pick.filename);
            assert!(pick.context_length >= 8192, "{}", pick.filename);
            assert!(
                !pick.title.contains('.'),
                "{}: a title is not a file",
                pick.filename
            );
            let ext = pick.engine.file_format().expect("a pick is a file");
            assert!(pick.filename.ends_with(ext), "{}", pick.filename);
        }
    }

    /// The household's existing assignments name these; a rename would orphan them.
    #[test]
    fn the_names_are_the_ones_ponds_already_assign() {
        let ids: Vec<String> = CURATED.iter().map(CuratedModel::id).collect();
        assert_eq!(
            ids,
            [
                "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL",
                "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL",
                "litert/gemma-4-E2B-it.litertlm",
                "litert/gemma-4-E4B-it.litertlm",
            ]
        );
    }

    #[test]
    fn a_pin_matches_only_its_own_revision_and_file() {
        let pick = &CURATED[0];
        assert_eq!(pinned(pick.repo, pick.revision, pick.filename), Some(pick));
        assert_eq!(pinned(pick.repo, "main", pick.filename), None);
        assert_eq!(pinned(pick.repo, pick.revision, "other.gguf"), None);
        assert_eq!(
            pick.url(),
            format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                pick.repo, pick.revision, pick.filename
            )
        );
        assert_eq!(
            file_pin(pick.repo, pick.revision, pick.filename),
            Some(FilePin {
                size_bytes: pick.size_bytes,
                sha256: pick.sha256
            })
        );
    }

    #[test]
    fn a_pick_is_found_by_its_catalogue_name() {
        let e4b = find(&ModelCategory::Gguf, "gemma-4-E4B-it-qat-UD-Q4_K_XL").unwrap();
        assert_eq!(e4b.engine, Engine::LlamaCpp);
        assert_eq!(e4b.size_mb(), 4020);
        assert!(find(&ModelCategory::Litert, "gemma-4-E4B-it-qat-UD-Q4_K_XL").is_none());
        let litert = find(&ModelCategory::Litert, "gemma-4-E2B-it.litertlm").unwrap();
        assert_eq!(litert.name(), litert.filename);
        assert!(super::super::litert::is_litert_model(litert.name()));
    }

    #[test]
    fn file_pin_resolves_drafters() {
        let pin = file_pin(
            "unsloth/gemma-4-E2B-it-qat-GGUF",
            "main",
            "mtp-gemma-4-E2B-it.gguf",
        )
        .unwrap();
        assert_eq!(pin.size_bytes, 59_235_648);
        assert_eq!(
            pin.sha256,
            "586f2460b909008640981ec34060aa864e03c144fbabfb3173c4335087e4aae0"
        );
    }

    #[test]
    fn only_llama_cpp_picks_name_a_picture_add_on() {
        for pick in CURATED {
            assert_eq!(
                pick.pictures.is_some(),
                pick.engine == Engine::LlamaCpp,
                "{}",
                pick.filename
            );
        }
    }
}
