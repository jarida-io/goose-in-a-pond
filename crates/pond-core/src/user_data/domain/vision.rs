//! Vision types: the seam between `pond-adapters-vision` capture/detection and the Core.

use std::ops::RangeInclusive;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::user_data::domain::settings::Settings;

/// Every event type the pipeline emits; `face:<profile_id>` is a rule signal, not a type.
pub const EVENT_TYPES: &[&str] = &[
    "motion",
    "person",
    "pet",
    "vehicle",
    "package",
    "face_detected",
    "face_recognized",
    "face_unknown",
];

pub const DEFAULT_CAMERA_ID: &str = "camera-1";
pub const DEFAULT_FPS: u32 = 30;
pub const DEFAULT_MOTION_THRESHOLD: f64 = 0.05;
pub const DEFAULT_EVENT_COOLDOWN_SECS: u64 = 10;
pub const DEFAULT_CLASSIFIER_CONFIDENCE: f64 = 0.5;

/// 30 is what USB and IP cameras deliver; the Orin's decoders keep it cheap.
pub const FPS_RANGE: RangeInclusive<u32> = 1..=30;
pub const MOTION_THRESHOLD_RANGE: RangeInclusive<f64> = 0.005..=0.5;
pub const EVENT_COOLDOWN_SECS_RANGE: RangeInclusive<u64> = 1..=3600;
pub const CLASSIFIER_CONFIDENCE_RANGE: RangeInclusive<f64> = 0.05..=0.99;

pub fn clamp_fps(fps: u32) -> u32 {
    fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end())
}

pub fn clamp_motion_threshold(threshold: f64) -> f64 {
    clamp_real(threshold, &MOTION_THRESHOLD_RANGE, DEFAULT_MOTION_THRESHOLD)
}

pub fn clamp_event_cooldown_secs(secs: u64) -> u64 {
    secs.clamp(
        *EVENT_COOLDOWN_SECS_RANGE.start(),
        *EVENT_COOLDOWN_SECS_RANGE.end(),
    )
}

pub fn clamp_classifier_confidence(confidence: f64) -> f64 {
    clamp_real(
        confidence,
        &CLASSIFIER_CONFIDENCE_RANGE,
        DEFAULT_CLASSIFIER_CONFIDENCE,
    )
}

// NaN survives `f64::clamp`, so a non-finite value takes the default instead.
fn clamp_real(value: f64, range: &RangeInclusive<f64>, default: f64) -> f64 {
    if value.is_finite() {
        value.clamp(*range.start(), *range.end())
    } else {
        default
    }
}

/// The `camera_id` vision events carry; a blank setting falls back to the default name.
pub fn camera_id(s: &Settings) -> String {
    let id = s.vision_camera_id.trim();
    if id.is_empty() {
        DEFAULT_CAMERA_ID.to_string()
    } else {
        id.to_string()
    }
}

/// Trimmed, lower-cased and de-duplicated in order; empty still means every type.
pub fn normalize_event_types(types: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(types.len());
    for t in types {
        let t = t.trim().to_ascii_lowercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Stores every vision field as the pipeline will read it, so a saved value is the applied one.
pub fn normalize_settings(s: &mut Settings) {
    s.vision_fps = clamp_fps(s.vision_fps);
    s.vision_motion_threshold = clamp_motion_threshold(s.vision_motion_threshold);
    s.vision_event_cooldown_secs = clamp_event_cooldown_secs(s.vision_event_cooldown_secs);
    s.vision_classifier_confidence = clamp_classifier_confidence(s.vision_classifier_confidence);
    s.vision_event_types = normalize_event_types(&s.vision_event_types);
}

/// What one camera event carries; a type filter keeps it when any carried signal is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSignals<'a> {
    pub event_type: &'a str,
    pub face_detected: bool,
    pub face_recognized: bool,
    pub face_unknown: bool,
}

impl EventSignals<'_> {
    /// `enabled` empty keeps everything.
    pub fn passes(&self, enabled: &[String]) -> bool {
        if enabled.is_empty() {
            return true;
        }
        let on = |signal: &str| enabled.iter().any(|e| e.eq_ignore_ascii_case(signal));
        on(self.event_type)
            || (self.face_detected && on("face_detected"))
            || (self.face_recognized && on("face_recognized"))
            || (self.face_unknown && on("face_unknown"))
    }
}

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

