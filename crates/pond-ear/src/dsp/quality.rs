//! How much speech a clip holds, whether it clipped, and a rough SNR: energy over 20 ms frames.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quality {
    pub duration_s: f32,
    pub speech_s: f32,
    pub clipped_pct: f32,
    pub snr_db: f32,
    pub peak: f32,
    /// First and last speech sample, for trimming and centring.
    pub speech_start: usize,
    pub speech_end: usize,
}

const FRAME: usize = 320;

pub fn quality(x: &[f32]) -> Quality {
    let mut q = Quality {
        duration_s: x.len() as f32 / super::SR,
        ..Default::default()
    };
    if x.len() < FRAME {
        return q;
    }
    let rms: Vec<f32> = x
        .chunks(FRAME)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(f32::total_cmp);
    let noise = sorted[sorted.len() / 10].max(1e-5);
    let loud = sorted[sorted.len() * 9 / 10];
    // Speech: 3x the noise floor and above -50 dBFS.
    let gate = (noise * 3.0).max(0.003_16);
    let speech: Vec<usize> = rms
        .iter()
        .enumerate()
        .filter(|(_, &r)| r > gate)
        .map(|(i, _)| i)
        .collect();
    q.speech_s = speech.len() as f32 * FRAME as f32 / super::SR;
    if let (Some(&a), Some(&b)) = (speech.first(), speech.last()) {
        q.speech_start = a * FRAME;
        q.speech_end = ((b + 1) * FRAME).min(x.len());
    }
    let clipped = x.iter().filter(|v| v.abs() >= 0.999).count();
    q.clipped_pct = 100.0 * clipped as f32 / x.len() as f32;
    q.snr_db = 20.0 * (loud / noise).log10();
    q.peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(len: usize, from: usize, to: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let quiet = ((i * 7919) % 101) as f32 / 101.0 * 2e-4;
                if (from..to).contains(&i) {
                    0.3 * (i as f32 * 0.21).sin()
                } else {
                    quiet
                }
            })
            .collect()
    }

    #[test]
    fn finds_the_speech_span_and_its_snr() {
        let x = burst(16_000, 4_000, 9_000);
        let q = quality(&x);
        assert!(
            q.speech_start <= 4_000 && q.speech_start + FRAME > 4_000,
            "{q:?}"
        );
        assert!(
            q.speech_end >= 9_000 && q.speech_end <= 9_000 + FRAME,
            "{q:?}"
        );
        assert!(q.snr_db > 40.0 && q.clipped_pct == 0.0, "{q:?}");
    }

    #[test]
    fn a_short_or_silent_clip_has_no_speech() {
        assert_eq!(quality(&[0.1; 100]).speech_s, 0.0);
        assert_eq!(quality(&[0.0; 16_000]).speech_end, 0);
    }
}
