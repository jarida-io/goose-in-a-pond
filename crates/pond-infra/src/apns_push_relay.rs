//! APNs push relay for iPhones, beside [`crate::fcm_push_relay`] for Android.
//!
//! iOS throttles silent pushes and drops them for an app the person swiped away, so the wake is
//! a visible alert. It carries one fixed line and never the notice's own words: the phone
//! fetches those from the pond over its pinned channel, as it does after an Android wake. The
//! key is a secret: only the team, key id and bundle id may be logged. Every outbound call is
//! recorded as egress.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::mcp::domain::notice_delivery::{Wake, WakeGap};
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use pond_core::shared::services::egress::{self, EgressPurpose};
use pond_core::user_data::domain::push_token::PushPlatform;
use pond_core::user_data::ports::push_token::PushTokenRepository;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::push_token_log::token_log_prefix;

const APNS_PRODUCTION: &str = "https://api.push.apple.com";
const APNS_SANDBOX: &str = "https://api.sandbox.push.apple.com";
/// A provider token may be made at most once in 20 minutes and is refused after 60, so one is
/// reused for 40.
const PROVIDER_TOKEN_LIFETIME: Duration = Duration::from_secs(40 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long Apple keeps trying a phone that is off: the pond's own repeat wakes cover longer.
const KEEP_TRYING_SECS: u64 = 60 * 60;
/// The whole of what the push says. The notice itself never leaves the pond this way.
const WAKE_TITLE: &str = "Goose";
const WAKE_BODY: &str = "Something new from your Pond.";
/// `apns-collapse-id` may be 64 bytes at most.
const COLLAPSE_ID_BYTES: usize = 64;

/// The fields the pond needs from its Apple push key file.
#[derive(Debug, Clone, Deserialize)]
pub struct ApnsKey {
    /// The 10-character team id the key belongs to.
    pub team_id: String,
    /// The key's own 10-character id.
    pub key_id: String,
    /// The app's bundle id, which is the push topic.
    pub bundle_id: String,
    /// The `.p8` file's contents: a PKCS#8 P-256 private key in PEM.
    pub private_key: String,
    /// True for an app built for development, whose tokens belong to Apple's sandbox.
    #[serde(default)]
    pub sandbox: bool,
}

#[derive(serde::Serialize)]
struct ProviderClaims<'a> {
    iss: &'a str,
    iat: u64,
}

pub struct ApnsPushRelay {
    key: ApnsKey,
    signing_key: jsonwebtoken::EncodingKey,
    push_tokens: Arc<dyn PushTokenRepository>,
    http: reqwest::Client,
    /// `(provider token, made at)`.
    provider_token: Mutex<Option<(String, Instant)>>,
    /// APNs host. Always Apple's outside tests.
    base: String,
}

impl ApnsPushRelay {
    /// Load the key file, failing fast on a bad path, bad JSON or a key that is not P-256, so
    /// the caller can carry on without an Apple sender.
    pub fn from_key_file(path: &Path, push_tokens: Arc<dyn PushTokenRepository>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading the Apple push key at {}", path.display()))?;
        let key: ApnsKey = serde_json::from_str(&raw).context("the Apple push key is not JSON")?;
        for (name, value) in [
            ("team_id", &key.team_id),
            ("key_id", &key.key_id),
            ("bundle_id", &key.bundle_id),
        ] {
            anyhow::ensure!(!value.trim().is_empty(), "the Apple push key has no {name}");
        }
        let signing_key = jsonwebtoken::EncodingKey::from_ec_pem(key.private_key.as_bytes())
            .context("the Apple push key's private_key is not a P-256 PEM")?;
        let http = pond_net::client_builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("building the APNs HTTP client")?;
        tracing::info!(
            team = %key.team_id,
            key = %key.key_id,
            topic = %key.bundle_id,
            sandbox = key.sandbox,
            "APNs push relay active (alerts that carry no notice text)"
        );
        let base = if key.sandbox {
            APNS_SANDBOX
        } else {
            APNS_PRODUCTION
        };
        Ok(Self {
            key,
            signing_key,
            push_tokens,
            http,
            provider_token: Mutex::new(None),
            base: base.to_string(),
        })
    }

    fn provider_token(&self) -> Result<String> {
        let mut cached = self
            .provider_token
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((token, made)) = cached.as_ref() {
            if made.elapsed() < PROVIDER_TOKEN_LIFETIME {
                return Ok(token.clone());
            }
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(self.key.key_id.clone());
        let iat = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow!("the clock is before 1970"))?
            .as_secs();
        let token = jsonwebtoken::encode(
            &header,
            &ProviderClaims {
                iss: &self.key.team_id,
                iat,
            },
            &self.signing_key,
        )
        .context("signing the APNs provider token")?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }
}

