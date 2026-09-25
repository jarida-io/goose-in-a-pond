//! Driven port: face registration and matching. Raw images MUST be dropped after embedding,
//! and `delete_embeddings` must erase every row for a profile ("forget all biometric data").

use crate::user_data::domain::face_recognition::{
    BoundingBox, FaceEmbedding, FaceIdentification, FaceLandmarks,
};
use anyhow::Result;
use async_trait::async_trait;

/// [`FaceRecognition::identify_with_diagnostics`] result: the verdict plus liveness signals.
#[derive(Debug, Clone)]
pub struct FaceIdentificationDetails {
    pub identification: FaceIdentification,
    /// 5-point landmarks; absent for bbox-only detectors (e.g. UltraFace fallback).
    pub landmarks: Option<FaceLandmarks>,
    /// The L2-normalised embedding scored; absent if preprocessing rejected the frame.
    pub embedding: Option<Vec<f32>>,
    /// Detected bounding box, when the detector produced one.
    pub bbox: Option<BoundingBox>,
}

/// Driven port: biometric face registration + matching.
#[async_trait]
pub trait FaceRecognition: Send + Sync {
    /// Enroll a face for `profile_id`; multiple enrollments per profile are allowed.
    async fn register_face(
        &self,
        profile_id: &str,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceEmbedding>;

    /// Identify the face in `image_bytes` against all enrolled embeddings.
    async fn identify_face(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceIdentification>;

    /// List all enrolled embeddings for a profile.
    async fn list_embeddings(&self, profile_id: &str) -> Result<Vec<FaceEmbedding>>;

    /// Delete every stored face embedding for a profile.
    async fn delete_embeddings(&self, profile_id: &str) -> Result<u64>;

    /// Cosine-similarity threshold above which an identification is conclusive.
    fn match_threshold(&self) -> f32;

    /// Per-profile threshold override; `Ok(None)` means [`Self::match_threshold`] applies.
    async fn get_profile_threshold(&self, _profile_id: &str) -> Result<Option<f32>> {
        Ok(None)
    }

    /// Set (`Some`) or clear (`None`) a profile's threshold override; `note` is for audit.
    async fn set_profile_threshold(
        &self,
        _profile_id: &str,
        _threshold: Option<f32>,
        _note: Option<&str>,
    ) -> Result<()> {
        Err(anyhow::anyhow!(
            "this FaceRecognition implementation does not support per-profile thresholds"
        ))
    }

    /// [`Self::identify_face`] plus the landmarks and embedding, for multi-frame liveness checks.
    async fn identify_with_diagnostics(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceIdentificationDetails> {
        let identification = self.identify_face(image_bytes, bbox).await?;
        Ok(FaceIdentificationDetails {
            identification,
            landmarks: None,
            embedding: None,
            bbox: None,
        })
    }

    /// Diagnostic: cosine similarity of every stored pair (~1.0 for all = collapsed embedder).
    async fn pairwise_similarities(&self) -> Result<Vec<PairwiseSimilarity>>;
}

/// One pair from [`FaceRecognition::pairwise_similarities`].
#[derive(Debug, Clone)]
pub struct PairwiseSimilarity {
    pub id_a: String,
    pub id_b: String,
    pub profile_a: String,
    pub profile_b: String,
    /// Cosine in \[-1, 1\]; ideally 0.6–0.95 for one profile, below 0.4 across profiles.
    pub similarity: f32,
    /// Convenience flag: `profile_a == profile_b`.
    pub same_profile: bool,
}
