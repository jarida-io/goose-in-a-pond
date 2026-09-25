//! Mesh wire messages, `prost`-derived on plain structs so the build needs no `protoc`.

use prost::Message;

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId;

#[derive(Clone, PartialEq, Eq, Message)]
pub struct Handshake {
    #[prost(bytes = "vec", tag = "1")]
    pub peer_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub harness_hash: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    pub model_hash: Vec<u8>,
    /// ed25519 signature (64 bytes) over `peer_id || harness_hash || model_hash`.
    #[prost(bytes = "vec", tag = "4")]
    pub signature: Vec<u8>,
}

impl Handshake {
    pub fn new(peer: PeerId, harness: HarnessHash, model: ModelHash, signature: [u8; 64]) -> Self {
        Self {
            peer_id: peer.as_bytes().to_vec(),
            harness_hash: harness.as_bytes().to_vec(),
            model_hash: model.as_bytes().to_vec(),
            signature: signature.to_vec(),
        }
    }

    /// The bytes to sign or verify: the three identity fields concatenated, no signature.
    pub fn signed_payload(&self) -> Vec<u8> {
        [
            &self.peer_id[..],
            &self.harness_hash[..],
            &self.model_hash[..],
        ]
        .concat()
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, prost::DecodeError> {
        Message::decode(buf)
    }
}

/// One chat message in a mesh-inference request; text only (no images or tool calls).
#[derive(Clone, PartialEq, Eq, Message)]
pub struct ChatMessageWire {
    /// "system" | "user" | "assistant" | "tool". A string, not a prost enum, so an unknown
    /// role from a newer peer is the adapter's call rather than a decode failure.
    #[prost(string, tag = "1")]
    pub role: String,
    #[prost(string, tag = "2")]
    pub content: String,
}

/// The borrower's ask: run a completion against the lender's active model.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct InferenceRequest {
    /// Borrower-chosen; echoed on every `InferenceChunk` to demux in-flight requests.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(string, tag = "2")]
    pub system_prompt: String,
    #[prost(message, repeated, tag = "3")]
    pub messages: Vec<ChatMessageWire>,
    #[prost(uint32, tag = "4")]
    pub max_tokens: u32,
}

/// Token usage; the payload of the terminal `InferenceChunk` on success.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct UsageWire {
    #[prost(uint32, tag = "1")]
    pub prompt_tokens: u32,
    /// Visible output tokens, for context accounting; billing uses `charged_tokens`.
    #[prost(uint32, tag = "2")]
    pub completion_tokens: u32,
    /// Billed tokens: `completion_tokens` plus discarded empty attempts; debit this. `0` (an
    /// older peer) means "use `completion_tokens`", not "nothing owed".
    #[prost(uint32, tag = "3")]
    pub charged_tokens: u32,
}

/// One piece of the lender's streamed reply; `usage` and `error` are terminal, `text` is not.
#[derive(Clone, PartialEq, Eq, ::prost::Oneof)]
pub enum ChunkKind {
    #[prost(string, tag = "3")]
    Text(String),
    #[prost(message, tag = "4")]
    Usage(UsageWire),
    #[prost(string, tag = "5")]
    Error(String),
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct InferenceChunk {
    /// Matches the `InferenceRequest::request_id` this chunk answers.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    /// Monotonic per request from 0, so dropped or reordered chunks are detectable.
    #[prost(uint32, tag = "2")]
    pub seq: u32,
    #[prost(oneof = "ChunkKind", tags = "3, 4, 5")]
    pub kind: Option<ChunkKind>,
}

/// Asks a peer for its invoice; `PaymentRail::batch_settle` needs one only the peer can issue.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct InvoiceRequest {
    /// Echoed back on the matching `InvoiceResponse` for demux.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(uint64, tag = "2")]
    pub amount_millisats: u64,
}

/// A BOLT11 invoice from the peer's `issue_invoice`, or why not (e.g. Lightning is off).
#[derive(Clone, PartialEq, Eq, ::prost::Oneof)]
pub enum InvoiceResponseKind {
    #[prost(string, tag = "2")]
    Invoice(String),
    #[prost(string, tag = "3")]
    Error(String),
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct InvoiceResponse {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(oneof = "InvoiceResponseKind", tags = "2, 3")]
    pub kind: Option<InvoiceResponseKind>,
}

