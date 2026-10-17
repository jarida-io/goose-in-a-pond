//! One phone's connection to the setup channel (`pond-setup-channel.md`): a state machine from rx
//! writes to tx notifications, and the task that runs it. A transport only moves bytes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pond_core::network::domain::credential::Credential;
use pond_core::network::domain::discriminator::Discriminator;
use pond_core::network::domain::wifi::{is_valid_ssid, rank_networks, NetworkStatus};
use pond_core::network::ports::wifi_station::WifiStation;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::network::services::spoken_code::{SpokenCodeDeps, SpokenCodeService, SpokenCodeVia};
use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::handshake::{
    ChannelVerify, Handshake, PakeInitRequest, PakeRefusal, PakeVerifyRequest,
};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use zeroize::{Zeroize, Zeroizing};

use super::framing::{self, FrameError, Reassembler, DEFAULT_ATT_MTU, MAX_MESSAGE_LEN};
use super::messages::{self, codes, ok, refused, Phase, RequestType, WifiJoinRequest};
use super::sealing::{self, ChannelCipher};
use crate::capabilities::SetupCapabilities;

/// A session ends after this long with no request; every request starts it again.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// However busy it keeps, a session ends this long after it opened, so a phone pinging inside the
/// idle timeout cannot hold the only session forever.
pub const MAX_SESSION_LIFETIME: Duration = Duration::from_secs(600);
/// Exchanges one connection may start; the guess limits proper are the handshake's, shared.
pub const MAX_PAKE_INITS: u8 = 3;
/// A dense block of flats can show more networks than one 16 KiB answer holds once sealed.
pub const MAX_SCAN_NETWORKS: usize = 50;

/// Writes the transport may queue while the session answers a slow request (a Wi-Fi join).
const INBOUND_QUEUE: usize = 64;
/// Notifications queued for the transport: one 16 KiB answer at the smallest MTU is ~820.
const OUTBOUND_QUEUE: usize = 1024;

/// The audit trail's name for this transport.
const TRANSPORT: &str = "setup_channel";

/// What the pond says about itself on the channel.
#[derive(Debug, Clone)]
pub struct SetupChannelConfig {
    /// The card's id, in `hello` and in the advertised name.
    pub discriminator: Discriminator,
    /// This pond's TLS key pin, `sha256/<base64>`. The phone binds it into the exchange, so a
    /// completed exchange proves it.
    pub pin: String,
    pub https_port: u16,
    pub hostname: String,
}

/// Everything a session reads, as pond-core ports, so one pond's sessions share its limits.
pub struct SetupChannelContext {
    config: SetupChannelConfig,
    handshake: Arc<dyn Handshake>,
    spoken_codes: Arc<SpokenCodeService>,
    settings: Arc<dyn SettingsRepository>,
    profiles: Arc<dyn ProfileRepository>,
    claims: Option<Arc<SetupClaimService>>,
    onboarding: Option<Arc<dyn OnboardingRepository>>,
    wifi: Option<Arc<dyn WifiStation>>,
    event_log: Option<Arc<dyn EventLog>>,
}

impl SetupChannelContext {
    /// `spoken_codes` must be the instance the HTTPS route uses, or the two double the rate.
    pub fn new(
        config: SetupChannelConfig,
        handshake: Arc<dyn Handshake>,
        spoken_codes: Arc<SpokenCodeService>,
        settings: Arc<dyn SettingsRepository>,
        profiles: Arc<dyn ProfileRepository>,
    ) -> Self {
        Self {
            config,
            handshake,
            spoken_codes,
            settings,
            profiles,
            claims: None,
            onboarding: None,
            wifi: None,
            event_log: None,
        }
    }

    /// Without it the pond reads as unclaimed, as one with no setup record does.
    pub fn with_claims(mut self, claims: Arc<SetupClaimService>) -> Self {
        self.claims = Some(claims);
        self
    }

    pub fn with_onboarding(mut self, onboarding: Arc<dyn OnboardingRepository>) -> Self {
        self.onboarding = Some(onboarding);
        self
    }

    /// Without it `wifi_scan` and `wifi_join` answer `unsupported`, and `hello` offers neither.
    pub fn with_wifi(mut self, wifi: Arc<dyn WifiStation>) -> Self {
        self.wifi = Some(wifi);
        self
    }

    pub fn with_event_log(mut self, event_log: Arc<dyn EventLog>) -> Self {
        self.event_log = Some(event_log);
        self
    }

    pub fn config(&self) -> &SetupChannelConfig {
        &self.config
    }

    /// A failed read counts as claimed, which narrows what a phone is offered.
    async fn claimed(&self) -> bool {
        let Some(claims) = &self.claims else {
            return false;
        };
        match claims.claim_state().await {
            Ok(state) => state.is_claimed(),
            Err(error) => {
                tracing::warn!(%error, "claim state unreadable; the setup channel reports claimed");
                true
            }
        }
    }
}

/// Why a session ended. Only the phone leaving or going quiet ends a healthy one: nothing about
/// advertising does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    PhoneDisconnected,
    Idle,
    UnreadableFrame,
    OversizeFrame,
    /// A new message began before the last one finished.
    Overrun,
    SealingFailure,
    AttemptsExhausted,
    /// Open for [`MAX_SESSION_LIFETIME`], however active.
    LifetimeCap,
}

impl SessionEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PhoneDisconnected => "phone_disconnected",
            Self::Idle => "idle",
            Self::UnreadableFrame => "unreadable_frame",
            Self::OversizeFrame => "oversize_frame",
            Self::Overrun => "overrun",
            Self::SealingFailure => "sealing_failure",
            Self::AttemptsExhausted => "attempts_exhausted",
            Self::LifetimeCap => "lifetime_cap",
        }
    }
}

/// What one write produced: the notifications to send, in order, and whether the session ended.
#[derive(Debug, Default)]
pub struct Reply {
    pub notifications: Vec<Vec<u8>>,
    pub end: Option<SessionEnd>,
}

enum State {
    /// Before the exchange. Remembers the runs this connection started, so a verify can name only
    /// its own.
    Clear {
        runs: Vec<(String, Credential)>,
    },
    Sealed {
        cipher: ChannelCipher,
    },
}

/// An answer before framing: in the clear, or already a sealed envelope.
struct Answer {
    message: Zeroizing<Vec<u8>>,
    end_after: Option<SessionEnd>,
}

/// One connection. Feed it every rx write; send everything it returns as tx notifications.
pub struct SetupSession {
    ctx: Arc<SetupChannelContext>,
    reassembler: Reassembler,
    att_mtu: u16,
    state: State,
    pake_inits: u8,
    opened_at: Instant,
    last_request: Instant,
    ended: Option<SessionEnd>,
}

impl SetupSession {
    pub fn new(ctx: Arc<SetupChannelContext>, now: Instant) -> Self {
        Self {
            ctx,
            reassembler: Reassembler::new(),
            att_mtu: DEFAULT_ATT_MTU,
            state: State::Clear { runs: Vec::new() },
            pake_inits: 0,
            opened_at: now,
            last_request: now,
            ended: None,
        }
    }

