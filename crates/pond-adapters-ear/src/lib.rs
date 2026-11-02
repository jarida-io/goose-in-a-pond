//! The ear behind the voice ports. One `pond_ear::Ear` runs on its own thread with a permanent
//! microphone claim, fed the microphone and everything the pond plays, and serves the voice loop
//! as its wake detector, its voice input and its interrupt watch.

mod hearing;
mod reference;
mod worker;

pub use hearing::Hearing;

use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_audio::MicHandle;
use pond_core::models::ports::interrupt::{Interrupt, InterruptWatch};
use pond_core::models::ports::voice_input::VoiceInput;
use pond_core::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use pond_core::shared::domain::agent::{ThrottledAudioLevelSink, WorkflowState};
use pond_core::shared::services::chat::StateObserver;
use pond_ear::ear::WakeConfig;
use pond_ear::{Ear, Household, ModelFile};
use tokio::sync::oneshot;

use worker::{Cmd, Heard, ListenFrom, RefChunk, Segment, Woke, Worker, WorkerState};

/// Speech-to-text for what the ear endpoints.
pub trait Transcriber: Send + Sync {
    /// 16 kHz mono f32 to text, Whisper's artifacts stripped and the wake phrase left in; blocking.
    fn transcribe(&self, pcm: &[f32]) -> Result<String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeMode {
    /// The ear's "hey goose" CNN.
    HeyGoose,
    /// Whisper on each speech segment the ear endpoints, for any phrase.
    Segments,
}

/// Played when the wake word is heard.
pub type Cue = Arc<dyn Fn() + Send + Sync>;

/// Mono f32 at a sample rate, as `pond_audio_out::PlaybackTap` delivers it.
pub type Tap = Arc<dyn Fn(&[f32], u32) + Send + Sync>;

/// A longer segment is matched on its opening only: the wake phrase starts a request, and a
/// television talking for half a minute is not worth transcribing whole.
pub const WAKE_SCAN_SECONDS: usize = 6;

pub struct EarConfig {
    pub wake: WakeMode,
    /// Normalized forms of the wake phrase (`pond_voice::text::wake_triggers`): matched in
    /// segments and stripped from the front of every transcript.
    pub triggers: Vec<String>,
    /// Shown while the loop waits for the wake word.
    pub prompt: String,
    /// The least time from the mixer pulling a sample to the microphone hearing it. The
    /// canceller's delay search covers the rest, but only after the stamp, never before.
    pub output_latency_ms: u32,
    pub cue: Option<Cue>,
    pub audio_level: Option<Arc<ThrottledAudioLevelSink>>,
}

impl EarConfig {
    pub fn new(wake: WakeMode, triggers: Vec<String>, prompt: impl Into<String>) -> EarConfig {
        EarConfig {
            wake,
            triggers,
            prompt: prompt.into(),
            output_latency_ms: 10,
            cue: None,
            audio_level: None,
        }
    }
}

/// Silero as the ear loads it: converted once to `.vmw` beside the ONNX, since parsing ONNX in a
/// long-lived process grows it by three times the model.
pub fn load_vad(silero_onnx: &Path) -> Result<ModelFile> {
    let vmw = pond_ear::convert::ensure_vmw(silero_onnx)
        .with_context(|| format!("converting {}", silero_onnx.display()))?;
    ModelFile::read(&vmw).with_context(|| format!("loading {}", vmw.display()))
}

enum Primed {
    Woke(Woke),
    Audio(Vec<f32>),
    Onset(Instant),
}

pub struct EarRig {
    cmds: Sender<Cmd>,
    refs: Sender<RefChunk>,
    triggers: Vec<String>,
    prompt: String,
    transcriber: Arc<dyn Transcriber>,
    last_wake: Mutex<Option<Woke>>,
    primed: Mutex<Option<Primed>>,
}

impl EarRig {
    /// Start the ear on `mic` with Silero `vad`; call inside the runtime. It claims the mic until
    /// the rig is dropped, so the ear's clock never has a gap.
    pub fn start(
        cfg: EarConfig,
        vad: ModelFile,
        mic: MicHandle,
        transcriber: Arc<dyn Transcriber>,
    ) -> Result<Arc<EarRig>> {
        let wake = WakeConfig::hey_goose().context("loading the wake model")?;
        let mut ear_cfg = pond_ear::Config::new(wake, vad);
        ear_cfg.segments = cfg.wake == WakeMode::Segments;
        let ear = Ear::new(ear_cfg, Household::default()).context("starting the ear")?;
        EarRig::with_hearing(cfg, Box::new(ear), mic, transcriber)
    }

