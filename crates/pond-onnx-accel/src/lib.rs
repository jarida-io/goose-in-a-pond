//! ONNX sessions that serve on the CPU from the first call and move to the Jetson's GPU (TensorRT,
//! then CUDA) once their engine is ready, so an engine build never holds up an event. A reply
//! waits only if it starts while a session is being set up (see `gpu_gate`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use ort::ep::{self, ArenaExtendStrategy, ExecutionProviderDispatch};
use ort::session::Session;
use ort::value::Tensor;
use pond_core::models::domain::acceleration::{record_onnx_device, OnnxDevice};
use pond_core::models::domain::gpu_gate::gpu_gate;

/// TensorRT's default workspace is the whole device, which on unified memory is the household's RAM.
const WORKSPACE_BYTES: usize = 256 * 1024 * 1024;
/// How often a deferred engine build looks at free memory again.
const RECHECK: Duration = Duration::from_secs(60);
/// Each model logs its inference times after this many runs, or this long, whichever comes first.
const REPORT_EVERY: Duration = Duration::from_secs(600);
const REPORT_RUNS: u64 = 50;
const RECENT_RUNS: usize = 256;
const STAMP_FILE: &str = "source.stamp";
const CONTEXT_MODEL: &str = "model_ctx.onnx";

/// Where engines are cached, and when one may be built.
#[derive(Debug, Clone)]
pub struct GpuPlan {
    /// One directory per runtime build: an engine is valid only for the TensorRT that made it.
    pub cache_dir: PathBuf,
    /// Building an engine waits for this much available memory.
    pub build_headroom_mb: u64,
    /// Loading a cached engine waits for this much, so the language model keeps room to grow.
    pub load_headroom_mb: u64,
}

static GPU: OnceLock<GpuPlan> = OnceLock::new();
// Sessions loaded before the GPU was enabled, so enabling it moves them too.
static LOADED: Mutex<Vec<Weak<Shared>>> = Mutex::new(Vec::new());

/// Moves every session loaded so far, and each one loaded later, to the GPU when the runtime
/// has TensorRT. The caller picks the moment: after the language model has taken its GPU memory.
pub fn enable_gpu(plan: GpuPlan) {
    if GPU.set(plan).is_err() {
        return;
    }
    if !tensorrt_in_runtime() {
        tracing::info!(
            "onnx: the loaded ONNX Runtime has no TensorRT provider; models stay on the cpu"
        );
        return;
    }
    let loaded: Vec<_> = lock(&LOADED).iter().filter_map(Weak::upgrade).collect();
    tracing::info!(
        models = loaded.len(),
        "onnx: moving models to the GPU, one at a time"
    );
    for shared in loaded {
        queue(AcceleratedSession(shared));
    }
}

/// The model's name for logs, status and its engine directory, and the input the pond feeds it.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub name: &'static str,
    /// Shape of the single float input; the warm-up run builds the engine for exactly this.
    pub input_shape: Vec<i64>,
    /// Whether the GPU is worth its memory for this model; a model that is not stays on the CPU.
    pub gpu: bool,
}

/// A model's session, on whichever device it has reached. Clones share it.
#[derive(Clone)]
pub struct AcceleratedSession(Arc<Shared>);

struct Shared {
    spec: ModelSpec,
    path: PathBuf,
    current: Mutex<Arc<Mutex<Session>>>,
    device: Mutex<OnnxDevice>,
    stats: Mutex<Stats>,
    queued: AtomicBool,
}

struct Stats {
    recent: Vec<u32>,
    next: usize,
    runs: u64,
    since: Instant,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AcceleratedSession {
    /// Loads the model on the CPU; it moves to the GPU once [`enable_gpu`] has been called.
    pub fn load(path: impl Into<PathBuf>, spec: ModelSpec) -> Result<Self> {
        let path = path.into();
        let session = cpu_session(&path)?;
        record_onnx_device(spec.name, OnnxDevice::Cpu);
        let name = spec.name;
        let this = Self(Arc::new(Shared {
            spec,
            path,
            current: Mutex::new(Arc::new(Mutex::new(session))),
            device: Mutex::new(OnnxDevice::Cpu),
            stats: Mutex::new(Stats {
                recent: Vec::with_capacity(RECENT_RUNS),
                next: 0,
                runs: 0,
                since: Instant::now(),
            }),
            queued: AtomicBool::new(false),
        }));
        tracing::info!(model = name, "onnx: {name} loaded on the cpu");
        if this.moves_to_gpu() {
            lock(&LOADED).push(Arc::downgrade(&this.0));
            if GPU.get().is_some() && tensorrt_in_runtime() {
                queue(this.clone());
            }
        }
        Ok(this)
    }

    pub fn name(&self) -> &'static str {
        self.0.spec.name
    }

