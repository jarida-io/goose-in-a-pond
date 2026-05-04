//! Driven port: SpeakerIdentification
//!
//! Abstracts speaker identification so the wake-word flow and session tagging
//! do not depend on any specific embedding backend.
//!
//! Implementations:
//! - `OnnxSpeakerAdapter` (pond-adapters-speaker-embed) — in-process ONNX x-vector
//!   TDNN model via the `ort` crate (512-d embeddings); stores BLOBs in SQLite.

use crate::domain::biometric::SpeakerEmbedding;
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: speaker identification from raw audio bytes.
///
/// Implementations MUST:
/// - Discard `audio_bytes` immediately after extracting the embedding.
/// - Never write raw audio to disk.
/// - Log every identification event (including non-matches) to `biometric_audit_log`.
#[async_trait]
pub trait SpeakerIdentification: Send + Sync {
    /// Enrol a speaker: extract an embedding from `audio_bytes` and persist it
    /// linked to `profile_id`.
    ///
    /// Call at least 3 times per user for reliable identification.
    async fn register_speaker(
        &self,
        profile_id: &str,
        audio_bytes: &[u8],
    ) -> Result<SpeakerEmbedding>;

    /// Identify the speaker from raw audio.
    ///
    /// Returns `Some((profile_id, confidence))` when the best cosine-similarity
    /// match clears the per-model threshold, or `None` for unknown speakers.
    /// Unknown speakers are NOT an error — the session proceeds without attribution.
    async fn identify_speaker(
        &self,
        audio_bytes: &[u8],
    ) -> Result<Option<(String, f32)>>;

    /// Remove all stored embeddings for `profile_id`.
    ///
    /// Called by `DELETE /api/v1/users/:id/biometrics`.
    async fn delete_speaker(&self, profile_id: &str) -> Result<()>;

    /// Return the number of stored embeddings for `profile_id`.
    async fn enrollment_count(&self, profile_id: &str) -> Result<u32>;

    /// Return metadata for every stored embedding for `profile_id`.
    /// Raw BLOB vectors are never included.
    async fn list_enrollments(&self, profile_id: &str) -> Result<Vec<SpeakerEmbedding>>;
}
