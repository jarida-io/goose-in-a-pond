//! Pure-Rust motion detection by differencing a small luma grid; cheap enough for the Jetson.

use std::collections::VecDeque;
use std::time::Duration;

use chrono::{DateTime, Utc};
use pond_core::user_data::domain::vision::{Frame, LumaPlane};

/// Motion is measured across about this much time, so a threshold means the same at any fps.
pub const MOTION_SPAN: Duration = Duration::from_millis(200);

/// Bounds the grids held while none is old enough yet, should timestamps stall.
const MAX_HISTORY: usize = 64;

/// Half a frame short of [`MOTION_SPAN`]: a jittery 5 fps stream still compares consecutive
/// frames, as does anything slower.
pub fn min_reference_age(fps: u32) -> Duration {
    MOTION_SPAN.saturating_sub(Duration::from_secs(1) / (2 * fps.max(1)))
}

/// Tuning for [`MotionDetector`]; the defaults compare consecutive frames.
#[derive(Debug, Clone)]
pub struct MotionConfig {
    /// Downsample grid width (cells).
    pub grid_w: u32,
    /// Downsample grid height (cells).
    pub grid_h: u32,
    /// Minimum per-cell luma change (0–255) to count the cell as changed.
    pub pixel_delta: u8,
    /// Fraction of changed cells (0–1) at which motion is declared.
    pub changed_fraction: f64,
    /// A frame is compared with the newest earlier frame at least this much older.
    pub min_reference_age: Duration,
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self {
            grid_w: 64,
            grid_h: 36,
            pixel_delta: 25,
            changed_fraction: 0.05,
            min_reference_age: Duration::ZERO,
        }
    }
}

/// Frame-differencing detector; feed frames in order to [`MotionDetector::observe`].
pub struct MotionDetector {
    cfg: MotionConfig,
    /// Oldest first; the front is the newest grid old enough to compare against, once one is.
    history: VecDeque<(DateTime<Utc>, Vec<u8>)>,
    last_change: Option<f64>,
}

impl MotionDetector {
    pub fn new(cfg: MotionConfig) -> Self {
        Self {
            cfg,
            history: VecDeque::new(),
            last_change: None,
        }
    }

    /// `Some(changed_fraction)` on motion since the reference frame, else `None` (malformed too).
    pub fn observe(&mut self, frame: &Frame) -> Option<f64> {
        if !frame.is_well_formed() || frame.width == 0 || frame.height == 0 {
            return None;
        }
        let grid = self.luma_grid(frame);
        self.compare(grid, frame.captured_at)
    }

    /// [`MotionDetector::observe`] for a luma picture, which may be smaller than the camera's.
    pub fn observe_luma(&mut self, luma: &LumaPlane, at: DateTime<Utc>) -> Option<f64> {
        if !luma.is_well_formed() {
            return None;
        }
        let grid = self.sample_grid(luma.width, luma.height, |x, y| {
            luma.pixels[(y * luma.width + x) as usize]
        });
        self.compare(grid, at)
    }

    fn compare(&mut self, grid: Vec<u8>, at: DateTime<Utc>) -> Option<f64> {
        // After a backwards clock step every held grid would look newer than this frame.
        if self.history.back().is_some_and(|(last, _)| *last > at) {
            self.history.clear();
        }
        let min_age = chrono::Duration::from_std(self.cfg.min_reference_age)
            .unwrap_or_else(|_| chrono::Duration::zero());
        while self.history.len() >= 2 && at - self.history[1].0 >= min_age {
            self.history.pop_front();
        }
        let fraction = self
            .history
            .front()
            .filter(|(then, _)| at - *then >= min_age)
            .map(|(_, reference)| {
                let changed = grid
                    .iter()
                    .zip(reference.iter())
                    .filter(|(a, b)| a.abs_diff(**b) > self.cfg.pixel_delta)
                    .count();
                changed as f64 / grid.len() as f64
            });
        if self.history.len() >= MAX_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back((at, grid));
        self.last_change = fraction;
        fraction.filter(|f| *f >= self.cfg.changed_fraction)
    }

