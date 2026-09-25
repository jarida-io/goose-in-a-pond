//! Silero VAD as a [`SpeechDetector`], for end-of-speech: steady noise that an RMS gate calls
//! speech scores ~0.08 here, speech ~0.95. It needs exact, non-overlapping 512-sample windows
//! ([`Windower`]) with its LSTM state carried; the first windows after a reset score low, so
//! onset stays on the energy gate.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ndarray::Array;
use ort::session::Session;
use ort::value::Tensor;
use pond_voice::dsp::{SpeechDetector, Windower};

/// Samples per inference. Fixed by the model.
pub const WINDOW: usize = 512;
/// The model is trained at 16 kHz and the capture path already runs there.
pub const SAMPLE_RATE: i64 = 16_000;
/// Shape of the recurrent state the model threads between windows.
const STATE_SHAPE: (usize, usize, usize) = (2, 1, 128);

/// Speech probability threshold: Silero's default; not a knob (speech ~0.945, noise < 0.09).
pub const DEFAULT_THRESHOLD: f32 = 0.5;

pub struct SileroDetector {
    session: Session,
    windower: Windower,
    state: Array<f32, ndarray::Ix3>,
    threshold: f32,
    /// Last verdict, repeated when a short read completes no window; a silence default would
    /// inject spurious silence and end utterances early.
    last: bool,
}

impl SileroDetector {
    /// Load the model; a missing file is an error so the caller can fall back to the energy gate.
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        Self::with_threshold(model_path, DEFAULT_THRESHOLD)
    }

    pub fn with_threshold(model_path: impl AsRef<Path>, threshold: f32) -> Result<Self> {
        let path: PathBuf = model_path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(anyhow!("silero VAD model not found at {}", path.display()));
        }
        let session = Session::builder()
            .context("failed to create ort session builder")?
            // One thread: ~0.5 ms per window on a Jetson, whose cores the LLM needs.
            .with_intra_threads(1)
            // ort builder errors carry the builder, which `context` can't take; flatten them.
            .map_err(|e| anyhow!("failed to pin ort intra-op threads: {e}"))?
            .commit_from_file(&path)
            .with_context(|| format!("failed to load silero VAD model at {}", path.display()))?;

        tracing::info!(path = %path.display(), threshold, "silero VAD loaded");
        Ok(Self {
            session,
            windower: Windower::new(WINDOW),
            state: Array::zeros(STATE_SHAPE),
            threshold,
            last: false,
        })
    }

    /// Run one 512-sample window, advancing the recurrent state.
    fn infer(&mut self, window: &[f32]) -> Result<f32> {
        let input = Array::from_shape_vec((1, WINDOW), window.to_vec())
            .context("silero: window was not 512 samples")?;
        let sr = Array::from_shape_vec((), vec![SAMPLE_RATE]).expect("scalar shape is valid");

        let outputs = self
            .session
            .run(ort::inputs![
                "input" => Tensor::from_array(input)?,
                "state" => Tensor::from_array(self.state.clone())?,
                "sr"    => Tensor::from_array(sr)?,
            ])
            .context("silero VAD inference failed")?;

        // Carry the state forward: dropping it silently degrades the model to per-frame.
        let (shape, next) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .context("silero: could not read the recurrent state back")?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        self.state = Array::from_shape_vec(ndarray::IxDyn(&dims), next.to_vec())
            .context("silero: returned state had an unexpected shape")?
            .into_dimensionality::<ndarray::Ix3>()
            .context("silero: returned state was not 3-dimensional")?;

        let (_, prob) = outputs["output"]
            .try_extract_tensor::<f32>()
            .context("silero: could not read the speech probability")?;
        prob.first()
            .copied()
            .ok_or_else(|| anyhow!("silero: empty probability output"))
    }
}

impl SpeechDetector for SileroDetector {
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        // Inference failure counts as speech: a dead detector must not cut an utterance short;
        // it degrades to the hard recording cap instead.
        let mut latest = None;
        let mut failure = None;

        // Collected first, as the closure can't also borrow `session` and `state`.
        let mut windows: Vec<[f32; WINDOW]> = Vec::new();
        self.windower.push(frame, |w| {
            let mut owned = [0.0f32; WINDOW];
            owned.copy_from_slice(w);
            windows.push(owned);
        });

        for window in &windows {
            match self.infer(window) {
                Ok(p) => latest = Some(p >= self.threshold),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = failure {
            tracing::warn!(error = %e, "silero VAD failed; treating as speech so the turn is not cut short");
            self.last = true;
            return true;
        }

        if let Some(verdict) = latest {
            self.last = verdict;
        }
        self.last
    }

    fn reset(&mut self) {
        // All three, or the next utterance inherits this one's state, leftover samples or verdict.
        self.windower.reset();
        self.state = Array::zeros(STATE_SHAPE);
        self.last = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model is not in the repo; tests needing it are `#[ignore]`d and read `SILERO_MODEL`.
    fn model_path() -> Option<PathBuf> {
        std::env::var("SILERO_MODEL").ok().map(PathBuf::from)
    }

    #[test]
    fn a_missing_model_is_an_error_not_a_panic() {
        // `.map(drop)`: `SileroDetector` holds an ort `Session`, which is not `Debug`.
        let err = SileroDetector::new("/nonexistent/silero.onnx")
            .map(drop)
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    /// The speech half is in `tests/against_real_audio.rs`.
    #[test]
    #[ignore = "needs the model; set SILERO_MODEL"]
    fn steady_noise_and_silence_both_read_as_silence() {
        let Some(path) = model_path() else {
            panic!("set SILERO_MODEL to onnx/model.onnx from onnx-community/silero-vad")
        };
        let mut d = SileroDetector::new(&path).expect("load");

        // Noise at 0.01 amplitude: twice the RMS gate's threshold, which calls it speech.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..WINDOW * 20)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.01
            })
            .collect();
        assert!(
            !d.is_speech(&noise),
            "steady noise at twice the RMS threshold must not read as speech"
        );

        d.reset();
        // Digital silence, unambiguously.
        assert!(!d.is_speech(&vec![0.0; WINDOW * 10]));
    }

    #[test]
    #[ignore = "needs the model; set SILERO_MODEL"]
    fn a_short_read_holds_the_previous_verdict() {
        let Some(path) = model_path() else { return };
        let mut d = SileroDetector::new(&path).expect("load");
        // Under 512 samples completes no window, so the last verdict must be repeated.
        let before = d.is_speech(&vec![0.0; 100]);
        assert!(!before, "the initial verdict is silence");
        assert_eq!(d.is_speech(&vec![0.0; 100]), before);
    }
}
