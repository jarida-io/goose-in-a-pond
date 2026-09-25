//! FaceEmbeddingExtractor port. Implementations MUST NOT write pixels or crops to disk.

use crate::user_data::domain::face_recognition::{BoundingBox, FaceLandmarks};
use anyhow::Result;
use async_trait::async_trait;

/// Face embedding (unit-norm) from JPEG/PNG/WebP bytes. With `landmarks`, warp to the 112×112
/// template first: ArcFace on unaligned crops makes everyone match at ~0.5–0.6.
#[async_trait]
pub trait FaceEmbeddingExtractor: Send + Sync {
    /// Extract an embedding; `Ok(None)` if no face or quality gates fail, `Err` for infra faults.
    async fn extract_embedding(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
        landmarks: Option<FaceLandmarks>,
    ) -> Result<Option<Vec<f32>>>;

    /// Embedding dimensionality, checked against stored embeddings before comparing.
    fn embedding_dims(&self) -> u32;
}
