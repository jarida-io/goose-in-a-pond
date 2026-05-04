//! Biometric domain types — voice prints and face embeddings.
//!
//! Privacy guarantees enforced by all implementations:
//! - Raw audio bytes are discarded immediately after embedding extraction.
//! - Raw video frames are never written to disk.
//! - Embeddings are stored as opaque BLOBs — not reconstructable to original biometric.
//! - All identification events are audited in `biometric_audit_log`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A stored speaker (voice) embedding for a household member.
///
/// The raw audio that produced this embedding is discarded immediately after extraction.
/// Only this metadata record and the BLOB (in the database) are retained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerEmbedding {
    pub id: String,
    pub profile_id: String,
    /// Model that produced this embedding (e.g. `"x-vector"` → 512-d).
    pub model: String,
    /// Number of float32 dimensions in the embedding vector.
    pub dims: u32,
    /// Cosine-similarity threshold for a positive match (0.0–1.0).
    pub threshold: f32,
    pub created_at: DateTime<Utc>,
}

/// Response from `POST /api/v1/enroll`.
///
/// Fields are `None` when that modality was not supplied in the request
/// (e.g. audio-only enrolment leaves `face_embedding` as `None`).
#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub profile_id: String,
    pub speaker_embedding: Option<SpeakerEmbedding>,
    /// Populated once face recognition is wired.
    pub face_embedding: Option<serde_json::Value>,
}