/// A small 8-bit luma picture, row-major (`len == width * height`).
#[derive(Debug, Clone, PartialEq)]
pub struct LumaPlane {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl LumaPlane {
    pub fn is_well_formed(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.pixels.len() == self.width as usize * self.height as usize
    }
}

/// A frame left as the JPEG the camera (or a hardware encoder) produced; never re-encoded.
#[derive(Debug, Clone)]
pub struct JpegFrame {
    /// Shared so the live feed, snapshots and face recognition read one buffer.
    pub jpeg: Arc<Vec<u8>>,
    /// The same picture already decoded off the CPU; `None` leaves decoding to the pipeline.
    pub luma: Option<LumaPlane>,
    /// The same picture scaled for the detector off the CPU, on the frames that carry one.
    pub analysis: Option<Frame>,
    pub captured_at: DateTime<Utc>,
}

/// What a frame source yields.
#[derive(Debug, Clone)]
pub enum CapturedFrame {
    Raw(Frame),
    Jpeg(JpegFrame),
}

impl CapturedFrame {
    pub fn captured_at(&self) -> DateTime<Utc> {
        match self {
            CapturedFrame::Raw(f) => f.captured_at,
            CapturedFrame::Jpeg(j) => j.captured_at,
        }
    }
}

impl From<Frame> for CapturedFrame {
    fn from(frame: Frame) -> Self {
        CapturedFrame::Raw(frame)
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

/// A box in fractions of the frame's width and height, origin top-left, so it means the same
/// at any resolution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FractionBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FractionBox {
    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// Intersection over union; 0 for boxes that do not overlap.
    pub fn iou(&self, other: &Self) -> f32 {
        let w = (self.x + self.w).min(other.x + other.w) - self.x.max(other.x);
        let h = (self.y + self.h).min(other.y + other.h) - self.y.max(other.y);
        if w <= 0.0 || h <= 0.0 {
            return 0.0;
        }
        let inter = w * h;
        inter / (self.area() + other.area() - inter)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

/// One object a detector found, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedObject {
    /// Event label, e.g. `"pet"`, `"package"`, `"person"`.
    pub label: String,
    pub confidence: f64,
    pub bbox: FractionBox,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_measured_in_fractions_of_the_frame() {
        let a = FractionBox {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let b = FractionBox {
            x: 0.25,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
        assert!((a.iou(&b) - 1.0 / 3.0).abs() < 1e-6);
        let apart = FractionBox {
            x: 0.6,
            y: 0.6,
            w: 0.1,
            h: 0.1,
        };
        assert_eq!(a.iou(&apart), 0.0);
        assert!(a.contains(0.25, 0.25) && !a.contains(0.75, 0.25));
    }

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

    #[test]
    fn luma_plane_well_formedness() {
        let plane = |w: u32, h: u32, len: usize| LumaPlane {
            width: w,
            height: h,
            pixels: vec![0; len],
        };
        assert!(plane(4, 2, 8).is_well_formed());
        assert!(!plane(4, 2, 7).is_well_formed());
        assert!(!plane(0, 0, 0).is_well_formed());
    }

    #[test]
    fn out_of_range_vision_settings_are_clamped_to_what_the_pipeline_honours() {
        let mut s = Settings {
            vision_fps: 60,
            vision_motion_threshold: 0.0,
            vision_event_cooldown_secs: 0,
            vision_classifier_confidence: 2.0,
            ..Settings::default()
        };
        normalize_settings(&mut s);
        assert_eq!(s.vision_fps, 30);
        assert_eq!(s.vision_motion_threshold, 0.005);
        assert_eq!(s.vision_event_cooldown_secs, 1);
        assert_eq!(s.vision_classifier_confidence, 0.99);

        assert_eq!(clamp_fps(0), 1);
        for fps in [5, 15, 24, 30] {
            assert_eq!(clamp_fps(fps), fps, "{fps} fps is honoured");
        }
        assert_eq!(clamp_event_cooldown_secs(86_400), 3600);
        assert_eq!(clamp_motion_threshold(f64::NAN), DEFAULT_MOTION_THRESHOLD);
        assert_eq!(
            clamp_classifier_confidence(f64::INFINITY),
            DEFAULT_CLASSIFIER_CONFIDENCE
        );
    }

    #[test]
    fn defaults_are_inside_their_own_ranges() {
        let mut s = Settings::default();
        let before = (
            s.vision_fps,
            s.vision_motion_threshold,
            s.vision_event_cooldown_secs,
            s.vision_classifier_confidence,
            s.vision_event_types.clone(),
        );
        normalize_settings(&mut s);
        let after = (
            s.vision_fps,
            s.vision_motion_threshold,
            s.vision_event_cooldown_secs,
            s.vision_classifier_confidence,
            s.vision_event_types.clone(),
        );
        assert_eq!(before, after);
        assert_eq!(s.vision_event_types.len(), EVENT_TYPES.len());
    }

    #[test]
    fn event_types_are_trimmed_lowercased_and_deduplicated_in_order() {
        let raw = vec![
            " Person ".to_string(),
            "pet".to_string(),
            "PERSON".to_string(),
            "".to_string(),
        ];
        assert_eq!(normalize_event_types(&raw), vec!["person", "pet"]);
        assert!(normalize_event_types(&[]).is_empty());
    }

    #[test]
    fn an_event_passes_when_any_signal_it_carries_is_enabled() {
        let recognised = EventSignals {
            event_type: "person",
            face_detected: true,
            face_recognized: true,
            face_unknown: false,
        };
        let stranger = EventSignals {
            event_type: "person",
            face_detected: true,
            face_recognized: false,
            face_unknown: true,
        };
        let only = |t: &str| vec![t.to_string()];

        assert!(recognised.passes(&only("face_recognized")));
        assert!(!stranger.passes(&only("face_recognized")));
        assert!(stranger.passes(&only("face_unknown")));
        assert!(!recognised.passes(&only("face_unknown")));
        assert!(recognised.passes(&only("face_detected")));
        assert!(stranger.passes(&only("Person")));
        assert!(!recognised.passes(&only("pet")));
        assert!(recognised.passes(&[]), "empty enables every type");

        let plain = EventSignals {
            event_type: "motion",
            face_detected: false,
            face_recognized: false,
            face_unknown: false,
        };
        assert!(!plain.passes(&only("face_detected")));
    }
}
