//! The pond and the phone both end-to-end test binaries drive.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use pond_connect::setup_channel::framing::{self, Reassembler};
use pond_connect::setup_channel::sealing::ChannelCipher;
use pond_connect::setup_channel::{
    SessionEnd, SessionHandle, SetupChannel, SetupChannelConfig, SetupChannelContext,
};
use pond_core::network::domain::wifi::WifiSecurity;
use pond_core::network::mocks::mock_code_announcer::MockCodeAnnouncer;
use pond_core::network::mocks::mock_wifi_station::{sample_network, MockWifiStation};
use pond_core::network::ports::setup_code_store::SetupCodeStore;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::network::services::spoken_code::SpokenCodeService;
use pond_core::security::mocks::mock_event_log::MockEventLog;
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_infra::db::Database;
use pond_infra::file_setup_code_store::FileSetupCodeStore;
use pond_infra::pairing_cpace::{
    channel_identifier, device_channel_identifier, session_id, setup_channel_identifier, Responder,
};
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use serde_json::{json, Value};

pub const PIN: &str = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";
pub const HOME_PASSPHRASE: &str = "otter-lantern-quietly-91";
pub const MTUS: [u16; 2] = [23, 185];

pub struct Pond {
    pub channel: SetupChannel,
    pub store: Arc<FileSetupCodeStore>,
    pub code: String,
    pub handshake: Arc<SqliteHandshakeAdapter>,
    pub announcer: Arc<MockCodeAnnouncer>,
    pub station: Arc<MockWifiStation>,
    pub events: Arc<MockEventLog>,
    _db: Database,
    _dir: tempfile::TempDir,
}

pub async fn pond() -> Pond {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::init(dir.path()).await.unwrap();
    let store = Arc::new(FileSetupCodeStore::new(dir.path()));
    let record = store.create_if_missing().await.unwrap();
    let claims = Arc::new(SetupClaimService::new(store.clone()));
    let handshake = Arc::new(
        SqliteHandshakeAdapter::new(db.system.clone(), Some(PIN.to_string()))
            .with_setup_claims(claims.clone()),
    );
    let announcer = Arc::new(MockCodeAnnouncer::new());
    let station = Arc::new(
        MockWifiStation::new()
            .with_network(
                sample_network("Home", WifiSecurity::Wpa2, 61),
                Some(HOME_PASSPHRASE),
            )
            .with_network(
                sample_network("Home", WifiSecurity::Wpa2, 88),
                Some(HOME_PASSPHRASE),
            )
            .with_network(sample_network("Cafe", WifiSecurity::Open, 40), None),
    );
    let events = Arc::new(MockEventLog::new());
    let ctx = SetupChannelContext::new(
        SetupChannelConfig {
            discriminator: record.discriminator,
            pin: PIN.into(),
            https_port: 8443,
            hostname: "pond-e2e".into(),
        },
        handshake.clone(),
        Arc::new(SpokenCodeService::new(Some(announcer.clone()))),
        Arc::new(MockSettingsRepository::new()),
        Arc::new(MockProfileRepository::new()),
    )
    .with_claims(claims)
    .with_wifi(station.clone())
    .with_event_log(events.clone());
    Pond {
        channel: SetupChannel::new(ctx),
        store,
        code: record.code.digits().to_string(),
        handshake,
        announcer,
        station,
        events,
        _db: db,
        _dir: dir,
    }
}

/// What a completed exchange leaves the phone holding.
pub struct Paired {
    pub session_token: String,
    pub refresh_token: String,
    pub keys_hex: [String; 2],
}

/// A phone on the far side of the transport: it frames at its MTU, writes chunk by chunk, and
/// reassembles the pond's notifications.
pub struct Phone {
    pub handle: SessionHandle,
    pub att_mtu: u16,
    pub cipher: Option<ChannelCipher>,
    next_id: u64,
    client_id: String,
    /// Every byte the pond notified, for scanning.
    pub heard: Vec<u8>,
}

