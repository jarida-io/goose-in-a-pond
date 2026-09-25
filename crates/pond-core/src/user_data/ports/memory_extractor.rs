//! MemoryExtractor port — extracts durable facts from conversation turns.

use crate::user_data::domain::memory::{MemorySegment, MemoryTier};
use anyhow::Result;
use async_trait::async_trait;

/// A single fact extracted from a conversation turn.
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub content: String,
    pub segment: MemorySegment,
    pub importance: f32,
    pub tier: MemoryTier,
    /// For corrections: the wrong claim being fixed, so consolidation can't revert it.
    pub corrects: Option<String>,
}

/// Driven port: extract durable facts from a user–assistant exchange.
#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    /// Return a turn's facts worth remembering (at most 3); `existing_content` is for dedup.
    async fn extract(
        &self,
        user_message: &str,
        assistant_response: &str,
        existing_content: &[String],
    ) -> Result<Vec<ExtractedFact>>;
}