    /// The link's ATT_MTU once negotiated; notifications are split to `att_mtu - 3`.
    pub fn set_att_mtu(&mut self, att_mtu: u16) {
        self.att_mtu = att_mtu;
    }

    pub fn is_sealed(&self) -> bool {
        matches!(self.state, State::Sealed { .. })
    }

    pub fn end(&self) -> Option<SessionEnd> {
        self.ended
    }

    pub fn idle_deadline(&self) -> Instant {
        self.last_request + IDLE_TIMEOUT
    }

    pub fn lifetime_deadline(&self) -> Instant {
        self.opened_at + MAX_SESSION_LIFETIME
    }

    /// The sooner of the idle and lifetime deadlines: when [`Self::expire_if_due`] next matters.
    pub fn deadline(&self) -> Instant {
        self.idle_deadline().min(self.lifetime_deadline())
    }

    /// Ends the session at [`MAX_SESSION_LIFETIME`], or if no request arrived for
    /// [`IDLE_TIMEOUT`].
    pub fn expire_if_due(&mut self, now: Instant) -> Option<SessionEnd> {
        if self.ended.is_none() {
            if now >= self.lifetime_deadline() {
                self.finish(SessionEnd::LifetimeCap);
            } else if now >= self.idle_deadline() {
                self.finish(SessionEnd::Idle);
            }
        }
        self.ended
    }

    /// The transport saw the phone go.
    pub fn phone_disconnected(&mut self) {
        if self.ended.is_none() {
            self.finish(SessionEnd::PhoneDisconnected);
        }
    }

    /// One rx write, split anywhere. A write that completes a request is answered.
    pub async fn receive(&mut self, write: &[u8], now: Instant) -> Reply {
        // A write that arrives after a deadline must not restart the idle timer.
        self.expire_if_due(now);
        if let Some(end) = self.ended {
            return Reply {
                notifications: Vec::new(),
                end: Some(end),
            };
        }
        let message = match self.reassembler.push(write) {
            Ok(None) => return Reply::default(),
            Ok(Some(message)) => message,
            Err(FrameError::TooLong { .. }) => return self.ended_reply(SessionEnd::OversizeFrame),
            Err(FrameError::Overrun) => return self.ended_reply(SessionEnd::Overrun),
        };
        self.last_request = now;
        let answer = if self.is_sealed() {
            self.handle_sealed(&message).await
        } else {
            self.handle_clear(&message, now).await
        };
        match answer {
            Ok(answer) => {
                let notifications = self.notifications(&answer.message);
                if let Some(end) = answer.end_after {
                    self.finish(end);
                }
                Reply {
                    notifications,
                    end: self.ended,
                }
            }
            Err(end) => self.ended_reply(end),
        }
    }

    fn ended_reply(&mut self, end: SessionEnd) -> Reply {
        self.finish(end);
        Reply {
            notifications: Vec::new(),
            end: Some(end),
        }
    }

    fn finish(&mut self, end: SessionEnd) {
        self.ended = Some(end);
        // Drops the cipher, and with it the keys.
        self.state = State::Clear { runs: Vec::new() };
        tracing::info!(
            kind = "setup_channel_ended",
            reason = end.as_str(),
            "setup channel session ended"
        );
    }

    fn notifications(&self, message: &[u8]) -> Vec<Vec<u8>> {
        match framing::split(message, self.att_mtu) {
            Ok(chunks) => chunks,
            Err(error) => {
                tracing::error!(%error, "a setup channel answer was too long to send");
                Vec::new()
            }
        }
    }

    async fn handle_clear(&mut self, message: &[u8], now: Instant) -> Result<Answer, SessionEnd> {
        // An envelope before any keys exist is not a request this side can read.
        if sealing::is_envelope(message) {
            return Err(SessionEnd::UnreadableFrame);
        }
        let header = messages::parse_header(message).ok_or(SessionEnd::UnreadableFrame)?;
        let id = header.id;
        let mut end_after = None;
        let response = match RequestType::parse(&header.kind) {
            None => refused(id, &header.kind, codes::UNKNOWN_TYPE, None),
            Some(kind) if kind.phase() == Phase::Sealed => {
                refused(id, kind.as_str(), codes::WRONG_PHASE, None)
            }
            Some(RequestType::Hello) => self.hello(id).await,
            Some(RequestType::SpokenCode) => self.spoken_code(id, now).await,
            Some(RequestType::PakeInit) => {
                if self.pake_inits >= MAX_PAKE_INITS {
                    end_after = Some(SessionEnd::AttemptsExhausted);
                    refused(id, "pake_init", codes::TOO_MANY_ATTEMPTS, None)
                } else {
                    self.pake_inits += 1;
                    self.pake_init(id, message).await
                }
            }
            Some(RequestType::PakeVerify) => self.pake_verify(id, message).await,
            Some(_) => refused(id, &header.kind, codes::WRONG_PHASE, None),
        };
        log_answer(&response);
        let message = serde_json::to_vec(&response).map_err(|_| SessionEnd::UnreadableFrame)?;
        Ok(Answer {
            message: Zeroizing::new(message),
            end_after,
        })
    }

    async fn handle_sealed(&mut self, message: &[u8]) -> Result<Answer, SessionEnd> {
        let State::Sealed { cipher } = &mut self.state else {
            return Err(SessionEnd::SealingFailure);
        };
        // A clear message, a tampered one and a replayed one all end here.
        let plaintext = cipher
            .open_envelope(message)
            .map_err(|_| SessionEnd::SealingFailure)?;
        let header = messages::parse_header(&plaintext).ok_or(SessionEnd::UnreadableFrame)?;
        let id = header.id;
        let response = match RequestType::parse(&header.kind) {
            None => refused(id, &header.kind, codes::UNKNOWN_TYPE, None),
            Some(kind) if kind.phase() == Phase::Clear => {
                refused(id, kind.as_str(), codes::WRONG_PHASE, None)
            }
            Some(RequestType::Status) => self.status(id).await,
            Some(RequestType::WifiScan) => self.wifi_scan(id).await,
            Some(RequestType::WifiJoin) => self.wifi_join(id, &plaintext).await,
            Some(_) => refused(id, &header.kind, codes::WRONG_PHASE, None),
        };
        log_answer(&response);
        let plain =
            Zeroizing::new(serde_json::to_vec(&response).map_err(|_| SessionEnd::UnreadableFrame)?);
        let State::Sealed { cipher } = &mut self.state else {
            return Err(SessionEnd::SealingFailure);
        };
        let envelope = cipher
            .seal_envelope(&plain)
            .map_err(|_| SessionEnd::SealingFailure)?;
        Ok(Answer {
            message: Zeroizing::new(envelope),
            end_after: None,
        })
    }

    async fn hello(&self, id: u64) -> Value {
        let capabilities =
            SetupCapabilities::probe(self.ctx.wifi.as_deref(), &self.ctx.spoken_codes).await;
        ok(
            id,
            RequestType::Hello,
            json!({
                "protocol": messages::PROTOCOL_VERSION,
                "pond_id": self.ctx.config.discriminator,
                "claimed": self.ctx.claimed().await,
                "pin": self.ctx.config.pin,
                "capabilities": capabilities.offered(),
            }),
        )
    }

