//! FaceDetector port — optional face-localisation stage.
//!
//! A [`FaceDetector`] finds the primary face in an image and returns its
//! bounding box plus (optionally) five canonical landmarks.  The embedding
//! pipeline then either
//!
//!   * crops to the bbox, if only a bbox is returned; or
//!   * warps the image to the canonical 112×112 pose using a similarity
//!     transform fitted to the landmarks, which is what ArcFace was trained
//!     on and what real-world accuracy requires.
//!
//! # No-op default
//!
//! [`NoopFaceDetector`] implements the trait by always returning `Ok(None)`,
//! which is semantically "I don't know where the face is — let the embedder
//! fall back to its default crop".  Use it as a placeholder when wiring
//! [`crate::ports::face_recognition`] in environments that do not bundle a
//! detection model.

use crate::domain::face_recognition::DetectedFace;
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: locate the primary face in an image.
#[async_trait]
pub trait FaceDetector: Send + Sync {
    /// Detect the primary face in `image_bytes`.
    ///
    /// Returns `Ok(Some(face))` when a face is confidently located,
    /// `Ok(None)` when no face is detected or the detector is unsure,
    /// and `Err` only for infrastructure failures (model load, decode).
    async fn detect_face(&self, image_bytes: &[u8]) -> Result<Option<DetectedFace>>;

    /// Whether this detector emits the five-point landmark set required for
    /// similarity-transform alignment.  Callers can short-circuit to the
    /// crop-only path when this returns `false`.
    fn produces_landmarks(&self) -> bool {
        false
    }
}

/// No-op detector: always returns `Ok(None)`.  Callers fall back to
/// center-square cropping.  Safe default when no detection model is bundled.
pub struct NoopFaceDetector;

#[async_trait]
impl FaceDetector for NoopFaceDetector {
    async fn detect_face(&self, _image_bytes: &[u8]) -> Result<Option<DetectedFace>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_detector_returns_none() {
        let d = NoopFaceDetector;
        assert!(d.detect_face(b"whatever").await.unwrap().is_none());
    }

    #[test]
    fn noop_does_not_produce_landmarks() {
        assert!(!NoopFaceDetector.produces_landmarks());
    }
}