    pub fn moves_to_gpu(&self) -> bool {
        self.0.spec.gpu
    }

    pub fn device(&self) -> OnnxDevice {
        *lock(&self.0.device)
    }

    /// Runs `f` on the current session. A GPU failure moves the model back to the CPU for the
    /// calls after this one, so one fault never costs every later event.
    pub fn run<R>(&self, f: impl FnOnce(&mut Session) -> Result<R>) -> Result<R> {
        let started = Instant::now();
        let current = Arc::clone(&lock(&self.0.current));
        let result = f(&mut lock(&current));
        let device = self.device();
        match &result {
            Ok(_) => self.note(started.elapsed(), device),
            Err(e) if device != OnnxDevice::Cpu => {
                tracing::warn!(model = self.name(), error = %format!("{e:#}"), "onnx: {} failed on {}; back to the cpu", self.name(), device.as_str());
                match cpu_session(&self.0.path) {
                    Ok(cpu) => self.swap(cpu, OnnxDevice::Cpu),
                    Err(e) => {
                        tracing::warn!(model = self.name(), error = %format!("{e:#}"), "onnx: cpu session failed too")
                    }
                }
            }
            Err(_) => {}
        }
        result
    }

    /// Builds or loads the GPU session and switches to it; the CPU session serves meanwhile.
    pub fn move_to_gpu(&self, plan: &GpuPlan) -> Result<OnnxDevice> {
        let name = self.name();
        let waiting = Instant::now();
        let _gpu = gpu_gate().setup_blocking();
        let waited = waiting.elapsed();
        if waited >= Duration::from_millis(10) {
            tracing::info!(
                model = name,
                waited_ms = waited.as_millis() as u64,
                "onnx: {name} waited {:.1} s for in-process decoding to finish before its GPU setup",
                waited.as_secs_f64()
            );
        }
        let engine_dir = plan.cache_dir.join(name);
        let cached = engine_cached(&engine_dir, &self.0.path);
        let may_build = has_room(mem_available_mb(), plan.build_headroom_mb);
        let started = Instant::now();
        let (session, device, first) = gpu_session(
            &self.0.path,
            &engine_dir,
            &plan.cache_dir,
            &self.0.spec,
            may_build,
        )?;
        let ready = started.elapsed();
        self.swap(session, device);
        tracing::info!(
            model = name,
            device = device.as_str(),
            engine = if cached { "cached" } else { "built" },
            ready_ms = ready.as_millis() as u64,
            one_run_ms = first.as_secs_f64() * 1000.0,
            "onnx: {name} moved to {} ({} in {:.1} s; one run {:.1} ms)",
            device.as_str(),
            if cached {
                "cached engine loaded"
            } else {
                "engine built"
            },
            ready.as_secs_f64(),
            first.as_secs_f64() * 1000.0,
        );
        Ok(device)
    }

    /// Whether this model's engine for `plan` is cached, so moving it costs no build.
    pub fn engine_cached(&self, plan: &GpuPlan) -> bool {
        engine_cached(&plan.cache_dir.join(self.name()), &self.0.path)
    }

    fn swap(&self, session: Session, device: OnnxDevice) {
        *lock(&self.0.current) = Arc::new(Mutex::new(session));
        *lock(&self.0.device) = device;
        record_onnx_device(self.name(), device);
        let mut s = lock(&self.0.stats);
        s.recent.clear();
        s.next = 0;
        s.runs = 0;
        s.since = Instant::now();
    }