    async fn spoken_code(&self, id: u64, now: Instant) -> Value {
        let ctx = &self.ctx;
        let deps = SpokenCodeDeps {
            settings: ctx.settings.as_ref(),
            profiles: ctx.profiles.as_ref(),
            handshake: ctx.handshake.as_ref(),
            event_log: ctx.event_log.as_deref(),
        };
        match ctx
            .spoken_codes
            .request(deps, SpokenCodeVia::SetupChannel, now.into_std())
            .await
        {
            Ok(Ok(issued)) => ok(
                id,
                RequestType::SpokenCode,
                json!({"spoken": true, "expires_at": issued.expires_at}),
            ),
            Ok(Err(refusal)) => refused(
                id,
                RequestType::SpokenCode.as_str(),
                refusal.code(),
                refusal.retry_after().map(|wait| wait.as_secs_ceil()),
            ),
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "setup channel spoken code failed");
                refused(id, "spoken_code", codes::INTERNAL_ERROR, None)
            }
        }
    }

    async fn pake_init(&mut self, id: u64, message: &[u8]) -> Value {
        const KIND: &str = "pake_init";
        let Ok(body) = serde_json::from_slice::<Value>(message) else {
            return refused(id, KIND, codes::INVALID_REQUEST, None);
        };
        // Read first, as HTTPS does, so an unknown secret is named rather than a bad body.
        if let Some(credential) = body.get("credential") {
            if Credential::deserialize(credential).is_err() {
                return refused(id, KIND, codes::INVALID_CREDENTIAL, None);
            }
        }
        let Ok(request) = serde_json::from_value::<PakeInitRequest>(body) else {
            return refused(id, KIND, codes::INVALID_REQUEST, None);
        };
        let credential = request.credential;
        match self.ctx.handshake.init_channel_pake(request).await {
            Ok(Ok(challenge)) => {
                if let State::Clear { runs } = &mut self.state {
                    runs.push((challenge.challenge_id.clone(), credential));
                }
                ok(
                    id,
                    RequestType::PakeInit,
                    serde_json::to_value(&challenge).unwrap_or_default(),
                )
            }
            Ok(Err(refusal)) => {
                if refusal == PakeRefusal::ChannelBindingMismatch {
                    // A phone at the wrong pond, or something relaying one: record it.
                    self.audit_pairing(false, credential, None, Some(refusal.code()))
                        .await;
                }
                refused(
                    id,
                    KIND,
                    refusal.code(),
                    refusal.retry_after().map(|wait| wait.as_secs_ceil()),
                )
            }
            Err(error) => {
                tracing::warn!(%error, "setup channel pake_init failed");
                refused(id, KIND, codes::INTERNAL_ERROR, None)
            }
        }
    }

    async fn pake_verify(&mut self, id: u64, message: &[u8]) -> Value {
        let Ok(request) = serde_json::from_slice::<PakeVerifyRequest>(message) else {
            return refused(id, "pake_verify", codes::INVALID_REQUEST, None);
        };
        let State::Clear { runs } = &mut self.state else {
            return refused(id, "pake_verify", codes::WRONG_PHASE, None);
        };
        let Some(position) = runs
            .iter()
            .position(|(run, _)| *run == request.challenge_id)
        else {
            // Audited as HTTPS audits it, though nothing was spent.
            let device_name = request.device_name.as_deref();
            let unknown = Credential::Pairing;
            self.audit_pairing(false, unknown, device_name, Some(codes::UNKNOWN_CHALLENGE))
                .await;
            return verify_refused(id, codes::UNKNOWN_CHALLENGE);
        };
        let (_, credential) = runs.swap_remove(position);
        let device_name = request.device_name.clone();

        let ChannelVerify { mut response, keys } =
            match self.ctx.handshake.verify_channel_pake(request).await {
                Ok(verified) => verified,
                Err(error) => {
                    tracing::warn!(%error, "setup channel pake_verify failed");
                    self.audit_pairing(
                        false,
                        credential,
                        device_name.as_deref(),
                        Some("internal_error"),
                    )
                    .await;
                    return verify_refused(id, codes::INTERNAL_ERROR);
                }
            };
        if !response.accepted {
            let reason = response
                .rejection_reason
                .clone()
                .unwrap_or_else(|| "rejected".into());
            self.audit_pairing(false, credential, device_name.as_deref(), Some(&reason))
                .await;
            return verify_refused(id, &reason);
        }
        let Some(keys) = keys else {
            tracing::error!("an accepted setup channel run came back without its keys");
            return verify_refused(id, codes::INTERNAL_ERROR);
        };

        let mut cipher = ChannelCipher::pond(&keys);
        drop(keys);
        let sealed = serde_json::to_vec(&response)
            .map(Zeroizing::new)
            .map_err(|_| ())
            .and_then(|plain| cipher.seal_to_base64(&plain).map_err(|_| ()));
        response.session_token.zeroize();
        response.refresh_token.zeroize();
        let Ok(sealed) = sealed else {
            return verify_refused(id, codes::INTERNAL_ERROR);
        };
        self.state = State::Sealed { cipher };
        self.audit_pairing(true, credential, device_name.as_deref(), None)
            .await;
        ok(
            id,
            RequestType::PakeVerify,
            json!({
                "accepted": true,
                "server_proof": response.server_proof,
                "sealed": sealed,
            }),
        )
    }

    async fn status(&self, id: u64) -> Value {
        let network = match &self.ctx.wifi {
            Some(station) => station.status().await,
            None => NetworkStatus::disconnected(),
        };
        let onboarded = match &self.ctx.onboarding {
            Some(onboarding) => onboarding.is_complete().await.unwrap_or_else(|error| {
                tracing::warn!(%error, "onboarding state unreadable; reporting not onboarded");
                false
            }),
            None => false,
        };
        ok(
            id,
            RequestType::Status,
            json!({
                "claimed": self.ctx.claimed().await,
                "onboarded": onboarded,
                "network": network,
                "https_port": self.ctx.config.https_port,
                "hostname": self.ctx.config.hostname,
            }),
        )
    }

    async fn wifi_scan(&self, id: u64) -> Value {
        let Some(station) = &self.ctx.wifi else {
            return refused(id, "wifi_scan", codes::UNSUPPORTED, None);
        };
        let mut networks = match station.scan().await {
            Ok(networks) => rank_networks(networks),
            Err(error) => return refused(id, "wifi_scan", error.code(), None),
        };
        networks.truncate(MAX_SCAN_NETWORKS);
        loop {
            let answer = ok(id, RequestType::WifiScan, json!({ "networks": networks }));
            let fits = serde_json::to_vec(&answer)
                .map(|bytes| sealing::envelope_len(bytes.len()) <= MAX_MESSAGE_LEN)
                .unwrap_or(false);
            if fits || networks.is_empty() {
                return answer;
            }
            networks.truncate(networks.len() / 2);
        }
    }

    async fn wifi_join(&self, id: u64, plaintext: &[u8]) -> Value {
        const KIND: &str = "wifi_join";
        let request = match serde_json::from_slice::<WifiJoinRequest>(plaintext) {
            Ok(request) if is_valid_ssid(&request.ssid) => request,
            _ => return refused(id, KIND, codes::INVALID_REQUEST, None),
        };
        let Some(station) = &self.ctx.wifi else {
            return refused(id, KIND, codes::UNSUPPORTED, None);
        };
        match station
            .join(&request.ssid, request.passphrase.as_ref(), request.hidden)
            .await
        {
            Ok(joined) => ok(
                id,
                RequestType::WifiJoin,
                json!({
                    "address": joined.ipv4.to_string(),
                    "https_port": self.ctx.config.https_port,
                    "hostname": self.ctx.config.hostname,
                }),
            ),
            Err(error) => refused(id, KIND, error.code(), None),
        }
    }

    /// The same events as an HTTPS pairing, so the audit trail reads one way for both; a device
    /// resuming its own session is recorded as that, never as a new pairing.
    async fn audit_pairing(
        &self,
        paired: bool,
        credential: Credential,
        device_name: Option<&str>,
        reason: Option<&str>,
    ) {
        let device = device_name.unwrap_or("unnamed");
        let method = method(credential);
        let resume = credential == Credential::Device;
        let reason_text = reason.unwrap_or("unspecified");
        match (paired, resume) {
            (true, false) => {
                tracing::info!(device, method, transport = TRANSPORT, "pairing accepted")
            }
            (true, true) => tracing::info!(device, method, transport = TRANSPORT, "device resumed"),
            (false, false) => tracing::warn!(
                device,
                method,
                transport = TRANSPORT,
                reason = reason_text,
                "pairing rejected"
            ),
            (false, true) => tracing::warn!(
                device,
                method,
                transport = TRANSPORT,
                reason = reason_text,
                "device resume rejected"
            ),
        }
        let Some(event_log) = self.ctx.event_log.as_ref() else {
            return;
        };
        let action = match (paired, resume) {
            (true, false) => "auth.device_paired",
            (true, true) => "auth.device_resumed",
            (false, false) => "auth.pairing_verify_failed",
            (false, true) => "auth.device_resume_failed",
        };
        let mut event = Event::new(EventCategory::Auth, action)
            .sensitivity(PrivacySensitivity::Sensitive)
            .attr("method", method)
            .attr("transport", TRANSPORT);
        if let Some(name) = device_name {
            event = event.attr("device_name", name);
        }
        if let Some(reason) = reason.filter(|_| !paired) {
            event = event.attr("rejection_reason", reason);
        }
        if let Err(error) = event_log.append(event).await {
            tracing::warn!(%error, action, "failed to record a setup channel pairing event");
        }
    }
}

