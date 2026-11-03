//! One Voice Match enrolment take through an ear of its own: whether the "hey goose" CNN fires on
//! it, the speaker model's embedding of the whole utterance, and the member's wake template.
//! Nothing is kept but the result, and the template is features, never audio.

use anyhow::{Context, Result};
use pond_ear::ear::{Config, SpeakerConfig, WakeConfig};
use pond_ear::{Ear, Event, Household, ModelFile};

/// The longest take embedded whole; a longer one keeps its newest part.
pub const ENROL_MAX_SECONDS: f32 = 12.0;
const RATE: usize = 16_000;
/// Quiet after the take, so the CNN and the endpointer see the phrase end.
const TAIL_SECONDS: usize = 1;
/// Whisper's word times are rough by about this much either side of the phrase.
const SPAN_PAD_SECONDS: f32 = 0.05;
/// What follows the phrase is kept this long, for what the member's other words score.
const REST_SECONDS: f32 = 1.5;

/// The CNN's verdict on a take, and the embedding when one was asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeHeard {
    pub cnn_heard: bool,
    pub embedding: Option<Vec<f32>>,
    /// The phrase's fbank frames and those after it, when the phrase's span was given and usable.
    pub template: Option<(Vec<f32>, Vec<f32>)>,
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

    /// 16 kHz mono PCM; blocking. `span` is where the phrase was said, in seconds from the start.
    pub fn hear(
        &mut self,
        pcm: &[f32],
        embed: bool,
        span: Option<(f32, f32)>,
    ) -> Result<TakeHeard> {
        self.ear.reset();
        let longest = (ENROL_MAX_SECONDS * RATE as f32) as usize;
        let cut = pcm.len().saturating_sub(longest);
        let take = &pcm[cut..];
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
        let template = span.and_then(|(start, end)| {
            let len = end - start;
            let usable = (pond_ear::wake::personal::MIN_PHRASE_S
                ..=pond_ear::wake::personal::MAX_PHRASE_S)
                .contains(&len);
            if !usable {
                tracing::info!(
                    seconds = len,
                    "enrol: the phrase was timed wrong; no template"
                );
                return None;
            }
            let at = |s: f32| ((s.max(0.0) * RATE as f32) as usize).saturating_sub(cut);
            let phrase = self.frames(at(start - SPAN_PAD_SECONDS), at(end + SPAN_PAD_SECONDS));
            let rest = self.frames(at(end + 0.1), at(end + 0.1 + REST_SECONDS));
            (!phrase.is_empty()).then_some((phrase, rest))
        });
        Ok(TakeHeard {
            cnn_heard,
            embedding,
            template,
        })
    }

    /// The ear's fbank frames between samples of the take, as the room's ear computes them.
    fn frames(&self, from: usize, to: usize) -> Vec<f32> {
        use pond_ear::engine::fbank::{NUM_BINS, SHIFT};
        let (f0, f1) = (from / SHIFT, to / SHIFT);
        let mut out = vec![0.0; f1.saturating_sub(f0) * NUM_BINS];
        let n = self.ear.front().frames(f0, f1, &mut out);
        out.truncate(n * NUM_BINS);
        out
    }
}
