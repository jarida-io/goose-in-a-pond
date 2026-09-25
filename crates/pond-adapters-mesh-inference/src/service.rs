use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};

use async_trait::async_trait;

use pond_core::mesh::domain::capabilities::PeerCapabilities;
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::token_count::TokenCount;
use pond_core::mesh::ports::credit_ledger::CreditLedger;
use pond_core::mesh::ports::mesh_transport::{MeshTransport, MeshTransportError};
use pond_core::mesh::ports::payment_rail::PaymentRail;
use pond_core::mesh::ports::peer_capability_query::{
    PeerCapabilityQuery, PeerCapabilityQueryError,
};
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_core::mesh::ports::usage_tally::UsageTally;
use pond_core::models::ports::provider::{LlmProvider, StreamToken};
use pond_core::models::services::thought_filter::ThoughtFilter;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_mesh_protocol::wire::{
    CapabilityRequest, CapabilityResponse, ChunkKind, InferenceChunk, InferenceRequest,
    InvoiceRequest, InvoiceResponse, InvoiceResponseKind, MeshFrame, MeshFrameKind,
};

use crate::from_wire_message;

/// Errors from [`MeshInferenceService::request_invoice`].
#[derive(thiserror::Error, Debug)]
pub enum InvoiceRequestError {
    #[error("mesh transport error: {0}")]
    Transport(#[from] MeshTransportError),
    #[error("peer {0} reported an error: {1}")]
    PeerError(PeerId, String),
    #[error("no invoice response from peer {0} before timeout")]
    Timeout(PeerId),
}

/// The *only* consumer of `MeshTransport::recv()` (a second loop would steal frames): serves
/// `*Request`s, and routes `*Response`/`InferenceChunk` frames to the outbound call they answer.
pub struct MeshInferenceService {
    pub(crate) transport: Arc<dyn MeshTransport>,
    pub(crate) peer_directory: Arc<dyn PeerDirectory>,
    pub(crate) credit_ledger: Arc<dyn CreditLedger>,
    pub(crate) usage_tally: Arc<dyn UsageTally>,
    /// Read live per request, never cached, so the lend ceiling can change without a restart.
    pub(crate) settings_repo: Arc<dyn SettingsRepository>,
    /// How long to wait for each reply chunk from a silent peer (per chunk, not overall).
    pub(crate) chunk_timeout: std::time::Duration,
    backing_provider: Arc<dyn LlmProvider>,
    /// Our Lightning wallet for inbound `InvoiceRequest`s; `None` answers them with an error.
    payment_rail: Option<Arc<dyn PaymentRail>>,
    pending: Mutex<HashMap<u64, mpsc::UnboundedSender<InferenceChunk>>>,
    pending_invoices: Mutex<HashMap<u64, mpsc::UnboundedSender<InvoiceResponse>>>,
    pending_capabilities: Mutex<HashMap<u64, mpsc::UnboundedSender<CapabilityResponse>>>,
    next_request_id: AtomicU64,
    /// Lend throttle: tokens lent per peer this window; in-memory, for capping, not accounting.
    lend_window: std::sync::Mutex<HashMap<PeerId, LendWindowState>>,
    /// How long a lend window stays open before resetting.
    lend_window_duration: std::time::Duration,
}

struct LendWindowState {
    started_at: std::time::Instant,
    tokens_lent: u64,
}

/// Unregisters the pending entry however the stream ends, so it can't leak. `Drop` can't
/// `.await`, so cleanup runs on a spawned task.
pub(crate) struct PendingGuard {
    service: Arc<MeshInferenceService>,
    request_id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let service = self.service.clone();
        let request_id = self.request_id;
        tokio::spawn(async move {
            service.unregister_pending(request_id).await;
        });
    }
}

