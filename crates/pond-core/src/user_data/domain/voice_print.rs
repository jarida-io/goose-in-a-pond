//! Voice Match's stored evidence: one embedding per enrolment take, and whether the wake CNN heard
//! each take. No audio is ever kept.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One take's speaker embedding for a household member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoicePrint {
    pub id: String,
    pub profile_id: String,
    pub embedding: Vec<f32>,
    /// The producing model; prints from another model are never compared with this one's.
    pub model_name: String,
    pub created_at: DateTime<Utc>,
}

/// Whether the "hey goose" CNN fired on one enrolment take of the household's phrase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeTake {
    pub id: String,
    /// `None` for a take taught with Voice Match off and nobody named.
    pub profile_id: Option<String>,
    /// The normalized wake phrase the take was read for.
    pub phrase: String,
    pub cnn_heard: bool,
    pub created_at: DateTime<Utc>,
}
