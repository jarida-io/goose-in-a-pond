//! Kokoro ONNX session: `input_ids` (i64, `[1, ≤512]`), `style` (f32, `[1, 256]`) and `speed`
//! (f32, `[1]`) in, mono f32 at 24 kHz out. Loaded lazily and unloadable: the pond is mostly idle.

use anyhow::{anyhow, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use pond_core::models::domain::acceleration::{record_onnx_device, OnnxDevice, TTS_KOKORO};
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
    /// Preserves the optimized graph in a disk cache so subsequent loads skip graph optimization.
    pub fn load(model_path: &Path, intra_threads: Option<usize>) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!(
                "Kokoro model not found at {}",
                model_path.display()
            ));
        }

        let cache = Self::cache_path(model_path);
        if Self::is_cache_valid(&cache, model_path) {
            match Self::load_session(&cache, intra_threads, None) {
                Ok(session) => {
                    tracing::info!(
                        path = %cache.display(),
                        threads = ?intra_threads,
                        "Kokoro session loaded from disk cache"
                    );
                    return Ok(Self {
                        session,
                        model_path: model_path.to_path_buf(),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        path = %cache.display(),
                        error = %e,
                        "Kokoro disk cache invalid or incompatible; rebuilding"
                    );
                    let _ = std::fs::remove_file(&cache);
                }
            }
        }

        let session = match Self::load_session(model_path, intra_threads, Some(&cache)) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "loading Kokoro with disk caching failed; falling back to uncached load"
                );
                Self::load_session(model_path, intra_threads, None).with_context(|| {
                    format!("failed to load Kokoro model at {}", model_path.display())
                })?
            }
        };

        tracing::info!(
            path = %model_path.display(),
            cache = %cache.display(),
            threads = ?intra_threads,
            "Kokoro session loaded and cached on disk"
        );
        Ok(Self {
            session,
            model_path: model_path.to_path_buf(),
        })
    }

    pub fn cache_path(model_path: &Path) -> PathBuf {
        let stem = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("model");
        let file_name = format!("{stem}.cache.onnx");
        if let Some(parent) = model_path.parent() {
            parent.join(file_name)
        } else {
            PathBuf::from(file_name)
        }
    }

    pub fn is_cache_valid(cache_path: &Path, model_path: &Path) -> bool {
        let Ok(cache_meta) = cache_path.metadata() else {
            return false;
        };
        if cache_meta.len() == 0 {
            return false;
        }
        let Ok(source_meta) = model_path.metadata() else {
            return false;
        };
        if let (Ok(c_mod), Ok(s_mod)) = (cache_meta.modified(), source_meta.modified()) {
            if s_mod > c_mod {
                return false;
            }
        }
        true
    }

    fn load_session(
        path: &Path,
        intra_threads: Option<usize>,
        save_optimized_to: Option<&Path>,
    ) -> Result<Session> {
        let mut builder = Session::builder().context("failed to create ort session builder")?;
        if let Some(n) = intra_threads {
            builder = builder
                .with_intra_threads(n)
                .map_err(|e| anyhow!("failed to set ort intra-op threads to {n}: {e}"))?;
        }
        builder = builder
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("failed to set ort optimization level: {e}"))?;
        // Each clause is a new input length and ORT keeps a memory pattern per length, so with
        // patterns on the process grew with every new clause the pond spoke.
        builder = builder
            .with_memory_pattern(false)
            .map_err(|e| anyhow!("failed to turn off ort memory patterns: {e}"))?;
        // The CPU even with the Orin's GPU runtime: CUDA synthesised 7x faster but took 650-940 MB
        // beside E4B and halved the language model's decode while both ran. No arena, as ort's
        // default already has it: an arena would hold the longest unit's peak for good.
        let cpu = ort::ep::CPU::default().with_arena_allocator(false).build();
        builder = builder
            .with_execution_providers([cpu])
            .map_err(|e| anyhow!("failed to set ort cpu execution provider: {e}"))?;
        if let Some(cache_out) = save_optimized_to {
            builder = match builder.with_optimized_model_path(cache_out) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "could not set ort optimized model path; proceeding without caching"
                    );
                    e.recover()
                }
            };
        }
        let session = builder.commit_from_file(path).map_err(|e| anyhow!("{e}"))?;
        record_onnx_device(TTS_KOKORO, OnnxDevice::Cpu);
        Ok(session)
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
        run(&mut self.session, chunk.padded(), style, speed)
    }
}

/// One forward pass: `ids` is the padded token sequence, `style` its [`STYLE_DIM`] voice row.
pub(crate) fn run(
    session: &mut Session,
    ids: Vec<i64>,
    style: &[f32],
    speed: f32,
) -> Result<Vec<f32>> {
    if ids.len() > crate::tokenizer::MAX_CONTEXT {
        return Err(anyhow!(
            "Kokoro chunk is {} tokens, over the {} limit — chunking failed upstream",
            ids.len(),
            crate::tokenizer::MAX_CONTEXT
        ));
    }

    // (shape, Vec), not `Array2`: `ort` uses ndarray 0.17, the workspace 0.16.
    let n = ids.len() as i64;
    let outputs = session
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

    #[test]
    fn cache_path_names_matching_cache_file() {
        let p = Path::new("/models/kokoro/model_q4f16.onnx");
        assert_eq!(
            Engine::cache_path(p),
            PathBuf::from("/models/kokoro/model_q4f16.cache.onnx")
        );
    }

    #[test]
    fn cache_validity_checks_existence_and_size() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("model.onnx");
        let cache = tmp.path().join("model.cache.onnx");

        std::fs::write(&src, b"model data").unwrap();
        assert!(!Engine::is_cache_valid(&cache, &src));

        std::fs::write(&cache, b"").unwrap();
        assert!(!Engine::is_cache_valid(&cache, &src));

        std::fs::write(&cache, b"cached data").unwrap();
        assert!(Engine::is_cache_valid(&cache, &src));
    }
}