    fn note(&self, took: Duration, device: OnnxDevice) {
        let micros = took.as_micros().min(u32::MAX as u128) as u32;
        tracing::debug!(
            model = self.name(),
            device = device.as_str(),
            us = micros,
            "onnx run"
        );
        let mut s = lock(&self.0.stats);
        if s.recent.len() < RECENT_RUNS {
            s.recent.push(micros);
        } else {
            let i = s.next;
            s.recent[i] = micros;
        }
        s.next = (s.next + 1) % RECENT_RUNS;
        s.runs += 1;
        if s.runs < REPORT_RUNS && s.since.elapsed() < REPORT_EVERY {
            return;
        }
        let mut sorted = s.recent.clone();
        sorted.sort_unstable();
        let at = |q: usize| sorted[(sorted.len() - 1) * q / 100] as f64 / 1000.0;
        tracing::info!(
            model = self.name(),
            device = device.as_str(),
            runs = s.runs,
            median_ms = at(50),
            p95_ms = at(95),
            "onnx: {} on {}: {} runs, median {:.1} ms, p95 {:.1} ms",
            self.name(),
            device.as_str(),
            s.runs,
            at(50),
            at(95),
        );
        s.runs = 0;
        s.since = Instant::now();
    }
}

/// Capped CPU thread pool so fallback sessions do not spawn one thread per core per model.
fn cpu_session(path: &Path) -> Result<Session> {
    if !path.exists() {
        return Err(anyhow!("ONNX model not found at {}", path.display()));
    }
    Session::builder()
        .context("failed to create ort session builder; is the ONNX Runtime library available?")?
        .with_intra_threads(2)
        .map_err(|e| anyhow!("{e}"))?
        .with_inter_threads(1)
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(path)
        .with_context(|| format!("failed to load ONNX model at {}", path.display()))
}

fn tensorrt_in_runtime() -> bool {
    use ort::ep::ExecutionProvider;
    ep::TensorRT::default().is_available().unwrap_or(false)
}

fn cuda() -> ExecutionProviderDispatch {
    ep::CUDA::default()
        // Power-of-two arena growth over-allocates out of the household's RAM.
        .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
        .build()
        .error_on_failure()
}

fn commit(path: &Path, eps: &[ExecutionProviderDispatch]) -> Result<Session> {
    Session::builder()
        .context("failed to create ort session builder")?
        // The GPU does the work; a CPU pool would only hold threads for nodes it never gets.
        .with_intra_threads(1)
        .map_err(|e| anyhow!("{e}"))?
        .with_inter_threads(1)
        .map_err(|e| anyhow!("{e}"))?
        .with_execution_providers(eps)
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(path)
        .map_err(|e| anyhow!("{e}"))
}

/// Runs zeros of the pond's input shape to build or initialize the engine, then times one run.
fn warm_up(session: &mut Session, shape: &[i64], cached: bool) -> Result<Duration> {
    let n = shape.iter().product::<i64>().max(0) as usize;
    let input = session
        .inputs()
        .first()
        .ok_or_else(|| anyhow!("the model has no inputs"))?
        .name()
        .to_string();
    let zeros = vec![0f32; n];
    let untimed_runs = if cached { 1 } else { 2 };
    for _ in 0..untimed_runs {
        let tensor = Tensor::from_array((shape.to_vec(), zeros.clone()))?;
        session.run(ort::inputs![input.as_str() => tensor])?;
    }
    let started = Instant::now();
    let tensor = Tensor::from_array((shape.to_vec(), zeros))?;
    session.run(ort::inputs![input.as_str() => tensor])?;
    Ok(started.elapsed())
}

