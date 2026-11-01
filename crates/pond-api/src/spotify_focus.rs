//! Spotify as audio the pond pauses while it makes a sound. Spotify's Developer Policy III.7: "Do
//! not permit any device or system to segue, mix, re-mix, or overlap any Spotify Content with any
//! other audio content (including other Spotify Content)."
//!
//! Through the Web API, so whichever device is playing is paused (the Spotify app, a speaker, the
//! pond's own Spotify page) and resumed on it afterwards, unless someone has changed it since. It
//! gives nobody a way to control Spotify with their voice (Policy III.3): nothing said to the pond
//! plays, skips or stops Spotify, and what was playing comes back by itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use pond_core::models::ports::audio_focus::{AudioFocus, Paused};
use pond_core::security::ports::secret::SecretRepository;
use reqwest::{Method, StatusCode};
use serde_json::Value;

/// A call this slow is given up on; the sound it holds up waits less than that anyway.
const CALL_TIMEOUT: Duration = Duration::from_secs(4);

/// Episodes as well as tracks, so an episode playing has an `item` to compare on resume.
const TYPES: (&str, &str) = ("additional_types", "track,episode");

pub type Secrets = Arc<dyn SecretRepository + Send + Sync>;

pub struct SpotifyFocus {
    secrets: OnceLock<Secrets>,
    http: reqwest::Client,
    api: String,
    /// Only the server refreshes the token: a refresh can replace the refresh token, which then has
    /// to be stored, and only the server's store writes (the voice child reads a read-only one).
    may_refresh: bool,
    told_expired: AtomicBool,
    told_refused: AtomicBool,
}

impl SpotifyFocus {
    pub fn new(may_refresh: bool) -> Arc<Self> {
        Self::with_api("https://api.spotify.com/v1", may_refresh)
    }

    fn with_api(api: &str, may_refresh: bool) -> Arc<Self> {
        let http = pond_net::client_builder()
            .timeout(CALL_TIMEOUT)
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            secrets: OnceLock::new(),
            http,
            api: api.trim_end_matches('/').to_string(),
            may_refresh,
            told_expired: AtomicBool::new(false),
            told_refused: AtomicBool::new(false),
        })
    }

    /// Where the token is read from; nothing is paused before this. A setter because the server's
    /// store is made later in startup than its voice. The first call wins.
    pub fn use_secrets(&self, secrets: Secrets) {
        let _ = self.secrets.set(secrets);
    }

    /// One Web API call with the stored token, retried once with a fresh one after a 401 where this
    /// process may refresh. `None`: not signed in, refused by the network mode, or no answer.
    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
    ) -> Option<reqwest::Response> {
        let secrets = self.secrets.get()?;
        let providers = pond_core::user_data::services::oauth_providers::builtin_oauth_providers();
        let spotify = providers.iter().find(|p| p.id == "spotify")?;
        let token = secrets.get(&spotify.token_key).await.ok().flatten()?;

        let answer = self.send(method.clone(), path, query, &token).await?;
        if answer.status() != StatusCode::UNAUTHORIZED {
            return Some(answer);
        }
        if !self.may_refresh {
            if !self.told_expired.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "Spotify's token has expired, and only the pond's server refreshes it: this \
                     voice session cannot pause Spotify while GIAP speaks until the server has"
                );
            }
            return None;
        }
        let fresh = crate::routes::refresh_spotify_token(secrets.as_ref(), &self.http).await?;
        self.send(method, path, query, &fresh).await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        token: &str,
    ) -> Option<reqwest::Response> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.api)).ok()?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let label = if method == Method::GET { "GET" } else { "PUT" };
        // The gate logs its own refusals.
        let call = pond_core::shared::services::egress::begin_for(
            url.as_str(),
            label,
            pond_core::shared::services::egress::EgressPurpose::Music,
            "",
        )
        .ok()?;
        let sent = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .send()
            .await;
        call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
        sent.ok()
    }

    /// What Spotify says is playing now; `None` when nothing is active there (204) or no answer.
    async fn now_playing(&self) -> Option<Value> {
        let answer = self.call(Method::GET, "/me/player", &[TYPES]).await?;
        let status = answer.status();
        if status == StatusCode::NO_CONTENT {
            return None;
        }
        if !status.is_success() {
            if !self.told_refused.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    %status,
                    "Spotify would not say what is playing, so it cannot be paused while GIAP speaks"
                );
            }
            return None;
        }
        answer.json().await.ok()
    }
}

fn disallowed(now: &Value, action: &str) -> bool {
    now["actions"]["disallows"][action].as_bool() == Some(true)
}

