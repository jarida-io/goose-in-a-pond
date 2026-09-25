//! Pure-Rust motion detection by differencing a small luma grid; cheap enough for the Jetson.

use pond_core::user_data::domain::vision::Frame;

/// Tuning for [`MotionDetector`]; the defaults suit cameras at ~2 fps.
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
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self {
            grid_w: 64,
            grid_h: 36,
            pixel_delta: 25,
            changed_fraction: 0.05,
        }
    }
}

/// Frame-differencing detector; feed frames in order to [`MotionDetector::observe`].
pub struct MotionDetector {
    cfg: MotionConfig,
    prev: Option<Vec<u8>>,
}

impl MotionDetector {
    pub fn new(cfg: MotionConfig) -> Self {
        Self { cfg, prev: None }
    }

    /// `Some(changed_fraction)` on motion since the last frame, else `None` (malformed too).
    pub fn observe(&mut self, frame: &Frame) -> Option<f64> {
        if !frame.is_well_formed() || frame.width == 0 || frame.height == 0 {
            return None;
        }
        let grid = self.luma_grid(frame);
        let fraction = match &self.prev {
            None => {
                self.prev = Some(grid);
                return None;
            }
            Some(prev) => {
                let changed = grid
                    .iter()
                    .zip(prev.iter())
                    .filter(|(a, b)| a.abs_diff(**b) > self.cfg.pixel_delta)
                    .count();
                changed as f64 / grid.len() as f64
            }
        };
        self.prev = Some(grid);
        (fraction >= self.cfg.changed_fraction).then_some(fraction)
    }

    /// Point-sample one pixel per cell into a `grid_w`×`grid_h` BT.601 luma grid.
    fn luma_grid(&self, frame: &Frame) -> Vec<u8> {
        let (gw, gh) = (self.cfg.grid_w.max(1), self.cfg.grid_h.max(1));
        let mut grid = Vec::with_capacity((gw * gh) as usize);
        for gy in 0..gh {
            let y = (gy as u64 * frame.height as u64 / gh as u64) as u32;
            for gx in 0..gw {
                let x = (gx as u64 * frame.width as u64 / gw as u64) as u32;
                let i = ((y * frame.width + x) * 3) as usize;
                let (r, g, b) = (
                    frame.rgb[i] as u32,
                    frame.rgb[i + 1] as u32,
                    frame.rgb[i + 2] as u32,
                );
                grid.push(((r * 299 + g * 587 + b * 114) / 1000) as u8);
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
