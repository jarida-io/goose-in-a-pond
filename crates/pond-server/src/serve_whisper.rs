//! serve's Whisper. The full model loads when `/transcribe`, enrolment or a room whose model
//! cannot hear first needs it, and is let go after a quiet spell; a room whose model hears gets
//! the wake-only copy instead (`pond_adapters_whisper::wake_model`), about a third of the memory.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_whisper::{wake_model, TranscribeOpts, WhisperRsInput};
use pond_audio::MicHandle;

use crate::speech_model::SpeechEngine;

/// How long a model nobody holds stays loaded after its last use.
const IDLE: Duration = Duration::from_secs(120);
const REAP_EVERY: Duration = Duration::from_secs(30);
/// The full model (about 330 MB on the Jetson) loads only with this much available; below it, a
/// transcription uses the wake-only copy instead. Loading it beside a room conversation, the
/// encoder and Kokoro ran the 8 GB board out of memory.
const FULL_NEEDS_AVAILABLE_MB: u64 = 700;

struct Held {
    whisper: Arc<WhisperRsInput>,
    path: PathBuf,
    used: Instant,
}

pub struct ServeWhisper {
    full_path: RwLock<PathBuf>,
    wake_dir: PathBuf,
    mic: MicHandle,
    full: Mutex<Option<Held>>,
    wake: Mutex<Option<Held>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ServeWhisper {
    /// Nothing loads here; call inside the runtime, which runs the idle release.
    pub fn new(full_path: PathBuf, data_dir: &Path, mic: MicHandle) -> Arc<ServeWhisper> {
        let me = Arc::new(ServeWhisper {
            full_path: RwLock::new(full_path),
            wake_dir: data_dir.join("models").join("whisper-wake"),
            mic,
            full: Mutex::new(None),
            wake: Mutex::new(None),
        });
        let weak: Weak<ServeWhisper> = Arc::downgrade(&me);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(REAP_EVERY);
            loop {
                every.tick().await;
                match weak.upgrade() {
                    Some(me) => me.release_idle(),
                    None => return,
                }
            }
        });
        me
    }

    fn full_path(&self) -> PathBuf {
        self.full_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The full model, loaded now if nothing has it. Blocking.
    pub fn full(&self) -> Result<Arc<WhisperRsInput>> {
        self.hold(&self.full, self.full_path(), "full", WhisperRsInput::new)
    }

    /// The room's wake-only copy of the full model, made and loaded now if need be. Blocking.
    pub fn wake(&self) -> Result<Arc<WhisperRsInput>> {
        let path = wake_model::ensure(&self.full_path(), &self.wake_dir)?;
        self.hold(&self.wake, path, "wake", WhisperRsInput::new_wake)
    }

    /// The full model when it is loaded or there is room for it, else the wake-only copy.
    pub fn transcribe_wav(&self, wav: &[u8]) -> Result<String> {
        let loaded = lock(&self.full).is_some();
        let available = available_mb();
        if loaded || available.is_none_or(|mb| mb >= FULL_NEEDS_AVAILABLE_MB) {
            return self.full()?.transcribe_wav_bytes(wav);
        }
        tracing::info!(
            target: "giap::trace",
            kind = "whisper_full_skipped",
            available_mb = available,
            needs_mb = FULL_NEEDS_AVAILABLE_MB,
            "transcribing with the wake-only Whisper: too little memory for the full one"
        );
        let decoded = pond_voice::dsp::decode_wav(wav)?;
        let pcm = pond_voice::dsp::resample_to_16k(&decoded.samples, decoded.sample_rate);
        self.wake()?
            .transcribe_pcm_with(&pcm, TranscribeOpts::wake_word(), None)
    }

    /// The states the loaded models hold, for the room's memory lines.
    pub fn held_states(&self) -> Option<(Vec<&'static str>, usize)> {
        [&self.wake, &self.full]
            .into_iter()
            .find_map(|slot| lock(slot).as_ref().and_then(|h| h.whisper.held_states()))
    }

    fn hold(
        &self,
        slot: &Mutex<Option<Held>>,
        path: PathBuf,
        which: &'static str,
        load: fn(PathBuf, MicHandle) -> Result<WhisperRsInput>,
    ) -> Result<Arc<WhisperRsInput>> {
        let mut held = lock(slot);
        if let Some(h) = held.as_mut().filter(|h| h.path == path) {
            h.used = Instant::now();
            return Ok(h.whisper.clone());
        }
        let started = Instant::now();
        let whisper = Arc::new(load(path.clone(), self.mic.clone())?);
        tracing::info!(
            target: "giap::trace",
            kind = "whisper_loaded",
            which,
            model = %path.display(),
            load_ms = started.elapsed().as_millis() as u64,
            "loaded Whisper"
        );
        *held = Some(Held {
            whisper: whisper.clone(),
            path,
            used: Instant::now(),
        });
        Ok(whisper)
    }

    /// Let go of each model nobody holds and nobody has used for [`IDLE`].
    fn release_idle(&self) {
        for (slot, which) in [(&self.full, "full"), (&self.wake, "wake")] {
            let mut held = lock(slot);
            let idle = held
                .as_ref()
                .is_some_and(|h| Arc::strong_count(&h.whisper) == 1 && h.used.elapsed() >= IDLE);
            if idle {
                *held = None;
                tracing::info!(
                    target: "giap::trace",
                    kind = "whisper_released",
                    which,
                    "released Whisper after a quiet spell"
                );
            }
        }
    }
}

#[async_trait]
impl SpeechEngine for ServeWhisper {
    async fn current(&self) -> PathBuf {
        self.full_path()
    }

    /// Loads `path` before taking it into use, so a file that will not load changes nothing.
    async fn load(&self, path: PathBuf) -> Result<()> {
        let started = Instant::now();
        let whisper =
            tokio::task::block_in_place(|| WhisperRsInput::new(path.clone(), self.mic.clone()))?;
        *self.full_path.write().unwrap_or_else(|e| e.into_inner()) = path.clone();
        *lock(&self.full) = Some(Held {
            whisper: Arc::new(whisper),
            path: path.clone(),
            used: Instant::now(),
        });
        // A room started on the old model keeps its copy until it restarts.
        *lock(&self.wake) = None;
        tracing::info!(
            target: "giap::trace",
            kind = "whisper_loaded",
            which = "full",
            model = %path.display(),
            load_ms = started.elapsed().as_millis() as u64,
            "switched Whisper"
        );
        Ok(())
    }
}

/// `MemAvailable`, where the system reports it.
fn available_mb() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map(|kb| kb / 1024)
}
