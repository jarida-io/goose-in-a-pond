//! How the pond listens in a room, built from settings and the data directory: the ear (echo
//! cancelled, Silero-endpointed, interrupted only by the wake word or a household voice) where
//! Silero is available, else the Whisper detector with energy gates and barge-in. `chat --voice`
//! builds it, and serve builds the same.

use std::path::Path;
use std::sync::{Arc, RwLock};

use pond_adapters_ear::{EarConfig, EarPart, EarRig, Transcriber, WakeMode};
use pond_adapters_whisper::{
    KeywordDetectorConfig, TranscribeOpts, WhisperBackend, WhisperKeywordDetector, WhisperRsInput,
};
use pond_audio::MicHandle;
use pond_core::models::domain::wake_detector::{
    choose_wake_detector, WakeDetector, WakeDetectorReason,
};
use pond_core::models::ports::interrupt::InterruptWatch;
use pond_core::models::ports::speech_energy::SpeechEnergy;
use pond_core::models::ports::voice_input::VoiceInput;
use pond_core::models::ports::wake_word::StreamingWakeWordDetector;
use pond_core::models::services::instant_activation::InstantActivation;
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use pond_core::shared::services::chat::{ChatService, SpokenObserver, StateObserver};
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::profile::ProfileRepository;

use crate::model_download;

#[derive(Clone)]
pub enum WhisperHandle {
    Loaded(Arc<WhisperRsInput>),
    OnDemand {
        get: Arc<dyn Fn() -> anyhow::Result<Arc<WhisperRsInput>> + Send + Sync>,
        skip_scan: Arc<dyn Fn() -> bool + Send + Sync>,
        skip_transcribe: Arc<dyn Fn() -> bool + Send + Sync>,
    },
}

impl From<Arc<WhisperRsInput>> for WhisperHandle {
    fn from(w: Arc<WhisperRsInput>) -> Self {
        Self::Loaded(w)
    }
}

impl WhisperHandle {
    fn get(&self) -> anyhow::Result<Arc<WhisperRsInput>> {
        match self {
            Self::Loaded(w) => Ok(w.clone()),
            Self::OnDemand { get, .. } => get(),
        }
    }

    fn should_skip_scan(&self) -> bool {
        match self {
            Self::Loaded(_) => false,
            Self::OnDemand { skip_scan, .. } => skip_scan(),
        }
    }

    fn should_skip_transcribe(&self) -> bool {
        match self {
            Self::Loaded(_) => false,
            Self::OnDemand {
                skip_transcribe, ..
            } => skip_transcribe(),
        }
    }
}

pub struct ListeningOptions<'a> {
    pub settings: &'a Settings,
    pub data_dir: &'a Path,
    /// The process's one microphone owner.
    pub mic: &'a MicHandle,
    /// Speech-to-text for either front end; the Whisper one also records through it.
    pub whisper: WhisperHandle,
    /// In place of `settings.voice_wake_word`.
    pub wake_word: Option<&'a str>,
    /// Every turn listens at once, with no wake word.
    pub no_wake_word: bool,
    /// Live microphone level for a client's meter.
    pub audio_level: Option<Arc<ThrottledAudioLevelSink>>,
    /// Voice Match's speaker model; the ear recognises nobody without it.
    pub speaker: Option<pond_adapters_ear::ModelFile>,
    /// The wake CNN heard every taught member's latest enrolment run.
    pub cnn_hears_household: bool,
    /// What the ear's Whisper reads before each stretch of speech; its owner keeps it current.
    pub whisper_prompt: Option<WhisperPrompt>,
}

/// The ear's Whisper prompt, shared with whoever rewrites it when the household changes.
pub type WhisperPrompt = Arc<RwLock<Option<String>>>;

