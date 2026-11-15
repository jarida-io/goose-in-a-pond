//! Kokoro on the GPU for long speech, in a helper process that lives for one speech.
//!
//! ONNX Runtime's CUDA provider gives its memory back only when its process exits, not when a
//! session is dropped, and on unified memory that is the room the next spoken turn's encoder needs.

use anyhow::{anyhow, bail, Context, Result};
use ort::ep::cuda::ConvAlgorithmSearch;
use ort::ep::ArenaExtendStrategy;
use ort::session::Session;
use pond_core::models::domain::acceleration::{record_onnx_device, OnnxDevice, TTS_KOKORO};
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::tokenizer::MAX_CONTEXT;
use crate::voices::STYLE_DIM;

/// Speech this long with more to follow starts the helper: a shorter unit is said sooner on the
/// CPU than the helper takes to load.
pub const GPU_FROM_TOKENS: usize = 100;
/// Memory that must be available before the helper starts; it holds about 700 MB on the Orin Nano.
pub const GPU_NEEDS_MB: u64 = 750;

const READY: [u8; 4] = *b"KGPU";
const AUDIO: u32 = 0;
const FAILED: u32 = 1;
/// Two minutes of audio: no pass is longer, so a longer frame is a corrupt one.
const MOST_SAMPLES: usize = 24_000 * 120;

const LOAD_WITHIN: Duration = Duration::from_secs(30);
const PASS_WITHIN: Duration = Duration::from_secs(30);
/// A speech whose last unit never came (a barge-in, a dropped phone) must not keep the memory.
const IDLE_AFTER: Duration = Duration::from_secs(20);
/// After a failure, how long speech stays on the CPU before the helper is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(600);
const EXIT_WITHIN: Duration = Duration::from_secs(5);
/// On the Orin Nano a released encoder's 595 MB was not yet available when the release returned.
const ROOM_SHOWS_WITHIN: Duration = Duration::from_secs(3);

/// How to start the helper: the pond's own binary and the arguments that make it one.
#[derive(Debug, Clone)]
pub struct HelperCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// The two ends of a started helper.
pub struct Link {
    pub to_helper: Box<dyn Write + Send>,
    pub from_helper: Box<dyn Read + Send>,
    pub child: Option<Child>,
}

enum Frame {
    Ready,
    Audio(Vec<f32>),
    Failed(String),
    Gone,
}

struct Helper {
    to_helper: Box<dyn Write + Send>,
    from_helper: Receiver<Frame>,
    child: Option<Arc<Mutex<Child>>>,
    started: Instant,
    used: Instant,
    passes: usize,
}

impl Helper {
    fn alive(&self) -> bool {
        match &self.child {
            Some(child) => matches!(lock(child).try_wait(), Ok(None)),
            None => true,
        }
    }

    fn pass(&mut self, ids: &[i64], style: &[f32], speed: f32) -> Result<Vec<f32>> {
        let mut request = Vec::with_capacity(8 + style.len() * 4 + ids.len() * 8);
        request.extend_from_slice(&(ids.len() as u32).to_le_bytes());
        request.extend_from_slice(&speed.to_le_bytes());
        style
            .iter()
            .for_each(|v| request.extend_from_slice(&v.to_le_bytes()));
        ids.iter()
            .for_each(|v| request.extend_from_slice(&v.to_le_bytes()));
        self.to_helper
            .write_all(&request)
            .and_then(|()| self.to_helper.flush())
            .context("the Kokoro GPU helper stopped taking requests")?;
        match self.from_helper.recv_timeout(PASS_WITHIN) {
            Ok(Frame::Audio(audio)) => {
                self.used = Instant::now();
                self.passes += 1;
                Ok(audio)
            }
            Ok(Frame::Failed(why)) => bail!("the Kokoro GPU helper could not run a pass: {why}"),
            Ok(Frame::Ready) => bail!("the Kokoro GPU helper answered out of turn"),
            Ok(Frame::Gone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("the Kokoro GPU helper exited during a pass")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => bail!(
                "the Kokoro GPU helper gave no audio within {} s",
                PASS_WITHIN.as_secs()
            ),
        }
    }
}

/// Long speech on the GPU: decides which units go there and owns the helper while one runs.
pub struct GpuSpeech {
    start: Box<dyn Fn() -> Result<Link> + Send + Sync>,
    /// The weights the helper loads; `None` when the starter needs no file.
    model: Option<PathBuf>,
    /// Read as each long speech begins, so the household's switch needs no restart.
    switch: Box<dyn Fn() -> bool + Send + Sync>,
    fetch_model: Box<dyn Fn() + Send + Sync>,
    make_room: Box<dyn Fn() -> bool + Send + Sync>,
    available_mb: Box<dyn Fn() -> Option<u64> + Send + Sync>,
    idle_after: Duration,
    running: Mutex<Option<Helper>>,
    /// The running helper's process, apart from `running` so a stop never waits for a pass.
    child: Mutex<Option<Arc<Mutex<Child>>>>,
    /// A stop was asked for, so the pass it fails is no fault of the helper's.
    stopping: AtomicBool,
    retry_at: Mutex<Option<Instant>>,
}

impl GpuSpeech {
    /// `model` is the full-precision file: the quantised ones have no GPU kernels and the
    /// half-precision one comes back as NaN on CUDA.
    pub fn new(command: HelperCommand, model: PathBuf) -> Self {
        let mut speech = Self::with_starter(move || spawn(&command));
        speech.model = Some(model);
        speech
    }

