//! The wake word: a depthwise-separable CNN over the last 1.2 s of the same fbank frames Voice
//! Match uses, every 80 ms. "hey goose" ships inside the crate; provenance in models/hey-goose.md.

pub mod detector;
pub mod personal;

use crate::convert::ModelFile;
use crate::engine::fbank::{self, NUM_BINS};
use crate::Error;

pub const BINS: usize = NUM_BINS;
/// Frames the detector sees: 1.2 s at 10 ms.
pub const WINDOW_FRAMES: usize = 120;
/// Samples that yield exactly `WINDOW_FRAMES` frames.
pub const WINDOW_SAMPLES: usize = fbank::FRAME + (WINDOW_FRAMES - 1) * fbank::SHIFT;
/// The detector re-runs every 8 frames (80 ms).
pub const HOP_FRAMES: usize = 8;

/// The "hey goose" detector, trained on Kokoro voices only: the ONNX is the shipped artifact,
/// converted when loaded.
pub const HEY_GOOSE_ONNX: &[u8] = include_bytes!("../../models/hey-goose.onnx");
pub const HEY_GOOSE_SHA256: &str =
    "32ae17038863a9b9829524afc2dea0e1aa8a8e5d393934f672f402239f10f699";
pub const HEY_GOOSE_PHRASE: &str = "hey goose";
/// Streamed as the ear runs it, the highest validation recall with at most one false accept per
/// hour of keyword-free speech (models/hey-goose.md).
pub const HEY_GOOSE_THRESHOLD: f32 = 0.97;

/// The embedded "hey goose" model, converted for the engine (about 10 ms).
pub fn hey_goose() -> Result<ModelFile, Error> {
    ModelFile::from_onnx(HEY_GOOSE_ONNX)
}

/// Per-window CMN makes the detector indifferent to microphone gain and channel colour.
pub fn window_cmn(feats: &mut [f32]) {
    fbank::cmn(feats, feats.len() / BINS);
}

/// The detector's input for one window of audio: fbank, then window CMN, exactly as trained.
pub fn window_features(audio: &[f32]) -> Vec<f32> {
    let n = fbank::num_frames(audio.len()).min(WINDOW_FRAMES);
    let mut f = vec![0f32; WINDOW_FRAMES * BINS];
    for t in 0..n {
        fbank::frame(
            &fbank::WESPEAKER,
            &audio[t * fbank::SHIFT..],
            &mut f[t * BINS..(t + 1) * BINS],
        );
    }
    window_cmn(&mut f);
    f
}

pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_exactly_the_frames_asked_for() {
        assert_eq!(fbank::num_frames(WINDOW_SAMPLES), WINDOW_FRAMES);
    }

    #[test]
    fn the_embedded_model_is_the_one_recorded_and_converts() {
        let sha = crate::convert::sha256::hex(&crate::convert::sha256::sha256(HEY_GOOSE_ONNX));
        assert_eq!(sha, HEY_GOOSE_SHA256);
        let m = hey_goose().unwrap();
        let model = m.model();
        assert_eq!(model.arch, crate::engine::model::Arch::Kws);
        assert_eq!(model.emb_dim, 1);
        assert_eq!(
            crate::convert::sha256::hex(&m.source_sha256()),
            HEY_GOOSE_SHA256
        );
    }

    #[test]
    fn the_converter_writes_the_labs_bytes_exactly() {
        // The lab converter's .vmw of this ONNX; a change here must bump `vmw::CONVERTER`.
        let vmw = crate::convert::sha256::sha256(hey_goose().unwrap().bytes());
        assert_eq!(
            crate::convert::sha256::hex(&vmw),
            "0f8a18c512d40ce2ce4792c2203fb5b3e0cfb542ac761fb4cb1c3a27bc584e17"
        );
    }
}
