use std::sync::Arc;

use async_trait::async_trait;
use libp2p::Multiaddr;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::ports::mesh_transport::{MeshTransport, MeshTransportError};
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_mesh_protocol::identity::MeshKeypair;

use crate::swarm_task::{self, Command, SwarmHandles};

pub struct Libp2pMeshTransportConfig {
    pub listen_addr: Multiaddr,
    pub harness_hash: HarnessHash,
    pub model_hash: ModelHash,
    pub keypair: MeshKeypair,
    /// Who this Pond trusts. Not optional: it is the only check between a circle and the internet.
    pub peer_directory: Arc<dyn PeerDirectory>,
}

/// Real `MeshTransport` over libp2p: channels to the `Swarm`'s background task (`swarm_task`).
pub struct Libp2pMeshTransport {
    local_peer_id: PeerId,
    command_tx: tokio::sync::mpsc::UnboundedSender<Command>,
    inbound_rx: Mutex<tokio::sync::mpsc::UnboundedReceiver<(PeerId, Vec<u8>)>>,
    _task: JoinHandle<()>,
}

impl Libp2pMeshTransport {
    pub async fn new(config: Libp2pMeshTransportConfig) -> anyhow::Result<Self> {
        let local_peer_id = config.keypair.peer_id();
        let SwarmHandles {
            command_tx,
            inbound_rx,
            task,
        } = swarm_task::spawn(config).await?;
        Ok(Self {
            local_peer_id,
            command_tx,
            inbound_rx: Mutex::new(inbound_rx),
            _task: task,
        })
    }

    /// Reserve a relay slot through a reachable peer, making this node dialable via
    /// `.../p2p/<relay_peer>/p2p-circuit/p2p/<self>`. Not part of the port.
    pub async fn reserve_relay(
        &self,
        relay_peer: PeerId,
        relay_address: String,
    ) -> Result<(), MeshTransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.send_command(Command::ReserveRelay {
            relay_peer,
            relay_address,
            reply: reply_tx,
        })?;
        reply_rx
            .await
            .map_err(|_| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))?
    }

    fn send_command(&self, command: Command) -> Result<(), MeshTransportError> {
        self.command_tx
            .send(command)
            .map_err(|_| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))
    }
}

#[async_trait]
impl MeshTransport for Libp2pMeshTransport {
    async fn connect(&self, peer: PeerId, address: String) -> Result<(), MeshTransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.send_command(Command::Connect {
            peer,
            address,
            reply: reply_tx,
        })?;
        reply_rx
            .await
            .map_err(|_| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))?
    }

    async fn send(&self, peer: PeerId, frame: Vec<u8>) -> Result<(), MeshTransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.send_command(Command::Send {
            peer,
            frame,
            reply: reply_tx,
        })?;
        reply_rx
            .await
            .map_err(|_| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))?
    }

    async fn connected_peers(&self) -> Result<Vec<PeerId>, MeshTransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.send_command(Command::ConnectedPeers { reply: reply_tx })?;
        reply_rx
            .await
            .map_err(|_| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))
    }

    async fn recv(&self) -> Result<(PeerId, Vec<u8>), MeshTransportError> {
        self.inbound_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))
    }

    fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    async fn listen_addresses(&self) -> Result<Vec<String>, MeshTransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.send_command(Command::ListenAddrs { reply: reply_tx })?;
        let addrs = reply_rx
            .await
            .map_err(|_| MeshTransportError::Transport("mesh swarm task has stopped".to_string()))?
            .into_iter()
            .map(|addr| addr.to_string())
            .collect();
        Ok(addrs)
    }
}
