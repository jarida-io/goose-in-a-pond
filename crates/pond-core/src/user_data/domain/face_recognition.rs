//! Face recognition types. Only embedding vectors are persisted, never raw images.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceEmbedding {
    /// Unique row ID (UUID v4).
    pub id: String,
    pub profile_id: String,
    /// Face vector; not reconstructable into the original image.
    pub embedding: Vec<f32>,
    /// Vector length (512 ArcFace, 128 MobileFaceNet); only compare embeddings of equal dims.
    pub model_dims: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceIdentification {
    /// The identified profile, if confidence exceeded the threshold.
    pub profile_id: Option<String>,
    /// Cosine similarity in \[0.0, 1.0\]; `None` if no face was detected.
    pub confidence: Option<f32>,
    /// Whether the identification was conclusive (confidence ≥ threshold).
    pub identified: bool,
}

impl FaceIdentification {
    pub fn found(profile_id: String, confidence: f32) -> Self {
        Self {
            profile_id: Some(profile_id),
            confidence: Some(confidence),
            identified: true,
        }
    }

    /// Build a result where no stored face matched.
    pub fn unknown(confidence: Option<f32>) -> Self {
        Self {
            profile_id: None,
            confidence,
            identified: false,
        }
    }

    pub fn no_face() -> Self {
        Self {
            profile_id: None,
            confidence: None,
            identified: false,
        }
    }
}

/// Face bounding box in source-image pixels (origin top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl BoundingBox {
    /// Parse `"x,y,w,h"` (four unsigned integers, comma-separated).
    pub fn parse_csv(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(',').map(|p| p.trim()).collect();
        if parts.len() != 4 {
            return None;
        }
        Some(Self {
            x: parts[0].parse().ok()?,
            y: parts[1].parse().ok()?,
            width: parts[2].parse().ok()?,
            height: parts[3].parse().ok()?,
        })
    }
}

/// Five facial landmarks in source-image pixels, used to warp faces to ArcFace's 112×112 pose.
/// Without that alignment, unrelated faces score ~0.5 similarity.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FaceLandmarks {
    pub left_eye: (f32, f32),
    pub right_eye: (f32, f32),
    pub nose: (f32, f32),
    pub left_mouth: (f32, f32),
    pub right_mouth: (f32, f32),
}

impl FaceLandmarks {
    /// Points in the ArcFace/SCRFD reference order the similarity transform expects.
    pub fn as_array(&self) -> [(f32, f32); 5] {
        [
            self.left_eye,
            self.right_eye,
            self.nose,
            self.left_mouth,
            self.right_mouth,
        ]
    }
}

/// Detector output; bbox-only detectors (UltraFace) leave `landmarks` as `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedFace {
    pub bbox: BoundingBox,
    pub landmarks: Option<FaceLandmarks>,
    pub score: f32,
}
