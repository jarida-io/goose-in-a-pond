//! serve's Whisper. The full model loads when `/transcribe`, enrolment or a room whose model
//! cannot hear first needs it, and is let go after a quiet spell. While the chat model hears
//! recordings itself, Whisper only spots the wake word and writes a rough transcript, and its
//! encoder needs the memory more: the room gets a wake-only copy of tiny.en
//! (`pond_adapters_whisper::wake_model`) and `/transcribe` gets tiny.en.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_whisper::{wake_model, WhisperRsInput};
use pond_audio::MicHandle;
use pond_core::models::ports::voice_input::ModelHears;

use crate::speech_model::SpeechEngine;
use crate::voice_models::WhisperModel;

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
    which: &'static str,
    used: Instant,
}

pub struct ServeWhisper {
    full_path: RwLock<PathBuf>,
    tiny: Option<WhisperModel>,
    wake_dir: PathBuf,
    mic: MicHandle,
    full: Mutex<Option<Held>>,
    wake: Mutex<Option<Held>>,
    hearing: OnceLock<Arc<dyn ModelHears>>,
    /// One fetch of tiny.en at a time: startup and the room both ask for it.
    fetching: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ServeWhisper {
    /// Nothing loads here; call inside the runtime, which runs the idle release.
    pub fn new(
        full_path: PathBuf,
        tiny: Option<WhisperModel>,
        data_dir: &Path,
        mic: MicHandle,
    ) -> Arc<ServeWhisper> {
        let me = Arc::new(ServeWhisper {
            full_path: RwLock::new(full_path),
            tiny,
            wake_dir: data_dir.join("models").join("whisper-wake"),
            mic,
            full: Mutex::new(None),
            wake: Mutex::new(None),
            hearing: OnceLock::new(),
            fetching: tokio::sync::Mutex::new(()),
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

    /// Asks the chat model whether it hears, for `/transcribe`; the room asks it itself.
    pub fn set_hearing(&self, hearing: Arc<dyn ModelHears>) {
        let _ = self.hearing.set(hearing);
    }

    /// Downloads tiny.en if it is not on disk yet.
    pub async fn fetch_tiny(&self) {
        let Some(tiny) = &self.tiny else {
            return;
        };
        let _one = self.fetching.lock().await;
        if tiny.path.is_file() {
            return;
        }
        let Some(download) = &tiny.download else {
            return;
        };
        if let Some(parent) = tiny.path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match crate::model_download::download_file(&download.url, &tiny.path, download.size_mb)
            .await
        {
            Ok(()) => tracing::info!(
                target: "giap::trace",
                kind = "whisper_tiny_fetched",
                model = %tiny.path.display(),
                "fetched tiny.en for a chat model that hears"
            ),
            Err(e) => tracing::warn!(
                error = format!("{e:#}"),
                "tiny.en did not download; the wake-only Whisper is made from the full model"
            ),
        }
    }

    fn tiny_on_disk(&self) -> Option<PathBuf> {
        self.tiny
            .as_ref()
            .map(|t| t.path.clone())
            .filter(|p| p.is_file())
    }

    /// The room's wake-only copy of tiny.en, or of the full model until tiny.en is on disk, made
    /// and loaded now if need be. Blocking.
    pub fn wake(&self) -> Result<Arc<WhisperRsInput>> {
        let source = self.tiny_on_disk().unwrap_or_else(|| self.full_path());
        let path = wake_model::ensure(&source, &self.wake_dir)?;
        self.hold(&self.wake, path, "wake", WhisperRsInput::new)
    }

    /// Blocking, and on a runtime's blocking thread: every caller runs it in `spawn_blocking`.
    fn model_hears(&self) -> bool {
        let (Some(hearing), Ok(runtime)) =
            (self.hearing.get(), tokio::runtime::Handle::try_current())
        else {
            return false;
        };
        runtime.block_on(hearing.model_hears())
    }

    /// tiny.en while the chat model hears; otherwise the full model when it is loaded or there
    /// is room for it, else the wake-only copy.
    pub fn transcribe_wav(&self, wav: &[u8]) -> Result<String> {
        if self.model_hears() {
            let whisper = match self.tiny_on_disk() {
                Some(tiny) => self.hold(&self.full, tiny, "tiny", WhisperRsInput::new)?,
                None => self.wake()?,
            };
            return whisper.transcribe_wav_bytes(wav);
        }
        let loaded = lock(&self.full)
            .as_ref()
            .is_some_and(|h| h.path == self.full_path());
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
        self.wake()?.transcribe_wav_bytes(wav)
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
            which,
            used: Instant::now(),
        });
        Ok(whisper)
    }

    /// Let go of each model nobody holds and nobody has used for [`IDLE`].
    fn release_idle(&self) {
        for slot in [&self.full, &self.wake] {
            let mut held = lock(slot);
            let idle = held
                .as_ref()
                .filter(|h| Arc::strong_count(&h.whisper) == 1 && h.used.elapsed() >= IDLE)
                .map(|h| h.which);
            if let Some(which) = idle {
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
            which: "full",
            used: Instant::now(),
        });
        // A copy made from the old model goes; a room started on it keeps it until it restarts.
        if self.tiny_on_disk().is_none() {
            *lock(&self.wake) = None;
        }
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