/// "What do you offer right now?"; queried live, as wallets and models come and go.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct CapabilityRequest {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct CapabilityResponse {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(bool, tag = "2")]
    pub inference_available: bool,
    #[prost(bool, tag = "3")]
    pub lightning_available: bool,
}

#[derive(Clone, PartialEq, Eq, ::prost::Oneof)]
pub enum MeshFrameKind {
    #[prost(message, tag = "1")]
    Request(InferenceRequest),
    #[prost(message, tag = "2")]
    Chunk(InferenceChunk),
    #[prost(message, tag = "3")]
    InvoiceRequest(InvoiceRequest),
    #[prost(message, tag = "4")]
    InvoiceResponse(InvoiceResponse),
    #[prost(message, tag = "5")]
    CapabilityRequest(CapabilityRequest),
    #[prost(message, tag = "6")]
    CapabilityResponse(CapabilityResponse),
}

/// The one type passed to `MeshTransport::send`/`recv`: `recv()` has a single consumer, and
/// all families share `request_id` at tag 1, so a bare-message decode could misparse.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct MeshFrame {
    #[prost(oneof = "MeshFrameKind", tags = "1, 2, 3, 4, 5, 6")]
    pub kind: Option<MeshFrameKind>,
}

impl MeshFrame {
    pub fn request(req: InferenceRequest) -> Self {
        Self {
            kind: Some(MeshFrameKind::Request(req)),
        }
    }

    pub fn chunk(chunk: InferenceChunk) -> Self {
        Self {
            kind: Some(MeshFrameKind::Chunk(chunk)),
        }
    }

    pub fn invoice_request(req: InvoiceRequest) -> Self {
        Self {
            kind: Some(MeshFrameKind::InvoiceRequest(req)),
        }
    }

    pub fn invoice_response(resp: InvoiceResponse) -> Self {
        Self {
            kind: Some(MeshFrameKind::InvoiceResponse(resp)),
        }
    }

    pub fn capability_request(req: CapabilityRequest) -> Self {
        Self {
            kind: Some(MeshFrameKind::CapabilityRequest(req)),
        }
    }