/// The prompt for `wake_word` and every member's name, read now.
pub async fn household_prompt(
    wake_word: &str,
    profiles: Option<&(dyn ProfileRepository + Send + Sync)>,
) -> Option<String> {
    let names = match profiles {
        Some(repo) => match repo.list().await {
            Ok(list) => list.into_iter().map(|p| p.display_name).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "room voice: no member names for Whisper");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    pond_core::models::domain::speech_prompt::room_prompt(wake_word, &names)
}

/// Which front end listens, and why, for status lines and the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front {
    Ear {
        detector: WakeDetector,
        reason: WakeDetectorReason,
    },
    Whisper(WhyNotTheEar),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNotTheEar {
    /// `vad_backend` is `rms`, the escape hatch.
    EnergyGateChosen,
    /// Silero could not be fetched.
    NoSilero,
    /// The ear could not start.
    EarFailed,
}

pub struct Listening {
    pub input: Arc<dyn VoiceInput>,
    pub wake: Arc<dyn StreamingWakeWordDetector>,
    pub interrupt: Option<Arc<dyn InterruptWatch>>,
    pub energy: Option<Arc<dyn SpeechEnergy>>,
    pub state_observer: Option<StateObserver>,
    /// Told what the loop says, so the ear can tell its echo from a person.
    pub spoken_observer: Option<SpokenObserver>,
    pub front: Front,
    /// What the ear's parts hold; empty on the Whisper front end.
    pub ear_parts: Vec<EarPart>,
    /// The ear itself, for Voice Match's household; `None` on the Whisper front end.
    pub ear: Option<Arc<EarRig>>,
    pub wake_word: String,
    /// Calibration takes behind the wake word's triggers.
    pub calibrated: usize,
}

impl Listening {
    /// `chat` listening this way: input, wake detector, and the ear's interrupts and state
    /// hook, or the energy barge-in.
    pub fn apply(&self, chat: ChatService) -> ChatService {
        let mut chat = chat
            .with_voice_input(self.input.clone())
            .with_wake_word_detector(self.wake.clone());
        if let Some(watch) = &self.interrupt {
            chat = chat.with_interrupt_watch(watch.clone());
        }
        if let Some(observer) = &self.state_observer {
            chat = chat.with_state_observer(observer.clone());
        }
        if let Some(observer) = &self.spoken_observer {
            chat = chat.with_spoken_observer(observer.clone());
        }
        if let Some(energy) = &self.energy {
            chat = chat.with_speech_energy(energy.clone());
        }
        chat
    }

    /// The wake detector this front end listens with, and why.
    pub fn wake(&self) -> (WakeDetector, WakeDetectorReason) {
        match self.front {
            Front::Ear { detector, reason } => (detector, reason),
            Front::Whisper(_) => (WakeDetector::Whisper, WakeDetectorReason::NoEar),
        }
    }

    /// One line for a status display.
    pub fn describe(&self) -> String {
        match self.front {
            Front::Ear { detector, reason } => {
                let detector = match detector {
                    WakeDetector::HeyGoose => "the hey goose model",
                    WakeDetector::Whisper => "Whisper on speech segments",
                };
                let reason = match reason {
                    WakeDetectorReason::Chosen => "as set",
                    WakeDetectorReason::Calibrated => "it hears this household",
                    WakeDetectorReason::NotCalibrated => "not calibrated",
                    WakeDetectorReason::CustomPhrase => "a custom phrase",
                    WakeDetectorReason::NoEar => "no ear",
                };
                format!("echo cancelled; wake word by {detector} ({reason})")
            }
            Front::Whisper(why) => {
                let why = match why {
                    WhyNotTheEar::EnergyGateChosen => "vad_backend is rms",
                    WhyNotTheEar::NoSilero => "Silero is unavailable",
                    WhyNotTheEar::EarFailed => "the ear failed to start",
                };
                format!("Whisper wake word and energy gates ({why})")
            }
        }
    }
}

/// Whisper through its accurate profile, for the ear: the state it already prepared at load, so
/// the ear never builds Whisper's second, wake-word state.
struct WhisperTranscriber {
    whisper: WhisperHandle,
    prompt: Option<WhisperPrompt>,
}

impl WhisperTranscriber {
    fn run(&self, pcm: &[f32], opts: TranscribeOpts) -> anyhow::Result<String> {
        let prompt = self
            .prompt
            .as_ref()
            .and_then(|p| p.read().unwrap_or_else(|e| e.into_inner()).clone());
        crate::room_memory::measured("whisper", || {
            let whisper = self.whisper.get()?;
            whisper.transcribe_pcm_with(pcm, opts, prompt.as_deref())
        })
    }
}

impl Transcriber for WhisperTranscriber {
    // Beam search on room audio: greedy decoding looped ("I think I think ...") or came back
    // empty on a spoken "hey goose" at the nano.
    fn transcribe(&self, pcm: &[f32]) -> anyhow::Result<String> {
        if self.whisper.should_skip_transcribe() {
            return Ok(String::new());
        }
        self.run(pcm, TranscribeOpts::accurate())
    }

    fn scan(&self, pcm: &[f32]) -> anyhow::Result<String> {
        if self.whisper.should_skip_scan() {
            return Ok(String::new());
        }
        self.run(pcm, TranscribeOpts::segment_scan())
    }
}

/// The wake detector `build_listening` would choose, for a status shown before it runs.
pub fn expected_wake(
    settings: &Settings,
    cnn_hears_household: bool,
) -> (WakeDetector, WakeDetectorReason) {
    pond_core::models::domain::wake_detector::household_wake_detector(settings, cnn_hears_household)
}

pub async fn build_listening(mut opts: ListeningOptions<'_>) -> Listening {
    let settings = opts.settings;
    let wake_word = opts
        .wake_word
        .unwrap_or(settings.voice_wake_word.as_str())
        .to_string();
    if settings.vad_backend.eq_ignore_ascii_case("rms") {
        return whisper_front(opts, wake_word, WhyNotTheEar::EnergyGateChosen).await;
    }
    let Some(silero) = model_download::ensure_silero_model(opts.data_dir).await else {
        return whisper_front(opts, wake_word, WhyNotTheEar::NoSilero).await;
    };

    let (detector, reason) = choose_wake_detector(
        &settings.voice_wake_detector,
        &wake_word,
        opts.cnn_hears_household,
    );
    let mode = match detector {
        WakeDetector::HeyGoose => WakeMode::HeyGoose,
        WakeDetector::Whisper => WakeMode::Segments,
    };
    let triggers =
        pond_voice::text::wake_triggers(&wake_word, &settings.voice_wake_word_transcriptions);
    let mut cfg = EarConfig::new(mode, triggers, format!("say \"{wake_word}\""));
    cfg.cue = Some(Arc::new(pond_audio_out::play_wake_ping));
    cfg.audio_level = opts.audio_level.clone();
    cfg.speaker = opts.speaker.take();
    let transcriber: Arc<dyn Transcriber> = Arc::new(WhisperTranscriber {
        whisper: opts.whisper.clone(),
        prompt: opts.whisper_prompt.clone(),
    });
    let mic = opts.mic.clone();
    // Loading converts Silero once and warms every ring: CPU work, off the runtime's workers.
    let started = tokio::task::spawn_blocking(move || {
        let vad = pond_adapters_ear::load_vad(&silero)?;
        EarRig::start(cfg, vad, mic, transcriber)
    })
    .await;
    let ear = match started {
        Ok(Ok(ear)) => ear,
        Ok(Err(e)) => {
            tracing::warn!(
                error = format!("{e:#}"),
                "room voice: the ear did not start"
            );
            return whisper_front(opts, wake_word, WhyNotTheEar::EarFailed).await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "room voice: the ear panicked while starting");
            return whisper_front(opts, wake_word, WhyNotTheEar::EarFailed).await;
        }
    };
    pond_audio_out::set_playback_tap(Some(ear.tap()));
    // Scheduled speech goes through the audio bus, not the loop.
    pond_audio_out::set_spoken_tap(Some(ear.spoken_hook()));
    tracing::info!(detector = ?detector, reason = ?reason, "room voice: the ear listens");

    let wake: Arc<dyn StreamingWakeWordDetector> = if opts.no_wake_word {
        Arc::new(InstantActivation)
    } else {
        ear.clone()
    };
    Listening {
        input: ear.clone(),
        wake,
        interrupt: Some(ear.clone()),
        energy: None,
        state_observer: Some(ear.state_hook()),
        spoken_observer: Some(ear.spoken_hook()),
        front: Front::Ear { detector, reason },
        ear_parts: ear.parts(),
        ear: Some(ear.clone()),
        wake_word,
        calibrated: settings.voice_wake_word_transcriptions.len(),
    }
}

