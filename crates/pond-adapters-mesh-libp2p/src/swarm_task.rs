//! Sole owner of the `Swarm`, which can't be shared: commands in, inbound frames out.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId};
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, kad, noise, relay, tcp, yamux, Multiaddr, Swarm};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId as DomainPeerId;
use pond_core::mesh::ports::mesh_transport::MeshTransportError;
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_mesh_protocol::identity::MeshKeypair;
use pond_mesh_protocol::wire::Handshake;

use crate::adapter::Libp2pMeshTransportConfig;
use crate::behaviour::{
    MeshBehaviour, MeshBehaviourEvent, MeshRequest, MeshResponse, MESH_PROTOCOL,
};
use crate::identity::{domain_peer_to_libp2p, to_libp2p_keypair};

type Libp2pPeerId = libp2p::PeerId;
type InboundFrame = (DomainPeerId, Vec<u8>);

pub enum Command {
    Connect {
        peer: DomainPeerId,
        address: String,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    },
    Send {
        peer: DomainPeerId,
        frame: Vec<u8>,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    },
    ConnectedPeers {
        reply: oneshot::Sender<Vec<DomainPeerId>>,
    },
    /// Adapter-only test/setup helper: multiaddrs are libp2p-specific.
    ListenAddrs {
        reply: oneshot::Sender<Vec<Multiaddr>>,
    },
    /// Reserve a relay slot on a reachable peer, to be dialable via `.../p2p-circuit/p2p/<self>`.
    ReserveRelay {
        relay_peer: DomainPeerId,
        relay_address: String,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    },
}

pub struct SwarmHandles {
    pub command_tx: mpsc::UnboundedSender<Command>,
    pub inbound_rx: mpsc::UnboundedReceiver<InboundFrame>,
    pub task: JoinHandle<()>,
}

pub async fn spawn(config: Libp2pMeshTransportConfig) -> anyhow::Result<SwarmHandles> {
    let local_handshake_bytes =
        build_local_handshake(&config.keypair, config.harness_hash, config.model_hash)
            .encode_to_vec();
    let swarm = build_swarm(config.keypair, config.listen_addr).await?;

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

    let event_loop = EventLoop {
        swarm,
        command_rx,
        inbound_tx,
        harness_hash: config.harness_hash,
        model_hash: config.model_hash,
        peer_directory: config.peer_directory,
        local_handshake_bytes,
        pending_connect: HashMap::new(),
        pending_handshake: HashMap::new(),
        connected: HashMap::new(),
        known_addresses: HashMap::new(),
        listen_addrs: Vec::new(),
    };
    let task = tokio::spawn(event_loop.run());

    Ok(SwarmHandles {
        command_tx,
        inbound_rx,
        task,
    })
}

fn build_local_handshake(
    keypair: &MeshKeypair,
    harness: HarnessHash,
    model: ModelHash,
) -> Handshake {
    let unsigned = Handshake::new(keypair.peer_id(), harness, model, [0u8; 64]);
    let signature = keypair.sign(&unsigned.signed_payload());
    Handshake::new(keypair.peer_id(), harness, model, signature)
}

/// WebSocket listener port (free HTTP/TLS tunnels can't carry raw TCP): offset from the TCP port
/// to stay stable per identity; `0` (OS-assigned) stays `0`.
fn ws_port_for(tcp_port: u16) -> u16 {
    if tcp_port == 0 {
        0
    } else {
        tcp_port + 1
    }
}