/// TensorRT from the model's EP-context model, built first when there is none, else CUDA alone.
/// The context model holds only the engine: a session on the ONNX model also keeps its weights and
/// TensorRT's parse of them resident, which for AdaFace measured 1.1 GB against 0.26 GB on the Orin.
fn gpu_session(
    path: &Path,
    engine_dir: &Path,
    timing_dir: &Path,
    spec: &ModelSpec,
    may_build: bool,
) -> Result<(Session, OnnxDevice, Duration)> {
    let context = engine_dir.join(CONTEXT_MODEL);
    if engine_cached(engine_dir, path) {
        match load_context(&context, spec) {
            Ok((s, first)) => return Ok((s, OnnxDevice::TensorRt, first)),
            // A rebuild deletes the cache first, and without the room it fails or starves the
            // language model; keep the engine for the next start.
            Err(e) if !may_build => {
                return Err(e.context(format!(
                    "the cached engine for {} does not load; it is kept, since building it again \
                     needs more free memory than there is",
                    spec.name
                )))
            }
            Err(e) => {
                tracing::warn!(model = spec.name, error = %format!("{e:#}"), "onnx: the cached engine for {} does not load; building it again", spec.name)
            }
        }
    }
    let tensorrt = build_context(path, engine_dir, timing_dir, spec).and_then(|()| {
        write_stamp(engine_dir, path);
        load_context(&context, spec)
    });
    match tensorrt {
        Ok((s, first)) => return Ok((s, OnnxDevice::TensorRt, first)),
        Err(e) => {
            tracing::warn!(model = spec.name, error = %format!("{e:#}"), "onnx: TensorRT failed for {}; trying CUDA", spec.name)
        }
    }
    let mut s = commit(path, &[cuda()])?;
    let first = warm_up(&mut s, &spec.input_shape, false)?;
    Ok((s, OnnxDevice::Cuda, first))
}

/// Builds the engine (fp16, shared timing cache) and has TensorRT write the context model beside it.
fn build_context(
    path: &Path,
    engine_dir: &Path,
    timing_dir: &Path,
    spec: &ModelSpec,
) -> Result<()> {
    // TensorRT writes a context model only when it builds; an engine left here would be loaded.
    let _ = std::fs::remove_dir_all(engine_dir);
    std::fs::create_dir_all(engine_dir)
        .with_context(|| format!("cannot create {}", engine_dir.display()))?;
    let context = engine_dir.join(CONTEXT_MODEL);
    let trt = ep::TensorRT::default()
        .with_fp16(true)
        .with_max_workspace_size(WORKSPACE_BYTES)
        .with_timing_cache(true)
        .with_timing_cache_path(timing_dir.display())
        .with_engine_cache(true)
        // Relative to the context model, which is how its node names the engine.
        .with_engine_cache_path(".")
        .with_dump_ep_context_model(true)
        .with_ep_context_file_path(context.display())
        .with_ep_context_embed_mode(0)
        .build()
        .error_on_failure();
    let mut s = commit(path, &[trt, cuda()])?;
    // A dynamic-shape engine is built on this run, not when the session is created.
    warm_up(&mut s, &spec.input_shape, false)?;
    drop(s);
    if !context.is_file() {
        return Err(anyhow!(
            "TensorRT built {} but wrote no context model",
            spec.name
        ));
    }
    Ok(())
}

fn load_context(context: &Path, spec: &ModelSpec) -> Result<(Session, Duration)> {
    let trt = ep::TensorRT::default()
        .with_fp16(true)
        .with_max_workspace_size(WORKSPACE_BYTES)
        .build()
        .error_on_failure();
    let mut s = commit(context, &[trt, cuda()])?;
    let first = warm_up(&mut s, &spec.input_shape, true)?;
    Ok((s, first))
}

/// `<len> <mtime>` of the model an engine was built from; a changed model means a new build.
fn source_stamp(model: &Path) -> Option<String> {
    let meta = std::fs::metadata(model).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("{} {mtime}", meta.len()))
}

fn engine_cached(engine_dir: &Path, model: &Path) -> bool {
    let Some(want) = source_stamp(model) else {
        return false;
    };
    let stamped = std::fs::read_to_string(engine_dir.join(STAMP_FILE)).unwrap_or_default();
    stamped.trim() == want && engine_dir.join(CONTEXT_MODEL).is_file()
}

fn write_stamp(engine_dir: &Path, model: &Path) {
    if let Some(stamp) = source_stamp(model) {
        if let Err(e) = std::fs::write(engine_dir.join(STAMP_FILE), stamp) {
            tracing::warn!(dir = %engine_dir.display(), "onnx: cannot stamp the engine cache: {e}");
        }
    }
}

/// `MemAvailable` in MB; `None` where there is no /proc/meminfo, which never defers a build.
fn mem_available_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_mem_available_mb(&info)
}

