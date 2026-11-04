//! One Voice Match enrolment take through an ear of its own: whether the "hey goose" CNN fires on
//! it, and the speaker model's embedding of the whole utterance. Nothing is kept but the result.

use anyhow::{Context, Result};
use pond_ear::ear::{Config, SpeakerConfig, WakeConfig};
use pond_ear::{Ear, Event, Household, ModelFile};

/// The longest take embedded whole; a longer one keeps its newest part.
pub const ENROL_MAX_SECONDS: f32 = 12.0;
const RATE: usize = 16_000;
/// Quiet after the take, so the CNN and the endpointer see the phrase end.
const TAIL_SECONDS: usize = 1;

/// The CNN's verdict on a take, and the embedding when one was asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeHeard {
    pub cnn_heard: bool,
    pub embedding: Option<Vec<f32>>,
}

pub struct TakeEar {
    ear: Ear,
}

impl TakeEar {
    /// Silero `vad` as `load_vad` gives it; `speaker` is Voice Match's `.vmw`, if a print is wanted.
    pub fn new(vad: ModelFile, speaker: Option<ModelFile>) -> Result<TakeEar> {
        let wake = WakeConfig::hey_goose().context("loading the wake model")?;
        let mut cfg = Config::new(wake, vad);
        // A take is the phone's recording: there is no far end to cancel.
        cfg.aec = None;
        cfg.ring_seconds = ENROL_MAX_SECONDS + TAIL_SECONDS as f32 + 1.0;
        cfg.speaker = speaker.map(|model| SpeakerConfig {
            model,
            label: crate::VOICE_MATCH_LABEL.into(),
            max_seconds: ENROL_MAX_SECONDS,
        });
        let ear = Ear::new(cfg, Household::default()).context("starting the enrolment ear")?;
        Ok(TakeEar { ear })
    }

    pub fn has_speaker(&self) -> bool {
        self.ear.has_speaker()
    }

    /// 16 kHz mono PCM; blocking.
    pub fn hear(&mut self, pcm: &[f32], embed: bool) -> Result<TakeHeard> {
        self.ear.reset();
        let longest = (ENROL_MAX_SECONDS * RATE as f32) as usize;
        let take = &pcm[pcm.len().saturating_sub(longest)..];
        let mut events = Vec::new();
        self.ear.push_into(take, &mut events);
        self.ear
            .push_into(&vec![0.0; TAIL_SECONDS * RATE], &mut events);
        let cnn_heard = events.iter().any(|e| matches!(e, Event::Wake { .. }));
        let embedding = match embed {
            true => Some(
                self.ear
                    .embed(0, take.len())
                    .context("embedding the take")?,
            ),
            false => None,
        };
        Ok(TakeHeard {
            cnn_heard,
            embedding,
        })
    }
}
