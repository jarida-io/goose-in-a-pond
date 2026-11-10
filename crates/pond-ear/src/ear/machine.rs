//! The ear's state machine and clock. Everything runs on the caller's thread in `push`; nothing
//! allocates per chunk except the events it returns.

use std::time::Instant;

use crate::convert::ModelFile;
use crate::engine::fbank::{FRAME, SHIFT};
use crate::engine::kernels::Rung;
use crate::engine::silero;
use crate::score::{identify, Cohort, Policy, Print};
use crate::Error;

use super::aec::{Aec, AecConfig};
use super::endpoint::{Endpoint, Endpointer, Outcome, Segmenter};
use super::event::Event;
use super::front::{Front, RATE};
use super::parts::{SpeakerNet, VadSilero, WakeCnn};
use super::{Bytes, Part};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Listening for the wake word only.
    Wait,
    /// Someone is talking to the pond.
    Listen,
    /// The reply is being worked out.
    Think,
    /// The reply is playing: barge-ins are reported.
    Speak,
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Wait => "wait",
            State::Listen => "listen",
            State::Think => "think",
            State::Speak => "speak",
        }
    }
}

/// Who may be recognised, and how sure Voice Match must be.
#[derive(Clone, Debug, Default)]
pub struct Household {
    pub prints: Vec<Print>,
    pub cohort: Cohort,
    pub policy: Policy,
}

pub struct WakeConfig {
    pub model: ModelFile,
    pub label: String,
    pub threshold: f32,
}

impl WakeConfig {
    /// The embedded "hey goose" detector at its trained threshold.
    pub fn hey_goose() -> Result<WakeConfig, Error> {
        Ok(WakeConfig {
            model: crate::wake::hey_goose()?,
            label: crate::wake::HEY_GOOSE_PHRASE.into(),
            threshold: crate::wake::HEY_GOOSE_THRESHOLD,
        })
    }
}

pub struct SpeakerConfig {
    pub model: ModelFile,
    pub label: String,
    /// Longest span Voice Match embeds; fixes its arena.
    pub max_seconds: f32,
}

/// Audio and frames the ear keeps: past the endpointer's 30 s longest utterance, so `audio` still
/// holds an utterance's start when it ends.
pub const RING_SECONDS: f32 = 32.0;
/// The newest 3 s of a span is all Voice Match reads.
pub const SPEAKER_MAX_SECONDS: f32 = 3.0;
/// Speech over the reply for this long is a barge-in.
pub const BARGE_IN_MS: u32 = 300;
/// Pauses shorter than this inside a barge-in are between syllables, not the end of it (the
/// official Silero `min_silence_duration_ms`).
pub const BARGE_IN_GAP_MS: u32 = 100;
/// Pushes are cut to this, so one huge push behaves exactly like many small ones.
const MAX_PUSH: usize = 8_000;
/// Score history kept for whoever reads it (a chart), dropped oldest-first when nobody does.
const SCORE_HISTORY: usize = 4_096;

pub struct Config {
    pub wake: WakeConfig,
    /// Silero VAD v5; the endpoint rule needs it.
    pub vad: ModelFile,
    /// Voice Match; without it nobody is recognised.
    pub speaker: Option<SpeakerConfig>,
    pub rung: Rung,
    pub ring_seconds: f32,
    pub endpoint: Endpoint,
    pub barge_in_ms: u32,
    /// The echo canceller; `None` feeds the microphone straight to the front end.
    pub aec: Option<AecConfig>,
    /// Report each speech segment outside Listen (`Event::Segment`) for a wake detector on the
    /// host, in place of the wake CNN, which then never runs.
    pub segments: bool,
}

