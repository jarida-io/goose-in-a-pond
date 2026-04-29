//! FaceEmbeddingExtractor port — driven port for computing face embeddings.
//!
//! Implementors run an ONNX pipeline that:
//!   1. Resolves a face region (either caller-supplied bbox, landmarks from a
//!      detector, or a center-square fallback).
//!   2. Either crops+resizes (bbox path) or similarity-warps to canonical
//!      112×112 (landmarks path) before feeding the embedding network.
//!   3. Returns a unit-norm float vector suitable for cosine-similarity
//!      comparison.
//!
//! # Alignment matters
//! ArcFace and MobileFaceNet are trained on faces warped to a canonical
//! 112×112 template via a similarity transform fitted to five landmarks.
//! Running the network on an un-aligned crop produces embeddings that
//! collapse toward a common direction and causes everyone to match everyone
//! at ~0.5–0.6 similarity — the exact failure mode the phase-2 hardening
//! work is addressing.
//!
//! # Privacy
//! Implementations MUST NOT write raw image pixels or intermediate crops to
//! disk.  Only the embedding vector may leave the method boundary.

use crate::domain::face_recognition::{BoundingBox, FaceLandmarks};
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: compute a face embedding from raw image bytes.
///
/// Implementors are expected to:
/// - Accept any common image format (JPEG, PNG, WebP) in `image_bytes`.
/// - If `landmarks` is supplied, similarity-warp to the canonical 112×112
///   template before running the network.
/// - Otherwise crop by `bbox` (or the center square) and resize.
/// - Return `Ok(None)` when the input is empty or fails quality gates.
/// - Discard all intermediate pixel data before returning.
#[async_trait]
pub trait FaceEmbeddingExtractor: Send + Sync {
    /// Extract a face embedding from `image_bytes`.
    ///
    /// Returns `Ok(None)` when no face is detected or the crop fails the
    /// adapter's quality gates.  Returns `Err` only on infrastructure
    /// failures (model not loaded, image decode error, etc.).
    async fn extract_embedding(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
        landmarks: Option<FaceLandmarks>,
    ) -> Result<Option<Vec<f32>>>;

    /// Dimensionality of the embeddings this extractor produces.
    /// Used to validate compatibility when comparing stored embeddings.
    fn embedding_dims(&self) -> u32;
}
