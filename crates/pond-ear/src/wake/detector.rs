//! A self-contained streaming wake detector with its own audio and frames: the reference the ear's
//! shared front end must reproduce bit for bit, and a wake-only path for tools.

use std::time::Instant;

use crate::convert::ModelFile;
use crate::engine::exec::{self, Plan};
use crate::engine::fbank::{self, FRAME, SHIFT};
use crate::engine::kernels::Rung;
use crate::Error;

use super::{sigmoid, window_cmn, BINS, HOP_FRAMES, WINDOW_FRAMES, WINDOW_SAMPLES};

const KEEP_AUDIO: usize = 16_000 * 20;
const KEEP_FRAMES: usize = 400;
/// The wake segment reaches 100 ms either side of the speech the energy gate found.
pub const SEGMENT_PAD: usize = 1_600;

#[derive(Clone, Copy, Debug)]
pub struct Score {
    /// Seconds since the stream began, at the end of the scored window.
    pub t: f64,
    pub p: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Trigger {
    pub t: f64,
    pub p: f32,
    /// The wake segment, as absolute sample indices of the stream.
    pub start: usize,
    pub end: usize,
}

pub struct Detector {
    file: ModelFile,
    plan: Plan,
    arena: Vec<f32>,
    pub rung: Rung,
    pub threshold: f32,
    pub cooldown_s: f32,
    audio: Vec<f32>,
    audio_base: usize,
    frames: Vec<f32>,
    frames_base: usize,
    next_frame: usize,
    cooldown_until: usize,
    /// Nanoseconds per scored window, most recent last.
    pub hop_ns: Vec<u64>,
}

impl Detector {
    pub fn new(file: ModelFile, rung: Rung, threshold: f32) -> Result<Detector, Error> {
        let plan = exec::plan(&file.model(), WINDOW_FRAMES).map_err(Error::Plan)?;
        Ok(Detector {
            arena: vec![0f32; plan.arena],
            file,
            plan,
            rung,
            threshold,
            cooldown_s: 2.0,
            audio: Vec::new(),
            audio_base: 0,
            frames: Vec::new(),
            frames_base: 0,
            next_frame: 0,
            cooldown_until: 0,
            hop_ns: Vec::new(),
        })
    }

    pub fn macs(&self) -> u64 {
        self.plan.macs
    }

    /// Score one window of features (already mean-normalised).
    pub fn score(&mut self, feats: &[f32]) -> f32 {
        let mut out = [0f32; 1];
        let t = Instant::now();
        // Planned for this model and window at construction, so it cannot fail.
        let _ = exec::run(
            &self.file.model(),
            &self.plan,
            self.rung,
            feats,
            &mut self.arena,
            &mut out,
            None,
        );
        self.hop_ns.push(t.elapsed().as_nanos() as u64);
        if self.hop_ns.len() > 2000 {
            self.hop_ns.drain(..1000);
        }
        sigmoid(out[0])
    }

    /// Audio of the stream between absolute samples, if still held.
    pub fn audio(&self, start: usize, end: usize) -> Vec<f32> {
        let s = start.max(self.audio_base) - self.audio_base;
        let e = end.max(self.audio_base) - self.audio_base;
        self.audio[s.min(self.audio.len())..e.min(self.audio.len())].to_vec()
    }

    pub fn push(&mut self, pcm: &[f32]) -> (Vec<Score>, Vec<Trigger>) {
        self.audio.extend_from_slice(pcm);
        let (mut scores, mut triggers) = (Vec::new(), Vec::new());
        let mut frame = [0f32; BINS];
        loop {
            let need = self.next_frame * SHIFT + FRAME;
            if need > self.audio_base + self.audio.len() {
                break;
            }
            let off = self.next_frame * SHIFT - self.audio_base;
            fbank::frame(&self.audio[off..], &mut frame);
            self.frames.extend_from_slice(&frame);
            self.next_frame += 1;

            let n = self.next_frame;
            if n >= WINDOW_FRAMES && (n - WINDOW_FRAMES).is_multiple_of(HOP_FRAMES) {
                let first = n - WINDOW_FRAMES - self.frames_base;
                let mut w = self.frames[first * BINS..(first + WINDOW_FRAMES) * BINS].to_vec();
                window_cmn(&mut w);
                let p = self.score(&w);
                let end = (n - 1) * SHIFT + FRAME;
                let t = end as f64 / 16_000.0;
                scores.push(Score { t, p });
                if p >= self.threshold && n >= self.cooldown_until {
                    let start = end.saturating_sub(WINDOW_SAMPLES);
                    let win = self.audio(start, end);
                    let q = crate::dsp::quality(&win);
                    let (s, e) = if q.speech_end > q.speech_start {
                        (start + q.speech_start, start + q.speech_end)
                    } else {
                        (start, end)
                    };
                    triggers.push(Trigger {
                        t,
                        p,
                        start: s.saturating_sub(SEGMENT_PAD).max(start),
                        end: (e + SEGMENT_PAD).min(end),
                    });
                    self.cooldown_until = n + (self.cooldown_s * 100.0) as usize;
                }
            }
        }
        if self.audio.len() > KEEP_AUDIO + 16_000 {
            let drop = self.audio.len() - KEEP_AUDIO;
            self.audio.drain(..drop);
            self.audio_base += drop;
        }
        let held = self.frames.len() / BINS;
        if held > KEEP_FRAMES + 100 {
            let drop = held - KEEP_FRAMES;
            self.frames.drain(..drop * BINS);
            self.frames_base += drop;
        }
        (scores, triggers)
    }
}
