//! Proof-of-settlement record exchanged by `PaymentRail` and `UsageTally`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::millisats::Millisats;
use super::peer_id::PeerId;

/// Millisats per borrowed token, fixed so a borrower cannot pick its own rate (value provisional).
pub const MESH_SETTLEMENT_MILLISATS_PER_TOKEN: u64 = 30;

/// A completed Lightning settlement; `preimage` is the payment proof and must be retained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementRecord {
    pub peer_id: PeerId,
    pub amount: Millisats,
    pub preimage: String,
    pub settled_at: DateTime<Utc>,
}