impl MeshInferenceService {
    /// Build the service and spawn its `recv()` loop; lent requests run on `backing_provider`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        transport: Arc<dyn MeshTransport>,
        peer_directory: Arc<dyn PeerDirectory>,
        credit_ledger: Arc<dyn CreditLedger>,
        usage_tally: Arc<dyn UsageTally>,
        settings_repo: Arc<dyn SettingsRepository>,
        backing_provider: Arc<dyn LlmProvider>,
        chunk_timeout: std::time::Duration,
        lend_window_duration: std::time::Duration,
        payment_rail: Option<Arc<dyn PaymentRail>>,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            transport,
            peer_directory,
            credit_ledger,
            usage_tally,
            settings_repo,
            chunk_timeout,
            backing_provider,
            payment_rail,
            pending: Mutex::new(HashMap::new()),
            pending_invoices: Mutex::new(HashMap::new()),
            pending_capabilities: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
            lend_window: std::sync::Mutex::new(HashMap::new()),
            lend_window_duration,
        });
        tokio::spawn(Self::run(service.clone()));
        service
    }

    /// The `LlmProvider` for borrowing: a handle into this service and its `recv()` loop.
    pub fn provider(self: &Arc<Self>) -> crate::MeshInferenceProvider {
        crate::MeshInferenceProvider::new(self.clone())
    }

    /// A fresh outbound request id; replies echo it.
    pub(crate) fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a reply channel for `request_id`. Call before sending, or a fast reply is lost.
    pub(crate) async fn register_pending(
        self: &Arc<Self>,
        request_id: u64,
    ) -> (mpsc::UnboundedReceiver<InferenceChunk>, PendingGuard) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.lock().await.insert(request_id, tx);
        let guard = PendingGuard {
            service: self.clone(),
            request_id,
        };
        (rx, guard)
    }

    pub(crate) async fn unregister_pending(&self, request_id: u64) {
        self.pending.lock().await.remove(&request_id);
    }

    /// May `peer` be served under the lend throttle? Rolls over an expired window.
    fn lend_window_check(&self, peer: PeerId, ceiling: u64) -> bool {
        if ceiling == 0 {
            return true;
        }
        let mut window = self.lend_window.lock().unwrap_or_else(|e| e.into_inner());
        let state = window.entry(peer).or_insert_with(|| LendWindowState {
            started_at: std::time::Instant::now(),
            tokens_lent: 0,
        });
        if state.started_at.elapsed() >= self.lend_window_duration {
            state.started_at = std::time::Instant::now();
            state.tokens_lent = 0;
        }
        state.tokens_lent < ceiling
    }

    /// Adds tokens lent to `peer`'s current window (no-op if never checked).
    fn lend_window_record(&self, peer: PeerId, tokens: u64) {
        let mut window = self.lend_window.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = window.get_mut(&peer) {
            state.tokens_lent = state.tokens_lent.saturating_add(tokens);
        }
    }

    async fn run(self: Arc<Self>) {
        loop {
            match self.transport.recv().await {
                Ok((peer, bytes)) => {
                    let this = self.clone();
                    // Off the recv loop: a slow local model must not block the next frame.
                    tokio::spawn(async move { this.dispatch(peer, bytes).await });
                }
                Err(err) => {
                    tracing::warn!("mesh-inference: recv loop stopped: {err}");
                    return;
                }
            }
        }
    }

    async fn dispatch(&self, peer: PeerId, bytes: Vec<u8>) {
        let frame = match MeshFrame::decode(&bytes[..]) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::warn!("mesh-inference: malformed frame from {peer}: {err}");
                return;
            }
        };
        match frame.kind {
            Some(MeshFrameKind::Request(request)) => self.serve_request(peer, request).await,
            Some(MeshFrameKind::Chunk(chunk)) => {
                let pending = self.pending.lock().await;
                if let Some(tx) = pending.get(&chunk.request_id) {
                    // A dropped receiver means the requester gave up.
                    let _ = tx.send(chunk);
                }
                // An unknown request_id is the expected finished/gave-up race; drop it silently.
            }
            Some(MeshFrameKind::InvoiceRequest(request)) => {
                self.serve_invoice_request(peer, request).await
            }
            Some(MeshFrameKind::InvoiceResponse(response)) => {
                let pending = self.pending_invoices.lock().await;
                if let Some(tx) = pending.get(&response.request_id) {
                    let _ = tx.send(response);
                }
                // Unknown request_id: the same expected race as above.
            }
            Some(MeshFrameKind::CapabilityRequest(request)) => {
                self.serve_capability_request(peer, request).await
            }
            Some(MeshFrameKind::CapabilityResponse(response)) => {
                let pending = self.pending_capabilities.lock().await;
                if let Some(tx) = pending.get(&response.request_id) {
                    let _ = tx.send(response);
                }
                // Same expected race as the other *Response arms above.
            }
            None => {
                tracing::warn!("mesh-inference: frame from {peer} carried no payload");
            }
        }
    }

    /// Server role: what we offer; inference always (there is always a `backing_provider`).
    async fn serve_capability_request(&self, peer: PeerId, request: CapabilityRequest) {
        let frame = MeshFrame::capability_response(CapabilityResponse {
            request_id: request.request_id,
            inference_available: true,
            lightning_available: self.payment_rail.is_some(),
        })
        .encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            tracing::warn!("mesh-inference: failed to send capability response to {peer}: {err}");
        }
    }

    /// Server role: issue an invoice from our own `payment_rail` for a peer that wants to pay.
    async fn serve_invoice_request(&self, peer: PeerId, request: InvoiceRequest) {
        let response_kind = match &self.payment_rail {
            Some(rail) => match rail
                .issue_invoice(Millisats::new(request.amount_millisats))
                .await
            {
                Ok(invoice) => InvoiceResponseKind::Invoice(invoice),
                Err(err) => {
                    tracing::warn!("mesh-inference: failed to issue invoice for {peer}: {err}");
                    InvoiceResponseKind::Error(err.to_string())
                }
            },
            None => InvoiceResponseKind::Error("no payment rail configured".to_string()),
        };
        let frame = MeshFrame::invoice_response(InvoiceResponse {
            request_id: request.request_id,
            kind: Some(response_kind),
        })
        .encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            tracing::warn!("mesh-inference: failed to send invoice response to {peer}: {err}");
        }
    }

    /// Client role: ask `peer` for an invoice for `amount`. Call per settlement attempt, never
    /// cache: a Lightning invoice is not necessarily reusable.
    pub async fn request_invoice(
        &self,
        peer: PeerId,
        amount: Millisats,
    ) -> Result<String, InvoiceRequestError> {
        let request_id = self.next_request_id();
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_invoices.lock().await.insert(request_id, tx);

        let frame = MeshFrame::invoice_request(InvoiceRequest {
            request_id,
            amount_millisats: amount.value(),
        })
        .encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            self.pending_invoices.lock().await.remove(&request_id);
            return Err(InvoiceRequestError::Transport(err));
        }

        let result = match tokio::time::timeout(self.chunk_timeout, rx.recv()).await {
            Ok(Some(response)) => match response.kind {
                Some(InvoiceResponseKind::Invoice(invoice)) => Ok(invoice),
                Some(InvoiceResponseKind::Error(message)) => {
                    Err(InvoiceRequestError::PeerError(peer, message))
                }
                None => Err(InvoiceRequestError::PeerError(
                    peer,
                    "empty invoice response".to_string(),
                )),
            },
            Ok(None) => Err(InvoiceRequestError::Timeout(peer)),
            Err(_elapsed) => Err(InvoiceRequestError::Timeout(peer)),
        };
        self.pending_invoices.lock().await.remove(&request_id);
        result
    }

    /// Local attempts at a completion with no visible text before sending it anyway; cheaper than
    /// the borrower retrying over the mesh (`backing_provider` lacks Goose's empty-turn handling).
    const MAX_EMPTY_COMPLETION_ATTEMPTS: u32 = 2;

    /// Server role: stream `backing_provider`'s reply as chunks ending in one `usage` or `error`.
    async fn serve_request(&self, peer: PeerId, request: InferenceRequest) {
        // Refuse before spending local compute if the lend throttle is exhausted.
        let ceiling = self
            .settings_repo
            .get()
            .await
            .map(|s| s.mesh_lend_token_ceiling)
            .unwrap_or(0);
        if !self.lend_window_check(peer, ceiling) {
            self.send_chunk(
                peer,
                InferenceChunk {
                    request_id: request.request_id,
                    seq: 0,
                    kind: Some(ChunkKind::Error(
                        "lend window exhausted — this pond has reached its lending limit for \
                         this peer for the current window; try again shortly"
                            .to_string(),
                    )),
                },
            )
            .await;
            return;
        }

        let messages: Vec<_> = request.messages.iter().map(from_wire_message).collect();

        // Discarded attempts still cost local compute, so they count against the lend window.
        let mut discarded_tokens_total: u32 = 0;
        let mut sent_usage: Option<pond_mesh_protocol::wire::UsageWire> = None;

        for attempt in 0..Self::MAX_EMPTY_COMPLETION_ATTEMPTS {
            let is_last_attempt = attempt + 1 == Self::MAX_EMPTY_COMPLETION_ATTEMPTS;
            let mut stream = self
                .backing_provider
                .stream_complete(&request.system_prompt, messages.clone());

            // Buffer until the attempt shows visible text (a sent chunk can't be un-sent, and a
            // `<think>` block may precede an answer); then stream the rest.
            let mut pending: Vec<InferenceChunk> = Vec::new();
            let mut filter = ThoughtFilter::new();
            let mut seen_visible = false;
            let mut seq = 0u32;
            // No per-chunk token count, so max_tokens is enforced on a chars/4 estimate.
            let mut estimated_tokens: u32 = 0;
            let mut attempt_usage = None;

            while let Some(item) = stream.next().await {
                match item {
                    Ok(StreamToken::Text(text)) => {
                        let visible = filter.push(&text);
                        estimated_tokens =
                            estimated_tokens.saturating_add((text.chars().count() / 4) as u32);
                        let chunk = InferenceChunk {
                            request_id: request.request_id,
                            seq,
                            kind: Some(ChunkKind::Text(text)),
                        };
                        seq += 1;

                        if seen_visible {
                            self.send_chunk(peer, chunk).await;
                        } else if !visible.trim().is_empty() {
                            seen_visible = true;
                            for buffered in pending.drain(..) {
                                self.send_chunk(peer, buffered).await;
                            }
                            self.send_chunk(peer, chunk).await;
                        } else {
                            pending.push(chunk);
                        }

                        if estimated_tokens >= request.max_tokens {
                            // The borrower's own cap: end normally, with a usage chunk.
                            break;
                        }
                    }
                    Ok(StreamToken::Usage(stats)) => {
                        attempt_usage = Some(pond_mesh_protocol::wire::UsageWire {
                            prompt_tokens: stats.prompt_tokens,
                            completion_tokens: stats.completion_tokens,
                            // Set below once this attempt is the one sent.
                            charged_tokens: 0,
                        });
                    }
                    Err(err) => {
                        // A real error is not the empty-turn case: surface it, don't retry.
                        for buffered in pending.drain(..) {
                            self.send_chunk(peer, buffered).await;
                        }
                        self.send_chunk(
                            peer,
                            InferenceChunk {
                                request_id: request.request_id,
                                seq,
                                kind: Some(ChunkKind::Error(err.to_string())),
                            },
                        )
                        .await;
                        return; // error chunk is terminal — don't also send usage
                    }
                }
            }
            if !seen_visible && !filter.flush().trim().is_empty() {
                seen_visible = true;
                for buffered in pending.drain(..) {
                    self.send_chunk(peer, buffered).await;
                }
            }

            if seen_visible || is_last_attempt {
                // The estimate if the provider reported no usage or max_tokens cut it short.
                let mut usage = attempt_usage.unwrap_or(pond_mesh_protocol::wire::UsageWire {
                    prompt_tokens: 0,
                    completion_tokens: estimated_tokens,
                    charged_tokens: 0,
                });
                // The full bill, discarded attempts included, so the borrower's ledger agrees.
                usage.charged_tokens =
                    discarded_tokens_total.saturating_add(usage.completion_tokens);
                sent_usage = Some(usage);
                self.send_chunk(
                    peer,
                    InferenceChunk {
                        request_id: request.request_id,
                        seq,
                        kind: Some(ChunkKind::Usage(sent_usage.clone().unwrap())),
                    },
                )
                .await;
                break;
            }

            // Discarded: still charge its tokens (reported count, else the chars/4 estimate).
            discarded_tokens_total = discarded_tokens_total.saturating_add(
                attempt_usage
                    .map(|u| u.completion_tokens)
                    .unwrap_or(estimated_tokens),
            );
            tracing::warn!(
                peer = %peer,
                attempt = attempt + 1,
                max = Self::MAX_EMPTY_COMPLETION_ATTEMPTS,
                "mesh: lend-side completion produced no visible text — retrying before replying"
            );
        }

        // Record exactly the `charged_tokens` the borrower was billed, so the ledgers agree.
        let charged_tokens = sent_usage.map(|u| u.charged_tokens).unwrap_or(0);
        let _ = self
            .usage_tally
            .record_lent(peer, TokenCount::new(charged_tokens as u64))
            .await;
        self.lend_window_record(peer, charged_tokens as u64);
    }

    async fn send_chunk(&self, peer: PeerId, chunk: InferenceChunk) {
        let frame = MeshFrame::chunk(chunk).encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            tracing::warn!("mesh-inference: failed to send reply to {peer}: {err}");
        }
    }
}

