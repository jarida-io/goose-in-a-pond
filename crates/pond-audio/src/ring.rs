//! The one shared rolling window of 16 kHz mono f32 that every subscriber snapshots.
//! Not a per-subscriber broadcast, which allocates per frame and drops silently for slow readers.

use std::collections::VecDeque;

pub const CAPTURE_RATE_HZ: u32 = 16_000;

/// Rolling mono f32 buffer, capped at a fixed number of samples.
#[derive(Debug)]
pub struct Ring {
    samples: VecDeque<f32>,
    capacity: usize,
    /// Total samples ever pushed. Lets a subscriber notice it fell behind.
    written: u64,
}

impl Ring {
    /// A ring holding `window_ms` of audio at `sample_rate`.
    pub fn with_window(sample_rate: u32, window_ms: u64) -> Self {
        let capacity = ((window_ms * sample_rate as u64) / 1000).max(1) as usize;
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            written: 0,
        }
    }

    /// Append normalised mono samples, evicting the oldest to stay in budget.
    pub fn push(&mut self, samples: &[f32]) {
        self.written += samples.len() as u64;
        self.samples.extend(samples.iter().copied());
        while self.samples.len() > self.capacity {
            self.samples.pop_front();
        }
    }

    /// The most recent `n` samples, or everything if fewer are held.
    pub fn recent(&self, n: usize) -> Vec<f32> {
        let start = self.samples.len().saturating_sub(n);
        self.samples.range(start..).copied().collect()
    }

    /// Everything currently buffered.
    pub fn snapshot(&self) -> Vec<f32> {
        self.samples.iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Monotonic count of samples ever written.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Drop everything buffered, keeping the write counter. Called at turn end.
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Interleaved device samples to mono f32; `convert` must map onto `[-1.0, 1.0]`.
pub fn to_mono_f32<T: Copy>(interleaved: &[T], channels: usize, convert: fn(T) -> f32) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().map(|&s| convert(s)).sum::<f32>() / channels as f32)
        .collect()
}

/// i16 to normalised f32. 32768 so `i16::MIN` maps to exactly -1.0.
pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32_768.0
}

/// u16 is offset binary — 32768 is silence, not full scale.
pub fn u16_to_f32(s: u16) -> f32 {
    (s as f32 - 32_768.0) / 32_768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_holds_exactly_its_window() {
        // 100 ms at 16 kHz = 1600 samples.
        let mut r = Ring::with_window(16_000, 100);
        assert_eq!(r.capacity(), 1600);
        r.push(&vec![0.5; 1000]);
        assert_eq!(r.len(), 1000);
        r.push(&vec![0.25; 1000]);
        assert_eq!(r.len(), 1600, "must cap, not grow");
    }

    #[test]
    fn the_oldest_samples_are_the_ones_evicted() {
        let mut r = Ring::with_window(1_000, 5); // 5 samples
        r.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        r.push(&[6.0, 7.0]);
        assert_eq!(r.snapshot(), vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn recent_returns_the_tail_and_never_over_reads() {
        let mut r = Ring::with_window(1_000, 10);
        r.push(&[1.0, 2.0, 3.0]);
        assert_eq!(r.recent(2), vec![2.0, 3.0]);
        assert_eq!(r.recent(99), vec![1.0, 2.0, 3.0], "asking for more is fine");
        assert!(Ring::with_window(1_000, 10).recent(5).is_empty());
    }

    #[test]
    fn the_write_counter_survives_eviction_and_clear() {
        let mut r = Ring::with_window(1_000, 3);
        r.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(r.len(), 3);
        assert_eq!(r.written(), 5, "counts everything ever pushed");
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.written(), 5, "clear drops audio, not history");
    }

    #[test]
    fn a_zero_length_window_still_yields_a_usable_ring() {
        let mut r = Ring::with_window(16_000, 0);
        assert_eq!(r.capacity(), 1, "never zero — push must not spin");
        r.push(&[1.0, 2.0]);
        assert_eq!(r.snapshot(), vec![2.0]);
    }

    // ── format normalisation ─────────────────────────────────────────────

    #[test]
    fn stereo_is_averaged_to_mono() {
        let interleaved = [1.0f32, -1.0, 0.5, 0.5];
        assert_eq!(to_mono_f32(&interleaved, 2, |s| s), vec![0.0, 0.5]);
    }

    #[test]
    fn mono_passes_through_unchanged() {
        let s = [0.1f32, -0.2, 0.3];
        assert_eq!(to_mono_f32(&s, 1, |s| s), vec![0.1, -0.2, 0.3]);
    }

    #[test]
    fn every_device_format_lands_in_the_same_range() {
        assert!((i16_to_f32(i16::MAX) - 1.0).abs() < 1e-4);
        assert_eq!(i16_to_f32(i16::MIN), -1.0, "MIN maps to exactly -1.0");
        assert_eq!(i16_to_f32(0), 0.0);

        assert!((u16_to_f32(u16::MAX) - 1.0).abs() < 1e-4);
        assert_eq!(u16_to_f32(0), -1.0);
        assert_eq!(
            u16_to_f32(32_768),
            0.0,
            "offset binary: midpoint is silence"
        );
    }

    #[test]
    fn a_partial_trailing_frame_is_dropped_not_read_past() {
        // 5 samples of stereo = 2 whole frames + 1 stray.
        let interleaved = [1.0f32, 1.0, 2.0, 2.0, 3.0];
        assert_eq!(to_mono_f32(&interleaved, 2, |s| s), vec![1.0, 2.0]);
    }

    #[test]
    fn zero_channels_yields_nothing_rather_than_dividing_by_zero() {
        assert!(to_mono_f32(&[1.0f32, 2.0], 0, |s| s).is_empty());
    }
}
