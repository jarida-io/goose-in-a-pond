//! FaceDetector port: optional face localisation (bbox, optionally five landmarks).
//! Landmarks let the embedder warp to ArcFace's canonical 112×112 pose instead of cropping.

use crate::user_data::domain::face_recognition::DetectedFace;
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: locate the primary face in an image.
#[async_trait]
pub trait FaceDetector: Send + Sync {
    /// Detect the primary face; `Ok(None)` if absent or unsure, `Err` only for infra failures.
    async fn detect_face(&self, image_bytes: &[u8]) -> Result<Option<DetectedFace>>;

    /// Whether this emits the five landmarks alignment needs; `false` means crop-only.
    fn produces_landmarks(&self) -> bool {
        false
    }
}

/// Always `Ok(None)`, so callers center-crop; for builds without a detection model.
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
