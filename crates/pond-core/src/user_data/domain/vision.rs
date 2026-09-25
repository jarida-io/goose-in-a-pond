//! Vision types: the seam between `pond-adapters-vision` capture/detection and the Core.

use chrono::{DateTime, Utc};

/// One captured camera frame, packed RGB24 (`len == width * height * 3`).
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Packed RGB24 pixel data, row-major.
    pub rgb: Vec<u8>,
    pub captured_at: DateTime<Utc>,
}

impl Frame {
    pub fn expected_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3
    }

    pub fn is_well_formed(&self) -> bool {
        self.rgb.len() == Self::expected_len(self.width, self.height)
    }
}

/// One classifier verdict about a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// Event label, e.g. `"pet"`, `"package"`, `"person"`.
    pub label: String,
    /// Classifier confidence in `[0, 1]`.
    pub confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_well_formedness() {
        let ok = Frame {
            width: 2,
            height: 2,
            rgb: vec![0; 12],
            captured_at: Utc::now(),
        };
        assert!(ok.is_well_formed());

        let bad = Frame {
            width: 2,
            height: 2,
            rgb: vec![0; 11],
            captured_at: Utc::now(),
        };
        assert!(!bad.is_well_formed());
    }
}
