//! Members' Uber accounts: connecting through Jarida's sign-in relay, keeping each member's tokens
//! in the pond's secret store, and renewing them before they lapse. The relay adds the app's client
//! secret; the tokens themselves are kept only here.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::security::ports::secret::SecretRepository;
use serde::Deserialize;
use serde_json::json;

use crate::UberAccessTokens;
use pond_core::rides::ports::RideAccounts;

/// The scopes a member grants: book and follow rides, show which account is connected, and keep
/// the connection without signing in again. Only what is used: a privileged scope needs Uber's
/// approval and widens what a leaked token can do.
pub const SCOPES: &[&str] = &["request", "profile", "offline_access"];

/// Renew this long before the access token lapses, so a booking never starts with a dying token.
const RENEW_MARGIN: chrono::Duration = chrono::Duration::minutes(10);
/// Longer than the relay waits for Uber (15 s), so the pond never gives up on an exchange the relay
/// can still finish: Uber's code is single use, and an answer nobody waits for loses it.
const RELAY_TIMEOUT: Duration = Duration::from_secs(25);
/// What relay calls are filed under in the egress log, the same as the music-token fetch. No chat
/// turn makes them, so none may be filed under the turn in flight.
const EGRESS_TOOL: pond_core::shared::services::egress::EgressPurpose =
    pond_core::shared::services::egress::EgressPurpose::Credentials;

/// Every key a member's Uber tokens are kept under starts with this. The pond's generic secrets
/// API refuses such keys, so only the host-only Uber routes can read, change or forget them.
pub const SECRET_KEY_PREFIX: &str = "UBER_";

const ACCESS_KEY: &str = "UBER_ACCESS_TOKEN:";
const REFRESH_KEY: &str = "UBER_REFRESH_TOKEN:";
const EXPIRES_KEY: &str = "UBER_TOKEN_EXPIRES_AT:";

fn key(prefix: &str, profile_id: &str) -> String {
    format!("{prefix}{profile_id}")
}

/// Whether `key` is one a member's Uber tokens are kept under, in any letter case.
pub fn is_member_token_key(key: &str) -> bool {
    key.get(..SECRET_KEY_PREFIX.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(SECRET_KEY_PREFIX))
}

/// Jarida's credentials service, which holds the Uber app's client secret.
pub struct SignInRelay {
    http: reqwest::Client,
    base_url: String,
}

/// What Uber returns for a sign-in or a renewal, as the relay passes it on.
/// No `Debug`: it holds the member's tokens.
#[derive(Deserialize)]
struct TokenSet {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct RelayRefusal {
    uber_error: Option<String>,
    error: Option<String>,
}

/// Why the credentials service gave no tokens. Only [`RelayError::Refused`] says anything about the
/// member's sign-in; the other leaves a kept connection standing.
#[derive(Debug, PartialEq)]
pub enum RelayError {
    /// Uber refused the code or refresh token itself, with its OAuth error code (`invalid_grant`).
    Refused(String),
    /// Nothing was learned about the sign-in: the service or Uber could not be reached, was busy or
    /// failing, or answered with something unreadable. Worth trying again later.
    Unavailable(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::Refused(code) => write!(f, "Uber refused the sign-in: {code}"),
            RelayError::Unavailable(why) => write!(f, "Uber sign-in could not finish: {why}"),
        }
    }
}

impl std::error::Error for RelayError {}

impl SignInRelay {
    /// `base_url` is the credentials service's address (`POND_CREDENTIALS_URL`).
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// The Uber app's client ID. Public; Uber's sign-in page needs it.
    pub async fn client_id(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Client {
            client_id: String,
        }
        let url = format!("{}/v1/uber/client", self.base_url);
        let response = self.send(self.http.get(&url), &url, "GET").await?;
        let client: Client = response.json().await.map_err(|_| unreadable())?;
        Ok(client.client_id)
    }

    async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<TokenSet> {
        self.tokens(
            "/v1/uber/token",
            json!({"code": code, "redirect_uri": redirect_uri}),
        )
        .await
    }

    async fn renew(&self, refresh_token: &str) -> Result<TokenSet> {
        self.tokens("/v1/uber/refresh", json!({"refresh_token": refresh_token}))
            .await
    }