fn method(credential: Credential) -> &'static str {
    match credential {
        Credential::Setup => "cpace-setup",
        Credential::Pairing => "cpace",
        Credential::Device => "cpace-device",
    }
}

/// A refused verify reads as HTTPS's: `accepted: false` and the reason, in `error` too.
fn verify_refused(id: u64, reason: &str) -> Value {
    let mut answer = refused(id, "pake_verify", reason, None);
    answer["accepted"] = json!(false);
    answer["rejection_reason"] = json!(reason);
    answer
}

/// Types and outcomes only: never a field's value.
fn log_answer(answer: &Value) {
    let request = answer["type"]
        .as_str()
        .and_then(RequestType::parse)
        .map_or("unknown", RequestType::as_str);
    let ok = answer["ok"].as_bool().unwrap_or(false);
    let error = if ok {
        ""
    } else {
        answer["error"].as_str().unwrap_or("")
    };
    tracing::info!(
        kind = "setup_channel_request",
        request,
        ok,
        error,
        "answered a setup channel request"
    );
}

/// The pond's setup channel. It hands out one session at a time, and a session it hands out
/// belongs to its connection: whether the pond is advertising never reaches it.
#[derive(Clone)]
pub struct SetupChannel {
    ctx: Arc<SetupChannelContext>,
    slot: Arc<Semaphore>,
    accepting: Arc<AtomicBool>,
}

/// The channel cannot take this phone now: another one holds the session, or the household
/// switched BLE setup off. Either way the transport refuses the central.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the setup channel is not taking a phone now")]
pub struct ChannelBusy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the setup session has ended")]
pub struct SessionEnded;

impl SetupChannel {
    pub fn new(ctx: SetupChannelContext) -> Self {
        Self {
            ctx: Arc::new(ctx),
            slot: Arc::new(Semaphore::new(1)),
            accepting: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn config(&self) -> &SetupChannelConfig {
        &self.ctx.config
    }

    pub fn is_session_open(&self) -> bool {
        self.slot.available_permits() == 0
    }

    /// Off refuses every new phone; a session already open runs on to its own end.
    pub fn set_accepting(&self, accepting: bool) {
        self.accepting.store(accepting, Ordering::SeqCst);
    }

    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    /// Starts the session for a phone that just connected; `ChannelBusy` while one is open or
    /// while the channel is not accepting, so the transport refuses that central. Needs a tokio
    /// runtime.
    pub fn open(&self) -> Result<SessionHandle, ChannelBusy> {
        if !self.is_accepting() {
            return Err(ChannelBusy);
        }
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| ChannelBusy)?;
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_QUEUE);
        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_QUEUE);
        let end = Arc::new(Mutex::new(None));
        let session = SetupSession::new(self.ctx.clone(), Instant::now());
        tokio::spawn(run_session(
            session,
            inbound_rx,
            outbound_tx,
            end.clone(),
            permit,
        ));
        tracing::info!(
            kind = "setup_channel_opened",
            "a phone opened the setup channel"
        );
        Ok(SessionHandle {
            writer: SessionWriter {
                inbound: inbound_tx,
            },
            notifications: SessionNotifications {
                outbound: outbound_rx,
                end,
            },
        })
    }
}

/// A session's two ends, for the connection that opened it. Keep it with the connection, never
/// with the advertisement: stopping advertising (the pond just joined Wi-Fi) must not touch it.
#[must_use = "a session lives as long as its phone stays connected and talking; keep the handle"]
pub struct SessionHandle {
    pub writer: SessionWriter,
    pub notifications: SessionNotifications,
}

enum Inbound {
    Write(Vec<u8>),
    AttMtu(u16),
    PhoneDisconnected,
}

/// Where the transport puts what the phone wrote. Dropping every clone means the same as
/// [`SessionWriter::phone_disconnected`]: nothing can reach the session any more.
#[derive(Clone)]
pub struct SessionWriter {
    inbound: mpsc::Sender<Inbound>,
}

impl SessionWriter {
    /// One rx write, as it arrived.
    pub async fn write(&self, bytes: &[u8]) -> Result<(), SessionEnded> {
        self.send(Inbound::Write(bytes.to_vec())).await
    }

    pub async fn set_att_mtu(&self, att_mtu: u16) -> Result<(), SessionEnded> {
        self.send(Inbound::AttMtu(att_mtu)).await
    }

    /// The link is gone; the session ends and frees the channel for the next phone.
    pub async fn phone_disconnected(&self) {
        let _ = self.send(Inbound::PhoneDisconnected).await;
    }

    async fn send(&self, message: Inbound) -> Result<(), SessionEnded> {
        self.inbound.send(message).await.map_err(|_| SessionEnded)
    }
}

