//! FCM-v1 push relay; replaces [`crate::stub_push_relay`] when a service-account key exists.
//!
//! Sends data-only wake pings (no title/body) so notification content never transits Google;
//! the phone pulls it from GIAP. The key is a secret: only `client_email`/`project_id` may be
//! logged. Every outbound call is recorded as egress. APNs/Expo tokens are skipped.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use pond_core::shared::services::egress::{check_egress, record_egress};
use pond_core::user_data::domain::push_token::PushPlatform;
use pond_core::user_data::ports::push_token::PushTokenRepository;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::push_token_log::token_log_prefix;

const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
/// Refresh the cached access token this long before its stated expiry.
const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// Per-call ceiling: the relay is awaited inline, so a hung socket would stall delivery.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The fields GIAP needs from a Firebase service-account key file.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccount {
    pub project_id: String,
    pub private_key: String,
    pub client_email: String,
    pub token_uri: String,
}

/// Parses structure only; the key is validated when the signing key is built.
pub fn parse_service_account(raw: &str) -> Result<ServiceAccount> {
    serde_json::from_str(raw).context("service-account JSON missing required fields")
}

const FCM_BASE_URL: &str = "https://fcm.googleapis.com";

pub fn fcm_send_url(base: &str, project_id: &str) -> String {
    format!("{base}/v1/projects/{project_id}/messages:send")
}

/// Data-only wake message: no `notification` block or title/body; content stays on the Pond.
pub fn wake_message(device_token: &str, notification: &Notification) -> Value {
    json!({
        "message": {
            "token": device_token,
            "data": {
                "notification_id": notification.id,
                "category": notification.category,
                "wake": "1",
            },
            "android": { "priority": "HIGH" },
        }
    })
}

#[derive(serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

pub struct FcmPushRelay {
    account: ServiceAccount,
    signing_key: jsonwebtoken::EncodingKey,
    push_tokens: Arc<dyn PushTokenRepository>,
    http: reqwest::Client,
    /// `(access_token, refresh_after)` — refreshed lazily on demand.
    cached_token: Mutex<Option<(String, Instant)>>,
    /// FCM host. Always [`FCM_BASE_URL`] outside tests.
    fcm_base: String,
}

impl FcmPushRelay {
    /// Load the key file, failing fast (bad path/JSON, non-RSA key) so the caller can use the stub.
    pub fn from_key_file(path: &Path, push_tokens: Arc<dyn PushTokenRepository>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading FCM service-account key at {}", path.display()))?;
        let account = parse_service_account(&raw)?;
        let signing_key = jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .context("service-account private_key is not a valid RSA PEM")?;
        tracing::info!(
            project = %account.project_id,
            client = %account.client_email,
            "FCM push relay active (data-only wake pings)"
        );
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("building the FCM HTTP client")?;
        Ok(Self {
            account,
            signing_key,
            push_tokens,
            http,
            cached_token: Mutex::new(None),
            fcm_base: FCM_BASE_URL.to_string(),
        })
    }