impl Config {
    pub fn new(wake: WakeConfig, vad: ModelFile) -> Config {
        Config {
            wake,
            vad,
            speaker: None,
            rung: Rung::fastest(),
            ring_seconds: RING_SECONDS,
            endpoint: Endpoint::adaptive(),
            barge_in_ms: BARGE_IN_MS,
            aec: Some(AecConfig::default()),
            segments: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Barge {
    onset: Option<usize>,
    last_voice: usize,
    fired: bool,
}

pub struct Ear {
    pub rung: Rung,
    aec: Option<Aec>,
    clean: Vec<f32>,
    mic_samples: usize,
    front: Front,
    /// `None` with `segments` on: the host's wake detector reads the segments, so the CNN would
    /// only hold its weights and arena.
    wake: Option<WakeCnn>,
    speaker: Option<SpeakerNet>,
    vad: VadSilero,
    endpointer: Endpointer,
    segmenter: Option<Segmenter>,
    household: Household,
    state: State,
    barge_in_ms: u32,
    barge: Barge,
    max_push: usize,
    scores: Vec<(f64, f32)>,
    aec_ns: u64,
}

impl Ear {
    pub fn new(cfg: Config, household: Household) -> Result<Ear, Error> {
        let wake = match cfg.segments {
            true => None,
            false => Some(WakeCnn::new(
                cfg.wake.model,
                &cfg.wake.label,
                cfg.wake.threshold,
            )?),
        };
        let vad = VadSilero::new(cfg.vad, "Silero v5")?;
        let speaker = match cfg.speaker {
            Some(s) => Some(SpeakerNet::new(s.model, &s.label, s.max_seconds)?),
            None => None,
        };
        let front = Front::new(cfg.ring_seconds);
        let max_push = MAX_PUSH.min(front.audio_capacity() / 4).max(1);
        let mut ear = Ear {
            rung: cfg.rung,
            aec: cfg.aec.map(Aec::new),
            clean: Vec::with_capacity(MAX_PUSH + aec_slack()),
            mic_samples: 0,
            front,
            wake,
            speaker,
            vad,
            endpointer: Endpointer::new(cfg.endpoint),
            segmenter: cfg.segments.then(Segmenter::default),
            household,
            state: State::Wait,
            barge_in_ms: cfg.barge_in_ms,
            barge: Barge::default(),
            max_push,
            scores: Vec::with_capacity(SCORE_HISTORY),
            aec_ns: 0,
        };
        ear.warm_up();
        Ok(ear)
    }

    /// Touch every arena and ring once, so what the OS reports after loading is what the ear holds
    /// when running.
    fn warm_up(&mut self) {
        if let Some(w) = self.wake.as_mut() {
            w.warm_up(self.rung);
        }
        if let Some(s) = self.speaker.as_mut() {
            s.warm_up(self.rung);
        }
        let silence = vec![0f32; self.max_push];
        let mut left = self.front.audio_capacity();
        while left > 0 {
            let n = left.min(silence.len());
            self.front.push(&silence[..n]);
            while self.vad.step(&self.front, self.rung).is_some() {}
            left -= n;
        }
        self.reset();
    }

    /// Start a new stream: forget audio, frames, cooldowns, state and the room.
    pub fn reset(&mut self) {
        if let Some(a) = self.aec.as_mut() {
            a.reset();
        }
        self.clean.clear();
        self.mic_samples = 0;
        self.front.reset();
        if let Some(w) = self.wake.as_mut() {
            w.reset();
        }
        if let Some(s) = self.speaker.as_mut() {
            s.reset();
        }
        self.vad.reset();
        self.endpointer.reset();
        if let Some(s) = self.segmenter.as_mut() {
            s.reset();
        }
        self.state = State::Wait;
        self.barge = Barge::default();
        self.scores.clear();
        self.aec_ns = 0;
    }

    pub fn set_household(&mut self, h: Household) {
        self.household = h;
    }

    /// Voice Match's network, loaded and warmed elsewhere, so a running ear starts recognising
    /// without a restart.
    pub fn set_speaker(&mut self, speaker: SpeakerNet) {
        self.speaker = Some(speaker);
    }

    pub fn household(&self) -> &Household {
        &self.household
    }

    pub fn has_speaker(&self) -> bool {
        self.speaker.is_some()
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpointer.cfg
    }

    pub fn set_endpoint(&mut self, e: Endpoint) {
        self.endpointer.cfg = e;
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Whether the ear reports segments for a host-side wake detector instead of running its CNN.
    pub fn segments(&self) -> bool {
        self.segmenter.is_some()
    }

    pub fn front(&self) -> &Front {
        &self.front
    }

    pub fn aec(&self) -> Option<&Aec> {
        self.aec.as_ref()
    }

    /// Microphone samples pushed so far: the clock `play` stamps against.
    pub fn mic_samples(&self) -> usize {
        self.mic_samples
    }

    /// Seconds of audio the parts have seen (the echo canceller holds back `aec::LATENCY`).
    pub fn audio_seconds(&self) -> f64 {
        self.front.samples() as f64 / RATE as f64
    }

    /// Wake scores since the last call, `(t, p)` per 80 ms hop.
    pub fn take_scores(&mut self) -> Vec<(f64, f32)> {
        std::mem::take(&mut self.scores)
    }

    /// The echo-cancelled audio between absolute samples, clamped to what the ring still holds.
    pub fn audio(&self, start: usize, end: usize, out: &mut Vec<f32>) {
        self.front.audio(start, end, out);
    }

    /// The far-end audio, 16 kHz, whose first sample plays when `mic_samples()` reads `at`.
    pub fn play(&mut self, at: usize, pcm: &[f32]) {
        if let Some(a) = self.aec.as_mut() {
            a.play(at, pcm);
        }
    }

    fn now(&self) -> f64 {
        self.front.samples() as f64 / RATE as f64
    }

    fn change(&mut self, to: State, t: f64) -> Option<Event> {
        (to != self.state).then(|| {
            let e = Event::State {
                t,
                from: self.state,
                to,
            };
            self.state = to;
            e
        })
    }

    /// The host moves the ear through Think and Speak (and back to Wait); a turn being listened to
    /// is dropped. Listening is started with `listen`.
    pub fn set_state(&mut self, to: State) -> Option<Event> {
        if to == State::Listen {
            return self.listen(self.front.samples());
        }
        self.endpointer.cancel();
        self.barge = Barge::default();
        let t = self.now();
        self.change(to, t)
    }

    /// Listen from absolute sample `start` with no wake phrase: push-to-talk, or the words that
    /// barged in (`start` from their `BargeIn`).
    pub fn listen(&mut self, start: usize) -> Option<Event> {
        self.endpointer
            .listen(start.max(self.front.oldest_sample()));
        if let Some(s) = self.segmenter.as_mut() {
            s.cancel();
        }
        self.barge = Barge::default();
        let t = self.now();
        self.change(State::Listen, t)
    }

    /// Feed 16 kHz mono microphone audio in any chunk size; returns what happened.
    pub fn push(&mut self, mic: &[f32]) -> Vec<Event> {
        let mut out = Vec::new();
        self.push_into(mic, &mut out);
        out
    }

    /// `push`, appending to the caller's event buffer.
    pub fn push_into(&mut self, mic: &[f32], out: &mut Vec<Event>) {
        for chunk in mic.chunks(self.max_push) {
            self.mic_samples += chunk.len();
            match self.aec.as_mut() {
                Some(aec) => {
                    let t = Instant::now();
                    let mut clean = std::mem::take(&mut self.clean);
                    clean.clear();
                    aec.process(chunk, &mut clean);
                    self.aec_ns += t.elapsed().as_nanos() as u64;
                    self.feed(&clean, out);
                    self.clean = clean;
                }
                None => self.feed(chunk, out),
            }
        }
    }

    /// The wake detector (every 80 ms) and voice activity (every 32 ms) run in time order, so the
    /// state machine sees events as they happened, whatever the chunking.
    fn feed(&mut self, pcm: &[f32], out: &mut Vec<Event>) {
        let new = self.front.push(pcm);
        let mut frame = new.start;
        loop {
            let wake_due = (frame < new.end).then(|| frame * SHIFT + FRAME);
            let vad_end = self.vad.next_end();
            let vad_due = (vad_end <= self.front.samples()).then_some(vad_end);
            match (wake_due, vad_due) {
                (Some(w), v) if v.is_none_or(|v| w <= v) => {
                    frame += 1;
                    self.on_wake_frame(frame, out);
                }
                (_, Some(_)) => {
                    if let Some((end, p)) = self.vad.step(&self.front, self.rung) {
                        self.on_vad(end, p, out);
                    }
                }
                _ => break,
            }
        }
        trim(&mut self.scores);
    }

    fn on_wake_frame(&mut self, n: usize, out: &mut Vec<Event>) {
        let Some(wake) = self.wake.as_mut() else {
            return;
        };
        let Some(hop) = wake.on_frame(&self.front, n, self.rung) else {
            return;
        };
        self.scores.push((hop.t, hop.p));
        let Some((s, e)) = hop.trigger else {
            return;
        };
        let (who, score) = self.identify(s, e);
        out.push(Event::Wake {
            t: hop.t,
            p: hop.p,
            start: s,
            end: e,
            who,
            score,
        });
        self.endpointer.wake(s, e);
        self.barge = Barge::default();
        out.extend(self.change(State::Listen, hop.t));
    }

    fn on_vad(&mut self, end: usize, p: f32, out: &mut Vec<Event>) {
        let t = end as f64 / RATE as f64;
        let listening = self.endpointer.listening();
        let step = self.endpointer.vad(end, p);
        if let Some(on) = step.speech {
            if self.state != State::Wait {
                out.push(Event::Speech { t, on });
            }
        }
        if self.state == State::Speak {
            self.barge_in(end, t, out);
        }
        let (speaking, cfg) = (self.endpointer.speaking(), self.endpointer.cfg);
        if let Some(s) = self.segmenter.as_mut() {
            if listening {
                s.skip(end);
            } else if let Some((start, end, cut)) = s.step(&cfg, end, speaking) {
                out.push(Event::Segment { t, start, end, cut });
            }
        }
        match step.outcome {
            Some(Outcome::Utterance { start, end }) => {
                out.push(Event::Utterance { t, start, end });
                out.extend(self.change(State::Wait, t));
            }
            Some(Outcome::Silence) => {
                out.push(Event::Silence { t });
                out.extend(self.change(State::Wait, t));
            }
            None => {}
        }
    }

    fn barge_in(&mut self, end: usize, t: f64, out: &mut Vec<Event>) {
        let ms = |samples: usize| samples as u64 * 1000 / RATE as u64;
        if !self.endpointer.speaking() {
            if ms(end - self.barge.last_voice.min(end)) > BARGE_IN_GAP_MS as u64 {
                self.barge = Barge::default();
            }
            return;
        }
        self.barge.last_voice = end;
        let onset = *self
            .barge
            .onset
            .get_or_insert(end.saturating_sub(silero::CHUNK));
        if !self.barge.fired && ms(end - onset) >= self.barge_in_ms as u64 {
            self.barge.fired = true;
            let (who, score) = self.identify(onset, end);
            out.push(Event::BargeIn {
                t,
                start: onset,
                end,
                who,
                score,
            });
        }
    }

    /// Voice Match on absolute samples `[start, end)`: the accepted print's id, and the best raw
    /// score (NaN with nobody enrolled, no speaker model, or a span too short to embed).
    pub fn identify(&mut self, start: usize, end: usize) -> (Option<String>, f32) {
        let Some(speaker) = self.speaker.as_mut() else {
            return (None, f32::NAN);
        };
        if self.household.prints.is_empty() {
            return (None, f32::NAN);
        }
        let (f0, f1) = frames_inside(start, end);
        let Ok(e) = speaker.embed(&self.front, f0, f1, self.rung) else {
            return (None, f32::NAN);
        };
        let h = &self.household;
        let d = identify(e, &h.prints, &h.cohort, &h.policy);
        let best = d.scores.first().map(|s| s.raw).unwrap_or(f32::NAN);
        (d.accepted, best)
    }

    /// Voice Match's embedding of absolute samples `[start, end)`: enrolment from the ear's own ring.
    pub fn embed(&mut self, start: usize, end: usize) -> Result<Vec<f32>, Error> {
        let Some(speaker) = self.speaker.as_mut() else {
            return Err(Error::WrongModel("no speaker model loaded".into()));
        };
        let (f0, f1) = frames_inside(start, end);
        speaker
            .embed(&self.front, f0, f1, self.rung)
            .map(<[f32]>::to_vec)
    }

    /// RAM and CPU per part, in the order audio flows through them.
    pub fn parts(&self) -> Vec<Part> {
        let mut parts = vec![Part {
            role: "front end",
            name: format!(
                "{:.0} s ring + fbank stream",
                self.front.audio_capacity() as f32 / RATE as f32
            ),
            bytes: Bytes {
                state: self.front.bytes(),
                ..Default::default()
            },
            ns: self.front.ns,
            calls: self.front.frame_count() as u64,
            macs: 0,
        }];
        if let Some(a) = self.aec.as_ref() {
            let c = a.config();
            parts.insert(
                0,
                Part {
                    role: "echo canceller",
                    name: format!(
                        "PBFDAF {} ms tail, delay search {} ms",
                        c.tail_ms, c.max_delay_ms
                    ),
                    bytes: Bytes {
                        state: a.bytes() + self.clean.capacity() * 4,
                        ..Default::default()
                    },
                    ns: self.aec_ns,
                    calls: (self.mic_samples / super::aec::BLOCK) as u64,
                    macs: 0,
                },
            );
        }
        if let Some(wake) = self.wake.as_ref() {
            parts.push(Part {
                role: "wake",
                name: format!("wake CNN ({})", wake.label),
                bytes: wake.bytes(self.rung),
                ns: wake.ns,
                calls: wake.calls,
                macs: wake.macs(),
            });
        }
        let ep = self.endpointer.cfg;
        parts.push(Part {
            role: "voice activity",
            name: format!(
                "{} (on {:.2}, off {:.2}, end after {} ms)",
                self.vad.label, ep.on, ep.off, ep.end_silence_ms
            ),
            bytes: self.vad.bytes(),
            ns: self.vad.ns,
            calls: self.vad.calls,
            macs: silero::MACS_PER_CALL,
        });
        if let Some(s) = self.speaker.as_ref() {
            parts.push(Part {
                role: "speaker",
                name: format!("Voice Match ({})", s.label),
                bytes: s.bytes(self.rung),
                ns: s.ns,
                calls: s.calls,
                macs: s.last_macs,
            });
        }
        parts
    }
}

/// The fbank frames wholly inside absolute samples `[start, end)`.
fn frames_inside(start: usize, end: usize) -> (usize, usize) {
    let f1 = if end >= FRAME {
        (end - FRAME) / SHIFT + 1
    } else {
        0
    };
    (start.div_ceil(SHIFT), f1)
}

/// The echo canceller emits whole blocks, so a push can yield up to two blocks more than it took.
fn aec_slack() -> usize {
    2 * super::aec::BLOCK
}

fn trim(v: &mut Vec<(f64, f32)>) {
    if v.len() > 2 * SCORE_HISTORY {
        v.drain(..v.len() - SCORE_HISTORY);
    }
}