async fn build_swarm(
    keypair: MeshKeypair,
    listen_addr: Multiaddr,
) -> anyhow::Result<Swarm<MeshBehaviour>> {
    let libp2p_keypair = to_libp2p_keypair(&keypair);

    let tcp_port = listen_addr
        .iter()
        .find_map(|p| match p {
            Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .unwrap_or(0);
    let ws_listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}/ws", ws_port_for(tcp_port))
        .parse()
        .expect("valid multiaddr literal");

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(libp2p_keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_websocket(noise::Config::new, yamux::Config::default)
        .await?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|local_keypair, relay_client| {
            let peer_id = local_keypair.public().to_peer_id();
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(local_keypair.clone()),
                gossipsub::Config::default(),
            )?;
            Ok(MeshBehaviour {
                identify: identify::Behaviour::new(identify::Config::new(
                    MESH_PROTOCOL.to_string(),
                    local_keypair.public(),
                )),
                kad: kad::Behaviour::new(peer_id, kad::store::MemoryStore::new(peer_id)),
                gossipsub,
                relay: relay::Behaviour::new(peer_id, relay::Config::default()),
                relay_client,
                dcutr: libp2p::dcutr::Behaviour::new(peer_id),
                mesh_rr: request_response::cbor::Behaviour::new(
                    [(
                        libp2p::StreamProtocol::new(MESH_PROTOCOL),
                        request_response::ProtocolSupport::Full,
                    )],
                    request_response::Config::default(),
                ),
                ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
            })
        })?
        .with_swarm_config(|cfg| {
            // Well above the ping interval, so ping, not this timeout, keeps idle links open.
            cfg.with_idle_connection_timeout(Duration::from_secs(60))
        })
        .build();

    swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));
    swarm.listen_on(listen_addr)?;
    swarm.listen_on(ws_listen_addr)?;
    Ok(swarm)
}

struct EventLoop {
    swarm: Swarm<MeshBehaviour>,
    command_rx: mpsc::UnboundedReceiver<Command>,
    inbound_tx: mpsc::UnboundedSender<InboundFrame>,
    harness_hash: HarnessHash,
    model_hash: ModelHash,
    /// Who this Pond trusts; consulted on every handshake, both directions.
    peer_directory: Arc<dyn PeerDirectory>,
    /// Signed once: our identity and hashes never change.
    local_handshake_bytes: Vec<u8>,
    /// Dials in flight, answered once the handshake (not just the connection) completes.
    pending_connect: HashMap<
        Libp2pPeerId,
        (
            DomainPeerId,
            oneshot::Sender<Result<(), MeshTransportError>>,
        ),
    >,
    /// Outbound `Handshake` requests, to match each response to its peer.
    pending_handshake: HashMap<OutboundRequestId, Libp2pPeerId>,
    /// Peers whose handshake has been verified in either direction.
    connected: HashMap<Libp2pPeerId, DomainPeerId>,
    /// Last address given for each peer, for the retry loop to redial; seeded from the directory.
    known_addresses: HashMap<Libp2pPeerId, (DomainPeerId, Multiaddr)>,
    /// Confirmed listen addresses, relay circuits included once a reservation is accepted.
    listen_addrs: Vec<Multiaddr>,
}

/// How often trusted-but-disconnected peers are redialed; a cheap safety net, not a hot path.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

