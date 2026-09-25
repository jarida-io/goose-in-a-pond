use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use futures::StreamExt;

use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::token_count::TokenCount;
use pond_core::mesh::ports::credit_ledger::CreditLedgerError;
use pond_core::mesh::ports::mesh_transport::MeshTransportError;
use pond_core::mesh::ports::peer_directory::PeerDirectoryError;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::{LlmProvider, StreamToken, TokenStream, UsageStats};
use pond_mesh_protocol::wire::{ChunkKind, InferenceRequest, MeshFrame};

use crate::service::MeshInferenceService;
use crate::to_wire_message;

/// Max tokens one mesh request may ask a peer to generate; not user-configurable yet.
const DEFAULT_MAX_TOKENS: u32 = 2048;

#[derive(thiserror::Error, Debug)]
pub enum MeshInferenceError {
    #[error("no trusted, connected, funded mesh peer is available")]
    NoPeerAvailable,
    #[error("mesh transport error: {0}")]
    Transport(#[from] MeshTransportError),
    #[error("peer directory error: {0}")]
    PeerDirectory(#[from] PeerDirectoryError),
    #[error("credit ledger error: {0}")]
    CreditLedger(#[from] CreditLedgerError),
    #[error("peer reported an error: {0}")]
    PeerError(String),
    #[error("no reply from peer {0} before timeout")]
    Timeout(PeerId),
}

/// Client role: an `LlmProvider` running on a trusted peer's compute.
pub struct MeshInferenceProvider {
    service: Arc<MeshInferenceService>,
    /// Last serving peer, for the sync `model_name()`; peer selection itself is async.
    last_peer: StdMutex<Option<PeerId>>,
}

impl MeshInferenceProvider {
    pub(crate) fn new(service: Arc<MeshInferenceService>) -> Self {
        Self {
            service,
            last_peer: StdMutex::new(None),
        }
    }

    /// First trusted, connected peer with credit; no model matching, peers don't advertise any.
    async fn select_peer(&self) -> Result<PeerId, MeshInferenceError> {
        let trusted = self.service.peer_directory.list_trusted_peers(None).await?;
        let connected: HashSet<PeerId> = self
            .service
            .transport
            .connected_peers()
            .await?
            .into_iter()
            .collect();
        for peer in trusted {
            if !connected.contains(&peer) {
                continue;
            }
            if self.service.credit_ledger.balance(peer).await?.value() > 0 {
                return Ok(peer);
            }
        }
        Err(MeshInferenceError::NoPeerAvailable)
    }
}

#[async_trait]
impl LlmProvider for MeshInferenceProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        let mut content = String::new();
        let mut stream = self.stream_complete(system_prompt, messages);
        while let Some(item) = stream.next().await {
            if let StreamToken::Text(text) = item? {
                content.push_str(&text);
            }
        }
        Ok(ChatMessage::assistant(content))
    }

    fn model_name(&self) -> String {
        match *self.last_peer.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(peer) => format!("mesh:{peer}"),
            // No request has gone out yet.
            None => "mesh".to_string(),
        }
    }

    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> TokenStream<'a> {
        Box::pin(async_stream::stream! {
            let peer = match self.select_peer().await {
                Ok(peer) => peer,
                Err(err) => {
                    yield Err(anyhow::Error::from(err));
                    return;
                }
            };
            *self.last_peer.lock().unwrap_or_else(|e| e.into_inner()) = Some(peer);

            let request_id = self.service.next_request_id();
            // Keeps `_pending_guard` alive so its Drop cleans up on any exit.
            let (mut replies, _pending_guard) = self.service.register_pending(request_id).await;

            let request = InferenceRequest {
                request_id,
                system_prompt: system_prompt.to_string(),
                messages: messages.iter().map(to_wire_message).collect(),
                max_tokens: DEFAULT_MAX_TOKENS,
            };
            let frame = MeshFrame::request(request).encode_to_vec();
            let sent_at = std::time::Instant::now();
            if let Err(err) = self.service.transport.send(peer, frame).await {
                yield Err(anyhow::Error::from(MeshInferenceError::Transport(err)));
                return;
            }

            // First-token latency, logged standalone: the wire has no `time_to_first_token_ms`
            // to feed Goose's TurnStats.
            let mut first_token_logged = false;

            loop {
                match tokio::time::timeout(self.service.chunk_timeout, replies.recv()).await {
                    Ok(Some(chunk)) => match chunk.kind {
                        Some(ChunkKind::Text(text)) => {
                            if !first_token_logged {
                                first_token_logged = true;
                                tracing::info!(
                                    peer = %peer,
                                    ttft_ms = sent_at.elapsed().as_millis() as u64,
                                    "mesh: borrow first-token latency"
                                );
                            }
                            yield Ok(StreamToken::Text(text));
                        }
                        Some(ChunkKind::Usage(usage)) => {
                            // `charged_tokens` includes the lender's discarded retries; `0` means
                            // an older peer, so fall back to `completion_tokens`.
                            let billed_tokens = if usage.charged_tokens > 0 {
                                usage.charged_tokens
                            } else {
                                usage.completion_tokens
                            };
                            // Borrow side: we owe `peer` — record_borrowed, not record_lent.
                            let _ = self
                                .service
                                .usage_tally
                                .record_borrowed(peer, TokenCount::new(billed_tokens as u64))
                                .await;
                            // At the fixed network-wide rate: a local setting would let a
                            // borrower pay less.
                            let spent = billed_tokens as u64
                                * pond_core::mesh::domain::settlement::MESH_SETTLEMENT_MILLISATS_PER_TOKEN;
                            if let Err(err) = self
                                .service
                                .credit_ledger
                                .debit(peer, Millisats::new(spent))
                                .await
                            {
                                // Log only — don't fail a response already streamed in full.
                                tracing::warn!(
                                    "mesh: failed to debit {peer} {spent} msat: {err}"
                                );
                            }
                            yield Ok(StreamToken::Usage(UsageStats {
                                prompt_tokens: usage.prompt_tokens,
                                completion_tokens: usage.completion_tokens,
                                // Not on the wire: unmeasured, not zero.
                                reasoning_tokens: None,
                            }));
                            break;
                        }
                        Some(ChunkKind::Error(message)) => {
                            yield Err(anyhow::Error::from(MeshInferenceError::PeerError(message)));
                            break;
                        }
                        None => {} // empty chunk payload — skip, wait for the next one
                    },
                    // Closed without a terminal chunk: the service's recv loop died.
                    Ok(None) => {
                        yield Err(anyhow::Error::from(MeshInferenceError::Timeout(peer)));
                        break;
                    }
                    Err(_elapsed) => {
                        yield Err(anyhow::Error::from(MeshInferenceError::Timeout(peer)));
                        break;
                    }
                }
            }
        })
    }
}