/// The Whisper detector on a timer, energy gates and the energy barge-in.
async fn whisper_front(
    opts: ListeningOptions<'_>,
    wake_word: String,
    why: WhyNotTheEar,
) -> Listening {
    let settings = opts.settings;
    let backend = opts
        .whisper
        .get()
        .expect("Whisper front end requires a loadable Whisper model");
    // Set on the shared backend, so both voice input and the wake-word detector use it.
    if let Some(detector) = build_speech_detector(&settings.vad_backend, opts.data_dir).await {
        backend.set_speech_detector(detector);
    }
    let wake: Arc<dyn StreamingWakeWordDetector> = if opts.no_wake_word {
        Arc::new(InstantActivation)
    } else {
        let kws_config = KeywordDetectorConfig {
            energy_threshold: settings.voice_kws_energy_threshold,
            post_trigger_silence_ms: settings.voice_kws_post_trigger_silence_ms,
            cooldown_ms: settings.voice_kws_cooldown_ms,
            ..KeywordDetectorConfig::default()
        };
        let mut builder = WhisperKeywordDetector::new(
            backend.clone() as Arc<dyn WhisperBackend>,
            wake_word.as_str(),
            opts.mic.clone(),
        )
        .with_transcriptions(settings.voice_wake_word_transcriptions.clone())
        .with_config(kws_config);
        if let Some(sink) = &opts.audio_level {
            builder = builder.with_audio_level_sink(sink.clone());
        }
        let detector = Arc::new(builder);
        // The command clip contains the wake word; strip exactly the triggers the detector used.
        backend.set_wake_words(detector.triggers());
        detector
    };
    Listening {
        input: backend.clone(),
        wake,
        interrupt: None,
        // Required for barge-in: the default `NoEnergy` is inert. It opens the mic while the
        // reply plays and hands what it hears to the next capture.
        energy: Some(Arc::new(pond_audio::MicEnergy::new(opts.mic))),
        state_observer: None,
        spoken_observer: None,
        front: Front::Whisper(why),
        ear_parts: Vec::new(),
        ear: None,
        wake_word,
        calibrated: settings.voice_wake_word_transcriptions.len(),
    }
}