    async fn tokens(&self, path: &str, body: serde_json::Value) -> Result<TokenSet> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .send(self.http.post(&url).json(&body), &url, "POST")
            .await?;
        Ok(response.json().await.map_err(|_| unreadable())?)
    }

    /// Gated by the egress guard and filed under the credentials service. A failure is a
    /// [`RelayError`]: a refusal only when the service passes on Uber's own refusal code.
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        url: &str,
        method: &'static str,
    ) -> Result<reqwest::Response> {
        let call = pond_core::shared::services::egress::begin_for(url, method, EGRESS_TOOL, "")
            .map_err(|denied| anyhow!("{denied}"))?;
        let sent = builder.timeout(RELAY_TIMEOUT).send().await;
        call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
        let response = sent.map_err(|_| {
            RelayError::Unavailable("Jarida's credentials service could not be reached".into())
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let reply: Option<RelayRefusal> = response.json().await.ok();
        Err(match reply {
            Some(RelayRefusal {
                uber_error: Some(code),
                ..
            }) => RelayError::Refused(code),
            reply => RelayError::Unavailable(
                reply
                    .and_then(|r| r.error)
                    .unwrap_or_else(|| format!("the credentials service answered {status}")),
            ),
        }
        .into())
    }
}

fn unreadable() -> RelayError {
    RelayError::Unavailable("the credentials service's answer was not readable".into())
}

/// Why a sign-in cannot start or renew on a pond whose credentials service is turned off.
pub const SIGN_IN_OFF: &str = "Uber sign-in needs Jarida's credentials service, which is turned \
                               off on this pond (POND_CREDENTIALS_URL)";

/// A finished code exchange whose tokens are not kept yet: see [`UberAccounts::keep`].
pub struct SignedIn {
    tokens: TokenSet,
    refresh_token: String,
}

/// Held across every write of a member's tokens and every forget, for the whole process: each
/// request builds its own `UberAccounts` and ride booking keeps another, so a lock in one instance
/// would not see the others. A renewal checks and writes under it, so a disconnect can never land
/// between the two.
static TOKENS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Each member's Uber connection, stored in the pond's secret store.
pub struct UberAccounts {
    secrets: Arc<dyn SecretRepository>,
    /// `None` with the credentials service turned off: no sign-in can start or renew, but the
    /// ones kept can still be listed and forgotten.
    relay: Option<SignInRelay>,
    renewing: tokio::sync::Mutex<()>,
}

impl UberAccounts {
    pub fn new(secrets: Arc<dyn SecretRepository>, relay: Option<SignInRelay>) -> Self {
        Self {
            secrets,
            relay,
            renewing: tokio::sync::Mutex::new(()),
        }
    }

    /// The credentials service, unless it is turned off on this pond.
    pub fn relay(&self) -> Option<&SignInRelay> {
        self.relay.as_ref()
    }

    fn require_relay(&self) -> Result<&SignInRelay> {
        self.relay.as_ref().context(SIGN_IN_OFF)
    }

    /// Exchange Uber's sign-in code through the relay. Nothing is kept until [`Self::keep`], so the
    /// caller can check in between that the member is still in the household.
    pub async fn exchange(&self, code: &str, redirect_uri: &str) -> Result<SignedIn> {
        let tokens = self
            .require_relay()?
            .exchange_code(code, redirect_uri)
            .await?;
        let refresh_token = tokens
            .refresh_token
            .clone()
            .context("Uber gave no refresh token; was offline_access granted?")?;
        Ok(SignedIn {
            tokens,
            refresh_token,
        })
    }

    /// Keep a member's tokens from a finished sign-in.
    pub async fn keep(
        &self,
        profile_id: &str,
        signed_in: SignedIn,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let _tokens = TOKENS.lock().await;
        self.store(
            profile_id,
            &signed_in.tokens.access_token,
            &signed_in.refresh_token,
            expiry(&signed_in.tokens, now),
        )
        .await
    }

    /// Forget a member's Uber connection; true when they had one. Their Uber account itself is
    /// untouched.
    pub async fn disconnect(&self, profile_id: &str) -> Result<bool> {
        let _tokens = TOKENS.lock().await;
        let had = self.get(REFRESH_KEY, profile_id).await?.is_some();
        for prefix in [ACCESS_KEY, REFRESH_KEY, EXPIRES_KEY] {
            self.secrets.delete(&key(prefix, profile_id)).await?;
        }
        Ok(had)
    }

    /// The members who have connected Uber, by profile id.
    pub async fn connected_members(&self) -> Result<Vec<String>> {
        let mut members: Vec<String> = self
            .secrets
            .list_keys()
            .await?
            .into_iter()
            .filter_map(|k| k.strip_prefix(REFRESH_KEY).map(str::to_string))
            .collect();
        members.sort();
        Ok(members)
    }

    /// The member's access token, renewed first when it is about to lapse.
    pub async fn access_token_at(&self, profile_id: &str, now: DateTime<Utc>) -> Result<String> {
        if let Some(token) = self.fresh_access_token(profile_id, now).await? {
            return Ok(token);
        }
        let _one_at_a_time = self.renewing.lock().await;
        // Another caller may have renewed while this one waited.
        if let Some(token) = self.fresh_access_token(profile_id, now).await? {
            return Ok(token);
        }
        let refresh = self
            .get(REFRESH_KEY, profile_id)
            .await?
            .ok_or_else(|| anyhow!("this member has not connected Uber"))?;
        let tokens = self.require_relay()?.renew(&refresh).await?;
        // A disconnect, or the member's removal, while the relay answered wins: never write back
        // tokens that were forgotten in the meantime. Checked and written under one lock, which
        // a disconnect also takes, so none can land in between.
        let _tokens = TOKENS.lock().await;
        if self.get(REFRESH_KEY, profile_id).await?.as_deref() != Some(refresh.as_str()) {
            bail!("this member's Uber connection was removed or replaced while it was renewed");
        }
        // Uber may or may not rotate the refresh token; keep the old one when it doesn't.
        let next_refresh = tokens.refresh_token.clone().unwrap_or(refresh);
        self.store(
            profile_id,
            &tokens.access_token,
            &next_refresh,
            expiry(&tokens, now),
        )
        .await?;
        Ok(tokens.access_token)
    }

    async fn fresh_access_token(
        &self,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>> {
        let Some(token) = self.get(ACCESS_KEY, profile_id).await? else {
            return Ok(None);
        };
        let expires_at = self
            .get(EXPIRES_KEY, profile_id)
            .await?
            .and_then(|v| v.parse::<i64>().ok())
            .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0));
        Ok(match expires_at {
            Some(at) if at - RENEW_MARGIN > now => Some(token),
            _ => None,
        })
    }

    async fn store(
        &self,
        profile_id: &str,
        access: &str,
        refresh: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.secrets
            .set(&key(ACCESS_KEY, profile_id), access)
            .await?;
        self.secrets
            .set(&key(REFRESH_KEY, profile_id), refresh)
            .await?;
        self.secrets
            .set(
                &key(EXPIRES_KEY, profile_id),
                &expires_at.timestamp().to_string(),
            )
            .await
    }

    async fn get(&self, prefix: &str, profile_id: &str) -> Result<Option<String>> {
        Ok(self
            .secrets
            .get(&key(prefix, profile_id))
            .await?
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()))
    }
}