    pub fn with_starter(start: impl Fn() -> Result<Link> + Send + Sync + 'static) -> Self {
        Self {
            start: Box::new(start),
            model: None,
            switch: Box::new(|| true),
            fetch_model: Box::new(|| {}),
            make_room: Box::new(|| false),
            available_mb: Box::new(mem_available_mb),
            idle_after: IDLE_AFTER,
            running: Mutex::new(None),
            child: Mutex::new(None),
            stopping: AtomicBool::new(false),
            retry_at: Mutex::new(None),
        }
    }

    /// Called before the helper starts, to free what speech does not need; whether it freed
    /// anything.
    pub fn with_room_maker(mut self, make_room: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.make_room = Box::new(make_room);
        self
    }

    pub fn with_memory_probe(
        mut self,
        available_mb: impl Fn() -> Option<u64> + Send + Sync + 'static,
    ) -> Self {
        self.available_mb = Box::new(available_mb);
        self
    }

    pub fn with_idle_after(mut self, idle_after: Duration) -> Self {
        self.idle_after = idle_after;
        self
    }

    pub fn with_switch(mut self, switch: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.switch = Box::new(switch);
        self
    }

    /// Called when long speech finds the weights missing, to fetch them for the next one.
    pub fn with_model_fetcher(mut self, fetch: impl Fn() + Send + Sync + 'static) -> Self {
        self.fetch_model = Box::new(fetch);
        self
    }

    pub fn is_running(&self) -> bool {
        lock(&self.running).is_some()
    }