/// The push for one notice: a fixed line, and the ids the app fetches the notice by.
fn wake_payload(notification: &Notification) -> Value {
    json!({
        "aps": {
            "alert": {"title": WAKE_TITLE, "body": WAKE_BODY},
            "sound": "default",
            // Lets the app run and, once it has an extension for it, put the notice's own
            // words in place of the fixed line.
            "content-available": 1,
            "mutable-content": 1,
        },
        "notification_id": notification.id,
        "category": notification.category,
        "wake": "1",
    })
}

/// One notice's wakes replace each other on the phone, so a repeat is not a second line.
fn collapse_id(notification: &Notification) -> String {
    let mut id = notification.id.clone();
    while id.len() > COLLAPSE_ID_BYTES {
        id.pop();
    }
    id
}

#[async_trait]
impl NotificationRelay for ApnsPushRelay {
    async fn relay(&self, notification: &Notification) -> Result<Wake> {
        let Some(stored) = self.push_tokens.get(&notification.target).await? else {
            return Ok(Wake::NotSent(WakeGap::NoToken));
        };
        if stored.platform != PushPlatform::Apns {
            return Ok(Wake::NotSent(WakeGap::NoSender));
        }
        let provider_token = self.provider_token()?;
        let url = format!("{}/3/device/{}", self.base, stored.token);
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|now| now.as_secs() + KEEP_TRYING_SECS)
            .unwrap_or(0);

        let call = egress::begin_for(&url, "POST", EgressPurpose::Push, "")?;
        let response = self
            .http
            .post(&url)
            .bearer_auth(provider_token)
            .header("apns-topic", &self.key.bundle_id)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", expires.to_string())
            .header("apns-collapse-id", collapse_id(notification))
            .json(&wake_payload(notification))
            .send()
            .await;
        call.finish(response.as_ref().ok().map(|r| r.status().as_u16()));
        let response = response.map_err(|e| anyhow!("APNs send failed: {e}"))?;

        let prefix = token_log_prefix(&stored.token);
        let status = response.status();
        if status.is_success() {
            tracing::info!(
                device = %notification.target,
                token_prefix = %prefix,
                category = %notification.category,
                "APNs wake delivered"
            );
            return Ok(Wake::Sent);
        }
        // Apple says why in a small JSON body: `{"reason": "BadDeviceToken"}`.
        let reason = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|body| {
                body.get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        Err(anyhow!(
            "APNs refused the wake for device {} (HTTP {status} {reason})",
            notification.target
        ))
    }
}

/// Wakes a phone through whichever of the pond's senders its token belongs to.
pub struct PushRelays(Vec<Arc<dyn NotificationRelay>>);

impl PushRelays {
    pub fn new(relays: Vec<Arc<dyn NotificationRelay>>) -> Self {
        Self(relays)
    }
}

