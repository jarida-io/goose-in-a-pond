//! Face recognition domain types.
//!
//! `FaceEmbedding` is the stored representation of a household member's facial
//! identity. Raw images are never retained — only the compact float vector
//! produced by the embedding model is persisted.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A stored face embedding for a household member profile.
///
/// Embeddings are model-specific opaque vectors.  The `model_dims` field
/// records the dimensionality so callers can verify they are comparing
/// embeddings from the same model family (ArcFace-512 vs MobileFaceNet-128).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceEmbedding {
    /// Unique row ID (UUID v4).
    pub id: String,
    /// The household member profile this embedding belongs to.
    pub profile_id: String,
    /// Compact float representation of the face — never reconstructable
    /// back to the original image.
    pub embedding: Vec<f32>,
    /// Dimensionality of the embedding vector (e.g. 512 for ArcFace,
    /// 128 for MobileFaceNet).  Used as a sanity-check when comparing.
    pub model_dims: u32,
    /// When this embedding was enrolled.
    pub created_at: DateTime<Utc>,
}

/// Why an identification attempt failed. Surfaced through the API so the
/// frontend can give users actionable guidance instead of a generic
/// "not recognised". `None` on a successful identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// No face detected in the frame at all.
    NoFace,
    /// Face was too small, too tilted, or otherwise failed the geometric
    /// quality gate (eye tilt, nose centering, minimum pixel size).
    QualityGate,
    /// Anti-spoof model classified the frame as non-live.
    AntiSpoof,
    /// No profile is enrolled with enough samples to be matchable.
    UnderEnrolled,
    /// Best score did not clear the absolute match threshold.
    BelowThreshold,
    /// Best profile failed the runner-up margin against the second-best.
    LostToRunnerUp,
    /// Best profile won the runner-up gate but failed the open-set
    /// gap-to-mean check.
    OpenSetGap,
    /// No profiles are enrolled in the system.
    NoEnrolledProfiles,
}

/// Result of a face identification attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceIdentification {
    /// The identified profile, if confidence exceeded the threshold.
    pub profile_id: Option<String>,
    /// Cosine similarity score in \[0.0, 1.0\].
    /// `None` if the image contained no detectable face.
    pub confidence: Option<f32>,
    /// Whether the identification was conclusive (confidence ≥ threshold).
    pub identified: bool,
    /// Why the identification failed, populated when `identified = false`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rejection_reason: Option<RejectionReason>,
}

impl FaceIdentification {
    /// Build a positive identification result.
    pub fn found(profile_id: String, confidence: f32) -> Self {
        Self {
            profile_id: Some(profile_id),
            confidence: Some(confidence),
            identified: true,
            rejection_reason: None,
        }
    }

    /// Build a result where no stored face matched, with a structured reason.
    pub fn unknown(confidence: Option<f32>, reason: RejectionReason) -> Self {
        Self {
            profile_id: None,
            confidence,
            identified: false,
            rejection_reason: Some(reason),
        }
    }

    /// Build a result where no face was detectable in the image.
    pub fn no_face() -> Self {
        Self {
            profile_id: None,
            confidence: None,
            identified: false,
            rejection_reason: Some(RejectionReason::NoFace),
        }
    }
}

/// Integer pixel bounding box around a face within a source image.
///
/// Coordinates are in the source image's pixel space (origin top-left).
/// Supplied by the client (UI crop), a face detector (mtCNN follow-up),
/// or synthesised by the adapter's center-square fallback when absent.
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

/// Five canonical facial landmarks in source-image pixel coordinates.
///
/// Order matches the ArcFace/SCRFD convention: left eye, right eye, nose tip,
/// left mouth corner, right mouth corner.  These drive the similarity-transform
/// alignment that warps every face into the canonical 112×112 pose ArcFace was
/// trained on — the single biggest lever for real-world identification
/// accuracy.  Without alignment, identical faces photographed at different
/// head poses land in different regions of the embedding space and ArcFace
/// produces ~0.5 similarity for truly-different people, which is why the
/// "anyone passes the threshold" failure mode emerges.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FaceLandmarks {
    pub left_eye:    (f32, f32),
    pub right_eye:   (f32, f32),
    pub nose:        (f32, f32),
    pub left_mouth:  (f32, f32),
    pub right_mouth: (f32, f32),
}

impl FaceLandmarks {
    /// Flat `[lx, ly, rx, ry, nx, ny, mlx, mly, mrx, mry]` — handy for
    /// iterating when computing the similarity transform.
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

/// Detector output: a localised face, a confidence score, and optional
/// landmarks.  Bbox-only detectors (UltraFace) leave `landmarks = None`;
/// alignment-capable detectors (SCRFD, RetinaFace) populate them so the
/// embedder can warp the crop to the canonical 112×112 pose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedFace {
    pub bbox:      BoundingBox,
    pub landmarks: Option<FaceLandmarks>,
    pub score:     f32,
}
