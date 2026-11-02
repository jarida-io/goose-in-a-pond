//! Visual overlays and structured summaries for live camera frames and event snapshots.

use anyhow::Result;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::face_recognition::{BoundingBox, FaceLandmarks};
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::vision::Frame;

use crate::jpeg_decode::decode_rgb;
use crate::snapshot::frame_to_jpeg_bytes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPerson {
    pub profile_id: String,
    pub display_name: String,
    pub enrollments: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameAnnotation {
    pub camera_id: String,
    pub event_type: Option<String>,
    pub event_confidence: Option<f64>,
    pub changed_fraction: Option<f64>,
    pub face_detected: bool,
    pub face_unknown: bool,
    pub identified_profile: Option<String>,
    pub identified_name: Option<String>,
    pub face_confidence: Option<f32>,
    pub face_bbox: Option<BoundingBox>,
    pub face_landmarks: Option<FaceLandmarks>,
    pub measured_fps: Option<f32>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl FrameAnnotation {
    pub fn from_event(
        event: &CameraEvent,
        measured_fps: Option<f32>,
        resolve_name: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let meta: serde_json::Value = event
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or(serde_json::Value::Null);
        let identified_profile = meta["identified_profile"].as_str().map(str::to_string);
        let identified_name = meta["identified_name"]
            .as_str()
            .map(str::to_string)
            .or_else(|| identified_profile.as_deref().and_then(resolve_name));
        let face_bbox = meta.get("face_bbox").and_then(|b| {
            Some(BoundingBox {
                x: b["x"].as_u64()? as u32,
                y: b["y"].as_u64()? as u32,
                width: b["width"].as_u64()? as u32,
                height: b["height"].as_u64()? as u32,
            })
        });
        let pt = |v: &serde_json::Value| -> Option<(f32, f32)> {
            let arr = v.as_array()?;
            Some((arr.first()?.as_f64()? as f32, arr.get(1)?.as_f64()? as f32))
        };
        let face_landmarks = meta.get("face_landmarks").and_then(|l| {
            Some(FaceLandmarks {
                left_eye: pt(&l["left_eye"])?,
                right_eye: pt(&l["right_eye"])?,
                nose: pt(&l["nose"])?,
                left_mouth: pt(&l["left_mouth"])?,
                right_mouth: pt(&l["right_mouth"])?,
            })
        });
        Self {
            camera_id: event.camera_id.clone(),
            event_type: Some(event.event_type.clone()),
            event_confidence: event.confidence,
            changed_fraction: meta["changed_fraction"].as_f64(),
            face_detected: meta["face_detected"].as_bool().unwrap_or(false)
                || identified_profile.is_some(),
            face_unknown: meta["face_unknown"].as_bool().unwrap_or(false),
            identified_profile,
            identified_name,
            face_confidence: meta["face_confidence"].as_f64().map(|f| f as f32),
            face_bbox,
            face_landmarks,
            measured_fps,
            updated_at: Some(event.created_at),
        }
    }

    pub fn face_label(&self) -> Option<String> {
        if let Some(name) = self
            .identified_name
            .as_deref()
            .or(self.identified_profile.as_deref())
        {
            let upper = name.to_ascii_uppercase();
            return Some(match self.face_confidence {
                Some(c) => format!("{upper} {:.0}%", c * 100.0),
                None => upper,
            });
        }
        if self.face_unknown || self.face_detected {
            return Some(match self.face_confidence {
                Some(c) => format!("UNKNOWN FACE {:.0}%", c * 100.0),
                None => "UNKNOWN FACE".to_string(),
            });
        }
        None
    }

    pub fn summary_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(fps) = self.measured_fps {
            parts.push(format!("fps={fps:.1}"));
        }
        if let Some(ref ev) = self.event_type {
            match self.event_confidence {
                Some(c) => parts.push(format!("event={ev} ({:.0}%)", c * 100.0)),
                None => parts.push(format!("event={ev}")),
            }
        }
        if let Some(ref name) = self
            .identified_name
            .as_ref()
            .or(self.identified_profile.as_ref())
        {
            match self.face_confidence {
                Some(c) => parts.push(format!("person={name} ({:.0}%)", c * 100.0)),
                None => parts.push(format!("person={name}")),
            }
        } else if self.face_unknown {
            parts.push("face=unknown".to_string());
        } else {
            parts.push("face=none_in_frame".to_string());
        }
        if let Some(b) = self.face_bbox {
            parts.push(format!("bbox=[{},{},{}x{}]", b.x, b.y, b.width, b.height));
        }
        if parts.is_empty() {
            "status=live (no active detections)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

pub fn format_known_people(people: &[KnownPerson]) -> String {
    let enrolled: Vec<String> = people
        .iter()
        .filter(|p| p.enrollments > 0)
        .map(|p| {
            format!(
                "{} ({} sample{})",
                p.display_name,
                p.enrollments,
                if p.enrollments == 1 { "" } else { "s" }
            )
        })
        .collect();
    if !enrolled.is_empty() {
        enrolled.join(", ")
    } else if !people.is_empty() {
        people
            .iter()
            .map(|p| format!("{} (0 samples)", p.display_name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        "none enrolled".to_string()
    }
}

pub const MODEL_IMAGE_MAX_DIM: u32 = 384;
pub const MODEL_JPEG_QUALITY: u8 = 60;

pub fn annotate_jpeg(
    jpeg: &[u8],
    annotation: &FrameAnnotation,
    known_people: &[KnownPerson],
) -> Result<Vec<u8>> {
    let ts = annotation.updated_at.unwrap_or_else(Utc::now);
    let mut frame = decode_rgb(jpeg, ts)?;
    annotate_frame(&mut frame, annotation, known_people);
    frame_to_jpeg_bytes(&frame)
}

/// Annotate and downscale/compress a JPEG (`<= 384px`, quality 60) so small VLMs pay minimal
/// vision-encoder and KV-cache cost when inspecting a frame as a last resort.
pub fn annotate_jpeg_for_model(
    jpeg: &[u8],
    annotation: &FrameAnnotation,
    known_people: &[KnownPerson],
) -> Result<Vec<u8>> {
    let ts = annotation.updated_at.unwrap_or_else(Utc::now);
    let mut frame = decode_rgb(jpeg, ts)?;
    annotate_frame(&mut frame, annotation, known_people);
    compress_frame_for_model(&frame)
}

pub fn compress_frame_for_model(frame: &Frame) -> Result<Vec<u8>> {
    if !frame.is_well_formed() {
        anyhow::bail!("malformed frame ({}x{})", frame.width, frame.height);
    }
    let img = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb.clone())
        .ok_or_else(|| anyhow::anyhow!("failed to construct RgbImage"))?;
    let max_side = frame.width.max(frame.height);
    let resized = if max_side > MODEL_IMAGE_MAX_DIM {
        let new_w =
            ((frame.width as u64 * MODEL_IMAGE_MAX_DIM as u64) / max_side as u64).max(1) as u32;
        let new_h =
            ((frame.height as u64 * MODEL_IMAGE_MAX_DIM as u64) / max_side as u64).max(1) as u32;
        image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, MODEL_JPEG_QUALITY).encode(
        resized.as_raw(),
        resized.width(),
        resized.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

pub fn annotate_frame(
    frame: &mut Frame,
    annotation: &FrameAnnotation,
    known_people: &[KnownPerson],
) {
    if !frame.is_well_formed() || frame.width < 16 || frame.height < 16 {
        return;
    }
    let scale: u32 = if frame.width >= 480 { 2 } else { 1 };
    let pad = 3 * scale;
    let bar_h = (7 * scale + pad * 2).min(frame.height);

    // Top HUD bar.
    darken_rect(frame, 0, 0, frame.width, bar_h);

    let cam = if annotation.camera_id.is_empty() {
        "CAMERA"
    } else {
        annotation.camera_id.as_str()
    };
    let mut hud_left = format!("CAM: {}", cam.to_ascii_uppercase());
    if let Some(fps) = annotation.measured_fps {
        hud_left.push_str(&format!(" | {fps:.1} FPS"));
    }
    if let Some(ref ev) = annotation.event_type {
        hud_left.push_str(&format!(" | {}", ev.to_ascii_uppercase()));
    }
    draw_text(frame, pad, pad, &hud_left, scale, [240, 248, 255]);

    let ts = annotation
        .updated_at
        .unwrap_or_else(frame_timestamp(frame))
        .format("%H:%M:%S UTC")
        .to_string();
    let ts_w = text_width(&ts, scale);
    if ts_w + pad * 2 < frame.width {
        draw_text(
            frame,
            frame.width - ts_w - pad,
            pad,
            &ts,
            scale,
            [180, 210, 230],
        );
    }

    // Face bounding box, label pill, and 5-point landmarks.
    if let Some(bbox) = annotation.face_bbox {
        let color =
            if annotation.identified_profile.is_some() || annotation.identified_name.is_some() {
                [34, 197, 94]
            } else {
                [245, 158, 11]
            };
        let x0 = bbox.x.min(frame.width.saturating_sub(1));
        let y0 = bbox.y.min(frame.height.saturating_sub(1));
        let w = bbox.width.max(4).min(frame.width.saturating_sub(x0));
        let h = bbox.height.max(4).min(frame.height.saturating_sub(y0));
        let thickness = scale.max(2);
        draw_rect_outline(frame, x0, y0, w, h, thickness, color);

        if let Some(label) = annotation.face_label() {
            let lw = text_width(&label, scale) + pad * 2;
            let lh = 7 * scale + pad * 2;
            let ly = if y0 >= lh { y0 - lh } else { y0 };
            fill_rect(frame, x0, ly, lw.min(frame.width - x0), lh, color);
            draw_text(frame, x0 + pad, ly + pad, &label, scale, [10, 15, 20]);
        }
    }

    if let Some(ref lm) = annotation.face_landmarks {
        for pt in [
            lm.left_eye,
            lm.right_eye,
            lm.nose,
            lm.left_mouth,
            lm.right_mouth,
        ] {
            let px = (pt.0.max(0.0) as u32).min(frame.width.saturating_sub(1));
            let py = (pt.1.max(0.0) as u32).min(frame.height.saturating_sub(1));
            fill_rect(
                frame,
                px.saturating_sub(scale),
                py.saturating_sub(scale),
                scale * 2 + 1,
                scale * 2 + 1,
                [56, 189, 248],
            );
        }
    }

    // Bottom status bar only names someone actually detected in this frame, so a vision model
    // reading the image pixels never mistakes the enrolled database roster for who is in the room.
    if let Some(label) = annotation.face_label() {
        if frame.height > bar_h * 2 {
            let status = format!("IN FRAME: {label}");
            let rw = (text_width(&status, scale) + pad * 2).min(frame.width);
            let ry = frame.height - bar_h;
            darken_rect(frame, 0, ry, rw, bar_h);
            draw_text(frame, pad, ry + pad, &status, scale, [167, 243, 208]);
        }
    }
    let _ = known_people;
}

fn frame_timestamp(frame: &Frame) -> impl FnOnce() -> DateTime<Utc> + '_ {
    move || frame.captured_at
}

fn darken_rect(frame: &mut Frame, x: u32, y: u32, w: u32, h: u32) {
    let x_end = (x + w).min(frame.width);
    let y_end = (y + h).min(frame.height);
    for py in y..y_end {
        for px in x..x_end {
            let idx = ((py * frame.width + px) * 3) as usize;
            if idx + 2 < frame.rgb.len() {
                frame.rgb[idx] = frame.rgb[idx] / 3;
                frame.rgb[idx + 1] = frame.rgb[idx + 1] / 3;
                frame.rgb[idx + 2] = frame.rgb[idx + 2] / 3;
            }
        }
    }
}

fn fill_rect(frame: &mut Frame, x: u32, y: u32, w: u32, h: u32, color: [u8; 3]) {
    let x_end = (x + w).min(frame.width);
    let y_end = (y + h).min(frame.height);
    for py in y..y_end {
        for px in x..x_end {
            put_pixel(frame, px, py, color);
        }
    }
}

fn draw_rect_outline(
    frame: &mut Frame,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    thickness: u32,
    color: [u8; 3],
) {
    if w == 0 || h == 0 {
        return;
    }
    let t = thickness.max(1);
    fill_rect(frame, x, y, w, t.min(h), color);
    fill_rect(frame, x, (y + h).saturating_sub(t), w, t.min(h), color);
    fill_rect(frame, x, y, t.min(w), h, color);
    fill_rect(frame, (x + w).saturating_sub(t), y, t.min(w), h, color);
}

fn put_pixel(frame: &mut Frame, x: u32, y: u32, color: [u8; 3]) {
    if x < frame.width && y < frame.height {
        let idx = ((y * frame.width + x) * 3) as usize;
        if idx + 2 < frame.rgb.len() {
            frame.rgb[idx] = color[0];
            frame.rgb[idx + 1] = color[1];
            frame.rgb[idx + 2] = color[2];
        }
    }
}

fn text_width(text: &str, scale: u32) -> u32 {
    let chars = text.chars().count() as u32;
    if chars == 0 {
        0
    } else {
        (chars * 6 - 1) * scale
    }
}

fn draw_text(frame: &mut Frame, x: u32, y: u32, text: &str, scale: u32, color: [u8; 3]) {
    let mut cx = x;
    for ch in text.chars() {
        let glyph = glyph_5x7(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5u32 {
                if (bits >> (4 - col)) & 1 == 1 {
                    fill_rect(
                        frame,
                        cx + col * scale,
                        y + (row as u32) * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
        cx += 6 * scale;
        if cx >= frame.width {
            break;
        }
    }
}

fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b00100,
        ],
        ',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b00100, 0b01000,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '|' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        '%' => [
            0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011,
        ],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        _ => [0b00000; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotating_a_frame_draws_hud_face_box_and_roster() {
        let mut frame = Frame {
            width: 160,
            height: 90,
            rgb: vec![120; 160 * 90 * 3],
            captured_at: Utc::now(),
        };
        let before = frame.rgb.clone();
        let ann = FrameAnnotation {
            camera_id: "porch".into(),
            event_type: Some("person".into()),
            event_confidence: Some(0.84),
            changed_fraction: Some(0.12),
            face_detected: true,
            face_unknown: false,
            identified_profile: Some("prof-1".into()),
            identified_name: Some("Jerry".into()),
            face_confidence: Some(0.58),
            face_bbox: Some(BoundingBox {
                x: 40,
                y: 25,
                width: 36,
                height: 40,
            }),
            face_landmarks: Some(FaceLandmarks {
                left_eye: (50.0, 38.0),
                right_eye: (64.0, 38.0),
                nose: (57.0, 46.0),
                left_mouth: (51.0, 54.0),
                right_mouth: (63.0, 54.0),
            }),
            measured_fps: Some(29.8),
            updated_at: Some(Utc::now()),
        };
        let people = vec![KnownPerson {
            profile_id: "prof-1".into(),
            display_name: "Jerry".into(),
            enrollments: 1,
        }];
        annotate_frame(&mut frame, &ann, &people);
        assert_ne!(frame.rgb, before);
        assert_eq!(ann.face_label().as_deref(), Some("JERRY 58%"));
        assert!(ann.summary_line().contains("person=Jerry (58%)"));
        assert!(ann.summary_line().contains("fps=29.8"));
    }

    #[test]
    fn compress_frame_for_model_downscales_and_shrinks_payload() {
        let frame = Frame {
            width: 640,
            height: 360,
            rgb: (0..(640 * 360 * 3)).map(|i| (i % 251) as u8).collect(),
            captured_at: Utc::now(),
        };
        let full_jpeg = crate::snapshot::frame_to_jpeg_bytes(&frame).unwrap();
        let model_jpeg = compress_frame_for_model(&frame).unwrap();
        let decoded = image::load_from_memory(&model_jpeg).unwrap();
        assert_eq!(decoded.width(), MODEL_IMAGE_MAX_DIM);
        assert_eq!(decoded.height(), 216);
        assert!(
            model_jpeg.len() < full_jpeg.len(),
            "compressed {}B must be smaller than full {}B",
            model_jpeg.len(),
            full_jpeg.len()
        );
    }
}