/// Where the transport takes what to notify. `None` from [`Self::next`] means the session ended:
/// disconnect the phone, and read why from [`Self::end`].
pub struct SessionNotifications {
    outbound: mpsc::Receiver<Vec<u8>>,
    end: Arc<Mutex<Option<SessionEnd>>>,
}

impl SessionNotifications {
    /// The next tx notification, at most `att_mtu - 3` bytes.
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.outbound.recv().await
    }

    pub fn end(&self) -> Option<SessionEnd> {
        *self.end.lock().unwrap_or_else(|e| e.into_inner())
    }
}

async fn run_session(
    mut session: SetupSession,
    mut inbound: mpsc::Receiver<Inbound>,
    outbound: mpsc::Sender<Vec<u8>>,
    end: Arc<Mutex<Option<SessionEnd>>>,
    _slot: OwnedSemaphorePermit,
) {
    let reason = loop {
        tokio::select! {
            message = inbound.recv() => match message {
                None | Some(Inbound::PhoneDisconnected) => {
                    session.phone_disconnected();
                    break SessionEnd::PhoneDisconnected;
                }
                Some(Inbound::AttMtu(att_mtu)) => session.set_att_mtu(att_mtu),
                Some(Inbound::Write(bytes)) => {
                    let reply = session.receive(&bytes, Instant::now()).await;
                    let mut delivered = true;
                    for notification in reply.notifications {
                        if outbound.send(notification).await.is_err() {
                            delivered = false;
                            break;
                        }
                    }
                    if !delivered {
                        session.phone_disconnected();
                        break SessionEnd::PhoneDisconnected;
                    }
                    if let Some(reason) = reply.end {
                        break reason;
                    }
                }
            },
            _ = tokio::time::sleep_until(session.deadline()) => {
                if let Some(reason) = session.expire_if_due(Instant::now()) {
                    break reason;
                }
            }
        }
    };
    // Recorded before `outbound` drops, so a transport that sees the end can read why.
    *end.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use pond_core::network::domain::wifi::WifiSecurity;
    use pond_core::network::mocks::mock_code_announcer::MockCodeAnnouncer;
    use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
    use pond_core::network::mocks::mock_wifi_station::{sample_network, MockWifiStation};
    use pond_core::network::services::advertising::should_advertise;
    use pond_core::security::ports::handshake::{
        ChannelKeys, HandshakeRequest, HandshakeResponse, PakeChallenge,
    };
    use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

    use super::*;

    const PIN: &str = "sha256/TnkUsN+AaLec3BDTh/KUwSokXTmCq2+4rlfBnmJxPkE=";
    const P2C: [u8; 32] = [1; 32];
    const C2P: [u8; 32] = [2; 32];

    /// Starts numbered runs and accepts a verify whose `confirm` is `good`, with fixed keys.
    #[derive(Default)]
    struct FakeHandshake {
        started: AtomicU32,
        verified: AtomicU32,
    }

    #[async_trait]
    impl Handshake for FakeHandshake {
        async fn handshake(&self, _: HandshakeRequest) -> anyhow::Result<HandshakeResponse> {
            anyhow::bail!("unused")
        }
        async fn validate_token(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn revoke_device(&self, _: &str) -> anyhow::Result<u64> {
            Ok(0)
        }
        async fn revoke_token(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn init_channel_pake(
            &self,
            request: PakeInitRequest,
        ) -> anyhow::Result<Result<PakeChallenge, PakeRefusal>> {
            if request.channel_binding != PIN {
                return Ok(Err(PakeRefusal::ChannelBindingMismatch));
            }
            let n = self.started.fetch_add(1, Ordering::SeqCst);
            Ok(Ok(PakeChallenge {
                challenge_id: format!("run-{n}"),
                challenge: "AAAA".into(),
                expires_at: "2026-10-17T10:01:00Z".into(),
                suite: "CPACE-RISTR255-SHA512".into(),
                share: "BBBB".into(),
            }))
        }
        async fn verify_channel_pake(
            &self,
            request: PakeVerifyRequest,
        ) -> anyhow::Result<ChannelVerify> {
            self.verified.fetch_add(1, Ordering::SeqCst);
            let accepted = request.confirm == "good";
            Ok(ChannelVerify {
                response: HandshakeResponse {
                    accepted,
                    session_token: accepted.then(|| "session-a".into()),
                    refresh_token: accepted.then(|| "refresh-a".into()),
                    expires_at: None,
                    hostname: "pond".into(),
                    server_version: "test".into(),
                    capabilities: vec![],
                    rejection_reason: (!accepted).then(|| "invalid_confirmation".into()),
                    server_proof: accepted.then(|| "ab".repeat(64)),
                },
                keys: accepted.then(|| ChannelKeys::new(&P2C, &C2P)),
            })
        }
    }

    struct Pond {
        handshake: Arc<FakeHandshake>,
        store: Arc<MockSetupCodeStore>,
        station: Arc<MockWifiStation>,
        ctx: Arc<SetupChannelContext>,
    }

    fn pond() -> Pond {
        let handshake = Arc::new(FakeHandshake::default());
        let store = Arc::new(MockSetupCodeStore::unclaimed());
        let station = Arc::new(
            MockWifiStation::new()
                .with_network(
                    sample_network("Home", WifiSecurity::Wpa2, 70),
                    Some("hunter2hunter2"),
                )
                .with_network(sample_network("Cafe", WifiSecurity::Open, 30), None),
        );
        let ctx = SetupChannelContext::new(
            SetupChannelConfig {
                discriminator: MockSetupCodeStore::DISCRIMINATOR,
                pin: PIN.into(),
                https_port: 8443,
                hostname: "pond.local".into(),
            },
            handshake.clone(),
            Arc::new(SpokenCodeService::new(Some(Arc::new(
                MockCodeAnnouncer::new(),
            )))),
            Arc::new(MockSettingsRepository::new()),
            Arc::new(MockProfileRepository::new()),
        )
        .with_claims(Arc::new(SetupClaimService::new(store.clone())))
        .with_wifi(station.clone());
        Pond {
            handshake,
            store,
            station,
            ctx: Arc::new(ctx),
        }
    }

    /// The phone's end of one session: frames at its MTU, reassembles the pond's notifications.
    struct Phone {
        att_mtu: u16,
        cipher: Option<ChannelCipher>,
    }

    impl Phone {
        fn new(att_mtu: u16) -> Self {
            Self {
                att_mtu,
                cipher: None,
            }
        }

        async fn send_raw(
            &self,
            session: &mut SetupSession,
            message: &[u8],
            now: Instant,
        ) -> Reply {
            let frame = framing::encode(message).unwrap();
            let mut last = Reply::default();
            for chunk in framing::chunks(&frame, self.att_mtu) {
                assert!(
                    last.notifications.is_empty(),
                    "answered before the request finished"
                );
                last = session.receive(chunk, now).await;
            }
            last
        }

        fn read(&mut self, reply: &Reply) -> Value {
            let mut reassembler = Reassembler::new();
            let mut message = None;
            for notification in &reply.notifications {
                assert!(notification.len() <= framing::chunk_len(self.att_mtu));
                assert!(message.is_none(), "one answer per request");
                message = reassembler.push(notification).unwrap();
            }
            let message = message.expect("an answer");
            match self.cipher.as_mut() {
                Some(cipher) => {
                    serde_json::from_slice(&cipher.open_envelope(&message).unwrap()).unwrap()
                }
                None => serde_json::from_slice(&message).unwrap(),
            }
        }

        async fn ask(&mut self, session: &mut SetupSession, request: Value, now: Instant) -> Value {
            let bytes = serde_json::to_vec(&request).unwrap();
            let message = match self.cipher.as_mut() {
                Some(cipher) => cipher.seal_envelope(&bytes).unwrap(),
                None => bytes,
            };
            let reply = self.send_raw(session, &message, now).await;
            assert_eq!(reply.end, None, "{request}");
            self.read(&reply)
        }

        /// A whole exchange against the fake handshake, then the sealed phase.
        async fn pair(&mut self, session: &mut SetupSession, now: Instant) -> Value {
            let init = self.ask(session, init_request(1, "setup"), now).await;
            assert_eq!(init["ok"], true, "{init}");
            let verify = self
                .ask(
                    session,
                    json!({"id": 2, "type": "pake_verify", "challenge_id": init["challenge_id"],
                           "share": "CCCC", "confirm": "good", "device_name": "Liz's phone"}),
                    now,
                )
                .await;
            let mut cipher = ChannelCipher::phone(&ChannelKeys::new(&P2C, &C2P));
            let tokens: Value = serde_json::from_slice(
                &cipher
                    .open_base64(verify["sealed"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
            self.cipher = Some(cipher);
            assert_eq!(tokens["session_token"], "session-a");
            verify
        }
    }

    fn init_request(id: u64, credential: &str) -> Value {
        json!({"id": id, "type": "pake_init", "credential": credential, "client_id": "phone-1",
               "client_type": "gotg", "client_version": "1.0", "channel_binding": PIN,
               "client_nonce": "AAAA"})
    }

    #[tokio::test]
    async fn hello_names_the_pond_its_pin_and_what_it_offers() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let hello = Phone::new(23)
            .ask(&mut session, json!({"id": 41, "type": "hello"}), now)
            .await;
        assert_eq!(
            hello,
            json!({"id": 41, "type": "hello", "ok": true, "protocol": 1, "pond_id": "7f3a",
                   "claimed": false, "pin": PIN,
                   "capabilities": ["wifi_scan", "wifi_join", "spoken_code"]})
        );

        pond.store.set_claimed_at(Some(chrono::Utc::now()));
        let hello = Phone::new(185)
            .ask(&mut session, json!({"id": 42, "type": "hello"}), now)
            .await;
        assert_eq!(hello["claimed"], true, "the claim is read live");
    }

    #[tokio::test]
    async fn the_exchange_seals_the_tokens_and_opens_the_sealed_phase() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        let verify = phone.pair(&mut session, now).await;
        assert_eq!(verify["ok"], true);
        assert_eq!(verify["accepted"], true);
        assert_eq!(verify["server_proof"], "ab".repeat(64));
        assert!(
            verify.get("session_token").is_none() && !verify.to_string().contains("session-a"),
            "tokens travel sealed only"
        );
        assert!(session.is_sealed());

        let status = phone
            .ask(&mut session, json!({"id": 3, "type": "status"}), now)
            .await;
        assert_eq!(
            status,
            json!({"id": 3, "type": "status", "ok": true, "claimed": false, "onboarded": false,
                   "network": {"state": "disconnected", "ssid": null, "ipv4": null},
                   "https_port": 8443, "hostname": "pond.local"})
        );
        let scan = phone
            .ask(&mut session, json!({"id": 4, "type": "wifi_scan"}), now)
            .await;
        assert_eq!(scan["networks"][0]["ssid"], "Home");
        assert_eq!(scan["networks"].as_array().unwrap().len(), 2);
        let join = phone
            .ask(
                &mut session,
                json!({"id": 5, "type": "wifi_join", "ssid": "Home", "passphrase": "hunter2hunter2"}),
                now,
            )
            .await;
        assert_eq!(
            join,
            json!({"id": 5, "type": "wifi_join", "ok": true, "address": "192.168.1.42",
                   "https_port": 8443, "hostname": "pond.local"})
        );
        assert_eq!(
            pond.station.joins()[0].passphrase.as_deref(),
            Some("hunter2hunter2")
        );
        let wrong = phone
            .ask(
                &mut session,
                json!({"id": 6, "type": "wifi_join", "ssid": "Home", "passphrase": "guess-guess"}),
                now,
            )
            .await;
        assert_eq!(wrong["error"], "wrong_password");
        assert_eq!(wrong["ok"], false);
    }

    /// The writer's half of the framing rule: every answer opens a fresh notification and ends
    /// on its own last one, so the phone's reader never sees two messages in one chunk.
    #[tokio::test]
    async fn answers_never_share_a_notification() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let phone = Phone::new(23);
        let mut stream = Reassembler::new();
        for id in 0..6u64 {
            let request = format!(r#"{{"id":{id},"type":"hello"}}"#);
            let reply = phone.send_raw(&mut session, request.as_bytes(), now).await;
            assert!(
                stream.is_idle(),
                "the previous answer ended on its own last chunk"
            );
            let (last, rest) = reply.notifications.split_last().unwrap();
            for notification in rest {
                assert_eq!(stream.push(notification), Ok(None));
            }
            let answer: Value =
                serde_json::from_slice(&stream.push(last).unwrap().unwrap()).unwrap();
            assert_eq!(answer["id"], id);
        }
    }

    #[tokio::test]
    async fn a_refused_verify_says_so_in_both_fields_and_stays_in_the_clear() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        let init = phone.ask(&mut session, init_request(1, "setup"), now).await;
        let verify = phone
            .ask(
                &mut session,
                json!({"id": 2, "type": "pake_verify", "challenge_id": init["challenge_id"],
                       "share": "CCCC", "confirm": "bad"}),
                now,
            )
            .await;
        assert_eq!(
            verify,
            json!({"id": 2, "type": "pake_verify", "ok": false, "accepted": false,
                   "error": "invalid_confirmation", "rejection_reason": "invalid_confirmation"})
        );
        assert!(!session.is_sealed());
        let again = phone
            .ask(
                &mut session,
                json!({"id": 3, "type": "pake_verify", "challenge_id": init["challenge_id"],
                       "share": "CCCC", "confirm": "good"}),
                now,
            )
            .await;
        assert_eq!(
            again["rejection_reason"], "unknown_challenge",
            "a run is verified once"
        );
        assert_eq!(pond.handshake.verified.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_verify_may_name_only_a_run_this_connection_started() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let verify = Phone::new(23)
            .ask(
                &mut session,
                json!({"id": 1, "type": "pake_verify", "challenge_id": "run-0",
                       "share": "CCCC", "confirm": "good"}),
                now,
            )
            .await;
        assert_eq!(verify["error"], "unknown_challenge");
        assert_eq!(verify["accepted"], false);
        assert_eq!(pond.handshake.verified.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn requests_in_the_wrong_phase_are_refused_and_the_session_goes_on() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        for (id, kind) in [(1, "status"), (2, "wifi_scan"), (3, "wifi_join")] {
            let answer = phone
                .ask(&mut session, json!({"id": id, "type": kind}), now)
                .await;
            assert_eq!(
                answer,
                json!({"id": id, "type": kind, "ok": false, "error": "wrong_phase"})
            );
        }
        let unknown = phone
            .ask(&mut session, json!({"id": 4, "type": "reboot"}), now)
            .await;
        assert_eq!(unknown["error"], "unknown_type");
        assert_eq!(unknown["type"], "reboot");
        assert!(pond.station.joins().is_empty());

        phone.pair(&mut session, now).await;
        for (id, kind) in [(5, "hello"), (6, "pake_init"), (7, "spoken_code")] {
            let answer = phone
                .ask(&mut session, json!({"id": id, "type": kind}), now)
                .await;
            assert_eq!(answer["error"], "wrong_phase", "{kind}");
        }
    }

    #[tokio::test]
    async fn malformed_bodies_are_refused_by_name() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        let bad_credential = phone.ask(&mut session, init_request(1, "token"), now).await;
        assert_eq!(bad_credential["error"], "invalid_credential");
        let missing = phone
            .ask(
                &mut session,
                json!({"id": 2, "type": "pake_init", "credential": "setup"}),
                now,
            )
            .await;
        assert_eq!(missing["error"], "invalid_request");
        let mut wrong_pin = init_request(3, "pairing");
        wrong_pin["channel_binding"] = json!("sha256/AAAA");
        let refused = phone.ask(&mut session, wrong_pin, now).await;
        assert_eq!(refused["error"], "channel_binding_mismatch");

        let mut session = SetupSession::new(pond.ctx.clone(), now);
        phone = Phone::new(23);
        phone.pair(&mut session, now).await;
        for (id, join) in [
            (4, json!({"type": "wifi_join"})),
            (5, json!({"type": "wifi_join", "ssid": ""})),
            (6, json!({"type": "wifi_join", "ssid": "x".repeat(33)})),
            (
                7,
                json!({"type": "wifi_join", "ssid": "Home", "passphrase": ""}),
            ),
        ] {
            let mut request = join;
            request["id"] = json!(id);
            let answer = phone.ask(&mut session, request, now).await;
            assert_eq!(answer["error"], "invalid_request", "{answer}");
        }
        assert!(pond.station.joins().is_empty());
    }

    #[tokio::test]
    async fn the_fourth_pake_init_is_refused_and_ends_the_session() {
        let pond = pond();
        let now = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        for id in 1..=u64::from(MAX_PAKE_INITS) {
            let answer = phone
                .ask(&mut session, init_request(id, "setup"), now)
                .await;
            assert_eq!(answer["ok"], true);
        }
        let message = serde_json::to_vec(&init_request(9, "setup")).unwrap();
        let reply = phone.send_raw(&mut session, &message, now).await;
        assert_eq!(reply.end, Some(SessionEnd::AttemptsExhausted));
        assert_eq!(
            phone.read(&reply),
            json!({"id": 9, "type": "pake_init", "ok": false, "error": "too_many_attempts"})
        );
        assert_eq!(pond.handshake.started.load(Ordering::SeqCst), 3);
        let after = phone
            .send_raw(&mut session, br#"{"id":10,"type":"hello"}"#, now)
            .await;
        assert!(after.notifications.is_empty());
        assert_eq!(after.end, Some(SessionEnd::AttemptsExhausted));
    }

    #[tokio::test]
    async fn unreadable_oversize_and_overrunning_frames_end_the_session() {
        let pond = pond();
        let now = Instant::now();
        let phone = Phone::new(23);
        for (message, end) in [
            (&b"not json"[..], SessionEnd::UnreadableFrame),
            (br#"{"type":"hello"}"#, SessionEnd::UnreadableFrame),
            (br#"[1,"hello"]"#, SessionEnd::UnreadableFrame),
            (br#"{"sealed":"AAAA"}"#, SessionEnd::UnreadableFrame),
        ] {
            let mut session = SetupSession::new(pond.ctx.clone(), now);
            let reply = phone.send_raw(&mut session, message, now).await;
            assert_eq!(reply.end, Some(end), "{message:?}");
            assert!(reply.notifications.is_empty());
        }

        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let too_long = ((MAX_MESSAGE_LEN + 1) as u32).to_be_bytes();
        assert_eq!(
            session.receive(&too_long, now).await.end,
            Some(SessionEnd::OversizeFrame)
        );

        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let two = [
            framing::encode(br#"{"id":1,"type":"hello"}"#).unwrap(),
            framing::encode(br#"{"id":2,"type":"hello"}"#).unwrap(),
        ]
        .concat();
        assert_eq!(
            session.receive(&two, now).await.end,
            Some(SessionEnd::Overrun)
        );
    }

    #[tokio::test]
    async fn after_the_exchange_a_clear_tampered_or_replayed_message_ends_the_session() {
        let pond = pond();
        let now = Instant::now();

        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        phone.pair(&mut session, now).await;
        let clear = phone
            .send_raw(&mut session, br#"{"id":3,"type":"status"}"#, now)
            .await;
        assert_eq!(clear.end, Some(SessionEnd::SealingFailure));
        assert!(!session.is_sealed(), "the keys went with the session");

        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        phone.pair(&mut session, now).await;
        let mut sealed = super::sealing::Sealer::new(&C2P)
            .seal(br#"{"id":3,"type":"status"}"#)
            .unwrap();
        sealed[0] ^= 0x80;
        let envelope = serde_json::to_vec(&json!({"sealed": base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD, &sealed)}))
        .unwrap();
        let tampered = phone.send_raw(&mut session, &envelope, now).await;
        assert_eq!(tampered.end, Some(SessionEnd::SealingFailure));

        let mut session = SetupSession::new(pond.ctx.clone(), now);
        let mut phone = Phone::new(23);
        phone.pair(&mut session, now).await;
        let first = phone
            .cipher
            .as_mut()
            .unwrap()
            .seal_envelope(br#"{"id":3,"type":"status"}"#)
            .unwrap();
        let reply = phone.send_raw(&mut session, &first, now).await;
        assert_eq!(phone.read(&reply)["ok"], true);
        let replayed = phone.send_raw(&mut session, &first, now).await;
        assert_eq!(replayed.end, Some(SessionEnd::SealingFailure));
    }

    #[tokio::test]
    async fn the_idle_timer_restarts_with_every_request() {
        let pond = pond();
        let t0 = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), t0);
        let mut phone = Phone::new(23);
        let step = IDLE_TIMEOUT - Duration::from_secs(1);
        let mut now = t0;
        // Four steps and the idle end both land inside the lifetime cap.
        for id in 0..4 {
            now += step;
            assert_eq!(session.expire_if_due(now), None);
            phone
                .ask(&mut session, json!({"id": id, "type": "hello"}), now)
                .await;
        }
        assert_eq!(session.expire_if_due(now + step), None);
        assert_eq!(
            session.expire_if_due(now + IDLE_TIMEOUT),
            Some(SessionEnd::Idle)
        );
        let late = phone
            .send_raw(
                &mut session,
                br#"{"id":9,"type":"hello"}"#,
                now + IDLE_TIMEOUT,
            )
            .await;
        assert!(late.notifications.is_empty());
    }

    /// The transport the driver gives an adapter, used the way a GATT server would use it.
    async fn ask_over(
        handle: &mut SessionHandle,
        phone: &mut Phone,
        request: Value,
    ) -> Option<Value> {
        let bytes = serde_json::to_vec(&request).unwrap();
        let message = match phone.cipher.as_mut() {
            Some(cipher) => cipher.seal_envelope(&bytes).unwrap(),
            None => bytes,
        };
        let frame = framing::encode(&message).unwrap();
        for chunk in framing::chunks(&frame, phone.att_mtu) {
            handle.writer.write(chunk).await.ok()?;
        }
        let mut reassembler = Reassembler::new();
        loop {
            let notification = handle.notifications.next().await?;
            if let Some(answer) = reassembler.push(&notification).unwrap() {
                let plain = match phone.cipher.as_mut() {
                    Some(cipher) => cipher.open_envelope(&answer).unwrap().to_vec(),
                    None => answer,
                };
                return Some(serde_json::from_slice(&plain).unwrap());
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_session_outlives_advertising_and_ends_only_when_the_phone_goes_quiet() {
        let pond = pond();
        let channel = SetupChannel::new(
            Arc::try_unwrap(pond.ctx).unwrap_or_else(|_| panic!("the test holds the only context")),
        );
        let mut handle = channel.open().unwrap();
        handle.writer.set_att_mtu(185).await.unwrap();
        let mut phone = Phone::new(185);

        let init = ask_over(&mut handle, &mut phone, init_request(1, "setup"))
            .await
            .unwrap();
        let verify = ask_over(
            &mut handle,
            &mut phone,
            json!({"id": 2, "type": "pake_verify", "challenge_id": init["challenge_id"],
                   "share": "CCCC", "confirm": "good"}),
        )
        .await
        .unwrap();
        let mut cipher = ChannelCipher::phone(&ChannelKeys::new(&P2C, &C2P));
        cipher
            .open_base64(verify["sealed"].as_str().unwrap())
            .unwrap();
        phone.cipher = Some(cipher);
        let join = ask_over(
            &mut handle,
            &mut phone,
            json!({"id": 3, "type": "wifi_join", "ssid": "Cafe"}),
        )
        .await
        .unwrap();
        assert_eq!(join["address"], "192.168.1.42");

        pond.store.set_claimed_at(Some(chrono::Utc::now()));
        assert!(
            !should_advertise(true, true),
            "a claimed pond on a network stops advertising"
        );

        for id in 4..8 {
            tokio::time::sleep(IDLE_TIMEOUT - Duration::from_secs(1)).await;
            let status = ask_over(&mut handle, &mut phone, json!({"id": id, "type": "status"}))
                .await
                .expect("the session is still open");
            assert_eq!(status["network"]["state"], "connected");
            assert_eq!(status["claimed"], true);
        }
        assert!(channel.is_session_open());

        tokio::time::sleep(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        assert_eq!(handle.notifications.next().await, None);
        assert_eq!(handle.notifications.end(), Some(SessionEnd::Idle));
        assert!(handle.writer.write(b"x").await.is_err());
    }

    #[tokio::test]
    async fn one_phone_at_a_time_and_a_disconnect_frees_the_channel() {
        let channel = SetupChannel::new(
            Arc::try_unwrap(pond().ctx).unwrap_or_else(|_| panic!("only context")),
        );
        let mut first = channel.open().unwrap();
        assert_eq!(channel.open().err(), Some(ChannelBusy));
        assert!(channel.is_session_open());

        first.writer.phone_disconnected().await;
        assert_eq!(first.notifications.next().await, None);
        assert_eq!(
            first.notifications.end(),
            Some(SessionEnd::PhoneDisconnected)
        );
        let SessionHandle {
            writer,
            mut notifications,
        } = channel.open().expect("the slot is free again");
        drop(writer);
        assert_eq!(
            notifications.next().await,
            None,
            "dropping every writer is a disconnect"
        );
        assert_eq!(notifications.end(), Some(SessionEnd::PhoneDisconnected));
        assert!(channel.open().is_ok());
    }

    #[tokio::test]
    async fn a_session_ends_at_its_lifetime_cap_however_busy_it_is() {
        let pond = pond();
        let t0 = Instant::now();
        let mut session = SetupSession::new(pond.ctx.clone(), t0);
        let mut phone = Phone::new(23);
        let step = IDLE_TIMEOUT - Duration::from_secs(1);
        let mut now = t0;
        let mut id = 0;
        while now + step < t0 + MAX_SESSION_LIFETIME {
            now += step;
            id += 1;
            phone
                .ask(&mut session, json!({"id": id, "type": "hello"}), now)
                .await;
        }
        assert_eq!(session.deadline(), t0 + MAX_SESSION_LIFETIME);
        let late = phone
            .send_raw(
                &mut session,
                br#"{"id":99,"type":"hello"}"#,
                t0 + MAX_SESSION_LIFETIME,
            )
            .await;
        assert!(late.notifications.is_empty());
        assert_eq!(late.end, Some(SessionEnd::LifetimeCap));
    }

    #[tokio::test(start_paused = true)]
    async fn a_phone_pinging_inside_the_idle_timeout_is_still_let_go() {
        let channel = SetupChannel::new(
            Arc::try_unwrap(pond().ctx).unwrap_or_else(|_| panic!("only context")),
        );
        let mut handle = channel.open().unwrap();
        let mut phone = Phone::new(185);
        let opened = Instant::now();
        let mut id = 0;
        while ask_over(&mut handle, &mut phone, json!({"id": id, "type": "hello"}))
            .await
            .is_some()
        {
            id += 1;
            tokio::time::sleep(IDLE_TIMEOUT - Duration::from_secs(1)).await;
        }
        assert_eq!(handle.notifications.end(), Some(SessionEnd::LifetimeCap));
        assert!(opened.elapsed() >= MAX_SESSION_LIFETIME);
        assert!(opened.elapsed() < MAX_SESSION_LIFETIME + IDLE_TIMEOUT);
        assert!(
            !channel.is_session_open(),
            "the slot is free for the next phone"
        );
    }

    #[tokio::test]
    async fn a_channel_that_is_not_accepting_refuses_new_phones_and_keeps_the_open_one() {
        let channel = SetupChannel::new(
            Arc::try_unwrap(pond().ctx).unwrap_or_else(|_| panic!("only context")),
        );
        let mut open = channel.open().unwrap();
        channel.set_accepting(false);
        let mut phone = Phone::new(185);
        let hello = ask_over(&mut open, &mut phone, json!({"id": 1, "type": "hello"})).await;
        assert_eq!(hello.unwrap()["ok"], true, "the open session runs on");

        open.writer.phone_disconnected().await;
        assert_eq!(open.notifications.next().await, None);
        assert_eq!(channel.open().err(), Some(ChannelBusy));
        channel.set_accepting(true);
        assert!(channel.open().is_ok());
    }
}
