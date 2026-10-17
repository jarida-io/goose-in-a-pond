//! The setup channel's cross-language vectors (`tests/fixtures/setup-channel-vectors.json`):
//! the key schedule, the first sealed messages in each direction, and one framed message.
//! goose-on-the-go copies the file byte for byte, so a difference here is a protocol change.
//! Regenerate with `SETUP_CHANNEL_VECTORS_WRITE=1 cargo test -p pond-connect --test setup_channel_vectors`.

use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use pond_connect::setup_channel::framing;
use pond_connect::setup_channel::sealing::{nonce, ChannelCipher};
use pond_core::security::ports::handshake::HandshakeResponse;
use pond_infra::pairing_cpace::{
    channel_key_schedule_for, CHANNEL_PHONE_TO_POND_INFO, CHANNEL_POND_TO_PHONE_INFO,
};
use serde_json::{json, Value};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap(), 16).unwrap())
        .collect()
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/setup-channel-vectors.json")
}

/// The setup run in the CPace vectors, so a phone can chain the two files: its ISK and sid.
fn cpace_setup_run() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pond-infra/tests/fixtures/cpace-vectors.json");
    let vectors: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    vectors["setup"].clone()
}

/// One sealed message as the vectors record it: the plaintext's exact bytes and what they seal to.
fn sealed_entry(
    direction: &str,
    key: &str,
    counter: u64,
    plaintext: &str,
    sealed_b64: &str,
    carried_as: &str,
) -> Value {
    let mut entry = json!({
        "direction": direction,
        "key": key,
        "counter": counter,
        "nonce": hex(&nonce(counter)),
        "plaintext": plaintext,
        "ciphertext": hex(&B64.decode(sealed_b64).unwrap()),
        "sealed": sealed_b64,
        "carried_as": carried_as,
    });
    if carried_as == "envelope" {
        entry["envelope"] = json!(json!({ "sealed": sealed_b64 }).to_string());
    }
    entry
}

fn vectors() -> Value {
    let setup = cpace_setup_run();
    let isk = unhex(setup["outputs"]["ISK"].as_str().unwrap());
    let sid = unhex(setup["outputs"]["sid"].as_str().unwrap());
    let schedule = channel_key_schedule_for(&isk, &sid);
    let keys = &schedule.keys;
    let server_proof = setup["outputs"]["T_server"].as_str().unwrap().to_string();

    // pake_verify's `sealed`: the HandshakeResponse the pond would answer over HTTPS.
    let handshake = HandshakeResponse {
        accepted: true,
        session_token: Some("vector-session-0001".into()),
        refresh_token: Some("vector-refresh-0001".into()),
        expires_at: Some("2026-10-18T09:00:00+00:00".into()),
        hostname: "pond-7f3a".into(),
        server_version: "0.1.0".into(),
        capabilities: vec!["chat".into(), "devices".into(), "settings".into()],
        rejection_reason: None,
        server_proof: Some(server_proof.clone()),
    };
    let p2c_0 = serde_json::to_string(&handshake).unwrap();
    let c2p_0 = r#"{"id":3,"type":"wifi_scan"}"#;
    let p2c_1 = r#"{"id":3,"type":"wifi_scan","ok":true,"networks":[{"ssid":"Home","security":"wpa2","signal":70,"known":false}]}"#;
    let c2p_1 =
        r#"{"id":4,"type":"wifi_join","ssid":"Home","passphrase":"correct horse battery staple"}"#;

    let mut pond = ChannelCipher::pond(keys);
    let mut phone = ChannelCipher::phone(keys);
    let p2c_0_sealed = pond.seal_to_base64(p2c_0.as_bytes()).unwrap();
    let c2p_0_sealed = phone.seal_to_base64(c2p_0.as_bytes()).unwrap();
    let p2c_1_sealed = pond.seal_to_base64(p2c_1.as_bytes()).unwrap();
    let c2p_1_sealed = phone.seal_to_base64(c2p_1.as_bytes()).unwrap();
    // Each side opens the other's, in the order a session sends them.
    let opened = |cipher: &mut ChannelCipher, sealed: &str| cipher.open_base64(sealed).unwrap();
    assert_eq!(
        opened(&mut phone, &p2c_0_sealed).as_slice(),
        p2c_0.as_bytes()
    );
    assert_eq!(
        opened(&mut pond, &c2p_0_sealed).as_slice(),
        c2p_0.as_bytes()
    );
    assert_eq!(
        opened(&mut phone, &p2c_1_sealed).as_slice(),
        p2c_1.as_bytes()
    );
    assert_eq!(
        opened(&mut pond, &c2p_1_sealed).as_slice(),
        c2p_1.as_bytes()
    );

    // Two answers as the pond notifies them. The first frame is exactly ten chunks, so its last
    // byte lands at the end of a chunk and the second message starts the next one.
    let hello = format!(
        r#"{{"id":100,"type":"hello","ok":true,"protocol":1,"pond_id":"7f3a","claimed":false,"pin":{},"capabilities":["wifi_scan","wifi_join","spoken_code"]}}"#,
        json!(setup["inputs"]["spki_pin"])
    );
    let spoken = r#"{"id":101,"type":"spoken_code","ok":true,"spoken":true,"expires_at":"2026-10-17T10:10:00Z"}"#;
    let framed: Vec<Value> = [hello.as_str(), spoken]
        .into_iter()
        .map(|message| {
            let frame = framing::encode(message.as_bytes()).unwrap();
            let chunks: Vec<String> = framing::split(message.as_bytes(), framing::DEFAULT_ATT_MTU)
                .unwrap()
                .iter()
                .map(|chunk| hex(chunk))
                .collect();
            json!({ "message": message, "frame": hex(&frame), "chunks": chunks })
        })
        .collect();
    assert_eq!(
        framing::encode(hello.as_bytes()).unwrap().len()
            % framing::chunk_len(framing::DEFAULT_ATT_MTU),
        0,
        "the first message must end exactly at a chunk boundary"
    );
    let notifications: Vec<Value> = framed
        .iter()
        .flat_map(|message| message["chunks"].as_array().unwrap().clone())
        .collect();

    json!({
        "description": "The BLE setup channel's keys, sealing and framing \
            (docs/architecture/pond-setup-channel.md). ISK and sid are the setup run's in \
            cpace-vectors.json. Byte strings are lowercase hex; `plaintext` is the exact bytes \
            sealed. The four sealed messages are one session's first four, in order. Regenerate \
            with SETUP_CHANNEL_VECTORS_WRITE=1 cargo test -p pond-connect --test setup_channel_vectors.",
        "spec": "docs/architecture/pond-setup-channel.md",
        "protocol": 1,
        "kdf": {
            "hash": "SHA-512",
            "extract": "HKDF-Extract(salt = sid, ikm = ISK)",
            "info_p2c": CHANNEL_POND_TO_PHONE_INFO,
            "info_c2p": CHANNEL_PHONE_TO_POND_INFO,
            "length": 32,
        },
        "inputs": {
            "source": "cpace-vectors.json setup.outputs",
            "ISK": hex(&isk),
            "sid": hex(&sid),
        },
        "outputs": {
            "prk": hex(&schedule.prk),
            "k_p2c": hex(keys.pond_to_phone()),
            "k_c2p": hex(keys.phone_to_pond()),
        },
        "aead": {
            "cipher": "ChaCha20-Poly1305",
            "nonce": "4 zero bytes || 8-byte big-endian counter, one counter per direction from 0",
            "associated_data": "",
            "tag_bytes": 16,
        },
        "sealed": [
            sealed_entry("pond_to_phone", "k_p2c", 0, &p2c_0, &p2c_0_sealed, "pake_verify.sealed"),
            sealed_entry("phone_to_pond", "k_c2p", 0, c2p_0, &c2p_0_sealed, "envelope"),
            sealed_entry("pond_to_phone", "k_p2c", 1, p2c_1, &p2c_1_sealed, "envelope"),
            sealed_entry("phone_to_pond", "k_c2p", 1, c2p_1, &c2p_1_sealed, "envelope"),
        ],
        "wire": {
            "pake_verify_answer": {
                "id": 2,
                "type": "pake_verify",
                "ok": true,
                "accepted": true,
                "server_proof": server_proof,
                "sealed": p2c_0_sealed,
            },
        },
        "framed": {
            "att_mtu": framing::DEFAULT_ATT_MTU,
            "chunk_bytes": framing::chunk_len(framing::DEFAULT_ATT_MTU),
            "rule": "each message starts its own chunk; a chunk never carries bytes of two messages",
            "messages": framed,
            "notifications": notifications,
        },
    })
}

