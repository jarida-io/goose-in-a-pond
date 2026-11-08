//! Kaldi filterbank features, step for step with torchaudio.compliance.kaldi.fbank. Frames are
//! independent, so a caller computes them as audio arrives and pays only CMN at the end.

use super::math::ln;
use super::tables::*;

pub const SAMPLE_RATE: usize = 16_000;
/// 25 ms.
pub const FRAME: usize = 400;
/// 10 ms.
pub const SHIFT: usize = 160;
pub const NUM_BINS: usize = 80;
const NFFT_HALF: usize = 256;

/// WeSpeaker's front end (`infer_onnx.py`), which the wake CNN was trained on too: the pond's
/// [-1, 1] samples scaled to int16 range, then a Hamming window.
const SCALE: f32 = 32768.0;

/// snip_edges=True: only whole frames.
pub const fn num_frames(samples: usize) -> usize {
    if samples < FRAME {
        0
    } else {
        1 + (samples - FRAME) / SHIFT
    }
}

/// One frame of log-mel energies from `FRAME` samples starting at `pcm[0]`.
pub fn frame(pcm: &[f32], out: &mut [f32]) {
    let pcm = &pcm[..FRAME];
    let out = &mut out[..NUM_BINS];

    // remove_dc_offset on the scaled samples; an f64 sum is exact enough to be order-free.
    let mut buf = [0f32; 2 * NFFT_HALF];
    let mut sum = 0f64;
    for (b, &x) in buf.iter_mut().zip(pcm) {
        *b = x * SCALE;
        sum += *b as f64;
    }
    let mean = (sum / FRAME as f64) as f32;
    for b in &mut buf[..FRAME] {
        *b -= mean;
    }

    // Preemphasis 0.97 with replicate padding: y[i] = x[i] - 0.97 x[i-1], x[-1] = x[0].
    for i in (1..FRAME).rev() {
        buf[i] -= 0.97 * buf[i - 1];
    }
    buf[0] -= 0.97 * buf[0];

    for (b, w) in buf[..FRAME].iter_mut().zip(HAMMING.iter()) {
        *b *= w;
    }

    let mut power = [0f32; NFFT_HALF];
    super::fft::power_spectrum_512(&buf, &mut power);

    let mut w = 0usize;
    for (b, o) in out.iter_mut().enumerate() {
        let (s, n) = (MEL_START[b] as usize, MEL_LEN[b] as usize);
        let mut acc = 0f32;
        for k in 0..n {
            acc += MEL_W[w + k] * power[s + k];
        }
        w += n;
        *o = ln(acc.max(f32::EPSILON));
    }
}

/// Per-utterance cepstral mean normalisation: subtract each bin's mean over time.
pub fn cmn(feats: &mut [f32], frames: usize) {
    if frames == 0 {
        return;
    }
    let mut mean = [0f64; NUM_BINS];
    for t in 0..frames {
        for (m, &v) in mean
            .iter_mut()
            .zip(&feats[t * NUM_BINS..(t + 1) * NUM_BINS])
        {
            *m += v as f64;
        }
    }
    let mean = mean.map(|m| (m / frames as f64) as f32);
    for t in 0..frames {
        for (v, m) in feats[t * NUM_BINS..(t + 1) * NUM_BINS]
            .iter_mut()
            .zip(&mean)
        {
            *v -= m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_count_whole_frames_only() {
        assert_eq!(num_frames(FRAME - 1), 0);
        assert_eq!(num_frames(FRAME), 1);
        assert_eq!(num_frames(FRAME + SHIFT - 1), 1);
        assert_eq!(num_frames(16_000), 98);
    }

    #[test]
    fn cmn_leaves_every_bin_with_zero_mean() {
        let mut f: Vec<f32> = (0..10 * NUM_BINS).map(|i| (i % 17) as f32 * 0.3).collect();
        cmn(&mut f, 10);
        for b in 0..NUM_BINS {
            let m: f32 = (0..10).map(|t| f[t * NUM_BINS + b]).sum::<f32>() / 10.0;
            assert!(m.abs() < 1e-5, "bin {b}: {m}");
        }
    }
}