    /// Cached OAuth access token, or a fresh JWT-bearer exchange.
    async fn access_token(&self) -> Result<String> {
        if let Some((token, refresh_after)) = self
            .cached_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            if Instant::now() < refresh_after {
                return Ok(token);
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before UNIX epoch")?
            .as_secs();
        let claims = Claims {
            iss: &self.account.client_email,
            scope: FCM_SCOPE,
            aud: &self.account.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &self.signing_key,
        )
        .context("signing FCM auth assertion")?;

        // Sensitive host: both restrictive egress modes refuse the token exchange.
        check_egress(&self.account.token_uri)?;

        let started = Instant::now();
        let response = self
            .http
            .post(&self.account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await;
        let latency = started.elapsed().as_millis() as u64;
        let response = match response {
            Ok(r) => {
                record_egress(
                    &self.account.token_uri,
                    "POST",
                    Some(r.status().as_u16()),
                    latency,
                );
                r
            }
            Err(e) => {
                record_egress(&self.account.token_uri, "POST", None, latency);
                return Err(anyhow!("FCM token exchange failed: {e}"));
            }
        };
        if !response.status().is_success() {
            return Err(anyhow!(
                "FCM token exchange rejected (HTTP {})",
                response.status()
            ));
        }
        let token: TokenResponse = response
            .json()
            .await
            .context("parsing FCM token response")?;

        let refresh_after = Instant::now()
            + Duration::from_secs(token.expires_in).saturating_sub(TOKEN_EXPIRY_MARGIN);
        *self.cached_token.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((token.access_token.clone(), refresh_after));
        Ok(token.access_token)
    }
}

#[async_trait]
impl NotificationRelay for FcmPushRelay {
    async fn relay(&self, notification: &Notification) -> Result<()> {
        let Some(stored) = self.push_tokens.get(&notification.target).await? else {
            tracing::debug!(
                device = %notification.target,
                "FCM relay: no push token registered; skipping background push"
            );
            return Ok(());
        };
        match stored.platform {
            PushPlatform::Fcm => {}
            PushPlatform::Apns => {
                tracing::info!(
                    device = %notification.target,
                    "APNs push not yet supported (needs an Apple Developer key); skipping"
                );
                return Ok(());
            }
            PushPlatform::Expo => {
                tracing::debug!(
                    device = %notification.target,
                    "expo-platform token registered but GIAP relays directly; skipping"
                );
                return Ok(());
            }
        }

        let access_token = self.access_token().await?;
        let url = fcm_send_url(&self.fcm_base, &self.account.project_id);
        let body = wake_message(&stored.token, notification);

        // Gate the send too: a token cached before the mode was tightened would keep pushing.
        check_egress(&url)?;

        let started = Instant::now();
        let response = self
            .http
            .post(&url)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .await;
        let latency = started.elapsed().as_millis() as u64;
        let response = match response {
            Ok(r) => {
                record_egress(&url, "POST", Some(r.status().as_u16()), latency);
                r
            }
            Err(e) => {
                record_egress(&url, "POST", None, latency);
                return Err(anyhow!("FCM send failed: {e}"));
            }
        };

        let prefix = token_log_prefix(&stored.token);
        if response.status().is_success() {
            tracing::info!(
                device = %notification.target,
                token_prefix = %prefix,
                category = %notification.category,
                "FCM wake ping delivered"
            );
            Ok(())
        } else {
            // 404/410 = stale token (app reinstalled); others = config/quota.
            Err(anyhow!(
                "FCM rejected wake ping for device {} (HTTP {})",
                notification.target,
                response.status()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::push_token::PushToken;

    /// Deliberately non-RSA key: these tests must return before signing, or `encode` fails.
    fn relay_with(tokens: Arc<dyn PushTokenRepository>) -> FcmPushRelay {
        FcmPushRelay {
            account: ServiceAccount {
                project_id: "goose-test".into(),
                private_key: "unused".into(),
                client_email: "svc@goose-test.iam.gserviceaccount.com".into(),
                // Unroutable: reaching the network fails the test instead of calling Google.
                token_uri: "http://127.0.0.1:1/token".into(),
            },
            signing_key: jsonwebtoken::EncodingKey::from_secret(b"not-an-rsa-key"),
            push_tokens: tokens,
            http: reqwest::Client::new(),
            cached_token: Mutex::new(None),
            // Unroutable for the same reason as token_uri above.
            fcm_base: "http://127.0.0.1:1".into(),
        }
    }

    #[derive(Default)]
    struct StubTokens {
        token: Mutex<Option<PushToken>>,
    }
    #[async_trait]
    impl PushTokenRepository for StubTokens {
        async fn upsert(&self, t: PushToken) -> Result<()> {
            *self.token.lock().unwrap() = Some(t);
            Ok(())
        }
        async fn get(&self, _device_id: &str) -> Result<Option<PushToken>> {
            Ok(self.token.lock().unwrap().clone())
        }
        async fn list(&self) -> Result<Vec<PushToken>> {
            Ok(self.token.lock().unwrap().clone().into_iter().collect())
        }
        async fn delete(&self, _device_id: &str) -> Result<()> {
            *self.token.lock().unwrap() = None;
            Ok(())
        }
    }

    fn notif() -> Notification {
        Notification {
            id: "n-1".into(),
            target: "dev-1".into(),
            category: "alert".into(),
            title: "t".into(),
            body: "b".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            data: None,
        }
    }

    async fn seeded(platform: PushPlatform, token: &str) -> Arc<StubTokens> {
        let tokens = Arc::new(StubTokens::default());
        tokens
            .upsert(PushToken {
                device_id: "dev-1".into(),
                token: token.into(),
                platform,
                updated_at: String::new(),
            })
            .await
            .unwrap();
        tokens
    }

    #[tokio::test]
    async fn relay_without_a_registered_token_is_a_no_op() {
        let relay = relay_with(Arc::new(StubTokens::default()));
        relay.relay(&notif()).await.unwrap();
    }

    #[tokio::test]
    async fn non_fcm_platforms_are_skipped_without_touching_the_network() {
        for platform in [PushPlatform::Apns, PushPlatform::Expo] {
            let relay = relay_with(seeded(platform, "token-abcdefghij").await);
            relay
                .relay(&notif())
                .await
                .expect("non-FCM platforms are skipped, not errors");
        }
    }

    // ── Signing + HTTP path ─────────────────────────────────────────────────
    // The RSA key is generated at run time: gitleaks scans history, so never commit a PEM.

    /// Generated once per test binary: RSA keygen takes about a second.
    fn test_signing_key() -> &'static jsonwebtoken::EncodingKey {
        use rsa::pkcs8::EncodePrivateKey;
        static KEY: std::sync::OnceLock<jsonwebtoken::EncodingKey> = std::sync::OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rand::thread_rng();
            // jsonwebtoken/ring requires >= 2047 bits for RS256.
            let private = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA key");
            let pem = private
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .expect("encode PKCS#8 PEM");
            jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).expect("load generated key")
        })
    }

    fn signing_relay_with_token_uri(
        base: &str,
        token_uri: &str,
        tokens: Arc<dyn PushTokenRepository>,
    ) -> FcmPushRelay {
        FcmPushRelay {
            account: ServiceAccount {
                project_id: "goose-test".into(),
                private_key: "unused-once-the-key-is-built".into(),
                client_email: "svc@goose-test.iam.gserviceaccount.com".into(),
                token_uri: token_uri.into(),
            },
            signing_key: test_signing_key().clone(),
            push_tokens: tokens,
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .unwrap(),
            cached_token: Mutex::new(None),
            fcm_base: base.into(),
        }
    }

    fn signing_relay(base: &str, tokens: Arc<dyn PushTokenRepository>) -> FcmPushRelay {
        signing_relay_with_token_uri(base, &format!("{base}/token"), tokens)
    }

    fn ok_token_response() -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(200)
            .set_body_json(json!({ "access_token": "test-access-token", "expires_in": 3600 }))
    }

    /// Serialises outbound-call tests: the egress sink is process-global, so counts would mix.
    static EGRESS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Shared capture buffer: `set_egress_sink` is first-wins, so a per-test sink is impossible.
    fn captured_egress() -> Arc<Mutex<Vec<pond_core::security::domain::event::Event>>> {
        use async_trait::async_trait;
        use pond_core::security::domain::event::{Event, EventQuery};
        use pond_core::security::ports::event_log::EventLog;

        struct CapturingLog(Arc<Mutex<Vec<Event>>>);
        #[async_trait]
        impl EventLog for CapturingLog {
            async fn append(&self, event: Event) -> Result<()> {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
                Ok(())
            }
            async fn query(&self, _q: EventQuery) -> Result<Vec<Event>> {
                Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
            }
            async fn purge(&self, _q: EventQuery) -> Result<u64> {
                Ok(0)
            }
        }

        static CAPTURE: std::sync::OnceLock<Arc<Mutex<Vec<Event>>>> = std::sync::OnceLock::new();
        CAPTURE
            .get_or_init(|| {
                let buffer = Arc::new(Mutex::new(Vec::new()));
                pond_core::shared::services::egress::set_egress_sink(Arc::new(CapturingLog(
                    buffer.clone(),
                )));
                buffer
            })
            .clone()
    }

    #[tokio::test]
    async fn signs_exchanges_and_sends() {
        let _egress_guard = EGRESS_LOCK.lock().await;
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            // The RFC 7523 grant, carrying our signed assertion.
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
            ))
            .and(body_string_contains("assertion="))
            .respond_with(ok_token_response())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .and(header("authorization", "Bearer test-access-token"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(json!({ "name": "ok" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        relay.relay(&notif()).await.expect("wake ping delivered");
        // `expect(...)` on both mocks is verified when the server drops.
    }

    /// Google rate-limits token issuance, so a burst of pushes must share one token.
    #[tokio::test]
    async fn the_access_token_is_reused_across_sends() {
        let _egress_guard = EGRESS_LOCK.lock().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .expect(1) // exactly once, despite two sends
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(2)
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        relay.relay(&notif()).await.unwrap();
        relay.relay(&notif()).await.unwrap();
    }

    #[tokio::test]
    async fn a_rejected_token_exchange_fails_without_sending() {
        let _egress_guard = EGRESS_LOCK.lock().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(401).set_body_json(json!({
                "error": "invalid_grant"
            })))
            .mount(&server)
            .await;
        // No send mock: reaching the send endpoint at all fails the test.

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        let err = relay.relay(&notif()).await.expect_err("must not proceed");
        assert!(
            err.to_string().contains("token exchange rejected"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn a_rejected_send_is_reported() {
        let _egress_guard = EGRESS_LOCK.lock().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        let err = relay.relay(&notif()).await.expect_err("404 must surface");
        assert!(err.to_string().contains("404"), "unexpected error: {err}");
    }

    /// The send 404s on purpose: `extract_host` drops the port, so only status tells the two
    /// events apart, and a failed push must still be recorded.
    #[tokio::test]
    async fn both_outbound_calls_are_recorded_as_egress() {
        use pond_core::security::domain::event::{EventCategory, PrivacySensitivity};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let _egress_guard = EGRESS_LOCK.lock().await;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let captured = captured_egress();
        captured.lock().unwrap_or_else(|e| e.into_inner()).clear();

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        relay.relay(&notif()).await.expect_err("404 send");

        // `record_egress` spawns the append, so let it land.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let events = captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|e| e.action == "egress.http")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            events.len(),
            2,
            "expected the token exchange and the send to both be recorded, got {events:#?}"
        );
        for ev in &events {
            assert_eq!(ev.category, EventCategory::Network);
            assert_eq!(ev.attributes.get("method"), Some(&"POST".into()));
            assert_eq!(ev.attributes.get("host"), Some(&"127.0.0.1".into()));
            // Loopback mock; real googleapis.com traffic classifies as Sensitive.
            assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Internal);
        }
        let statuses = events
            .iter()
            .filter_map(|e| e.attributes.get("status").cloned())
            .collect::<Vec<_>>();
        assert!(
            statuses.contains(&200_i64.into()),
            "token exchange not recorded: {statuses:?}"
        );
        assert!(
            statuses.contains(&404_i64.into()),
            "failed send not recorded: {statuses:?}"
        );
    }

    /// The token's 8th byte is mid-character; redaction only runs after a successful send.
    #[tokio::test]
    async fn a_multi_byte_token_survives_the_send_path() {
        let _egress_guard = EGRESS_LOCK.lock().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "日本語のトークンです").await,
        );
        relay
            .relay(&notif())
            .await
            .expect("must not panic redacting");
    }

    #[test]
    fn parses_a_service_account_and_rejects_incomplete_ones() {
        // Structure-only fixture; the fake key is a plain string, not a PEM.
        let ok = r#"{
            "type": "service_account",
            "project_id": "goose-test",
            "private_key": "not-a-real-key",
            "client_email": "svc@goose-test.iam.gserviceaccount.com",
            "token_uri": "https://oauth2.googleapis.com/token"
        }"#;
        let account = parse_service_account(ok).unwrap();
        assert_eq!(account.project_id, "goose-test");
        assert!(parse_service_account(r#"{"project_id": "x"}"#).is_err());
    }

    #[test]
    fn send_url_targets_the_project() {
        assert_eq!(
            fcm_send_url(FCM_BASE_URL, "goose-test"),
            "https://fcm.googleapis.com/v1/projects/goose-test/messages:send"
        );
    }

    #[test]
    fn the_default_base_is_googles() {
        let relay = signing_relay("http://unused", Arc::new(StubTokens::default()));
        assert_eq!(FCM_BASE_URL, "https://fcm.googleapis.com");
        assert!(relay.fcm_base.starts_with("http://")); // test override in effect
    }

    #[test]
    fn wake_messages_carry_no_notification_content() {
        let n = Notification {
            id: "n-1".into(),
            target: "device-1".into(),
            category: "alert".into(),
            title: "Failed pairing attempt".into(),
            body: "Someone tried to pair".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            data: None,
        };
        let msg = wake_message("fcm-token-abc", &n);

        assert_eq!(msg["message"]["token"], "fcm-token-abc");
        assert_eq!(msg["message"]["data"]["notification_id"], "n-1");
        assert_eq!(msg["message"]["data"]["category"], "alert");
        assert_eq!(msg["message"]["data"]["wake"], "1");
        assert_eq!(msg["message"]["android"]["priority"], "HIGH");
        assert!(msg["message"].get("notification").is_none());
        let serialized = msg.to_string();
        assert!(!serialized.contains("Failed pairing attempt"));
        assert!(!serialized.contains("Someone tried to pair"));
    }
}