/// When the access token lapses. Without `expires_in`, treat it as already lapsing so the next
/// use renews it rather than trusting a token of unknown life.
fn expiry(tokens: &TokenSet, now: DateTime<Utc>) -> DateTime<Utc> {
    tokens
        .expires_in
        .map(|secs| now + chrono::Duration::seconds(secs))
        .unwrap_or(now)
}

/// Uber's sign-in page for one attempt, on `sign_in` ([`crate::UberConfig::authorize_url`]).
/// `state` ties Uber's return to this attempt.
pub fn authorize_url(sign_in: &str, client_id: &str, redirect_uri: &str, state: &str) -> String {
    let scope = SCOPES.join(" ");
    format!(
        "{sign_in}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}",
        encode(client_id),
        encode(redirect_uri),
        encode(&scope),
        encode(state),
    )
}

fn encode(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

#[async_trait]
impl RideAccounts for UberAccounts {
    async fn is_connected(&self, profile_id: &str) -> Result<bool> {
        Ok(self.get(REFRESH_KEY, profile_id).await?.is_some())
    }
}

#[async_trait]
impl UberAccessTokens for UberAccounts {
    async fn access_token(&self, profile_id: &str) -> Result<String> {
        self.access_token_at(profile_id, Utc::now()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct MemorySecrets(Mutex<HashMap<String, String>>);

    #[async_trait]
    impl SecretRepository for MemorySecrets {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set(&self, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn delete(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
        async fn has(&self, key: &str) -> Result<bool> {
            Ok(self.0.lock().unwrap().contains_key(key))
        }
    }

    const CALLBACK: &str = "http://127.0.0.1:4000/api/v1/oauth/callback";

    fn now() -> DateTime<Utc> {
        "2026-10-06T08:00:00Z".parse().unwrap()
    }

    fn accounts(server: &MockServer) -> (UberAccounts, Arc<MemorySecrets>) {
        let secrets = Arc::new(MemorySecrets::default());
        let accounts = UberAccounts::new(
            secrets.clone(),
            Some(SignInRelay::new(reqwest::Client::new(), &server.uri())),
        );
        (accounts, secrets)
    }

    impl UberAccounts {
        /// A whole sign-in, for tests that are not about the gap between exchange and keep.
        async fn connect(&self, profile_id: &str, code: &str) -> Result<()> {
            let signed_in = self.exchange(code, CALLBACK).await?;
            self.keep(profile_id, signed_in, now()).await
        }
    }

    async fn relay_returns(server: &MockServer, route: &str, body: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn connecting_keeps_the_members_tokens_and_lists_them() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/token"))
            .and(body_partial_json(
                json!({"code": "c-1", "redirect_uri": CALLBACK}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);

        accounts.connect("liz", "c-1").await.unwrap();
        assert_eq!(
            secrets
                .get("UBER_REFRESH_TOKEN:liz")
                .await
                .unwrap()
                .as_deref(),
            Some("r-1")
        );
        assert_eq!(
            accounts.connected_members().await.unwrap(),
            vec!["liz".to_string()]
        );
        assert_eq!(accounts.access_token_at("liz", now()).await.unwrap(), "a-1");
    }

    #[tokio::test]
    async fn a_lapsing_token_is_renewed_and_a_rotated_refresh_token_kept() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 3600
            }),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/refresh"))
            .and(body_partial_json(json!({"refresh_token": "r-1"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a-2", "refresh_token": "r-2", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);
        accounts.connect("liz", "c-1").await.unwrap();

        // 55 minutes on, inside the renewal margin.
        let later = now() + chrono::Duration::minutes(55);
        assert_eq!(accounts.access_token_at("liz", later).await.unwrap(), "a-2");
        assert_eq!(
            secrets
                .get("UBER_REFRESH_TOKEN:liz")
                .await
                .unwrap()
                .as_deref(),
            Some("r-2")
        );
        // Fresh again: no second renewal (the mock expects exactly one).
        assert_eq!(accounts.access_token_at("liz", later).await.unwrap(), "a-2");
    }

    #[tokio::test]
    async fn a_renewal_without_a_new_refresh_token_keeps_the_old_one() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 60
            }),
        )
        .await;
        relay_returns(
            &server,
            "/v1/uber/refresh",
            json!({
                "access_token": "a-2", "expires_in": 3600
            }),
        )
        .await;
        let (accounts, secrets) = accounts(&server);
        accounts.connect("liz", "c-1").await.unwrap();
        assert_eq!(accounts.access_token_at("liz", now()).await.unwrap(), "a-2");
        assert_eq!(
            secrets
                .get("UBER_REFRESH_TOKEN:liz")
                .await
                .unwrap()
                .as_deref(),
            Some("r-1")
        );
    }

    #[tokio::test]
    async fn members_are_kept_apart_and_disconnecting_forgets_only_that_member() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a", "refresh_token": "r", "expires_in": 3600
            }),
        )
        .await;
        let (accounts, _) = accounts(&server);
        accounts.connect("liz", "c").await.unwrap();
        accounts.connect("jerry", "c").await.unwrap();

        assert!(accounts.is_connected("liz").await.unwrap());
        assert!(accounts.disconnect("liz").await.unwrap());
        assert!(!accounts.is_connected("liz").await.unwrap());
        assert!(!accounts.disconnect("liz").await.unwrap(), "nothing left");
        assert_eq!(
            accounts.connected_members().await.unwrap(),
            vec!["jerry".to_string()]
        );
        assert!(accounts.access_token_at("liz", now()).await.is_err());
        assert!(accounts.access_token_at("jerry", now()).await.is_ok());
    }

    #[tokio::test]
    async fn with_the_relay_off_sign_ins_are_still_listed_and_forgotten() {
        let secrets = Arc::new(MemorySecrets::default());
        for prefix in [ACCESS_KEY, REFRESH_KEY, EXPIRES_KEY] {
            secrets.set(&key(prefix, "liz"), "1").await.unwrap();
        }
        let accounts = UberAccounts::new(secrets.clone(), None);

        assert_eq!(
            accounts.connected_members().await.unwrap(),
            vec!["liz".to_string()]
        );
        assert!(accounts.disconnect("liz").await.unwrap());
        assert!(secrets.list_keys().await.unwrap().is_empty());

        let err = accounts.exchange("c", CALLBACK).await.err().unwrap();
        assert!(format!("{err:#}").contains("turned off"), "{err:#}");
    }

    #[tokio::test]
    async fn a_disconnect_while_a_renewal_is_out_is_not_undone() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 60
            }),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/refresh"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "access_token": "a-2", "refresh_token": "r-2", "expires_in": 3600
                    }))
                    .set_delay(std::time::Duration::from_millis(500)),
            )
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);
        accounts.connect("liz", "c-1").await.unwrap();
        let accounts = Arc::new(accounts);

        let renewing = tokio::spawn({
            let accounts = accounts.clone();
            async move { accounts.access_token_at("liz", now()).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        accounts.disconnect("liz").await.unwrap();

        assert!(renewing.await.unwrap().is_err());
        assert!(secrets.list_keys().await.unwrap().is_empty());
    }

    /// Pauses the first write of an access token, armed, until told to go on.
    #[derive(Default)]
    struct PausingSecrets {
        inner: MemorySecrets,
        armed: std::sync::atomic::AtomicBool,
        reached: tokio::sync::Notify,
        resume: tokio::sync::Notify,
    }

    #[async_trait]
    impl SecretRepository for PausingSecrets {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            self.inner.get(key).await
        }
        async fn set(&self, key: &str, value: &str) -> Result<()> {
            if key.starts_with(ACCESS_KEY)
                && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.reached.notify_one();
                self.resume.notified().await;
            }
            self.inner.set(key, value).await
        }
        async fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key).await
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            self.inner.list_keys().await
        }
        async fn has(&self, key: &str) -> Result<bool> {
            self.inner.has(key).await
        }
    }

    /// The gap the check above cannot close alone: a disconnect after the check, while the renewed
    /// tokens are being written. It must wait for the write and then forget all of it.
    #[tokio::test]
    async fn a_disconnect_cannot_land_between_a_renewals_check_and_its_write() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({ "access_token": "a-1", "refresh_token": "r-1", "expires_in": 60 }),
        )
        .await;
        relay_returns(
            &server,
            "/v1/uber/refresh",
            json!({ "access_token": "a-2", "refresh_token": "r-2", "expires_in": 3600 }),
        )
        .await;
        let secrets = Arc::new(PausingSecrets::default());
        let accounts = Arc::new(UberAccounts::new(
            secrets.clone(),
            Some(SignInRelay::new(reqwest::Client::new(), &server.uri())),
        ));
        accounts.connect("liz", "c-1").await.unwrap();

        secrets
            .armed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let renewing = tokio::spawn({
            let accounts = accounts.clone();
            async move { accounts.access_token_at("liz", now()).await }
        });
        secrets.reached.notified().await;
        let disconnecting = tokio::spawn({
            let accounts = accounts.clone();
            async move { accounts.disconnect("liz").await }
        });
        // Room for the disconnect to run now, if anything let it.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        secrets.resume.notify_one();

        assert_eq!(renewing.await.unwrap().unwrap(), "a-2");
        assert!(
            disconnecting.await.unwrap().unwrap(),
            "it forgot the renewed sign-in"
        );
        assert!(
            secrets.list_keys().await.unwrap().is_empty(),
            "nothing written back after the disconnect"
        );
    }

    #[tokio::test]
    async fn ubers_refusal_is_named_and_nothing_is_stored() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/uber/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "Uber refused the sign-in.", "uber_error": "invalid_grant"
            })))
            .mount(&server)
            .await;
        let (accounts, secrets) = accounts(&server);
        let err = accounts.connect("liz", "used").await.unwrap_err();
        assert_eq!(
            err.downcast_ref::<RelayError>(),
            Some(&RelayError::Refused("invalid_grant".into())),
            "{err:#}"
        );
        assert!(secrets.list_keys().await.unwrap().is_empty());
    }

    /// The relay's 502 says Uber did not answer, not that it turned the member down.
    #[tokio::test]
    async fn a_relay_that_cannot_reach_uber_is_not_a_revoked_sign_in() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({
                "access_token": "a-1", "refresh_token": "r-1", "expires_in": 60
            }),
        )
        .await;
        let (accounts, secrets) = accounts(&server);
        accounts.connect("liz", "c-1").await.unwrap();
        for (status, body) in [
            (
                502,
                json!({"error": "Uber is not answering right now. Try again later."}),
            ),
            (
                503,
                json!({"error": "This service does not relay Uber sign-ins."}),
            ),
            (429, json!({"error": "Too many requests. Try again later."})),
        ] {
            server.reset().await;
            Mock::given(method("POST"))
                .and(path("/v1/uber/refresh"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .mount(&server)
                .await;

            let err = accounts.access_token_at("liz", now()).await.unwrap_err();
            assert!(
                matches!(
                    err.downcast_ref::<RelayError>(),
                    Some(RelayError::Unavailable(_))
                ),
                "{status}: {err:#}"
            );
            assert!(!format!("{err:#}").contains("refused"), "{err:#}");
            assert!(accounts.is_connected("liz").await.unwrap());
            assert_eq!(
                secrets
                    .get("UBER_REFRESH_TOKEN:liz")
                    .await
                    .unwrap()
                    .as_deref(),
                Some("r-1")
            );
        }
    }

    #[tokio::test]
    async fn a_sign_in_without_offline_access_is_refused() {
        let server = MockServer::start().await;
        relay_returns(
            &server,
            "/v1/uber/token",
            json!({"access_token": "a", "expires_in": 3600}),
        )
        .await;
        let (accounts, secrets) = accounts(&server);
        assert!(accounts.connect("liz", "c").await.is_err());
        assert!(secrets.list_keys().await.unwrap().is_empty());
    }

    #[test]
    fn every_key_a_members_tokens_live_under_is_reserved() {
        for prefix in [ACCESS_KEY, REFRESH_KEY, EXPIRES_KEY] {
            assert!(is_member_token_key(&key(prefix, "liz")), "{prefix}");
        }
        assert!(is_member_token_key("uber_refresh_token:liz"));
        assert!(!is_member_token_key("SPOTIFY_ACCESS_TOKEN"));
        assert!(!is_member_token_key("UBER"));
        assert!(!is_member_token_key(""));
    }

    #[test]
    fn the_sign_in_page_asks_for_exactly_the_scopes_phase_2_uses() {
        let url = authorize_url(
            crate::PRODUCTION_AUTHORIZE_URL,
            "client-1",
            CALLBACK,
            "nonce-1",
        );
        assert!(url.starts_with("https://auth.uber.com/oauth/v2/authorize?client_id=client-1"));
        assert!(url.contains("&response_type=code"));
        assert!(
            url.contains("&scope=request%20profile%20offline_access&"),
            "{url}"
        );
        assert!(url
            .contains("&redirect_uri=http%3A%2F%2F127.0.0.1%3A4000%2Fapi%2Fv1%2Foauth%2Fcallback"));
        assert!(url.ends_with("&state=nonce-1"));
    }
}
