//! How the pond listens in a room, built from settings and the data directory: the ear (echo
//! cancelled, Silero-endpointed, interrupted only by the wake word or a household voice) where
//! Silero is available, else the Whisper detector with energy gates and barge-in. `chat --voice`
//! builds it, and serve builds the same.

use std::path::Path;
use std::sync::Arc;

use pond_adapters_ear::{EarConfig, EarRig, Transcriber, WakeMode};
use pond_adapters_whisper::{
    KeywordDetectorConfig, WhisperBackend, WhisperKeywordDetector, WhisperRsInput,
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
use pond_core::models::services::voice::quiet;
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use pond_core::shared::services::chat::{ChatService, StateObserver};
use pond_core::user_data::domain::settings::Settings;

use crate::model_download;

pub struct ListeningOptions<'a> {
    pub settings: &'a Settings,
    pub data_dir: &'a Path,
    /// The process's one microphone owner.
    pub mic: &'a MicHandle,
    /// Speech-to-text for either front end; the Whisper one also records through it.
    pub whisper: Arc<WhisperRsInput>,
    /// In place of `settings.voice_wake_word`.
    pub wake_word: Option<&'a str>,
    /// Every turn listens at once, with no wake word.
    pub no_wake_word: bool,
    /// Live microphone level for a client's meter.
    pub audio_level: Option<Arc<ThrottledAudioLevelSink>>,
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
    pub front: Front,
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
        if let Some(energy) = &self.energy {
            chat = chat.with_speech_energy(energy.clone());
        }
        chat
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
struct WhisperTranscriber(Arc<WhisperRsInput>);

impl Transcriber for WhisperTranscriber {
    fn transcribe(&self, pcm: &[f32]) -> anyhow::Result<String> {
        self.0.transcribe_pcm(pcm)
    }
}

/// Whether the CNN heard this household's calibration takes. Nothing records that result yet,
/// so `auto` keeps Whisper.
fn cnn_hears_household(_settings: &Settings) -> bool {
    false
}

pub async fn build_listening(opts: ListeningOptions<'_>) -> Listening {
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
        cnn_hears_household(settings),
    );
    let mode = match detector {
        WakeDetector::HeyGoose => WakeMode::HeyGoose,
        WakeDetector::Whisper => WakeMode::Segments,
    };
    let triggers =
        pond_voice::text::wake_triggers(&wake_word, &settings.voice_wake_word_transcriptions);
    let mut cfg = EarConfig::new(mode, triggers, format!("say \"{wake_word}\""));
    // `installed()` at each wake: the quiet is installed after the voice loop is wired.
    cfg.cue = Some(Arc::new(|| {
        pond_audio_out::play_wake_ping(quiet::installed())
    }));
    cfg.audio_level = opts.audio_level.clone();
    let transcriber: Arc<dyn Transcriber> = Arc::new(WhisperTranscriber(opts.whisper.clone()));
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
        front: Front::Ear { detector, reason },
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
    let backend = opts.whisper;
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
        front: Front::Whisper(why),
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
