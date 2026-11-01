//! Frame → YOLOX input: square BGR on a grey (114) letterbox, as raw 0–255 CHW floats.
//! No mean/std normalisation: YOLOX removed it upstream in v0.1.1.

use anyhow::{bail, Result};
use ndarray::Array4;
use pond_core::user_data::domain::vision::Frame;

/// Letterboxed `[1, 3, size, size]` BGR tensor, the picture top-left and grey to its right and below.
pub fn letterbox_bgr_chw(frame: &Frame, size: usize) -> Result<Array4<f32>> {
    if !frame.is_well_formed() || frame.width == 0 || frame.height == 0 {
        bail!("malformed frame ({}x{})", frame.width, frame.height);
    }
    let img = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb.clone())
        .ok_or_else(|| anyhow::anyhow!("frame buffer does not match its dimensions"))?;

    let ratio = (size as f32 / frame.width as f32).min(size as f32 / frame.height as f32);
    let new_w = ((frame.width as f32 * ratio).round() as u32).max(1);
    let new_h = ((frame.height as f32 * ratio).round() as u32).max(1);
    // The Jetson's analysis branch already scaled the frame to fit, so most frames skip this.
    let resized = if (new_w, new_h) == (frame.width, frame.height) {
        img
    } else {
        image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle)
    };

    // Grey letterbox background, matching YOLOX's training-time padding.
    let hw = size * size;
    let mut planes = vec![114.0f32; 3 * hw];
    {
        let (b_plane, rest) = planes.split_at_mut(hw);
        let (g_plane, r_plane) = rest.split_at_mut(hw);
        let rw = (new_w as usize).min(size);
        let rh = (new_h as usize).min(size);
        let raw = resized.as_raw();
        let stride = new_w as usize * 3;
        for y in 0..rh {
            let row_off = y * size;
            let src_row = &raw[y * stride..y * stride + rw * 3];
            let b_row = &mut b_plane[row_off..row_off + rw];
            let g_row = &mut g_plane[row_off..row_off + rw];
            let r_row = &mut r_plane[row_off..row_off + rw];
            for (x, px) in src_row.chunks_exact(3).enumerate() {
                // BGR channel order (YOLOX consumes cv2-style images).
                b_row[x] = px[2] as f32;
                g_row[x] = px[1] as f32;
                r_row[x] = px[0] as f32;
            }
        }
    }
    let tensor = Array4::from_shape_vec((1, 3, size, size), planes)
        .map_err(|e| anyhow::anyhow!("YOLOX tensor shape error: {e}"))?;
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