#[async_trait]
impl AudioFocus for SpotifyFocus {
    async fn pause(&self) -> Option<Paused> {
        let now = self.now_playing().await?;
        if now["is_playing"].as_bool() != Some(true) {
            return None;
        }
        if disallowed(&now, "pausing") {
            tracing::info!("Spotify is playing and will not be paused right now");
            return None;
        }
        let device = now["device"]["id"].as_str().map(str::to_string);
        let query: Vec<(&str, &str)> = device.iter().map(|d| ("device_id", d.as_str())).collect();
        let answer = self.call(Method::PUT, "/me/player/pause", &query).await?;
        if !answer.status().is_success() {
            tracing::warn!(status = %answer.status(), "Spotify refused to pause while GIAP speaks");
            return None;
        }
        tracing::info!(
            target: "giap::trace",
            kind = "spotify_paused_for_speech",
            device = now["device"]["id"].as_str().unwrap_or(""),
            "paused Spotify while GIAP speaks (Spotify's Developer Policy III.7)"
        );
        Some(Paused {
            device,
            item: now["item"]["uri"].as_str().map(str::to_string),
        })
    }

    async fn resume(&self, paused: Paused) {
        let Some(now) = self.now_playing().await else {
            // Stopped while GIAP spoke, or Spotify could not be asked: either way it stays paused.
            tracing::info!(
                "did not resume Spotify after GIAP spoke: it has nothing active, or could not be asked"
            );
            return;
        };
        let as_left = now["is_playing"].as_bool() != Some(true)
            && now["device"]["id"].as_str() == paused.device.as_deref()
            && now["item"]["uri"].as_str() == paused.item.as_deref();
        if !as_left || disallowed(&now, "resuming") {
            tracing::debug!("Spotify changed while GIAP spoke; leaving it as it is");
            return;
        }
        let query: Vec<(&str, &str)> = paused
            .device
            .iter()
            .map(|d| ("device_id", d.as_str()))
            .collect();
        match self.call(Method::PUT, "/me/player/play", &query).await {
            Some(answer) if answer.status().is_success() => tracing::info!(
                target: "giap::trace",
                kind = "spotify_resumed_after_speech",
                "resumed Spotify after GIAP spoke"
            ),
            Some(answer) => {
                tracing::warn!(status = %answer.status(), "Spotify refused to resume after GIAP spoke")
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct Store(Mutex<HashMap<String, String>>);

    #[async_trait]
    impl SecretRepository for Store {
        async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        async fn delete(&self, key: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
        async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
        async fn has(&self, key: &str) -> anyhow::Result<bool> {
            Ok(self.0.lock().unwrap().contains_key(key))
        }
    }

    async fn spotify(signed_in: bool) -> (MockServer, Arc<SpotifyFocus>) {
        let server = MockServer::start().await;
        let focus = SpotifyFocus::with_api(&format!("{}/v1", server.uri()), false);
        let store = Store::default();
        if signed_in {
            store.set("SPOTIFY_ACCESS_TOKEN", "tok").await.unwrap();
        }
        focus.use_secrets(Arc::new(store));
        (server, focus)
    }

    fn player(playing: bool, device: &str, uri: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "is_playing": playing,
            "device": { "id": device, "name": "Kitchen", "type": "Speaker" },
            "item": { "uri": uri },
            "actions": { "disallows": {} },
        }))
    }

    async fn expect_pause_calls(server: &MockServer, n: u64) {
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/pause"))
            .respond_with(ResponseTemplate::new(204))
            .expect(n)
            .mount(server)
            .await;
    }

    async fn expect_play_calls(server: &MockServer, n: u64) {
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/play"))
            .respond_with(ResponseTemplate::new(204))
            .expect(n)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn it_pauses_what_is_playing_on_the_device_playing_it() {
        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .and(query_param("additional_types", "track,episode"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(player(true, "dev-1", "spotify:track:1"))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/pause"))
            .and(query_param("device_id", "dev-1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            focus.pause().await,
            Some(Paused {
                device: Some("dev-1".into()),
                item: Some("spotify:track:1".into()),
            })
        );
    }

    #[tokio::test]
    async fn nothing_playing_or_nothing_active_is_left_alone() {
        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(player(false, "dev-1", "spotify:track:1"))
            .mount(&server)
            .await;
        expect_pause_calls(&server, 0).await;
        assert_eq!(focus.pause().await, None);

        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        expect_pause_calls(&server, 0).await;
        assert_eq!(focus.pause().await, None);
    }

    #[tokio::test]
    async fn a_player_that_disallows_pausing_is_left_playing() {
        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "is_playing": true,
                "device": { "id": "dev-1" },
                "actions": { "disallows": { "pausing": true } },
            })))
            .mount(&server)
            .await;
        expect_pause_calls(&server, 0).await;
        assert_eq!(focus.pause().await, None);
    }

    #[tokio::test]
    async fn not_signed_in_asks_spotify_nothing() {
        let (server, focus) = spotify(false).await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        assert_eq!(focus.pause().await, None);
    }

    fn was_playing() -> Paused {
        Paused {
            device: Some("dev-1".into()),
            item: Some("spotify:track:1".into()),
        }
    }

    #[tokio::test]
    async fn it_resumes_what_it_paused_where_it_was() {
        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(player(false, "dev-1", "spotify:track:1"))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/play"))
            .and(query_param("device_id", "dev-1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        focus.resume(was_playing()).await;
    }

    #[tokio::test]
    async fn it_leaves_alone_what_someone_changed_while_the_pond_spoke() {
        for (playing, device, uri) in [
            (true, "dev-1", "spotify:track:1"),  // already resumed by hand
            (false, "dev-1", "spotify:track:2"), // another track chosen
            (false, "dev-2", "spotify:track:1"), // moved to another device
        ] {
            let (server, focus) = spotify(true).await;
            Mock::given(method("GET"))
                .and(path("/v1/me/player"))
                .respond_with(player(playing, device, uri))
                .mount(&server)
                .await;
            expect_play_calls(&server, 0).await;
            focus.resume(was_playing()).await;
        }

        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        expect_play_calls(&server, 0).await;
        focus.resume(was_playing()).await;
    }

    #[tokio::test]
    async fn a_device_spotify_does_not_name_is_paused_and_resumed_without_one() {
        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "is_playing": true,
                "device": { "id": null },
                "item": null,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/pause"))
            .and(query_param_is_missing("device_id"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let paused = focus.pause().await.expect("it was playing");
        assert_eq!(
            paused,
            Paused {
                device: None,
                item: None
            }
        );

        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "is_playing": false,
                "device": { "id": null },
                "item": null,
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/play"))
            .and(query_param_is_missing("device_id"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        focus.resume(paused).await;
    }

    /// The whole chain as the pond wires it (the controller, the voice around the TTS, this), with
    /// Spotify's Web API played by a mock: the pause goes out before anything is said, and the resume
    /// only once the turn is over.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_turn_is_said_with_spotify_paused_and_spotify_comes_back_after() {
        use pond_core::models::ports::voice_output::VoiceOutput;
        use pond_core::models::services::voice::quiet::Quiet;
        use pond_core::models::services::voice::quiet_voice_output::QuietVoiceOutput;

        let (server, focus) = spotify(true).await;
        let server = Arc::new(server);
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(player(true, "dev-1", "spotify:track:1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(player(false, "dev-1", "spotify:track:1"))
            .mount(&server)
            .await;
        expect_pause_calls(&server, 1).await;
        expect_play_calls(&server, 1).await;

        /// Says nothing aloud; notes what Spotify had been asked by the time it would have.
        struct Voice {
            spotify: Arc<MockServer>,
            heard_before_speaking: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl VoiceOutput for Voice {
            async fn speak(&self, _text: &str) -> anyhow::Result<()> {
                let asked: Vec<String> = self
                    .spotify
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .map(|r| format!("{} {}", r.method, r.url.path()))
                    .collect();
                self.heard_before_speaking.lock().unwrap().extend(asked);
                Ok(())
            }
        }

        let grace = Duration::from_millis(100);
        let inner = Arc::new(Voice {
            spotify: server.clone(),
            heard_before_speaking: Mutex::new(Vec::new()),
        });
        let voice = QuietVoiceOutput::new(inner.clone(), Quiet::start(focus, grace));

        voice.begin_utterance();
        voice.speak("It is twelve degrees.").await.unwrap();
        voice.speak("Rain later.").await.unwrap();
        assert!(
            inner
                .heard_before_speaking
                .lock()
                .unwrap()
                .iter()
                .any(|r| r == "PUT /v1/me/player/pause"),
            "spoke before Spotify was paused"
        );
        tokio::time::sleep(grace * 3).await;
        let asked = server.received_requests().await.unwrap();
        assert!(
            !asked.iter().any(|r| r.url.path() == "/v1/me/player/play"),
            "resumed while the turn was still going"
        );

        voice.end_utterance();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == "/v1/me/player/play")
        {
            assert!(std::time::Instant::now() < deadline, "never resumed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_process_that_may_not_refresh_gives_up_on_an_expired_token() {
        let (server, focus) = spotify(true).await;
        Mock::given(method("GET"))
            .and(path("/v1/me/player"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        expect_pause_calls(&server, 0).await;
        // A refresh would go to accounts.spotify.com: none is attempted, so none leaves the machine.
        assert_eq!(focus.pause().await, None);
    }
}