impl EventLoop {
    async fn run(mut self) {
        self.load_known_addresses().await;
        let mut reconnect_tick = tokio::time::interval(RECONNECT_INTERVAL);
        // The first tick fires at once, redialing the peers loaded above.
        reconnect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => self.handle_swarm_event(event).await,
                command = self.command_rx.recv() => match command {
                    Some(command) => self.handle_command(command).await,
                    None => return,
                },
                _ = reconnect_tick.tick() => self.retry_disconnected_known_peers().await,
            }
        }
    }

    /// Seed `known_addresses` from `PeerDirectory`, so peers trusted before a restart are redialed.
    async fn load_known_addresses(&mut self) {
        let rows = match self.peer_directory.known_addresses().await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!("mesh: failed to load persisted peer addresses: {err}");
                return;
            }
        };
        for (peer, addr_str) in rows {
            let Ok(libp2p_peer) = domain_peer_to_libp2p(peer) else {
                continue;
            };
            let Ok(addr) = addr_str.parse::<Multiaddr>() else {
                continue;
            };
            self.known_addresses.insert(libp2p_peer, (peer, addr));
        }
    }

    /// Redial every trusted, known, disconnected peer, so a dropped link heals without `connect()`.
    async fn retry_disconnected_known_peers(&mut self) {
        let candidates: Vec<(Libp2pPeerId, DomainPeerId, Multiaddr)> = self
            .known_addresses
            .iter()
            .filter(|(libp2p_peer, _)| {
                !self.connected.contains_key(libp2p_peer)
                    && !self.pending_connect.contains_key(libp2p_peer)
            })
            .map(|(libp2p_peer, (peer, addr))| (*libp2p_peer, *peer, addr.clone()))
            .collect();

        for (libp2p_peer, peer, addr) in candidates {
            // Trust may have been revoked since the address was learned.
            if !Self::is_trusted(self.peer_directory.clone(), peer).await {
                continue;
            }
            // Fire-and-forget: nobody awaits the reply; the outcome surfaces via swarm events.
            let (reply, _ignored) = oneshot::channel();
            self.dial_peer(libp2p_peer, peer, addr, reply);
        }
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Connect {
                peer,
                address,
                reply,
            } => {
                let libp2p_peer = match domain_peer_to_libp2p(peer) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ =
                            reply.send(Err(MeshTransportError::MalformedAddress(err.to_string())));
                        return;
                    }
                };
                let addr: Multiaddr = match address.parse() {
                    Ok(addr) => addr,
                    Err(_) => {
                        let _ = reply.send(Err(MeshTransportError::MalformedAddress(address)));
                        return;
                    }
                };
                self.known_addresses
                    .insert(libp2p_peer, (peer, addr.clone()));
                // Best-effort: routes.rs adds trust before `connect()`, so the row exists; on
                // failure the in-memory copy above still serves this process.
                if let Err(err) = self
                    .peer_directory
                    .record_peer_address(peer, addr.to_string())
                    .await
                {
                    tracing::warn!("mesh: failed to persist last-known address for {peer}: {err}");
                }
                self.dial_peer(libp2p_peer, peer, addr, reply);
            }
            Command::Send { peer, frame, reply } => {
                let libp2p_peer = match domain_peer_to_libp2p(peer) {
                    Ok(p) => p,
                    Err(_) => {
                        let _ = reply.send(Err(MeshTransportError::PeerUnreachable(peer)));
                        return;
                    }
                };
                if !self.connected.contains_key(&libp2p_peer) {
                    let _ = reply.send(Err(MeshTransportError::PeerUnreachable(peer)));
                    return;
                }
                self.swarm
                    .behaviour_mut()
                    .mesh_rr
                    .send_request(&libp2p_peer, MeshRequest::Frame(frame));
                let _ = reply.send(Ok(()));
            }
            Command::ConnectedPeers { reply } => {
                let peers = self.connected.values().copied().collect();
                let _ = reply.send(peers);
            }
            Command::ListenAddrs { reply } => {
                let _ = reply.send(self.listen_addrs.clone());
            }
            Command::ReserveRelay {
                relay_peer,
                relay_address,
                reply,
            } => {
                let libp2p_relay_peer = match domain_peer_to_libp2p(relay_peer) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ =
                            reply.send(Err(MeshTransportError::MalformedAddress(err.to_string())));
                        return;
                    }
                };
                let addr: Multiaddr = match relay_address.parse() {
                    Ok(addr) => addr,
                    Err(_) => {
                        let _ =
                            reply.send(Err(MeshTransportError::MalformedAddress(relay_address)));
                        return;
                    }
                };
                let circuit_addr = addr
                    .with(Protocol::P2p(libp2p_relay_peer))
                    .with(Protocol::P2pCircuit);
                match self.swarm.listen_on(circuit_addr) {
                    Ok(_) => {
                        let _ = reply.send(Ok(()));
                    }
                    Err(err) => {
                        let _ = reply.send(Err(MeshTransportError::Transport(err.to_string())));
                    }
                }
            }
        }
    }

    fn dial_peer(
        &mut self,
        libp2p_peer: Libp2pPeerId,
        peer: DomainPeerId,
        addr: Multiaddr,
        reply: oneshot::Sender<Result<(), MeshTransportError>>,
    ) {
        self.swarm
            .behaviour_mut()
            .kad
            .add_address(&libp2p_peer, addr.clone());
        // `PortUse::Reuse` (default) is needed for DCUtR but collides on one machine, hence
        // POND_DEV_SAME_MACHINE_MESH. `Always`: a rejected handshake leaves the connection open.
        let mut opts = DialOpts::peer_id(libp2p_peer)
            .condition(PeerCondition::Always)
            .addresses(vec![addr.clone()]);
        if same_machine_dev_mesh_enabled(
            std::env::var("POND_DEV_SAME_MACHINE_MESH").ok().as_deref(),
        ) {
            opts = opts.allocate_new_port();
        }
        match self.swarm.dial(opts.build()) {
            Ok(()) => {
                // `Always` allows a second dial to a peer; fail the earlier caller's reply
                // rather than drop it (it would read as "mesh swarm task has stopped").
                if let Some((_, stale_reply)) =
                    self.pending_connect.insert(libp2p_peer, (peer, reply))
                {
                    let _ = stale_reply.send(Err(MeshTransportError::Transport(
                        "superseded by a newer connect attempt to the same peer".to_string(),
                    )));
                }
            }
            Err(err) => {
                let _ = reply.send(Err(MeshTransportError::Transport(err.to_string())));
            }
        }
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<MeshBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                self.listen_addrs.push(address);
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                if endpoint.is_dialer() && self.pending_connect.contains_key(&peer_id) {
                    let request_id = self.swarm.behaviour_mut().mesh_rr.send_request(
                        &peer_id,
                        MeshRequest::Handshake(self.local_handshake_bytes.clone()),
                    );
                    self.pending_handshake.insert(request_id, peer_id);
                }
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(&peer_id);
            }
            SwarmEvent::Behaviour(MeshBehaviourEvent::Identify(identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            })) => {
                // Relay reservations need a known external address; this is how we learn one.
                self.swarm.add_external_address(observed_addr);
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } => {
                if let Some((_, reply)) = self.pending_connect.remove(&peer_id) {
                    let _ = reply.send(Err(MeshTransportError::Transport(error.to_string())));
                }
            }
            SwarmEvent::Behaviour(MeshBehaviourEvent::MeshRr(
                request_response::Event::Message { peer, message, .. },
            )) => self.handle_mesh_message(peer, message).await,
            SwarmEvent::Behaviour(MeshBehaviourEvent::MeshRr(
                request_response::Event::OutboundFailure {
                    request_id, error, ..
                },
            )) => {
                // A dropped connection must still resolve the caller's reply, not leave it hanging.
                if let Some(libp2p_peer) = self.pending_handshake.remove(&request_id) {
                    if let Some((_, reply)) = self.pending_connect.remove(&libp2p_peer) {
                        let _ = reply.send(Err(MeshTransportError::Transport(error.to_string())));
                    }
                }
            }
            _ => {}
        }
    }

    async fn handle_mesh_message(
        &mut self,
        peer: Libp2pPeerId,
        message: request_response::Message<MeshRequest, MeshResponse>,
    ) {
        match message {
            request_response::Message::Request {
                request, channel, ..
            } => match request {
                MeshRequest::Handshake(bytes) => {
                    let verified = match self.verify_handshake(&bytes, peer) {
                        Some(claimed)
                            if Self::is_trusted(self.peer_directory.clone(), claimed).await =>
                        {
                            Some(claimed)
                        }
                        _ => None,
                    };
                    let response = if verified.is_some() {
                        // Our own handshake, so the dialer can verify us in turn.
                        MeshResponse::HandshakeAccepted(self.local_handshake_bytes.clone())
                    } else {
                        MeshResponse::HandshakeRejected
                    };
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .mesh_rr
                        .send_response(channel, response);
                    // No disconnect on rejection: it races `send_response` and can strand the
                    // dialer; staying out of `connected` already keeps it out of `recv()`.
                    if let Some(domain_peer) = verified {
                        self.connected.insert(peer, domain_peer);
                    }
                }
                MeshRequest::Frame(bytes) => {
                    if let Some(domain_peer) = self.connected.get(&peer).copied() {
                        let _ = self.inbound_tx.send((domain_peer, bytes));
                    }
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .mesh_rr
                        .send_response(channel, MeshResponse::FrameAck);
                }
            },
            request_response::Message::Response {
                request_id,
                response,
            } => {
                if let Some(libp2p_peer) = self.pending_handshake.remove(&request_id) {
                    match response {
                        // Verify theirs too: a bare "yes" would let any peer reach `recv()`.
                        MeshResponse::HandshakeAccepted(their_handshake) => {
                            let ok = match self.verify_handshake(&their_handshake, libp2p_peer) {
                                Some(claimed) => {
                                    Self::is_trusted(self.peer_directory.clone(), claimed).await
                                }
                                None => false,
                            };
                            if let Some((domain_peer, reply)) =
                                self.pending_connect.remove(&libp2p_peer)
                            {
                                if ok {
                                    self.connected.insert(libp2p_peer, domain_peer);
                                    let _ = reply.send(Ok(()));
                                } else {
                                    let _ = reply.send(Err(MeshTransportError::Transport(
                                        "peer accepted our handshake but failed ours: harness/model \
                                         mismatch, bad signature, or not a trusted peer"
                                            .to_string(),
                                    )));
                                }
                            }
                        }
                        _ => {
                            if let Some((domain_peer, reply)) =
                                self.pending_connect.remove(&libp2p_peer)
                            {
                                let _ = reply
                                    .send(Err(MeshTransportError::PeerUnreachable(domain_peer)));
                            }
                        }
                    }
                }
                // FrameAck needs no handling: `send()` reports dispatch, not delivery.
            }
        }
    }

    /// Verify a `Handshake`: signature, harness/model pin, and that the claimed identity is
    /// `conn_peer`. That binding is the security: `signed_payload()` has no nonce, so it replays.
    fn verify_handshake(&self, bytes: &[u8], conn_peer: Libp2pPeerId) -> Option<DomainPeerId> {
        verify_handshake_bytes(bytes, conn_peer, self.harness_hash, self.model_hash)
    }

    /// Is `peer` in the trust circle (the handshake only proves same build and model)? Takes an
    /// `Arc` because `Swarm` is `Send` but not `Sync`.
    async fn is_trusted(directory: Arc<dyn PeerDirectory>, peer: DomainPeerId) -> bool {
        match directory.trust_scope_of(peer).await {
            Ok(scope) => scope.is_some(),
            // Fail closed: an unreadable directory is not evidence of trust.
            Err(err) => {
                tracing::warn!("mesh: peer directory unreadable, refusing {peer}: {err}");
                false
            }
        }
    }
}

