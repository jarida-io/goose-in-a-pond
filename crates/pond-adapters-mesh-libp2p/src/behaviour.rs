//! A mesh node's libp2p `NetworkBehaviour` and message shapes; Circle-only, no public discovery.

use libp2p::swarm::NetworkBehaviour;
use libp2p::{dcutr, gossipsub, identify, kad, ping, relay, request_response};
use serde::{Deserialize, Serialize};

/// The only mesh protocol: a `Handshake` (dialer to listener), then opaque `Frame`s once accepted.
pub const MESH_PROTOCOL: &str = "/pond-mesh/1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshRequest {
    /// A prost-encoded `pond_mesh_protocol::wire::Handshake`.
    Handshake(Vec<u8>),
    /// An opaque frame, accepted only after a completed handshake.
    Frame(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshResponse {
    /// The responder's own prost-encoded `Handshake`, so both directions are authenticated.
    HandshakeAccepted(Vec<u8>),
    /// Hash mismatch, bad signature, identity not matching the connection, or untrusted peer.
    HandshakeRejected,
    FrameAck,
}

#[derive(NetworkBehaviour)]
pub struct MeshBehaviour {
    /// Required by `dcutr`, which hole-punches using the addresses `identify` observes.
    pub identify: identify::Behaviour,
    /// Address rendezvous only, not discovery: a peer publishes `hash(own PeerId) -> multiaddrs`
    /// so its Circle can re-find it; never queried for an unknown id.
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    /// One topic per Circle, for presence/address announcements only; internal.
    pub gossipsub: gossipsub::Behaviour,
    /// Relay for connected peers: there is no dedicated relay server; any member can relay.
    pub relay: relay::Behaviour,
    /// Use a connected peer as a relay when a target can't be dialed directly.
    pub relay_client: relay::client::Behaviour,
    /// Attempts to upgrade a relayed connection to a direct one.
    pub dcutr: dcutr::Behaviour,
    pub mesh_rr: request_response::cbor::Behaviour<MeshRequest, MeshResponse>,
    /// Keeps idle links alive past libp2p's idle timeout, so `connected_peers()` doesn't flap.
    pub ping: ping::Behaviour,
}