/// Returns the Silero VAD, or `None` (energy gate) on any failure: a worse VAD, never a deaf pond.
/// Diagnostics use stderr: `out!` is a no-op under `--json-events`, the desktop's only mode.
pub async fn build_speech_detector(
    vad_backend: &str,
    data_dir: &Path,
) -> Option<Box<dyn pond_voice::dsp::SpeechDetector + Send>> {
    if vad_backend.eq_ignore_ascii_case("rms") {
        // The escape hatch, for a board whose ONNX Runtime is broken.
        return None;
    }
    if !vad_backend.eq_ignore_ascii_case("silero") {
        // A hand-edited DB can land here: `apply_key` skips the API's `VAD_BACKENDS` validation.
        eprintln!("  Listen   unknown vad_backend \"{vad_backend}\" — using the energy gate.");
        return None;
    }

    // Fetched before `ready`, unlike Kokoro: it's 2 MB and `chat --voice` must work with no server.
    let path = model_download::ensure_silero_model(data_dir).await?;

    // Bounded: with no dylib, ort's `load-dynamic` init hangs forever instead of failing.
    const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let loading = tokio::time::timeout(
        LOAD_TIMEOUT,
        tokio::task::spawn_blocking(move || pond_adapters_silero::SileroDetector::new(&path)),
    )
    .await;

    match loading {
        Ok(Ok(Ok(detector))) => {
            tracing::info!("silero VAD active");
            Some(Box::new(detector) as Box<dyn pond_voice::dsp::SpeechDetector + Send>)
        }
        Ok(Ok(Err(e))) => {
            eprintln!("  Listen   silero VAD failed to load: {e}");
            eprintln!("           Using the energy gate.");
            None
        }
        Ok(Err(e)) => {
            eprintln!("  Listen   silero VAD load panicked: {e}");
            None
        }
        Err(_) => {
            eprintln!(
                "  Listen   silero VAD load timed out after {}s — the ONNX Runtime is \
                 probably missing or version-incompatible.",
                LOAD_TIMEOUT.as_secs()
            );
            eprintln!("           Using the energy gate.");
            None
        }
    }
}
