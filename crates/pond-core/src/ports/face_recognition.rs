//! FaceRecognition port — public biometric identity differentiation.
//!
//! Exposes register / identify / delete operations over household-member face
//! embeddings.  Implementors compose a [`crate::ports::face_embedding_extractor::FaceEmbeddingExtractor`]
//! with an optional [`crate::ports::face_detector::FaceDetector`] and a
//! persistent store (typically SQLite).  Matching uses top-K mean cosine
//! similarity with a runner-up margin to suppress the "everyone ~0.6"
//! failure mode.
//!
//! # Privacy guarantees
//! - Raw image bytes MUST be discarded immediately after embedding extraction.
//! - Stored embeddings are opaque BLOBs and are not reconstructable back to
//!   the original biometric.
//! - `delete_embeddings` removes every row for a profile to satisfy the
//!   "forget all biometric data" requirement of the onboarding contract.

use crate::domain::face_recognition::{
    BoundingBox, FaceEmbedding, FaceIdentification, FaceLandmarks,
};
use anyhow::Result;
use async_trait::async_trait;

/// Rich identification result returned by
/// [`FaceRecognition::identify_with_diagnostics`] — the basic verdict plus
/// the intermediate signals that let a caller build multi-frame liveness
/// checks (landmark motion, inter-frame embedding similarity) on top of
/// the normal matcher.
///
/// All auxiliary fields are optional because preprocessing can reject a
/// frame (no face, too blurry, spoof-gate tripped) before they are
/// produced.  `identification` always reflects the verdict for that frame.
#[derive(Debug, Clone)]
pub struct FaceIdentificationDetails {
    pub identification: FaceIdentification,
    /// 5-point landmarks from the detector, when present.  Absent if the
    /// detector produced bbox-only output (e.g. UltraFace fallback).
    pub landmarks: Option<FaceLandmarks>,
    /// L2-normalised embedding that was scored against the database.
    /// Absent if the preprocessing gate rejected the frame (returned
    /// early before inference).
    pub embedding: Option<Vec<f32>>,
    /// Detected bounding box, when the detector produced one.
    pub bbox: Option<BoundingBox>,
}

/// Driven port: biometric face registration + matching.
#[async_trait]
pub trait FaceRecognition: Send + Sync {
    /// Enroll a new face for `profile_id`.  Runs the detector (if one is
    /// configured) against `image_bytes` to locate a face + landmarks, then
    /// computes and persists the embedding.  Multiple enrollments per
    /// profile are allowed (improves matching robustness).
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

    /// Cosine-similarity threshold above which an identification is
    /// considered conclusive.  Combined with a runner-up margin in the
    /// default implementation.
    fn match_threshold(&self) -> f32;

    /// Read the per-profile threshold override.  `Ok(None)` means no
    /// override is configured and the global [`Self::match_threshold`]
    /// applies.  Implementations that don't support per-profile overrides
    /// should always return `Ok(None)`.
    async fn get_profile_threshold(&self, _profile_id: &str) -> Result<Option<f32>> {
        Ok(None)
    }

    /// Install / clear a per-profile threshold override.  Pass
    /// `Some(value)` to upsert; `None` to remove the override.  Optional
    /// `note` is a free-form audit string ("tightened after sibling
    /// false-match on YYYY-MM-DD").
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

    /// Same as [`Self::identify_face`] but also surfaces the detected
    /// landmarks and the L2-normalised embedding that was scored.  Callers
    /// use the extra signals to build multi-frame liveness checks
    /// (landmark-motion std-dev, inter-frame embedding cosine); a still
    /// photo shows near-zero values on both and can be rejected before the
    /// identity verdict is surfaced to the user.
    ///
    /// Default implementation simply wraps [`Self::identify_face`] and
    /// returns `None` for the diagnostic fields; implementations that can
    /// cheaply capture the landmarks + embedding should override to
    /// populate them.
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

    /// Diagnostic: pairwise cosine similarity across every stored embedding.
    ///
    /// Returned rows are `(id_a, id_b, profile_a, profile_b, similarity)`
    /// for every unordered pair.  Meant for the `/faces/debug/pairwise`
    /// endpoint — lets an operator visually verify embeddings are diverse
    /// between different people and consistent within one person.  A healthy
    /// embedder produces within-profile scores of 0.6–0.9 and cross-profile
    /// scores below 0.4; collapsed embedders produce ~1.0 for every pair.
    async fn pairwise_similarities(&self) -> Result<Vec<PairwiseSimilarity>>;
}

/// One pair from [`FaceRecognition::pairwise_similarities`].
#[derive(Debug, Clone)]
pub struct PairwiseSimilarity {
    pub id_a:         String,
    pub id_b:         String,
    pub profile_a:    String,
    pub profile_b:    String,
    /// Cosine similarity in \[-1.0, 1.0\].  Same profile → ideally 0.6–0.95;
    /// different profile → ideally below 0.4.
    pub similarity:   f32,
    /// Convenience flag: `profile_a == profile_b`.
    pub same_profile: bool,
}