impl Phone {
    pub async fn connect(pond: &Pond, att_mtu: u16, client_id: &str) -> Self {
        let handle = pond.channel.open().expect("the channel is free");
        handle.writer.set_att_mtu(att_mtu).await.unwrap();
        Self {
            handle,
            att_mtu,
            cipher: None,
            next_id: 1,
            client_id: client_id.into(),
            heard: Vec::new(),
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> bool {
        for chunk in framing::split(message, self.att_mtu).unwrap() {
            if self.handle.writer.write(&chunk).await.is_err() {
                return false;
            }
        }
        true
    }

    /// The next whole message, or `None` once the pond ended the session.
    pub async fn receive(&mut self) -> Option<Vec<u8>> {
        let mut reassembler = Reassembler::new();
        loop {
            let notification =
                tokio::time::timeout(Duration::from_secs(10), self.handle.notifications.next())
                    .await
                    .expect("the pond answers or ends the session")?;
            assert!(notification.len() <= framing::chunk_len(self.att_mtu));
            self.heard.extend_from_slice(&notification);
            if let Some(message) = reassembler.push(&notification).unwrap() {
                return Some(message);
            }
        }
    }

    pub async fn ask(&mut self, kind: &str, fields: Value) -> Value {
        self.try_ask(kind, fields)
            .await
            .unwrap_or_else(|| panic!("the session ended on {kind}"))
    }

    pub async fn try_ask(&mut self, kind: &str, mut fields: Value) -> Option<Value> {
        let id = self.next_id;
        self.next_id += 1;
        fields["id"] = json!(id);
        fields["type"] = json!(kind);
        let bytes = serde_json::to_vec(&fields).unwrap();
        let message = match self.cipher.as_mut() {
            Some(cipher) => cipher.seal_envelope(&bytes).unwrap(),
            None => bytes,
        };
        if !self.send(&message).await {
            return None;
        }
        let answer = self.receive().await?;
        let answer: Value = match self.cipher.as_mut() {
            Some(cipher) => {
                serde_json::from_slice(&cipher.open_envelope(&answer).unwrap()).unwrap()
            }
            None => serde_json::from_slice(&answer).unwrap(),
        };
        assert_eq!(
            (answer["id"].as_u64(), answer["type"].as_str()),
            (Some(id), Some(kind))
        );
        Some(answer)
    }

    /// The whole exchange with `password` under `credential`'s label, binding the pin `hello`
    /// reported. `Err` holds the refusal.
    pub async fn pake(&mut self, credential: &str, password: &str) -> Result<Paired, Value> {
        let hello = self.ask("hello", json!({})).await;
        let pin = hello["pin"].as_str().unwrap().to_string();
        let nonce = [0x5au8; 32];
        let init = self
            .ask(
                "pake_init",
                json!({"credential": credential, "client_id": self.client_id,
                       "client_type": "gotg", "client_version": "1.0.0",
                       "channel_binding": pin, "client_nonce": B64.encode(nonce)}),
            )
            .await;
        if init["ok"] != true {
            return Err(init);
        }
        let ci = match credential {
            "setup" => setup_channel_identifier(&pin, &self.client_id),
            "device" => device_channel_identifier(&pin, &self.client_id),
            _ => channel_identifier(&pin, &self.client_id),
        };
        let challenge = B64.decode(init["challenge"].as_str().unwrap()).unwrap();
        let ya = B64.decode(init["share"].as_str().unwrap()).unwrap();
        let responder = Responder::respond(
            password.as_bytes(),
            &ci,
            &session_id(&challenge, &nonce),
            &ya,
        )
        .unwrap();
        let verify = self
            .ask(
                "pake_verify",
                json!({"challenge_id": init["challenge_id"], "device_name": "Liz's phone",
                       "share": B64.encode(responder.share()),
                       "confirm": hex(&responder.confirmation())}),
            )
            .await;
        if verify["ok"] != true {
            return Err(verify);
        }
        assert_eq!(verify["accepted"], true);
        // The proof first: a phone opens nothing a pond it cannot verify sealed.
        let proof = unhex(verify["server_proof"].as_str().unwrap());
        assert!(responder.verify_server(&proof), "the pond proved the pin");
        let keys = responder.channel_keys();
        let mut cipher = ChannelCipher::phone(&keys);
        let tokens: Value = serde_json::from_slice(
            &cipher
                .open_base64(verify["sealed"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tokens["accepted"], true);
        assert_eq!(tokens["server_proof"], verify["server_proof"]);
        self.cipher = Some(cipher);
        Ok(Paired {
            session_token: tokens["session_token"].as_str().unwrap().into(),
            refresh_token: tokens["refresh_token"].as_str().unwrap().into(),
            keys_hex: [hex(keys.pond_to_phone()), hex(keys.phone_to_pond())],
        })
    }

    pub async fn ended(mut self) -> Option<SessionEnd> {
        while self.handle.notifications.next().await.is_some() {}
        self.handle.notifications.end()
    }

    /// Leaves, and waits until the pond can take the next phone.
    pub async fn leave(self, pond: &Pond) {
        self.handle.writer.phone_disconnected().await;
        self.ended().await;
        wait_until_free(&pond.channel).await;
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap(), 16).unwrap())
        .collect()
}

pub async fn wait_for_announcement(announcer: &MockCodeAnnouncer) -> String {
    for _ in 0..200 {
        if let Some(code) = announcer.announced().pop() {
            return code;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pond never read a code aloud");
}

pub async fn wait_until_free(channel: &SetupChannel) {
    for _ in 0..200 {
        if !channel.is_session_open() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the ended session never freed the channel");
}

/// The card's flow: claim, then move the pond onto the household's Wi-Fi.
pub async fn claim_and_join(pond: &Pond, att_mtu: u16) -> (Phone, Paired) {
    let mut phone = Phone::connect(pond, att_mtu, "install-e2e").await;
    let hello = phone.ask("hello", json!({})).await;
    assert_eq!(hello["ok"], true);
    assert_eq!(hello["protocol"], 1);
    assert_eq!(
        hello["pond_id"],
        pond.store
            .load()
            .await
            .unwrap()
            .unwrap()
            .discriminator
            .to_string()
    );
    assert_eq!(hello["claimed"], false);
    assert_eq!(hello["pin"], PIN);
    assert_eq!(
        hello["capabilities"],
        json!(["wifi_scan", "wifi_join", "spoken_code"])
    );

    let paired = phone
        .pake("setup", &pond.code.clone())
        .await
        .expect("the card claims the pond");
    assert!(pond
        .handshake
        .validate_token(&paired.session_token)
        .await
        .unwrap());
    assert!(pond
        .store
        .load()
        .await
        .unwrap()
        .unwrap()
        .claim_state()
        .is_claimed());

    let scan = phone.ask("wifi_scan", json!({})).await;
    assert_eq!(
        scan["networks"],
        json!([{"ssid": "Home", "security": "wpa2", "signal": 88, "known": false},
               {"ssid": "Cafe", "security": "open", "signal": 40, "known": false}]),
        "one entry per SSID, strongest first"
    );
    let join = phone
        .ask(
            "wifi_join",
            json!({"ssid": "Home", "passphrase": HOME_PASSPHRASE}),
        )
        .await;
    assert_eq!(
        join,
        json!({"id": join["id"], "type": "wifi_join", "ok": true, "address": "192.168.1.42",
               "https_port": 8443, "hostname": "pond-e2e"})
    );
    assert_eq!(
        pond.station.joins().last().unwrap().passphrase.as_deref(),
        Some(HOME_PASSPHRASE)
    );
    let status = phone.ask("status", json!({})).await;
    assert_eq!(status["claimed"], true);
    assert_eq!(
        status["network"],
        json!({"state": "connected", "ssid": "Home", "ipv4": "192.168.1.42"})
    );
    assert_eq!(
        (status["https_port"].as_u64(), status["hostname"].as_str()),
        (Some(8443), Some("pond-e2e"))
    );
    (phone, paired)
}
