//! ExtractionGate port: the critic a window's candidates pass before any of them is saved.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::ports::conversation_extractor::ExtractionWindow;
use crate::user_data::services::extraction_gate::GateCandidate;

/// Driven port: ask a critic which of one window's candidates to drop.
#[async_trait]
pub trait ExtractionGate: Send + Sync {
    /// The candidates to drop, as places from 0 with the critic's reason. `Ok(None)` when its
    /// reply cannot be read; `Err` when it was never heard, and the window is then read again.
    async fn judge(
        &self,
        window: ExtractionWindow<'_>,
        candidates: &[GateCandidate],
    ) -> Result<Option<Vec<(usize, String)>>>;
}