#[test]
fn setup_channel_vectors_match_the_fixture() {
    let path = fixture();
    let expected = vectors();
    if std::env::var_os("SETUP_CHANNEL_VECTORS_WRITE").is_some() {
        let mut text = serde_json::to_string_pretty(&expected).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
    let recorded: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .expect("the vector file is JSON");
    assert_eq!(
        recorded,
        expected,
        "{} no longer matches this implementation; the app checks the same file, so a change \
         here is a protocol change",
        path.display()
    );
}

#[test]
fn the_framed_example_ends_a_message_on_a_chunk_boundary_and_starts_the_next_fresh() {
    let recorded: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let framed = &recorded["framed"];
    let messages = framed["messages"].as_array().unwrap();
    let first_chunks = messages[0]["chunks"].as_array().unwrap();
    assert_eq!(
        unhex(first_chunks.last().unwrap().as_str().unwrap()).len(),
        20,
        "the first message's last chunk is full"
    );

    // A reader fed the notifications in order completes each message on its own last chunk.
    let mut reassembler = framing::Reassembler::new();
    let mut received = Vec::new();
    for chunk in framed["notifications"].as_array().unwrap() {
        let bytes = unhex(chunk.as_str().unwrap());
        assert!(bytes.len() <= 20);
        if let Some(message) = reassembler.push(&bytes).unwrap() {
            received.push(String::from_utf8(message).unwrap());
        }
    }
    let sent: Vec<&str> = messages
        .iter()
        .map(|m| m["message"].as_str().unwrap())
        .collect();
    assert_eq!(received, sent);

    // The same bytes coalesced across the boundary are an overrun the reader refuses.
    let first_end = unhex(first_chunks.last().unwrap().as_str().unwrap());
    let second_start = unhex(messages[1]["chunks"][0].as_str().unwrap());
    let mut reassembler = framing::Reassembler::new();
    for chunk in &first_chunks[..first_chunks.len() - 1] {
        reassembler.push(&unhex(chunk.as_str().unwrap())).unwrap();
    }
    assert_eq!(
        reassembler.push(&[first_end, second_start].concat()),
        Err(framing::FrameError::Overrun)
    );
}

#[test]
fn the_counters_run_as_the_spec_says() {
    let recorded: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let order: Vec<(String, u64)> = recorded["sealed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["direction"].as_str().unwrap().to_string(),
                entry["counter"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("pond_to_phone".to_string(), 0),
            ("phone_to_pond".to_string(), 0),
            ("pond_to_phone".to_string(), 1),
            ("phone_to_pond".to_string(), 1),
        ],
        "pake_verify's sealed is p2c 0, the phone's first request c2p 0, the pond's next p2c 1"
    );
    assert_eq!(
        recorded["wire"]["pake_verify_answer"]["sealed"],
        recorded["sealed"][0]["sealed"]
    );
}