    pub(crate) fn with_hearing(
        cfg: EarConfig,
        hearing: Box<dyn Hearing>,
        mic: MicHandle,
        transcriber: Arc<dyn Transcriber>,
    ) -> Result<Arc<EarRig>> {
        let (cmds, cmd_rx) = std::sync::mpsc::channel();
        let (refs, ref_rx) = std::sync::mpsc::channel();
        let segments = match cfg.wake {
            WakeMode::Segments => {
                let runtime = tokio::runtime::Handle::try_current()
                    .map_err(|_| anyhow!("the ear's Whisper wake needs a Tokio runtime"))?;
                let (tx, rx) = tokio::sync::mpsc::channel::<Segment>(1);
                runtime.spawn(worker::match_segments(
                    rx,
                    cmds.clone(),
                    transcriber.clone(),
                    cfg.triggers.clone(),
                ));
                Some(tx)
            }
            WakeMode::HeyGoose => None,
        };
        let worker = Worker {
            hearing,
            mic,
            cmds: cmd_rx,
            refs: ref_rx,
            segments,
            latency: (cfg.output_latency_ms as usize) * reference::RATE as usize / 1000,
            cue: cfg.cue,
            audio_level: cfg.audio_level,
            state: WorkerState::new(),
        };
        std::thread::Builder::new()
            .name("pond-ear".into())
            .spawn(move || worker.run())
            .context("spawning the ear thread")?;
        Ok(Arc::new(EarRig {
            cmds,
            refs,
            triggers: cfg.triggers,
            prompt: cfg.prompt,
            transcriber,
            last_wake: Mutex::new(None),
            primed: Mutex::new(None),
        }))
    }

    /// For `pond_audio_out::set_playback_tap`: what the speaker plays, for the echo canceller.
    /// It only stamps and queues, as the audio thread needs.
    pub fn tap(&self) -> Tap {
        let refs = self.refs.clone();
        Arc::new(move |pcm: &[f32], rate: u32| {
            let _ = refs.send(RefChunk {
                at: Instant::now(),
                rate,
                pcm: pcm.to_vec(),
            });
        })
    }

    /// For `ChatService::with_state_observer`: moves the ear through Think and Speak with the
    /// loop, and back to Wait.
    pub fn state_hook(&self) -> StateObserver {
        let cmds = self.cmds.clone();
        Arc::new(move |state: WorkflowState| {
            let _ = cmds.send(Cmd::Loop(state));
        })
    }

    /// Who Voice Match may recognise.
    pub fn set_household(&self, household: Household) {
        let _ = self.cmds.send(Cmd::Household(household));
    }

    fn command(&self, raw: &str) -> String {
        pond_voice::text::strip_leading_wake_word(raw, &self.triggers)
    }

    async fn transcribe(&self, pcm: Vec<f32>) -> Result<String> {
        let transcriber = self.transcriber.clone();
        tokio::task::spawn_blocking(move || transcriber.transcribe(&pcm))
            .await
            .context("transcription panicked")?
    }

    async fn heard(&self, from: ListenFrom) -> Result<Heard> {
        let (reply, heard) = oneshot::channel();
        self.cmds
            .send(Cmd::Listen { from, reply })
            .map_err(|_| anyhow!("the ear has stopped"))?;
        heard.await.map_err(|_| anyhow!("the ear has stopped"))
    }
}

impl Drop for EarRig {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Shutdown);
    }
}