    /// The fraction of the grid the last observed frame changed, threshold or not.
    pub fn last_change(&self) -> Option<f64> {
        self.last_change
    }

    /// Point-sample one pixel per cell into a `grid_w`×`grid_h` BT.601 luma grid.
    fn luma_grid(&self, frame: &Frame) -> Vec<u8> {
        self.sample_grid(frame.width, frame.height, |x, y| {
            let i = ((y * frame.width + x) * 3) as usize;
            let (r, g, b) = (
                frame.rgb[i] as u32,
                frame.rgb[i + 1] as u32,
                frame.rgb[i + 2] as u32,
            );
            ((r * 299 + g * 587 + b * 114) / 1000) as u8
        })
    }

    fn sample_grid(&self, width: u32, height: u32, luma_at: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        let (gw, gh) = (self.cfg.grid_w.max(1), self.cfg.grid_h.max(1));
        let mut grid = Vec::with_capacity((gw * gh) as usize);
        for gy in 0..gh {
            let y = (gy as u64 * height as u64 / gh as u64) as u32;
            for gx in 0..gw {
                let x = (gx as u64 * width as u64 / gw as u64) as u32;
                grid.push(luma_at(x, y));
            }
        }
        grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    pub(crate) fn flat_frame(width: u32, height: u32, luma: u8) -> Frame {
        Frame {
            width,
            height,
            rgb: vec![luma; Frame::expected_len(width, height)],
            captured_at: Utc::now(),
        }
    }

    /// A flat frame with a bright square covering roughly the top-left quarter.
    pub(crate) fn frame_with_square(width: u32, height: u32, background: u8) -> Frame {
        let mut f = flat_frame(width, height, background);
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = ((y * width + x) * 3) as usize;
                f.rgb[i] = 255;
                f.rgb[i + 1] = 255;
                f.rgb[i + 2] = 255;
            }
        }
        f
    }

    #[test]
    fn a_change_below_the_threshold_is_still_reported_as_the_last_change() {
        let mut d = MotionDetector::new(MotionConfig {
            changed_fraction: 0.9,
            ..Default::default()
        });
        assert_eq!(d.observe(&flat_frame(64, 36, 20)), None);
        assert_eq!(d.last_change(), None, "nothing to compare with yet");
        assert_eq!(d.observe(&frame_with_square(64, 36, 20)), None);
        assert!(d.last_change().is_some_and(|f| f > 0.1 && f < 0.9));
    }

    #[test]
    fn no_motion_on_still_frames() {
        let mut d = MotionDetector::new(MotionConfig::default());
        assert!(d.observe(&flat_frame(64, 36, 40)).is_none(), "first frame");
        assert!(d.observe(&flat_frame(64, 36, 40)).is_none(), "identical");
        // Sub-threshold global flicker (delta 10 < pixel_delta 25).
        assert!(d.observe(&flat_frame(64, 36, 50)).is_none());
    }

    #[test]
    fn detects_appearing_square() {
        let mut d = MotionDetector::new(MotionConfig::default());
        assert!(d.observe(&flat_frame(64, 36, 20)).is_none());
        let fraction = d
            .observe(&frame_with_square(64, 36, 20))
            .expect("quarter-frame change must trip the 5% threshold");
        assert!(
            fraction > 0.2,
            "roughly a quarter of cells changed: {fraction}"
        );
    }

    fn flat_at(luma: u8, ms: i64) -> Frame {
        let mut f = flat_frame(64, 36, luma);
        f.captured_at = DateTime::from_timestamp_millis(1_700_000_000_000 + ms).unwrap();
        f
    }

    fn detector_at(fps: u32) -> MotionDetector {
        MotionDetector::new(MotionConfig {
            min_reference_age: min_reference_age(fps),
            ..MotionConfig::default()
        })
    }

    #[test]
    fn the_reference_age_leaves_slow_streams_comparing_consecutive_frames() {
        assert_eq!(min_reference_age(1), Duration::ZERO);
        assert_eq!(min_reference_age(2), Duration::ZERO);
        assert_eq!(min_reference_age(5), Duration::from_millis(100));
        let at_30 = min_reference_age(30);
        assert!(at_30 > Duration::from_millis(180) && at_30 < MOTION_SPAN);
    }

    #[test]
    fn at_five_fps_each_frame_is_compared_with_the_one_before_despite_jitter() {
        let mut d = detector_at(5);
        // 170 ms and 230 ms gaps: the previous frame is always the reference, as before.
        assert!(d.observe(&flat_at(20, 0)).is_none());
        let mut square = frame_with_square(64, 36, 20);
        square.captured_at = flat_at(0, 170).captured_at;
        assert!(d.observe(&square).is_some(), "the square appeared");
        square.captured_at = flat_at(0, 400).captured_at;
        assert!(d.observe(&square).is_none(), "still there: no change");
        assert!(d.observe(&flat_at(20, 570)).is_some(), "the square left");
    }

    /// The same scene change rate (150 luma levels a second) at two frame rates.
    fn first_detection(fps: u32) -> (usize, i64) {
        let mut d = detector_at(fps);
        let step_ms = 1000 / fps as i64;
        let per_frame = (150 / fps) as u8;
        for i in 0..=fps as usize {
            let at = i as i64 * step_ms;
            if d.observe(&flat_at(i as u8 * per_frame, at)).is_some() {
                return (i, at);
            }
        }
        panic!("no motion within a second at {fps} fps");
    }

    #[test]
    fn a_threshold_means_the_same_at_five_and_thirty_fps() {
        // Per frame the change is 30 at 5 fps but 5 at 30 fps; over ~200 ms both exceed 25.
        let (_, at_5) = first_detection(5);
        let (frame_30, at_30) = first_detection(30);
        assert_eq!(at_5, 200);
        assert!(
            frame_30 > 1,
            "consecutive 30 fps frames differ by less than the threshold"
        );
        assert!(
            (180..=240).contains(&at_30),
            "30 fps detects after about the same span: {at_30} ms"
        );
    }

    #[test]
    fn a_backwards_clock_step_restarts_the_comparison() {
        let mut d = detector_at(30);
        assert!(d.observe(&flat_at(20, 10_000)).is_none());
        assert!(
            d.observe(&flat_at(200, 0)).is_none(),
            "no reference after the step"
        );
        assert!(d.observe(&flat_at(20, 250)).is_some());
    }

    #[test]
    fn a_luma_picture_and_the_rgb_frame_it_came_from_agree() {
        let to_luma = |f: &Frame| LumaPlane {
            width: f.width,
            height: f.height,
            pixels: f.rgb.chunks_exact(3).map(|p| p[0]).collect(),
        };
        let (still, moved) = (flat_frame(128, 72, 20), frame_with_square(128, 72, 20));
        let mut rgb = MotionDetector::new(MotionConfig::default());
        let mut luma = MotionDetector::new(MotionConfig::default());
        assert!(rgb.observe(&still).is_none());
        assert!(luma
            .observe_luma(&to_luma(&still), still.captured_at)
            .is_none());
        let from_rgb = rgb.observe(&moved).expect("motion in rgb");
        let from_luma = luma
            .observe_luma(&to_luma(&moved), moved.captured_at)
            .expect("motion in luma");
        assert_eq!(from_rgb, from_luma);

        let bad = LumaPlane {
            width: 4,
            height: 4,
            pixels: vec![0; 3],
        };
        assert!(luma.observe_luma(&bad, Utc::now()).is_none());
    }

    #[test]
    fn malformed_frame_fails_closed() {
        let mut d = MotionDetector::new(MotionConfig::default());
        let bad = Frame {
            width: 64,
            height: 36,
            rgb: vec![0; 10],
            captured_at: Utc::now(),
        };
        assert!(d.observe(&bad).is_none());
    }
}