    /// Whether this unit is said on the GPU, starting the helper for a long one that more
    /// follows. Blocks while the helper loads.
    pub fn takes(self: &Arc<Self>, tokens: usize, last: bool) -> bool {
        let mut running = lock(&self.running);
        if !(self.switch)() {
            if let Some(helper) = running.take() {
                self.finish(helper, "switched_off");
            }
            return false;
        }
        if let Some(helper) = running.take() {
            if helper.alive() {
                *running = Some(helper);
                return true;
            }
            self.finish(helper, "cut_short");
        }
        if last || tokens < GPU_FROM_TOKENS {
            return false;
        }
        if lock(&self.retry_at).is_some_and(|at| Instant::now() < at) {
            return false;
        }
        if self.model.as_ref().is_some_and(|model| !model.exists()) {
            skipped("model_missing", None);
            (self.fetch_model)();
            return false;
        }
        let made_room = (self.make_room)();
        let mut available = (self.available_mb)();
        let until = Instant::now() + ROOM_SHOWS_WITHIN;
        while made_room && available.is_some_and(|mb| mb < GPU_NEEDS_MB) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
            available = (self.available_mb)();
        }
        if available.is_some_and(|mb| mb < GPU_NEEDS_MB) {
            skipped("memory", available);
            return false;
        }
        match self.begin() {
            Ok(helper) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "kokoro_gpu_started",
                    load_ms = helper.started.elapsed().as_millis() as u64,
                    available_mb = available.unwrap_or(0),
                    "long speech moved to the GPU"
                );
                record_onnx_device(TTS_KOKORO, OnnxDevice::Cuda);
                *running = Some(helper);
                drop(running);
                self.watch();
                true
            }
            Err(e) => {
                tracing::warn!(
                    kind = "kokoro_gpu_failed",
                    error = %format!("{e:#}"),
                    "the Kokoro GPU helper did not start; speech stays on the CPU"
                );
                *lock(&self.retry_at) = Some(Instant::now() + RETRY_AFTER);
                false
            }
        }
    }

    /// One pass on the running helper. A failure ends the helper, and the caller says the pass
    /// on the CPU.
    pub fn synthesize(&self, ids: &[i64], style: &[f32], speed: f32) -> Result<Vec<f32>> {
        let mut running = lock(&self.running);
        let Some(helper) = running.as_mut() else {
            bail!("the Kokoro GPU helper is not running");
        };
        let audio = helper.pass(ids, style, speed);
        if audio.is_err() {
            let asked = self.stopping.swap(false, Ordering::SeqCst);
            if let Some(helper) = running.take() {
                self.kill();
                self.finish(helper, if asked { "cut_short" } else { "failed" });
            }
            if !asked {
                *lock(&self.retry_at) = Some(Instant::now() + RETRY_AFTER);
            }
        }
        audio
    }

    /// The speech is over: the helper exits and its memory comes back.
    pub fn end(&self, reason: &'static str) {
        if let Some(helper) = lock(&self.running).take() {
            self.finish(helper, reason);
        }
    }

    /// Ends the helper at once, even mid-pass: the pass in flight fails over to the CPU.
    pub fn stop_now(&self) {
        if lock(&self.child).is_some() {
            self.stopping.store(true, Ordering::SeqCst);
            self.kill();
        }
    }

    fn kill(&self) {
        if let Some(child) = lock(&self.child).as_ref() {
            let _ = lock(child).kill();
        }
    }

    fn begin(&self) -> Result<Helper> {
        let started = Instant::now();
        let Link {
            to_helper,
            from_helper,
            child,
        } = (self.start)()?;
        let child = child.map(|child| Arc::new(Mutex::new(child)));
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("kokoro-gpu-reader".into())
            .spawn(move || read_frames(from_helper, tx))
            .context("could not start the Kokoro GPU helper's reader")?;
        let give_up = |why: String| {
            if let Some(child) = &child {
                let _ = lock(child).kill();
                reap(child.clone());
            }
            anyhow!(why)
        };
        match rx.recv_timeout(LOAD_WITHIN) {
            Ok(Frame::Ready) => {}
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(give_up("it exited before it was ready".into()))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(give_up(format!(
                    "it was not ready within {} s",
                    LOAD_WITHIN.as_secs()
                )))
            }
        }
        *lock(&self.child) = child.clone();
        self.stopping.store(false, Ordering::SeqCst);
        Ok(Helper {
            to_helper,
            from_helper: rx,
            child,
            started,
            used: Instant::now(),
            passes: 0,
        })
    }

    fn finish(&self, helper: Helper, reason: &'static str) {
        let Helper {
            to_helper,
            child,
            started,
            passes,
            ..
        } = helper;
        // Closing its input is how the helper is told to exit.
        drop(to_helper);
        *lock(&self.child) = None;
        if let Some(child) = child {
            reap(child);
        }
        record_onnx_device(TTS_KOKORO, OnnxDevice::Cpu);
        tracing::info!(
            target: "giap::trace",
            kind = "kokoro_gpu_stopped",
            reason,
            passes,
            held_ms = started.elapsed().as_millis() as u64,
            "long speech left the GPU"
        );
    }

    fn watch(self: &Arc<Self>) {
        let speech = Arc::downgrade(self);
        let tick = self.idle_after.min(Duration::from_secs(1));
        let _ = std::thread::Builder::new()
            .name("kokoro-gpu-idle".into())
            .spawn(move || loop {
                std::thread::sleep(tick);
                let Some(speech) = speech.upgrade() else {
                    return;
                };
                // Busy means a pass is in flight, which is not idle.
                let Ok(mut running) = speech.running.try_lock() else {
                    continue;
                };
                let reason = match running.as_ref() {
                    None => return,
                    Some(helper) if !helper.alive() => "cut_short",
                    Some(helper) if helper.used.elapsed() >= speech.idle_after => "idle",
                    Some(_) => continue,
                };
                if let Some(helper) = running.take() {
                    speech.finish(helper, reason);
                }
                return;
            });
    }
}

