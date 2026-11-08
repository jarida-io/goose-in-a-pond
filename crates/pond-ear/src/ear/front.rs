//! The shared front end: rings of 16 kHz audio and of 80-dim fbank frames, addressed by absolute
//! index since the stream began, so a part asks for "frames 300..420" without knowing the wrap.

use crate::engine::fbank::{self, FRAME, NUM_BINS, SHIFT};

pub const RATE: usize = 16_000;

pub struct Front {
    audio: Vec<f32>,
    /// Samples pushed since the stream began.
    n_audio: usize,
    frames: Vec<f32>,
    /// Frames computed since the stream began.
    n_frames: usize,
    scratch: [f32; FRAME],
    pub ns: u64,
}

impl Front {
    /// Rings holding the last `seconds` of audio and of frames.
    pub fn new(seconds: f32) -> Front {
        let samples = (seconds * RATE as f32) as usize;
        let frames = samples / SHIFT;
        Front {
            audio: vec![0.0; samples],
            n_audio: 0,
            frames: vec![0.0; frames * NUM_BINS],
            n_frames: 0,
            scratch: [0.0; FRAME],
            ns: 0,
        }
    }

    pub fn reset(&mut self) {
        self.n_audio = 0;
        self.n_frames = 0;
        self.ns = 0;
    }

    pub fn bytes(&self) -> usize {
        (self.audio.len() + self.frames.len() + FRAME) * 4
    }

    pub fn audio_capacity(&self) -> usize {
        self.audio.len()
    }

    pub fn frame_capacity(&self) -> usize {
        self.frames.len() / NUM_BINS
    }

    /// Samples pushed so far.
    pub fn samples(&self) -> usize {
        self.n_audio
    }

    /// Frames computed so far.
    pub fn frame_count(&self) -> usize {
        self.n_frames
    }

    /// The oldest sample still held.
    pub fn oldest_sample(&self) -> usize {
        self.n_audio.saturating_sub(self.audio.len())
    }

    pub fn oldest_frame(&self) -> usize {
        self.n_frames.saturating_sub(self.frame_capacity())
    }

    /// Append audio and compute every frame now complete, raw (no CMN: each consumer normalises over
    /// its own span). Returns the absolute indices of the new frames.
    pub fn push(&mut self, pcm: &[f32]) -> std::ops::Range<usize> {
        let t = std::time::Instant::now();
        let cap = self.audio.len();
        for &v in pcm {
            self.audio[self.n_audio % cap] = v;
            self.n_audio += 1;
        }
        let first = self.n_frames;
        let fcap = self.frame_capacity();
        while self.n_frames * SHIFT + FRAME <= self.n_audio {
            let start = self.n_frames * SHIFT;
            for (i, s) in self.scratch.iter_mut().enumerate() {
                *s = self.audio[(start + i) % cap];
            }
            let slot = (self.n_frames % fcap) * NUM_BINS;
            fbank::frame(&self.scratch, &mut self.frames[slot..slot + NUM_BINS]);
            self.n_frames += 1;
        }
        self.ns += t.elapsed().as_nanos() as u64;
        first..self.n_frames
    }

    /// Copy samples `[start, end)` into `out` (cleared first), clamped to what is held.
    pub fn audio(&self, start: usize, end: usize, out: &mut Vec<f32>) {
        out.clear();
        let (s, e) = (start.max(self.oldest_sample()), end.min(self.n_audio));
        let cap = self.audio.len();
        out.extend((s..e.max(s)).map(|i| self.audio[i % cap]));
    }

    /// Copy frames `[start, end)` into `out` (row-major `[frame][bin]`), clamped to what is held.
    /// Returns how many frames were copied.
    pub fn frames(&self, start: usize, end: usize, out: &mut [f32]) -> usize {
        let (s, e) = (start.max(self.oldest_frame()), end.min(self.n_frames));
        let fcap = self.frame_capacity();
        let mut n = 0;
        for f in s..e.max(s) {
            if (n + 1) * NUM_BINS > out.len() {
                break;
            }
            let slot = (f % fcap) * NUM_BINS;
            out[n * NUM_BINS..(n + 1) * NUM_BINS]
                .copy_from_slice(&self.frames[slot..slot + NUM_BINS]);
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_a_one_shot_fbank_whatever_the_chunking_and_wrap() {
        let pcm: Vec<f32> = (0..16_000 * 3)
            .map(|i| ((i as f32 * 0.013).sin() + (i as f32 * 0.0007).cos()) * 0.2)
            .collect();
        let n = fbank::num_frames(pcm.len());
        let mut want = vec![0f32; n * NUM_BINS];
        for t in 0..n {
            fbank::frame(
                &pcm[t * SHIFT..],
                &mut want[t * NUM_BINS..(t + 1) * NUM_BINS],
            );
        }
        // A 1 s ring forces wrapping; odd chunk sizes force partial frames.
        let mut f = Front::new(1.0);
        for c in pcm.chunks(737) {
            f.push(c);
        }
        assert_eq!(f.frame_count(), n);
        let mut got = vec![0f32; 50 * NUM_BINS];
        assert_eq!(f.frames(n - 50, n, &mut got), 50);
        assert_eq!(&got[..], &want[(n - 50) * NUM_BINS..]);
        let mut a = Vec::new();
        f.audio(pcm.len() - 1000, pcm.len(), &mut a);
        assert_eq!(&a[..], &pcm[pcm.len() - 1000..]);
        // Older than the ring holds: clamped, not garbage.
        f.audio(0, 10, &mut a);
        assert!(a.is_empty());
    }
}