/// Client role for capability queries; no per-caller state, so no handle type.
#[async_trait]
impl PeerCapabilityQuery for MeshInferenceService {
    async fn capabilities_of(
        &self,
        peer: PeerId,
    ) -> Result<PeerCapabilities, PeerCapabilityQueryError> {
        let request_id = self.next_request_id();
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_capabilities
            .lock()
            .await
            .insert(request_id, tx);

        let frame = MeshFrame::capability_request(CapabilityRequest { request_id }).encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            self.pending_capabilities.lock().await.remove(&request_id);
            return Err(PeerCapabilityQueryError::Transport(err.to_string()));
        }

        let result = match tokio::time::timeout(self.chunk_timeout, rx.recv()).await {
            Ok(Some(response)) => Ok(PeerCapabilities {
                inference_available: response.inference_available,
                lightning_available: response.lightning_available,
            }),
            Ok(None) | Err(_) => Err(PeerCapabilityQueryError::Timeout(peer)),
        };
        self.pending_capabilities.lock().await.remove(&request_id);
        result
    }
}

/// `InvoiceRequester` over the inherent `request_invoice` (called fully qualified: not recursion).
#[async_trait]
impl pond_core::mesh::ports::invoice_requester::InvoiceRequester for MeshInferenceService {
    async fn request_invoice(
        &self,
        peer: PeerId,
        amount: Millisats,
    ) -> Result<String, pond_core::mesh::ports::invoice_requester::InvoiceRequesterError> {
        MeshInferenceService::request_invoice(self, peer, amount)
            .await
            .map_err(Into::into)
    }
}

impl From<InvoiceRequestError>
    for pond_core::mesh::ports::invoice_requester::InvoiceRequesterError
{
    fn from(err: InvoiceRequestError) -> Self {
        match err {
            InvoiceRequestError::Transport(err) => Self::Transport(err.to_string()),
            InvoiceRequestError::PeerError(peer, message) => Self::PeerError(peer, message),
            InvoiceRequestError::Timeout(peer) => Self::Timeout(peer),
        }
    }
}
