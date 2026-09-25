//! The mesh's trust dial; add `OpenLane` (public, zero-trust) when that capability is built.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustScope {
    /// A peer running on hardware the same owner controls.
    SelfOwned,
    /// A peer belonging to a trusted family/friends/community circle.
    Circle,
}
