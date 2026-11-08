//! Signal helpers: the pond's capture resampler, a streaming band-limited one for the playback
//! tap, and the energy quality gate that cuts wake segments.

pub mod quality;
pub mod resample;

pub use quality::{quality, Quality};
pub use resample::Resampler;

pub const SR: f32 = 16_000.0;

/// `pond_voice::dsp::resample_to_16k` sample for sample: linear interpolation, no anti-alias
/// filter, so a replayed file carries the same aliasing a live pond capture does.
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16_000 || src_rate == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = 16_000.0_f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    (0..new_len)
        .map(|i| {
            let src = i as f64 / ratio;
            let lo = src.floor() as usize;
            let hi = (lo + 1).min(samples.len().saturating_sub(1));
            let frac = (src - src.floor()) as f32;
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn interpolates_linearly_and_passes_16k_through() {
        let ramp: Vec<f32> = (0..480).map(|i| i as f32).collect();
        let y = super::resample_to_16k(&ramp, 48_000);
        assert_eq!(y.len(), 160);
        assert!(y.iter().enumerate().all(|(i, &v)| v == 3.0 * i as f32));
        let y = super::resample_to_16k(&ramp, 8_000);
        assert_eq!((y.len(), y[1], y[3]), (960, 0.5, 1.5));
        assert_eq!(super::resample_to_16k(&ramp, 16_000), ramp);
    }
}