/// A free function so the identity binding can be tested: honest peers never exercise it.
fn verify_handshake_bytes(
    bytes: &[u8],
    conn_peer: Libp2pPeerId,
    harness_hash: HarnessHash,
    model_hash: ModelHash,
) -> Option<DomainPeerId> {
    let handshake = Handshake::decode(bytes).ok()?;
    let peer_bytes: [u8; 32] = handshake.peer_id.clone().try_into().ok()?;
    let claimed_peer = DomainPeerId::from(peer_bytes);

    // The claimed identity must BE the connection's authenticated identity.
    if domain_peer_to_libp2p(claimed_peer).ok()? != conn_peer {
        return None;
    }

    let harness_bytes: [u8; 32] = handshake.harness_hash.clone().try_into().ok()?;
    if HarnessHash::from(harness_bytes) != harness_hash {
        return None;
    }
    let model_bytes: [u8; 32] = handshake.model_hash.clone().try_into().ok()?;
    if ModelHash::from(model_bytes) != model_hash {
        return None;
    }

    let signature: [u8; 64] = handshake.signature.clone().try_into().ok()?;
    let verified =
        pond_mesh_protocol::identity::verify(claimed_peer, &handshake.signed_payload(), &signature)
            .ok()?;
    verified.then_some(claimed_peer)
}

