//! Kokoro ONNX session: `input_ids` (i64, `[1, ≤512]`), `style` (f32, `[1, 256]`) and `speed`
//! (f32, `[1]`) in, mono f32 at 24 kHz out. Loaded lazily and unloadable: the pond is mostly idle.

use anyhow::{anyhow, Context, Result};
use ort::ep::{self, ArenaExtendStrategy, ExecutionProvider};
use ort::session::{RunOptions, Session};
use ort::value::Tensor;
use pond_core::models::domain::acceleration::{record_onnx_device, OnnxDevice, TTS_KOKORO};
use std::path::{Path, PathBuf};

use crate::tokenizer::Chunk;
use crate::voices::STYLE_DIM;

/// Kokoro's output sample rate, fixed by the vocoder.
pub const SAMPLE_RATE: u32 = 24_000;

/// What CUDA's arena may grow to. On the Orin it comes out of the household's RAM; 1 GiB is over
/// the 650-940 MB the session was measured to take, so it bounds a runaway without starving it.
const GPU_MEM_LIMIT_BYTES: usize = 1 << 30;

/// Where the graph runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Cuda,
}

impl Device {
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Cuda => "cuda",
        }
    }
}

/// How a session is built: where it runs, and the memory levers under trial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineOptions {
    pub device: Device,
    pub intra_threads: Option<usize>,
    /// Allocate from an arena and give it back after every run, so the longest unit's peak is
    /// not kept for good.
    pub shrink_arena: bool,
    /// Read the weights in place from a memory-mapped ORT-format copy, so they are clean page
    /// cache the kernel can drop, not anonymous memory that has to be swapped.
    pub mapped_weights: bool,
    /// Queue CUDA work on a stream of the least priority, behind the language model's.
    pub low_priority_stream: bool,
}

impl EngineOptions {
    pub fn on(device: Device, intra_threads: Option<usize>) -> Self {
        Self {
            device,
            intra_threads,
            shrink_arena: false,
            mapped_weights: false,
            low_priority_stream: false,
        }
    }

    /// The memory levers named in `POND_KOKORO_TRIAL` (`shrink`, `mapped`, `lowprio`), for the
    /// measurements that will choose their defaults.
    pub fn with_trial(mut self, trial: Option<&str>) -> Self {
        for lever in trial.unwrap_or_default().split(',').map(str::trim) {
            match lever {
                "shrink" => self.shrink_arena = true,
                "mapped" => self.mapped_weights = true,
                "lowprio" => self.low_priority_stream = true,
                _ => {}
            }
        }
        self
    }
}

/// Whether the loaded ONNX Runtime can run CUDA at all.
pub fn cuda_runtime_available() -> bool {
    ep::CUDA::default().is_available().unwrap_or(false)
}

/// A loaded Kokoro graph. Fields drop in order: the session before what it reads.
pub struct Engine {
    session: Session,
    run_options: Option<RunOptions>,
    _stream: Option<crate::cuda::Stream>,
    _mapped: Option<memmap2::Mmap>,
    model_path: PathBuf,
    options: EngineOptions,
}

impl Engine {
    /// Load the model; `intra_threads` caps ORT's pool, since the Jetson's LLM shares its cores.
    /// The CPU's optimized graph is kept in a disk cache so later loads skip graph optimization.
    pub fn load(model_path: &Path, intra_threads: Option<usize>) -> Result<Self> {
        Self::load_with(model_path, EngineOptions::on(Device::Cpu, intra_threads))
    }