impl Drop for GpuSpeech {
    fn drop(&mut self) {
        self.kill();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn skipped(reason: &'static str, available_mb: Option<u64>) {
    tracing::info!(
        target: "giap::trace",
        kind = "kokoro_gpu_skipped",
        reason,
        available_mb = available_mb.unwrap_or(0),
        needs_mb = GPU_NEEDS_MB,
        "long speech stays on the CPU"
    );
}

fn spawn(command: &HelperCommand) -> Result<Link> {
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("could not run {}", command.program.display()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        bail!("the Kokoro GPU helper started without its pipes");
    };
    Ok(Link {
        to_helper: Box::new(stdin),
        from_helper: Box::new(stdout),
        child: Some(child),
    })
}

/// Waits for an exiting helper so it leaves no zombie, and kills one that will not go.
fn reap(child: Arc<Mutex<Child>>) {
    let _ = std::thread::Builder::new()
        .name("kokoro-gpu-reap".into())
        .spawn(move || {
            let until = Instant::now() + EXIT_WITHIN;
            while Instant::now() < until {
                if !matches!(lock(&child).try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let mut child = lock(&child);
            let _ = child.kill();
            let _ = child.wait();
        });
}

fn read_frames(mut from: Box<dyn Read + Send>, frames: Sender<Frame>) {
    let mut magic = [0u8; 4];
    if from.read_exact(&mut magic).is_err() || magic != READY {
        let _ = frames.send(Frame::Gone);
        return;
    }
    let _ = frames.send(Frame::Ready);
    loop {
        let frame = read_frame(&mut from).unwrap_or(Frame::Gone);
        let gone = matches!(frame, Frame::Gone);
        if frames.send(frame).is_err() || gone {
            return;
        }
    }
}

fn read_frame(from: &mut impl Read) -> Option<Frame> {
    let mut head = [0u8; 8];
    from.read_exact(&mut head).ok()?;
    let status = u32::from_le_bytes(head[..4].try_into().ok()?);
    let len = u32::from_le_bytes(head[4..].try_into().ok()?) as usize;
    if len > MOST_SAMPLES {
        return None;
    }
    if status == AUDIO {
        let mut bytes = vec![0u8; len * 4];
        from.read_exact(&mut bytes).ok()?;
        return Some(Frame::Audio(
            bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
        ));
    }
    let mut bytes = vec![0u8; len];
    from.read_exact(&mut bytes).ok()?;
    Some(Frame::Failed(String::from_utf8_lossy(&bytes).into_owned()))
}

/// `MemAvailable`, where the kernel reports one.
pub fn mem_available_mb() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(kb / 1024)
}

/// The helper's whole life: load on the GPU, then answer passes until the pond closes the pipe.
pub fn run_helper(model: &Path, threads: usize) -> Result<()> {
    // Short of memory, the kernel should end the helper, which speech survives, before the pond.
    #[cfg(target_os = "linux")]
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
    let mut session = load_on_gpu(model, threads)?;
    serve(
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        |ids, style, speed| crate::engine::run(&mut session, ids, style, speed),
    )
}

fn load_on_gpu(model: &Path, threads: usize) -> Result<Session> {
    let cuda = ort::ep::CUDA::default()
        // ONNX Runtime's defaults held 1,150 MB on the Orin Nano against 700 MB with these.
        .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
        .with_conv_algorithm_search(ConvAlgorithmSearch::Heuristic)
        .with_conv_max_workspace(false)
        .with_conv1d_pad_to_nc1d(true)
        .build()
        // Without it a refused provider leaves this file on the CPU, slower than the pond's own.
        .error_on_failure();
    Session::builder()
        .context("failed to create ort session builder")?
        .with_intra_threads(threads)
        .map_err(|e| anyhow!("{e}"))?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow!("{e}"))?
        .with_memory_pattern(false)
        .map_err(|e| anyhow!("{e}"))?
        .with_log_level(ort::logging::LogLevel::Error)
        .map_err(|e| anyhow!("{e}"))?
        .with_execution_providers([cuda])
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(model)
        .map_err(|e| anyhow!("failed to load Kokoro on the GPU: {e}"))
}

/// Says it is ready, then answers each request with its audio or with why it has none.
pub(crate) fn serve(
    mut input: impl Read,
    mut output: impl Write,
    mut synth: impl FnMut(Vec<i64>, &[f32], f32) -> Result<Vec<f32>>,
) -> Result<()> {
    output.write_all(&READY)?;
    output.flush()?;
    loop {
        let mut head = [0u8; 8];
        match input.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let tokens = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let speed = f32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        if tokens > MAX_CONTEXT {
            bail!("a request named {tokens} tokens, over the {MAX_CONTEXT} a pass holds");
        }
        let mut bytes = vec![0u8; STYLE_DIM * 4 + tokens * 8];
        input.read_exact(&mut bytes)?;
        let (style, ids) = bytes.split_at(STYLE_DIM * 4);
        let style: Vec<f32> = style
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let ids: Vec<i64> = ids
            .chunks_exact(8)
            .map(|b| i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .collect();
        match synth(ids, &style, speed) {
            Ok(audio) => {
                output.write_all(&AUDIO.to_le_bytes())?;
                output.write_all(&(audio.len() as u32).to_le_bytes())?;
                let mut bytes = Vec::with_capacity(audio.len() * 4);
                audio
                    .iter()
                    .for_each(|v| bytes.extend_from_slice(&v.to_le_bytes()));
                output.write_all(&bytes)?;
            }
            Err(e) => {
                let why = format!("{e:#}");
                output.write_all(&FAILED.to_le_bytes())?;
                output.write_all(&(why.len() as u32).to_le_bytes())?;
                output.write_all(why.as_bytes())?;
            }
        }
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A helper on a thread: every pass is ten samples a token, at the speed asked for.
    fn stand_in(starts: Arc<AtomicUsize>) -> impl Fn() -> Result<Link> + Send + Sync {
        move || {
            starts.fetch_add(1, Ordering::SeqCst);
            let (requests, to_helper) = std::io::pipe()?;
            let (from_helper, answers) = std::io::pipe()?;
            std::thread::spawn(move || {
                let _ = serve(requests, answers, |ids, style, speed| {
                    anyhow::ensure!(style.len() == STYLE_DIM, "a style row is {STYLE_DIM} wide");
                    Ok(vec![speed; ids.len() * 10])
                });
            });
            Ok(Link {
                to_helper: Box::new(to_helper),
                from_helper: Box::new(from_helper),
                child: None,
            })
        }
    }

    fn speech(starts: &Arc<AtomicUsize>) -> GpuSpeech {
        GpuSpeech::with_starter(stand_in(starts.clone())).with_memory_probe(|| Some(2_000))
    }

    const STYLE: [f32; STYLE_DIM] = [0.25; STYLE_DIM];

    #[test]
    fn a_long_unit_that_more_follows_starts_the_helper_and_the_speech_ends_it() {
        let starts = Arc::new(AtomicUsize::new(0));
        let gpu = Arc::new(speech(&starts));

        assert!(gpu.takes(GPU_FROM_TOKENS, false));
        let audio = gpu.synthesize(&[0, 7, 8, 0], &STYLE, 1.5).unwrap();
        assert_eq!(audio, vec![1.5; 40]);

        assert!(gpu.takes(12, true), "the closing unit follows on the GPU");
        assert_eq!(gpu.synthesize(&[0, 9, 0], &STYLE, 1.0).unwrap().len(), 30);
        gpu.end("speech_ended");

        assert!(!gpu.is_running());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_short_unit_and_a_reply_of_one_unit_stay_on_the_cpu() {
        let starts = Arc::new(AtomicUsize::new(0));
        let gpu = Arc::new(speech(&starts));

        assert!(!gpu.takes(GPU_FROM_TOKENS - 1, false));
        assert!(!gpu.takes(400, true));
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn room_is_made_first_and_too_little_of_it_keeps_speech_on_the_cpu() {
        let starts = Arc::new(AtomicUsize::new(0));
        let made = Arc::new(AtomicUsize::new(0));
        let counting = made.clone();
        let gpu = Arc::new(
            speech(&starts)
                .with_room_maker(move || {
                    counting.fetch_add(1, Ordering::SeqCst);
                    false
                })
                .with_memory_probe(|| Some(GPU_NEEDS_MB - 1)),
        );

        assert!(!gpu.takes(300, false));
        assert_eq!(made.load(Ordering::SeqCst), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn room_that_was_just_made_is_waited_for() {
        let starts = Arc::new(AtomicUsize::new(0));
        let probes = Arc::new(AtomicUsize::new(0));
        let counting = probes.clone();
        let gpu = Arc::new(
            speech(&starts)
                .with_room_maker(|| true)
                .with_memory_probe(move || {
                    let freed = counting.fetch_add(1, Ordering::SeqCst) >= 2;
                    Some(if freed {
                        GPU_NEEDS_MB + 300
                    } else {
                        GPU_NEEDS_MB - 300
                    })
                }),
        );

        assert!(gpu.takes(300, false));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_helper_that_dies_fails_the_pass_and_is_not_tried_again_at_once() {
        let starts = Arc::new(AtomicUsize::new(0));
        let counting = starts.clone();
        let gpu = Arc::new(
            GpuSpeech::with_starter(move || {
                counting.fetch_add(1, Ordering::SeqCst);
                let (requests, to_helper) = std::io::pipe()?;
                let (from_helper, mut answers) = std::io::pipe()?;
                std::thread::spawn(move || {
                    let _ = answers.write_all(&READY);
                    drop((requests, answers));
                });
                Ok(Link {
                    to_helper: Box::new(to_helper),
                    from_helper: Box::new(from_helper),
                    child: None,
                })
            })
            .with_memory_probe(|| Some(2_000)),
        );

        assert!(gpu.takes(300, false));
        assert!(gpu.synthesize(&[0, 7, 0], &STYLE, 1.0).is_err());
        assert!(!gpu.is_running());
        assert!(
            !gpu.takes(300, false),
            "speech stays on the CPU for a while"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_helper_no_speech_uses_is_stopped() {
        let starts = Arc::new(AtomicUsize::new(0));
        let gpu = Arc::new(speech(&starts).with_idle_after(Duration::from_millis(40)));

        assert!(gpu.takes(300, false));
        let until = Instant::now() + Duration::from_secs(5);
        while gpu.is_running() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!gpu.is_running());
    }

    #[test]
    fn switched_off_nothing_goes_to_the_gpu_and_a_running_helper_ends() {
        let starts = Arc::new(AtomicUsize::new(0));
        let on = Arc::new(AtomicBool::new(true));
        let switch = on.clone();
        let gpu = Arc::new(speech(&starts).with_switch(move || switch.load(Ordering::SeqCst)));

        assert!(gpu.takes(300, false));
        on.store(false, Ordering::SeqCst);
        assert!(!gpu.takes(300, false));
        assert!(!gpu.is_running());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_weights_keep_speech_on_the_cpu_and_ask_for_their_fetch() {
        let starts = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(AtomicUsize::new(0));
        let counting = asked.clone();
        let mut gpu = speech(&starts).with_model_fetcher(move || {
            counting.fetch_add(1, Ordering::SeqCst);
        });
        gpu.model = Some(PathBuf::from("/nope/model.onnx"));
        let gpu = Arc::new(gpu);

        assert!(!gpu.takes(300, false));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_pass_the_helper_cannot_run_comes_back_as_its_reason() {
        let (requests, mut to_helper) = std::io::pipe().unwrap();
        let (mut from_helper, answers) = std::io::pipe().unwrap();
        let helper = std::thread::spawn(move || {
            serve(requests, answers, |_, _, _| bail!("no room on the device"))
        });

        let mut magic = [0u8; 4];
        from_helper.read_exact(&mut magic).unwrap();
        assert_eq!(magic, READY);
        let mut request = Vec::new();
        request.extend_from_slice(&1u32.to_le_bytes());
        request.extend_from_slice(&1.0f32.to_le_bytes());
        request.extend_from_slice(&[0u8; STYLE_DIM * 4]);
        request.extend_from_slice(&7i64.to_le_bytes());
        to_helper.write_all(&request).unwrap();

        match read_frame(&mut from_helper) {
            Some(Frame::Failed(why)) => assert!(why.contains("no room on the device"), "{why}"),
            _ => panic!("expected the failure to be reported"),
        }
        drop(to_helper);
        helper.join().unwrap().unwrap();
    }
}
