//! Frame → YOLOX input: square BGR on a grey (114) letterbox, as raw 0–255 CHW floats.
//! No mean/std normalisation: YOLOX removed it upstream in v0.1.1.

use anyhow::{bail, Result};
use ndarray::Array4;
use pond_core::user_data::domain::vision::Frame;

/// Letterboxed `[1, 3, size, size]` BGR tensor. No scale ratio: only labels are used.
pub fn letterbox_bgr_chw(frame: &Frame, size: usize) -> Result<Array4<f32>> {
    if !frame.is_well_formed() || frame.width == 0 || frame.height == 0 {
        bail!("malformed frame ({}x{})", frame.width, frame.height);
    }
    let img = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb.clone())
        .ok_or_else(|| anyhow::anyhow!("frame buffer does not match its dimensions"))?;

    let ratio = (size as f32 / frame.width as f32).min(size as f32 / frame.height as f32);
    let new_w = ((frame.width as f32 * ratio).round() as u32).max(1);
    let new_h = ((frame.height as f32 * ratio).round() as u32).max(1);
    let resized =
        image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle);

    // Grey letterbox background, matching YOLOX's training-time padding.
    let mut tensor = Array4::<f32>::from_elem((1, 3, size, size), 114.0);
    for (x, y, px) in resized.enumerate_pixels() {
        let [r, g, b] = px.0;
        // BGR channel order (YOLOX consumes cv2-style images).
        tensor[[0, 0, y as usize, x as usize]] = b as f32;
        tensor[[0, 1, y as usize, x as usize]] = g as f32;
        tensor[[0, 2, y as usize, x as usize]] = r as f32;
    }
    Ok(tensor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn frame(w: u32, h: u32, rgb: (u8, u8, u8)) -> Frame {
        let mut buf = Vec::with_capacity(Frame::expected_len(w, h));
        for _ in 0..(w * h) {
            buf.extend_from_slice(&[rgb.0, rgb.1, rgb.2]);
        }
        Frame {
            width: w,
            height: h,
            rgb: buf,
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn wide_frame_letterboxes_with_grey_padding_below() {
        // 640x360 (the pipeline default) → content fills 416x234, grey below.
        let t = letterbox_bgr_chw(&frame(640, 360, (255, 0, 0)), 416).unwrap();
        assert_eq!(t.shape(), &[1, 3, 416, 416]);
        // Top-left pixel is content: red → BGR channels (0, 0, 255).
        assert_eq!(t[[0, 0, 0, 0]], 0.0);
        assert_eq!(t[[0, 2, 0, 0]], 255.0);
        // Bottom rows are letterbox padding (114 on every channel).
        assert_eq!(t[[0, 0, 415, 0]], 114.0);
        assert_eq!(t[[0, 1, 415, 415]], 114.0);
    }

    #[test]
    fn malformed_frames_are_rejected() {
        let mut bad = frame(8, 8, (0, 0, 0));
        bad.rgb.truncate(10);
        assert!(letterbox_bgr_chw(&bad, 416).is_err());
    }
}