/// The wake in `segment`, if its opening holds a trigger; the whole is transcribed only then.
pub(crate) fn wake_in(
    transcriber: &dyn Transcriber,
    triggers: &[String],
    segment: Segment,
) -> Result<Option<Woke>> {
    let scan = segment
        .pcm
        .len()
        .min(WAKE_SCAN_SECONDS * reference::RATE as usize);
    let opening = transcriber.transcribe(&segment.pcm[..scan])?;
    if !pond_voice::text::heard_wake(&opening, triggers) {
        tracing::debug!(heard = %opening, "ear: segment without the wake phrase");
        return Ok(None);
    }
    let transcript = if scan < segment.pcm.len() {
        transcriber.transcribe(&segment.pcm)?
    } else {
        opening
    };
    tracing::info!(heard = %transcript, "ear: wake phrase in a segment");
    Ok(Some(Woke {
        start: segment.start,
        end: segment.end,
        pcm: segment.pcm,
        speaker: None,
        transcript: Some(transcript),
    }))
}

#[async_trait]
impl StreamingWakeWordDetector for EarRig {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        let (reply, woke) = oneshot::channel();
        self.cmds
            .send(Cmd::Wake { reply })
            .map_err(|_| anyhow!("the ear has stopped"))?;
        let woke = woke
            .await
            .map_err(|_| anyhow!("the ear has stopped"))?
            .map_err(|e| anyhow!(e))?;
        let wav = pond_voice::dsp::encode_wav_pcm16(
            &pond_voice::dsp::f32_to_pcm16(&woke.pcm),
            reference::RATE,
        );
        let speaker = woke.speaker.clone();
        *self.last_wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(woke);
        Ok(WakeWordActivation {
            captured_audio: Some(wav),
            speaker,
            endpointed: true,
        })
    }

    fn activation_prompt(&self) -> &str {
        &self.prompt
    }

    /// The interrupt watch covers turns; racing this too would take the same wake twice.
    fn supports_interruption(&self) -> bool {
        false
    }
}

#[async_trait]
impl VoiceInput for EarRig {
    async fn listen(&self) -> Result<Option<String>> {
        let primed = self.primed.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut from = ListenFrom::Now;
        match primed {
            Some(Primed::Woke(woke)) => {
                let raw = match woke.transcript {
                    Some(t) => t,
                    None => self.transcribe(woke.pcm).await?,
                };
                let command = self.command(&raw);
                if !command.is_empty() {
                    return Ok(Some(command));
                }
                // The wake phrase alone: the request comes after it.
                from = ListenFrom::At(woke.end);
            }
            Some(Primed::Audio(pcm)) => {
                let command = self.command(&self.transcribe(pcm).await?);
                if !command.is_empty() {
                    return Ok(Some(command));
                }
            }
            Some(Primed::Onset(onset)) => from = ListenFrom::Since(onset),
            None => {}
        }
        // A second round only for an utterance that was the wake phrase and nothing else.
        for _ in 0..2 {
            let Heard::Utterance {
                end,
                pcm,
                transcript,
                ..
            } = self.heard(from).await?
            else {
                return Ok(Some(String::new()));
            };
            let raw = match transcript {
                Some(t) => t,
                None => self.transcribe(pcm).await?,
            };
            let command = self.command(&raw);
            if !command.is_empty() || !pond_voice::text::heard_wake(&raw, &self.triggers) {
                return Ok(Some(command));
            }
            from = ListenFrom::At(end);
        }
        Ok(Some(String::new()))
    }

    fn prompt(&self) -> &str {
        "listening"
    }

    fn prime_with_captured(&self, wav: Vec<u8>) {
        let primed = match self
            .last_wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            Some(woke) => Primed::Woke(woke),
            None => match pond_voice::dsp::decode_wav(&wav) {
                Ok(decoded) => Primed::Audio(pond_voice::dsp::resample_to_16k(
                    &decoded.samples,
                    decoded.sample_rate,
                )),
                Err(e) => {
                    tracing::warn!(error = %e, "ear: a primed capture that is not a WAV");
                    return;
                }
            },
        };
        *self.primed.lock().unwrap_or_else(|e| e.into_inner()) = Some(primed);
    }

    fn prime_with_onset(&self, onset: Instant) {
        *self.primed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Primed::Onset(onset));
    }
}

#[async_trait]
impl InterruptWatch for EarRig {
    async fn wait_for_interrupt(&self) -> Interrupt {
        let (reply, interrupt) = oneshot::channel();
        if self.cmds.send(Cmd::Interrupt { reply }).is_err() {
            return std::future::pending().await;
        }
        match interrupt.await {
            Ok(interrupt) => interrupt,
            Err(_) => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests;
