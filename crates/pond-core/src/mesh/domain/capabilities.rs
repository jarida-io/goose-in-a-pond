//! What a mesh peer currently offers; queried live, not persisted.

/// A snapshot, not a promise: `inference_available` means the peer will *attempt* to serve now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerCapabilities {
    pub inference_available: bool,
    pub lightning_available: bool,
}