    pub fn capability_response(resp: CapabilityResponse) -> Self {
        Self {
            kind: Some(MeshFrameKind::CapabilityResponse(resp)),
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, prost::DecodeError> {
        Message::decode(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::MeshKeypair;

    fn sample_handshake() -> (Handshake, MeshKeypair) {
        let keypair = MeshKeypair::generate();
        let harness = HarnessHash::from([1u8; 32]);
        let model = ModelHash::from([2u8; 32]);
        let unsigned = Handshake::new(keypair.peer_id(), harness, model, [0u8; 64]);
        let signature = keypair.sign(&unsigned.signed_payload());
        (
            Handshake::new(keypair.peer_id(), harness, model, signature),
            keypair,
        )
    }

    #[test]
    fn encode_decode_roundtrips() {
        let (handshake, _keypair) = sample_handshake();
        let bytes = handshake.encode_to_vec();
        let decoded = Handshake::decode(&bytes[..]).unwrap();
        assert_eq!(handshake, decoded);
    }

    #[test]
    fn signature_verifies_against_signed_payload() {
        let (handshake, keypair) = sample_handshake();
        let signature: [u8; 64] = handshake.signature.clone().try_into().unwrap();
        assert!(crate::identity::verify(
            keypair.peer_id(),
            &handshake.signed_payload(),
            &signature
        )
        .unwrap());
    }

    #[test]
    fn decode_of_garbage_bytes_errors_not_panics() {
        let result = Handshake::decode(&[0xff, 0x00, 0x01][..]);
        let _ = result; // either Ok or Err is acceptable; a panic is not.
    }

    fn sample_request() -> InferenceRequest {
        InferenceRequest {
            request_id: 42,
            system_prompt: "You are a helpful assistant.".to_string(),
            messages: vec![ChatMessageWire {
                role: "user".to_string(),
                content: "hello mesh".to_string(),
            }],
            max_tokens: 256,
        }
    }

    #[test]
    fn inference_frame_request_roundtrips() {
        let frame = MeshFrame::request(sample_request());
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        assert!(matches!(decoded.kind, Some(MeshFrameKind::Request(_))));
    }

    #[test]
    fn inference_frame_text_chunk_roundtrips() {
        let frame = MeshFrame::chunk(InferenceChunk {
            request_id: 42,
            seq: 0,
            kind: Some(ChunkKind::Text("hel".to_string())),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn inference_frame_terminal_usage_chunk_roundtrips() {
        let frame = MeshFrame::chunk(InferenceChunk {
            request_id: 42,
            seq: 3,
            kind: Some(ChunkKind::Usage(UsageWire {
                prompt_tokens: 12,
                completion_tokens: 8,
                charged_tokens: 8,
            })),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        match decoded.kind {
            Some(MeshFrameKind::Chunk(InferenceChunk {
                kind: Some(ChunkKind::Usage(usage)),
                ..
            })) => {
                assert_eq!(usage.prompt_tokens, 12);
                assert_eq!(usage.completion_tokens, 8);
            }
            other => panic!("expected a terminal usage chunk, got {other:?}"),
        }
    }

    #[test]
    fn inference_frame_error_chunk_roundtrips() {
        let frame = MeshFrame::chunk(InferenceChunk {
            request_id: 7,
            seq: 0,
            kind: Some(ChunkKind::Error("insufficient credit".to_string())),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn request_and_chunk_are_never_confused_despite_sharing_a_request_id_tag() {
        let request = sample_request();
        let bytes = Message::encode_to_vec(&request);
        let decoded = InferenceChunk::decode(&bytes[..]);
        match decoded {
            Err(_) => {} // ideal outcome
            Ok(chunk) => assert_eq!(chunk.kind, None, "must not fabricate a chunk kind"),
        }
    }

    #[test]
    fn inference_frame_decode_of_garbage_bytes_errors_not_panics() {
        let result = MeshFrame::decode(&[0xff, 0x00, 0x01][..]);
        let _ = result;
    }

    #[test]
    fn invoice_request_frame_roundtrips() {
        let frame = MeshFrame::invoice_request(InvoiceRequest {
            request_id: 1,
            amount_millisats: 5000,
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        assert!(matches!(
            decoded.kind,
            Some(MeshFrameKind::InvoiceRequest(_))
        ));
    }

    #[test]
    fn invoice_response_with_invoice_roundtrips() {
        let frame = MeshFrame::invoice_response(InvoiceResponse {
            request_id: 1,
            kind: Some(InvoiceResponseKind::Invoice("lnbc1...".to_string())),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn invoice_response_with_error_roundtrips() {
        let frame = MeshFrame::invoice_response(InvoiceResponse {
            request_id: 2,
            kind: Some(InvoiceResponseKind::Error(
                "no payment rail configured".to_string(),
            )),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn invoice_request_and_inference_request_are_never_confused() {
        let invoice_req = InvoiceRequest {
            request_id: 9,
            amount_millisats: 1000,
        };
        let bytes = Message::encode_to_vec(&invoice_req);
        let decoded = InferenceRequest::decode(&bytes[..]);
        match decoded {
            Err(_) => {}
            Ok(req) => assert!(req.messages.is_empty(), "must not fabricate messages"),
        }
    }

    #[test]
    fn capability_request_frame_roundtrips() {
        let frame = MeshFrame::capability_request(CapabilityRequest { request_id: 5 });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        assert!(matches!(
            decoded.kind,
            Some(MeshFrameKind::CapabilityRequest(_))
        ));
    }

    #[test]
    fn capability_response_frame_roundtrips() {
        let frame = MeshFrame::capability_response(CapabilityResponse {
            request_id: 5,
            inference_available: true,
            lightning_available: false,
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        match decoded.kind {
            Some(MeshFrameKind::CapabilityResponse(resp)) => {
                assert!(resp.inference_available);
                assert!(!resp.lightning_available);
            }
            other => panic!("expected a capability response, got {other:?}"),
        }
    }
}
