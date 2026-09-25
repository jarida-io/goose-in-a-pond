//! Kokoro ONNX session: `input_ids` (i64, `[1, ≤512]`), `style` (f32, `[1, 256]`) and `speed`
//! (f32, `[1]`) in, mono f32 at 24 kHz out. Loaded lazily and unloadable: the pond is mostly idle.

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};

use crate::tokenizer::Chunk;
use crate::voices::STYLE_DIM;

/// Kokoro's output sample rate, fixed by the vocoder.
pub const SAMPLE_RATE: u32 = 24_000;

/// A loaded Kokoro graph.
pub struct Engine {
    session: Session,
    model_path: PathBuf,
}

impl Engine {
    /// Load the model; `intra_threads` caps ORT's pool, since the Jetson's LLM shares its cores.
    pub fn load(model_path: &Path, intra_threads: Option<usize>) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!(
                "Kokoro model not found at {}",
                model_path.display()
            ));
        }
        let mut builder = Session::builder().context("failed to create ort session builder")?;
        if let Some(n) = intra_threads {
            // ort's error here wraps the builder and isn't `std::error::Error`, so no `.context()`.
            builder = builder
                .with_intra_threads(n)
                .map_err(|e| anyhow!("failed to set ort intra-op threads to {n}: {e}"))?;
        }
        let session = builder
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load Kokoro model at {}", model_path.display()))?;

        tracing::info!(
            path = %model_path.display(),
            threads = ?intra_threads,
            "Kokoro session loaded"
        );
        Ok(Self {
            session,
            model_path: model_path.to_path_buf(),
        })
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Run one chunk; `style` must be [`STYLE_DIM`] long, `speed` 1.0 = as trained.
    pub fn synthesize(&mut self, chunk: &Chunk, style: &[f32], speed: f32) -> Result<Vec<f32>> {
        if style.len() != STYLE_DIM {
            return Err(anyhow!(
                "Kokoro style vector is {} wide, expected {STYLE_DIM}",
                style.len()
            ));
        }
        let ids = chunk.padded();
        if ids.len() > crate::tokenizer::MAX_CONTEXT {
            return Err(anyhow!(
                "Kokoro chunk is {} tokens, over the {} limit — chunking failed upstream",
                ids.len(),
                crate::tokenizer::MAX_CONTEXT
            ));
        }

        // (shape, Vec), not `Array2`: `ort` uses ndarray 0.17, the workspace 0.16.
        let n = ids.len() as i64;
        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => Tensor::from_array((vec![1_i64, n], ids))?,
                "style" => Tensor::from_array((vec![1_i64, STYLE_DIM as i64], style.to_vec()))?,
                "speed" => Tensor::from_array((vec![1_i64], vec![speed]))?,
            ])
            .context("Kokoro inference failed")?;

        let (_name, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow!("Kokoro produced no output tensor"))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .context("failed to extract Kokoro waveform")?;

        Ok(data.to_vec())
    }
}

/// Duration in seconds of a mono f32 buffer at [`SAMPLE_RATE`].
pub fn duration_secs(samples: &[f32]) -> f32 {
    samples.len() as f32 / SAMPLE_RATE as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Session` is not `Debug`, so match rather than `unwrap_err`.
    #[test]
    fn missing_model_is_an_error_not_a_panic() {
        match Engine::load(Path::new("/nope/kokoro.onnx"), None) {
            Ok(_) => panic!("loaded a model that does not exist"),
            Err(e) => assert!(e.to_string().contains("not found"), "{e}"),
        }
    }

    #[test]
    fn duration_is_sample_count_over_rate() {
        assert_eq!(duration_secs(&vec![0.0; SAMPLE_RATE as usize]), 1.0);
        assert_eq!(duration_secs(&[]), 0.0);
    }
}