/// Truthiness of `POND_DEV_SAME_MACHINE_MESH`, apart from the env read for testing.
fn same_machine_dev_mesh_enabled(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("TRUE"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> HarnessHash {
        HarnessHash::from([7u8; 32])
    }
    fn model() -> ModelHash {
        ModelHash::from([8u8; 32])
    }

    /// Guards the impersonation test below against a function that rejects everything.
    #[test]
    fn a_peer_presenting_its_own_handshake_on_its_own_connection_is_accepted() {
        let keypair = MeshKeypair::generate();
        let bytes = build_local_handshake(&keypair, harness(), model()).encode_to_vec();
        let conn = domain_peer_to_libp2p(keypair.peer_id()).unwrap();

        assert_eq!(
            verify_handshake_bytes(&bytes, conn, harness(), model()),
            Some(keypair.peer_id()),
        );
    }

    /// No nonce in `signed_payload()`, so every peer the victim dialled holds a valid copy.
    #[test]
    fn a_replayed_handshake_cannot_impersonate_the_peer_that_signed_it() {
        let victim = MeshKeypair::generate();
        let attacker = MeshKeypair::generate();

        let stolen = build_local_handshake(&victim, harness(), model()).encode_to_vec();
        let attacker_conn = domain_peer_to_libp2p(attacker.peer_id()).unwrap();

        assert_eq!(
            verify_handshake_bytes(&stolen, attacker_conn, harness(), model()),
            None,
            "a captured handshake replayed over another peer's connection was \
             accepted -- every frame that peer sends would be attributed to the \
             victim",
        );
    }

    #[test]
    fn a_mismatched_harness_or_model_is_refused_on_an_otherwise_honest_connection() {
        let keypair = MeshKeypair::generate();
        let conn = domain_peer_to_libp2p(keypair.peer_id()).unwrap();

        let wrong_harness =
            build_local_handshake(&keypair, HarnessHash::from([1u8; 32]), model()).encode_to_vec();
        assert_eq!(
            verify_handshake_bytes(&wrong_harness, conn, harness(), model()),
            None,
            "harness hash mismatch was accepted"
        );

        let wrong_model =
            build_local_handshake(&keypair, harness(), ModelHash::from([2u8; 32])).encode_to_vec();
        assert_eq!(
            verify_handshake_bytes(&wrong_model, conn, harness(), model()),
            None,
            "model hash mismatch was accepted"
        );
    }

    /// Even when the claimed identity matches the connection (a peer tampering with its own).
    #[test]
    fn a_signature_that_does_not_verify_is_refused() {
        let keypair = MeshKeypair::generate();
        let conn = domain_peer_to_libp2p(keypair.peer_id()).unwrap();
        let forged =
            Handshake::new(keypair.peer_id(), harness(), model(), [0u8; 64]).encode_to_vec();

        assert_eq!(
            verify_handshake_bytes(&forged, conn, harness(), model()),
            None,
        );
    }

    /// Unset or unrecognised must fail closed to the DCUtR-compatible default.
    #[test]
    fn same_machine_dev_mesh_defaults_to_disabled() {
        assert!(!same_machine_dev_mesh_enabled(None));
        assert!(!same_machine_dev_mesh_enabled(Some("")));
        assert!(!same_machine_dev_mesh_enabled(Some("0")));
        assert!(!same_machine_dev_mesh_enabled(Some("false")));
        assert!(!same_machine_dev_mesh_enabled(Some("yes")));
    }

    #[test]
    fn same_machine_dev_mesh_recognises_truthy_values() {
        assert!(same_machine_dev_mesh_enabled(Some("1")));
        assert!(same_machine_dev_mesh_enabled(Some("true")));
        assert!(same_machine_dev_mesh_enabled(Some("TRUE")));
    }
}
