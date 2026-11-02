//! The playback tap: every sound this process plays, copied as the mixer pulls it, so an echo
//! canceller knows exactly what the speaker is putting into the room.

use rodio::cpal::FromSample;
use rodio::{Sample, Source};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Receives mono f32 at the given sample rate. It runs on the audio callback thread, so it must
/// return at once: copy, stamp and hand off, never block.
pub type PlaybackTap = Arc<dyn Fn(&[f32], u32) + Send + Sync>;

static TAP: RwLock<Option<PlaybackTap>> = RwLock::new(None);

/// Install (or remove) the process-wide tap; sounds started afterwards report to it.
pub fn set_playback_tap(tap: Option<PlaybackTap>) {
    *TAP.write().unwrap_or_else(|e| e.into_inner()) = tap;
}

pub fn playback_tap() -> Option<PlaybackTap> {
    TAP.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Receives the text of each unit of speech as it goes to synthesis, so a listener can tell the
/// pond's own words, heard back through the room, from a person's.
pub type SpokenTap = Arc<dyn Fn(&str) + Send + Sync>;

static SPOKEN: RwLock<Option<SpokenTap>> = RwLock::new(None);

/// Install (or remove) the process-wide spoken-text tap.
pub fn set_spoken_tap(tap: Option<SpokenTap>) {
    *SPOKEN.write().unwrap_or_else(|e| e.into_inner()) = tap;
}

pub fn spoken_tap() -> Option<SpokenTap> {
    SPOKEN.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn report_spoken(text: &str) {
    if let Some(tap) = spoken_tap() {
        tap(text);
    }
}

/// Frames handed over per call: 10 ms at 48 kHz, so the callback thread calls the tap rarely.
const BLOCK_FRAMES: usize = 480;

/// A source that reports what is pulled from it to the tap installed when it was made.
pub struct Tapped<S>
where
    S: Source,
    S::Item: Sample,
{
    inner: S,
    tap: Option<PlaybackTap>,
    /// The frame being summed to mono, and how many of its channels have arrived.
    frame_sum: f32,
    frame_fill: u16,
    channels: u16,
    rate: u32,
    block: Vec<f32>,
}

/// `source`, reporting to the current tap; with none installed it is passed through untouched.
pub fn tapped<S>(source: S) -> Tapped<S>
where
    S: Source,
    S::Item: Sample,
    f32: FromSample<S::Item>,
{
    let tap = playback_tap();
    let channels = source.channels().max(1);
    let rate = source.sample_rate();
    Tapped {
        block: Vec::with_capacity(if tap.is_some() { BLOCK_FRAMES } else { 0 }),
        inner: source,
        tap,
        frame_sum: 0.0,
        frame_fill: 0,
        channels,
        rate,
    }
}

impl<S> Tapped<S>
where
    S: Source,
    S::Item: Sample,
{
    fn flush(&mut self) {
        if let Some(tap) = &self.tap {
            if !self.block.is_empty() {
                tap(&self.block, self.rate);
                self.block.clear();
            }
        }
    }
}

impl<S> Iterator for Tapped<S>
where
    S: Source,
    S::Item: Sample,
    f32: FromSample<S::Item>,
{
    type Item = S::Item;

    fn next(&mut self) -> Option<S::Item> {
        let next = self.inner.next();
        if self.tap.is_none() {
            return next;
        }
        match next {
            Some(v) => {
                if self.frame_fill == 0 {
                    // A frame boundary is where rodio may change the format.
                    let (channels, rate) = (self.inner.channels().max(1), self.inner.sample_rate());
                    if (channels, rate) != (self.channels, self.rate) {
                        self.flush();
                        self.channels = channels;
                        self.rate = rate;
                    }
                }
                self.frame_sum += f32::from_sample_(v);
                self.frame_fill += 1;
                if self.frame_fill == self.channels {
                    self.block.push(self.frame_sum / self.channels as f32);
                    self.frame_sum = 0.0;
                    self.frame_fill = 0;
                    if self.block.len() >= BLOCK_FRAMES {
                        self.flush();
                    }
                }
            }
            None => self.flush(),
        }
        next
    }
}

impl<S> Drop for Tapped<S>
where
    S: Source,
    S::Item: Sample,
{
    fn drop(&mut self) {
        // A stopped sink drops its sources: what was pulled was played.
        self.flush();
    }
}

impl<S> Source for Tapped<S>
where
    S: Source,
    S::Item: Sample,
    f32: FromSample<S::Item>,
{
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }

    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::sync::Mutex;

    /// The tap is process-wide, so its tests take turns.
    pub(crate) static TAP_LOCK: Mutex<()> = Mutex::new(());

    type Heard = Arc<Mutex<Vec<(Vec<f32>, u32)>>>;

    fn install() -> Heard {
        let heard: Heard = Arc::default();
        let sink = heard.clone();
        set_playback_tap(Some(Arc::new(move |pcm: &[f32], rate| {
            sink.lock().unwrap().push((pcm.to_vec(), rate));
        })));
        heard
    }

    #[test]
    fn the_tap_hears_exactly_what_the_mixer_pulls_and_no_more() {
        let _g = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let heard = install();
        let pcm: Vec<f32> = (0..2_000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut source = tapped(SamplesBuffer::new(1, 24_000, pcm.clone()));
        let pulled: Vec<f32> = source.by_ref().take(1_234).collect();
        assert_eq!(pulled, pcm[..1_234]);
        drop(source);
        set_playback_tap(None);
        let heard = heard.lock().unwrap();
        let got: Vec<f32> = heard.iter().flat_map(|(p, _)| p.clone()).collect();
        assert_eq!(
            got,
            pcm[..1_234],
            "the tap must see the pulled samples, in order"
        );
        assert!(heard.iter().all(|(_, rate)| *rate == 24_000));
        assert!(heard.iter().all(|(p, _)| p.len() <= BLOCK_FRAMES));
    }

    #[test]
    fn a_mixer_pulling_a_tapped_reply_reports_it_as_it_goes_not_ahead() {
        let _g = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let heard = install();
        let pcm: Vec<f32> = (0..24_000)
            .map(|i| (i as f32 * 0.003).sin() * 0.3)
            .collect();
        let (controller, mut mixer) = rodio::dynamic_mixer::mixer::<f32>(1, 16_000);
        controller.add(tapped(SamplesBuffer::new(1, 24_000, pcm.clone())));
        // A tenth of a second at the device's rate: the source has given about 2,400 samples.
        let pulled = mixer.by_ref().take(1_600).count();
        assert_eq!(pulled, 1_600);
        let so_far: usize = heard.lock().unwrap().iter().map(|(p, _)| p.len()).sum();
        assert!(
            (2_400 - BLOCK_FRAMES..=2_400 + BLOCK_FRAMES).contains(&so_far),
            "the tap had {so_far} samples after the mixer pulled 0.1 s"
        );
        let _ = mixer.by_ref().take(16_000).count();
        drop(mixer);
        set_playback_tap(None);
        let got: Vec<f32> = heard
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(p, _)| p.clone())
            .collect();
        assert_eq!(
            got, pcm,
            "every sample pulled, in order, at the source's own rate"
        );
    }

    #[test]
    fn stereo_is_reported_as_mono_and_integer_samples_as_f32() {
        let _g = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let heard = install();
        let frames: Vec<i16> = [i16::MAX, 0, -16_384, -16_384].repeat(10);
        let source = tapped(SamplesBuffer::new(2, 48_000, frames));
        assert_eq!(source.count(), 40, "the source itself is untouched");
        set_playback_tap(None);
        let got: Vec<f32> = heard
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(p, _)| p.clone())
            .collect();
        assert_eq!(got.len(), 20);
        assert!((got[0] - 0.5).abs() < 1e-3, "{}", got[0]);
        assert!((got[1] + 0.5).abs() < 1e-3, "{}", got[1]);
    }

    #[test]
    fn an_amplified_source_reports_the_level_that_plays() {
        let _g = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let heard = install();
        let source = tapped(SamplesBuffer::new(1, 16_000, vec![0.8f32; 100]).amplify(0.25));
        let _ = source.count();
        set_playback_tap(None);
        let heard = heard.lock().unwrap();
        assert!(heard
            .iter()
            .flat_map(|(p, _)| p.iter())
            .all(|v| (v - 0.2).abs() < 1e-6));
    }

    #[test]
    fn without_a_tap_a_source_passes_through() {
        let _g = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_playback_tap(None);
        let source = tapped(SamplesBuffer::new(1, 16_000, vec![0.1f32; 10]));
        assert!(source.tap.is_none());
        assert_eq!(source.count(), 10);
    }
}
