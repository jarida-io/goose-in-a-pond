//! Whisper models the pond fetches at a pinned revision of `ggerganov/whisper.cpp`, each checked
//! against its size and SHA-256 on the way in.

use super::curated::FilePin;

pub const REPO: &str = "ggerganov/whisper.cpp";
pub const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedWhisper {
    /// The catalogue name the ASR role names.
    pub name: &'static str,
    pub filename: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
    pub language: &'static str,
    /// Whisper's own size class, shown beside the language.
    pub size: &'static str,
    pub quantization: &'static str,
    pub description: &'static str,
}

impl PinnedWhisper {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{REPO}/resolve/{REVISION}/{}",
            self.filename
        )
    }

    /// The catalogue's unit: binary megabytes, as the other Whisper rows count them.
    pub fn size_mb(&self) -> u64 {
        self.size_bytes.div_ceil(1_048_576)
    }
}

pub const PINNED: &[PinnedWhisper] = &[PinnedWhisper {
    name: "small.en-q5_1",
    filename: "ggml-small.en-q5_1.bin",
    size_bytes: 190_098_681,
    sha256: "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30",
    language: "en",
    size: "small",
    quantization: "q5_1",
    description: "Whisper small.en, 5-bit: on the room bench it cut WER in babble from 30% to 14% \
                  against base, for about 117 MB more memory",
}];

pub fn pinned(repo: &str, revision: &str, filename: &str) -> Option<FilePin> {
    if repo != REPO || revision != REVISION {
        return None;
    }
    PINNED
        .iter()
        .find(|p| p.filename == filename)
        .map(|p| FilePin {
            size_bytes: p.size_bytes,
            sha256: p.sha256,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pin_is_complete_and_fetched_at_its_revision() {
        assert_eq!(REVISION.len(), 40);
        for p in PINNED {
            assert_eq!(p.sha256.len(), 64, "{}", p.filename);
            assert!(p.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(p.url().contains(REVISION) && p.url().ends_with(p.filename));
            assert_eq!(
                pinned(REPO, REVISION, p.filename),
                Some(FilePin {
                    size_bytes: p.size_bytes,
                    sha256: p.sha256
                })
            );
            assert_eq!(pinned(REPO, "main", p.filename), None);
        }
        assert_eq!(PINNED[0].size_mb(), 182);
    }

    #[test]
    fn the_hf_cache_finds_the_pin_through_the_shared_lookup() {
        let p = PINNED[0];
        assert!(super::super::curated::file_pin(REPO, REVISION, p.filename).is_some());
    }
}