    pub fn load_with(model_path: &Path, options: EngineOptions) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!(
                "Kokoro model not found at {}",
                model_path.display()
            ));
        }
        let run_options = options
            .shrink_arena
            .then(|| -> Result<RunOptions> {
                let mut run = RunOptions::new()?;
                let arenas = match options.device {
                    Device::Cpu => "cpu:0",
                    Device::Cuda => "cpu:0;gpu:0",
                };
                run.add_config_entry("memory.enable_memory_arena_shrinkage", arenas)?;
                Ok(run)
            })
            .transpose()?;
        let (session, mapped, stream) = match options.device {
            Device::Cpu if options.mapped_weights => {
                let (session, mapped) = Self::load_mapped(model_path, &options)?;
                (session, Some(mapped), None)
            }
            Device::Cpu => (Self::load_cpu_cached(model_path, &options)?, None, None),
            Device::Cuda => {
                let stream = if options.low_priority_stream {
                    match crate::cuda::least_priority_stream() {
                        Ok(stream) => Some(stream),
                        Err(e) => {
                            tracing::warn!("no low-priority CUDA stream for Kokoro: {e:#}");
                            None
                        }
                    }
                } else {
                    None
                };
                let builder = Self::builder(&options)?;
                let session = Self::commit_cuda(builder, model_path, stream.as_ref())?;
                (session, None, stream)
            }
        };
        record_onnx_device(
            TTS_KOKORO,
            match options.device {
                Device::Cpu => OnnxDevice::Cpu,
                Device::Cuda => OnnxDevice::Cuda,
            },
        );
        tracing::info!(
            path = %model_path.display(),
            device = options.device.as_str(),
            threads = ?options.intra_threads,
            shrink_arena = options.shrink_arena,
            mapped_weights = options.mapped_weights,
            low_priority_stream = stream.is_some(),
            "Kokoro session loaded"
        );
        Ok(Self {
            session,
            run_options,
            _stream: stream,
            _mapped: mapped,
            model_path: model_path.to_path_buf(),
            options,
        })
    }

    pub fn device(&self) -> Device {
        self.options.device
    }

    pub fn options(&self) -> &EngineOptions {
        &self.options
    }

    fn builder(options: &EngineOptions) -> Result<ort::session::builder::SessionBuilder> {
        let mut builder = Session::builder().context("failed to create ort session builder")?;
        if let Some(n) = options.intra_threads {
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
        Ok(builder)
    }

    fn cpu_provider(options: &EngineOptions) -> ep::ExecutionProviderDispatch {
        // An arena keeps the longest unit's peak for good, so it is on only to be shrunk.
        ep::CPU::default()
            .with_arena_allocator(options.shrink_arena)
            .build()
    }

    fn commit_cpu(
        builder: ort::session::builder::SessionBuilder,
        path: &Path,
        options: &EngineOptions,
        save_optimized_to: Option<(&Path, bool)>,
    ) -> Result<Session> {
        let mut builder = builder
            .with_execution_providers([Self::cpu_provider(options)])
            .map_err(|e| anyhow!("failed to set ort cpu execution provider: {e}"))?;
        if let Some((cache_out, as_ort)) = save_optimized_to {
            if as_ort {
                builder = builder
                    .with_config_entry("session.save_model_format", "ORT")
                    .map_err(|e| anyhow!("{e}"))?;
            }
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
        builder.commit_from_file(path).map_err(|e| anyhow!("{e}"))
    }

    /// The CPU session from the optimized ONNX cache, writing it first when stale.
    fn load_cpu_cached(model_path: &Path, options: &EngineOptions) -> Result<Session> {
        let cache = Self::cache_path(model_path);
        if Self::is_cache_valid(&cache, model_path) {
            match Self::commit_cpu(Self::builder(options)?, &cache, options, None) {
                Ok(session) => return Ok(session),
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
        match Self::commit_cpu(
            Self::builder(options)?,
            model_path,
            options,
            Some((&cache, false)),
        ) {
            Ok(s) => Ok(s),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "loading Kokoro with disk caching failed; falling back to uncached load"
                );
                Self::commit_cpu(Self::builder(options)?, model_path, options, None).with_context(
                    || format!("failed to load Kokoro model at {}", model_path.display()),
                )
            }
        }
    }

    /// The CPU session read in place from a memory-mapped ORT-format copy, made first if stale.
    fn load_mapped(model_path: &Path, options: &EngineOptions) -> Result<(Session, memmap2::Mmap)> {
        let ort_copy = Self::ort_cache_path(model_path);
        if !Self::is_cache_valid(&ort_copy, model_path) {
            let _ = std::fs::remove_file(&ort_copy);
            drop(Self::commit_cpu(
                Self::builder(options)?,
                model_path,
                options,
                Some((&ort_copy, true)),
            )?);
        }
        let file = std::fs::File::open(&ort_copy)
            .with_context(|| format!("failed to open {}", ort_copy.display()))?;
        // SAFETY: the copy is the pond's own cache; nothing writes it while a session maps it.
        let mapped = unsafe { memmap2::Mmap::map(&file) }
            .with_context(|| format!("failed to map {}", ort_copy.display()))?;
        let session = Self::builder(options)?
            .with_config_entry("session.use_ort_model_bytes_directly", "1")
            .map_err(|e| anyhow!("{e}"))?
            .with_config_entry("session.use_ort_model_bytes_for_initializers", "1")
            .map_err(|e| anyhow!("{e}"))?
            .with_execution_providers([Self::cpu_provider(options)])
            .map_err(|e| anyhow!("failed to set ort cpu execution provider: {e}"))?
            .commit_from_memory(&mapped)
            .map_err(|e| anyhow!("{e}"))?;
        Ok((session, mapped))
    }

    fn commit_cuda(
        builder: ort::session::builder::SessionBuilder,
        path: &Path,
        stream: Option<&crate::cuda::Stream>,
    ) -> Result<Session> {
        let mut cuda = ep::CUDA::default()
            // Exhaustive search benchmarks every convolution at each new input length: the
            // 0.6-1.6 s first run of each new chunk length on the Orin.
            .with_conv_algorithm_search(ep::cuda::ConvAlgorithmSearch::Heuristic)
            // Power-of-two growth over-allocates out of the household's RAM.
            .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
            .with_memory_limit(GPU_MEM_LIMIT_BYTES);
        if let Some(stream) = stream {
            // SAFETY: the stream outlives the session, which `Engine`'s field order guarantees.
            cuda = unsafe { cuda.with_compute_stream(stream.as_ptr()) };
        }
        builder
            .with_execution_providers([cuda.build().error_on_failure(), ep::CPU::default().build()])
            .map_err(|e| anyhow!("failed to set ort cuda execution provider: {e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn cache_path(model_path: &Path) -> PathBuf {
        Self::sibling(model_path, "cache.onnx")
    }

    /// The ORT-format copy the mapped load reads.
    pub fn ort_cache_path(model_path: &Path) -> PathBuf {
        Self::sibling(model_path, "cache.ort")
    }

    fn sibling(model_path: &Path, suffix: &str) -> PathBuf {
        let stem = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("model");
        let file_name = format!("{stem}.{suffix}");
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
        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array((vec![1_i64, n], ids))?,
            "style" => Tensor::from_array((vec![1_i64, STYLE_DIM as i64], style.to_vec()))?,
            "speed" => Tensor::from_array((vec![1_i64], vec![speed]))?,
        ];
        let outputs = match &self.run_options {
            Some(run) => self.session.run_with_options(inputs, run),
            None => self.session.run(inputs),
        }
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