#[async_trait]
impl NotificationRelay for PushRelays {
    async fn relay(&self, notification: &Notification) -> Result<Wake> {
        let mut gap = WakeGap::NoKey;
        for relay in &self.0 {
            match relay.relay(notification).await? {
                Wake::Sent => return Ok(Wake::Sent),
                // No token is the whole answer: no sender could have done better.
                Wake::NotSent(WakeGap::NoToken) => return Ok(Wake::NotSent(WakeGap::NoToken)),
                // This sender is for another kind of phone; the next may be the one.
                Wake::NotSent(_) => gap = WakeGap::NoKey,
            }
        }
        Ok(Wake::NotSent(gap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::push_token::PushToken;
    use pond_core::user_data::mocks::mock_push_token::MockPushTokenRepository;

    fn notice() -> Notification {
        Notification {
            id: "briefing:phone-1".into(),
            target: "phone-1".into(),
            category: "info".into(),
            title: "Schedule: Daily Briefing".into(),
            body: "The dollar is at 129 shillings.".into(),
            timestamp: "2026-11-13T05:00:00Z".into(),
            data: None,
        }
    }

    async fn tokens(platform: Option<PushPlatform>) -> Arc<MockPushTokenRepository> {
        let tokens = Arc::new(MockPushTokenRepository::new());
        if let Some(platform) = platform {
            tokens
                .upsert(PushToken {
                    device_id: "phone-1".into(),
                    token: "a1b2c3d4".into(),
                    platform,
                    updated_at: "2026-11-13T00:00:00Z".into(),
                })
                .await
                .unwrap();
        }
        tokens
    }

    /// A relay that never signs: its provider token is already in hand.
    fn relay(tokens: Arc<dyn PushTokenRepository>, base: &str) -> ApnsPushRelay {
        ApnsPushRelay {
            key: ApnsKey {
                team_id: "TEAM123456".into(),
                key_id: "KEY1234567".into(),
                bundle_id: "io.jarida.gooseonthego".into(),
                private_key: "unused".into(),
                sandbox: false,
            },
            signing_key: jsonwebtoken::EncodingKey::from_secret(b"not-a-p256-key"),
            push_tokens: tokens,
            http: pond_net::client_builder().build().unwrap(),
            provider_token: Mutex::new(Some(("provider-token".into(), Instant::now()))),
            base: base.to_string(),
        }
    }

    #[test]
    fn the_push_carries_a_fixed_line_and_none_of_the_notice() {
        let payload = wake_payload(&notice()).to_string();
        assert!(payload.contains(WAKE_BODY));
        assert!(!payload.contains("Daily Briefing"), "{payload}");
        assert!(!payload.contains("129 shillings"), "{payload}");
        assert!(payload.contains("\"notification_id\":\"briefing:phone-1\""));
        assert!(payload.contains("\"wake\":\"1\""));
    }

    #[test]
    fn a_long_id_is_cut_to_what_apple_takes() {
        let mut long = notice();
        long.id = "n".repeat(200);
        assert_eq!(collapse_id(&long).len(), COLLAPSE_ID_BYTES);
        assert_eq!(collapse_id(&notice()), "briefing:phone-1");
    }

    #[tokio::test]
    async fn only_an_iphones_token_is_sent_and_nothing_else_touches_the_network() {
        // Unroutable: reaching the network fails the test instead of calling Apple.
        let dead = "http://127.0.0.1:1";
        let none = relay(tokens(None).await, dead);
        assert_eq!(
            none.relay(&notice()).await.unwrap(),
            Wake::NotSent(WakeGap::NoToken)
        );
        let android = relay(tokens(Some(PushPlatform::Fcm)).await, dead);
        assert_eq!(
            android.relay(&notice()).await.unwrap(),
            Wake::NotSent(WakeGap::NoSender)
        );
    }

    #[tokio::test]
    async fn an_iphone_is_sent_an_alert_on_the_apps_topic() {
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/3/device/a1b2c3d4"))
            .and(header("authorization", "Bearer provider-token"))
            .and(header("apns-topic", "io.jarida.gooseonthego"))
            .and(header("apns-push-type", "alert"))
            .and(header("apns-priority", "10"))
            .and(header("apns-collapse-id", "briefing:phone-1"))
            .and(body_string_contains(WAKE_BODY))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let sender = relay(tokens(Some(PushPlatform::Apns)).await, &server.uri());
        assert_eq!(sender.relay(&notice()).await.unwrap(), Wake::Sent);
    }

    #[tokio::test]
    async fn a_refusal_names_apples_reason() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(410).set_body_json(json!({"reason": "Unregistered"})),
            )
            .mount(&server)
            .await;
        let sender = relay(tokens(Some(PushPlatform::Apns)).await, &server.uri());
        let refused = sender.relay(&notice()).await.unwrap_err().to_string();
        assert!(
            refused.contains("410") && refused.contains("Unregistered"),
            "{refused}"
        );
    }

    struct Fixed(Wake);
    #[async_trait]
    impl NotificationRelay for Fixed {
        async fn relay(&self, _: &Notification) -> Result<Wake> {
            Ok(self.0)
        }
    }

    #[tokio::test]
    async fn a_phone_is_woken_by_the_sender_its_token_belongs_to() {
        let other_kind: Arc<dyn NotificationRelay> =
            Arc::new(Fixed(Wake::NotSent(WakeGap::NoSender)));
        let sends: Arc<dyn NotificationRelay> = Arc::new(Fixed(Wake::Sent));
        let no_token: Arc<dyn NotificationRelay> = Arc::new(Fixed(Wake::NotSent(WakeGap::NoToken)));

        let both = PushRelays::new(vec![other_kind.clone(), sends]);
        assert_eq!(both.relay(&notice()).await.unwrap(), Wake::Sent);
        let only_other = PushRelays::new(vec![other_kind]);
        assert_eq!(
            only_other.relay(&notice()).await.unwrap(),
            Wake::NotSent(WakeGap::NoKey),
            "no key for that kind of phone"
        );
        let unregistered = PushRelays::new(vec![no_token]);
        assert_eq!(
            unregistered.relay(&notice()).await.unwrap(),
            Wake::NotSent(WakeGap::NoToken)
        );
    }
}
