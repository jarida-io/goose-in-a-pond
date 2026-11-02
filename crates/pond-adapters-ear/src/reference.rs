//! The echo canceller's reference: what the playback tap copied, at 16 kHz, stamped with the
//! microphone sample its first sample plays at.

use pond_ear::dsp::Resampler;

pub const RATE: u32 = 16_000;
/// A chunk stamped within this of where the last one ended continues it, so the tap's bursty
/// delivery does not shift a reply against its echo.
const CONTINUOUS: usize = RATE as usize * 40 / 1000;

pub struct Reference {
    resampler: Option<(u32, Resampler)>,
    /// Where the last chunk's 16 kHz output ended, on the microphone's clock.
    cursor: Option<usize>,
    out: Vec<f32>,
}

impl Default for Reference {
    fn default() -> Self {
        Self::new()
    }
}

impl Reference {
    pub fn new() -> Reference {
        Reference {
            resampler: None,
            cursor: None,
            out: Vec::new(),
        }
    }

    /// Resample one tapped chunk. `natural` is where its timing puts it on the microphone's clock
    /// and `floor` the first sample the ear has not heard yet, before which nothing can be
    /// cancelled any more. Returns the stamp and the 16 kHz samples, possibly none yet.
    pub fn stamp(
        &mut self,
        rate: u32,
        pcm: &[f32],
        natural: usize,
        floor: usize,
    ) -> (usize, &[f32]) {
        let continues = matches!(
            (&self.resampler, self.cursor),
            (Some((r, _)), Some(c)) if *r == rate && c.abs_diff(natural) <= CONTINUOUS
        );
        let at = match self.cursor {
            Some(c) if continues => c,
            _ => {
                self.resampler = Some((rate, Resampler::new(rate, RATE)));
                natural.max(floor)
            }
        };
        self.out.clear();
        if let Some((_, r)) = self.resampler.as_mut() {
            r.process(pcm, &mut self.out);
        }
        self.cursor = Some(at + self.out.len());
        (at, &self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    #[test]
    fn chunks_of_one_burst_are_stamped_end_to_end_at_16_khz() {
        let mut r = Reference::new();
        let (a, first) = r.stamp(24_000, &tone(480), 1_000, 900);
        let (a, n1) = (a, first.len());
        assert_eq!(a, 1_000);
        // Delivery jitters by a few ms; the burst still lines up sample for sample.
        let (b, second) = r.stamp(24_000, &tone(480), 1_000 + 320 + 50, 900);
        assert_eq!(b, a + n1, "a continuing chunk starts where the last ended");
        assert!((300..=330).contains(&second.len()), "{}", second.len());
    }

    #[test]
    fn a_new_burst_starts_where_its_timing_puts_it_but_never_in_the_past() {
        let mut r = Reference::new();
        let _ = r.stamp(24_000, &tone(480), 1_000, 900);
        let (later, _) = r.stamp(24_000, &tone(480), 40_000, 30_000);
        assert_eq!(later, 40_000);
        let (clamped, _) = r.stamp(48_000, &tone(960), 50_000, 52_000);
        assert_eq!(
            clamped, 52_000,
            "a stamp the ear already heard past is moved up"
        );
    }

    #[test]
    fn a_rate_change_starts_a_new_burst() {
        let mut r = Reference::new();
        let (a, out) = r.stamp(24_000, &tone(480), 0, 0);
        let end = a + out.len();
        let (b, out) = r.stamp(16_000, &tone(160), end, 0);
        assert_eq!(b, end);
        assert!(out.len() <= 160);
    }
}