fn parse_mem_available_mb(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

/// Whether an engine build may start with `available` MB free.
fn has_room(available: Option<u64>, headroom_mb: u64) -> bool {
    available.is_none_or(|a| a >= headroom_mb)
}

type Job = (AcceleratedSession, GpuPlan);

fn queue(session: AcceleratedSession) {
    let Some(plan) = GPU.get() else {
        return;
    };
    if !session.0.queued.swap(true, Ordering::SeqCst) {
        enqueue(session, plan.clone());
    }
}

/// One worker moves models one at a time: two engine builds at once would double the peak.
fn enqueue(session: AcceleratedSession, plan: GpuPlan) {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    let tx = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        if let Err(e) = std::thread::Builder::new()
            .name("onnx-gpu".into())
            .spawn(move || worker(rx))
        {
            tracing::warn!("onnx: cannot start the GPU worker, models stay on the cpu: {e}");
        }
        Mutex::new(tx)
    });
    let _ = lock(tx).send((session, plan));
}

fn worker(rx: mpsc::Receiver<Job>) {
    let mut waiting: Vec<Job> = Vec::new();
    let mut told: Vec<&'static str> = Vec::new();
    loop {
        match rx.recv_timeout(if waiting.is_empty() {
            Duration::from_secs(3600)
        } else {
            RECHECK
        }) {
            Ok(job) => waiting.push(job),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if waiting.is_empty() => return,
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(RECHECK),
        }
        // Cached engines first: loading one costs no build, so nothing waits behind a build.
        waiting.sort_by_key(|(s, p)| !s.engine_cached(p));
        let mut still = Vec::new();
        for (session, plan) in waiting.drain(..) {
            let (headroom, what) = if session.engine_cached(&plan) {
                (plan.load_headroom_mb, "load")
            } else {
                (plan.build_headroom_mb, "build")
            };
            let available = mem_available_mb();
            if !has_room(available, headroom) {
                if !told.contains(&session.name()) {
                    told.push(session.name());
                    tracing::info!(
                        model = session.name(),
                        available_mb = available.unwrap_or(0),
                        "onnx: {} stays on the cpu until {} MB is free to {what} its TensorRT engine ({} MB now)",
                        session.name(),
                        headroom,
                        available.unwrap_or(0),
                    );
                }
                still.push((session, plan));
                continue;
            }
            if let Err(e) = session.move_to_gpu(&plan) {
                tracing::warn!(model = session.name(), error = %format!("{e:#}"), "onnx: {} stays on the cpu", session.name());
            }
        }
        waiting = still;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_memory_is_read_from_meminfo() {
        let info =
            "MemTotal:        7802676 kB\nMemFree:  476000 kB\nMemAvailable:    1409000 kB\n";
        assert_eq!(parse_mem_available_mb(info), Some(1375));
        assert_eq!(parse_mem_available_mb("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn a_build_waits_for_headroom_but_never_for_an_unknown_reading() {
        assert!(!has_room(Some(1400), 2048));
        assert!(has_room(Some(4096), 2048));
        assert!(
            has_room(None, 2048),
            "a host without /proc/meminfo must not wait forever"
        );
    }

    #[test]
    fn an_engine_counts_as_cached_only_for_the_model_it_was_built_from() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("m.onnx");
        std::fs::write(&model, b"model bytes").unwrap();
        let engines = dir.path().join("engines");
        std::fs::create_dir_all(&engines).unwrap();
        assert!(
            !engine_cached(&engines, &model),
            "an empty cache holds nothing"
        );

        std::fs::write(engines.join(CONTEXT_MODEL), b"ctx").unwrap();
        assert!(
            !engine_cached(&engines, &model),
            "an engine without a stamp may be for another model"
        );

        write_stamp(&engines, &model);
        assert!(engine_cached(&engines, &model));

        std::fs::write(&model, b"a different, longer model").unwrap();
        assert!(
            !engine_cached(&engines, &model),
            "a replaced model needs a new engine"
        );
    }

    #[test]
    fn a_missing_model_fails_before_any_runtime_is_touched() {
        let err = AcceleratedSession::load(
            "/does/not/exist.onnx",
            ModelSpec {
                name: "missing",
                input_shape: vec![1, 3, 8, 8],
                gpu: true,
            },
        )
        .err()
        .expect("a missing model is an error");
        assert!(format!("{err:#}").contains("not found"));
    }
}
