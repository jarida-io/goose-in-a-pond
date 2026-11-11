//! A member's news over HTTP: their own settings, stories and digests only, the tiles their
//! interests feed, reading history and feedback, and why there is no news when there cannot be.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, NaiveDate, Utc};
use pond_api::build_router;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::domain::news::{
    Digest, DigestPicture, DigestSection, InterestRef, Story, StoryKind,
};
use pond_core::shared::ports::news::NewsStore;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_news::SqliteNewsStore;
use serde_json::{json, Value};
use tower::ServiceExt;

struct TwoPhones {
    amina: String,
    otieno: String,
}

#[async_trait]
impl Handshake for TwoPhones {
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used by this fixture")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(matches!(token, "amina-token" | "otieno-token"))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let device = match token {
            "amina-token" => &self.amina,
            "otieno-token" => &self.otieno,
            _ => return Ok(None),
        };
        Ok(Some(TokenCaller {
            client_id: device.clone(),
            device_id: device.clone(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used by this fixture")
    }
}

/// A Brave key, so the news is not blocked on web search.
#[derive(Default)]
struct OneKey;

#[async_trait]
impl SecretRepository for OneKey {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok((key == "BRAVE_SEARCH_API_KEY").then(|| "brave-test-key".to_string()))
    }
    async fn set(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn list_keys(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn has(&self, key: &str) -> Result<bool> {
        Ok(key == "BRAVE_SEARCH_API_KEY")
    }
}

struct Pond {
    router: axum::Router,
    amina: String,
    news: SqliteNewsStore,
    pool: sqlx::Pool<sqlx::Sqlite>,
    _dir: tempfile::TempDir,
}

async fn pond(with_key: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    state.skip_onboarding = true;
    if with_key {
        state.secret_repo = Some(Arc::new(OneKey));
    }
    let mut phones = Vec::new();
    let mut members = Vec::new();
    for name in ["Amina", "Otieno"] {
        let member = state
            .profile_repo
            .create(CreateProfileRequest {
                display_name: name.into(),
                avatar_emoji: "duck".into(),
            })
            .await
            .unwrap()
            .id;
        let device = state
            .device_registry
            .register(RegisterDeviceRequest {
                id: None,
                name: format!("{name}'s phone"),
                device_type: "gotg".into(),
                hostname: None,
                capabilities: vec![],
                room_id: None,
            })
            .await
            .unwrap()
            .id;
        SqliteDeviceAttribution::new(pool.clone())
            .set_device_profile(&device, Some(&member))
            .await
            .unwrap();
        phones.push(device);
        members.push(member);
    }
    state.handshake = Arc::new(TwoPhones {
        amina: phones[0].clone(),
        otieno: phones[1].clone(),
    });
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        amina: members[0].clone(),
        news: SqliteNewsStore::new(pool.clone()),
        pool,
        _dir: built.dir,
    }
}

async fn call(
    pond: &Pond,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"));
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = pond
        .router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn interest(pond: &Pond, id: &str, topic: &str, category: &str, news: &str) {
    sqlx::query(
        "INSERT INTO interests (id, profile_id, topic, topic_key, category, facets, depth, news, \
         avoid, origin, evidence, confirmed, created_at, updated_at) \
         VALUES (?, ?, ?, lower(?), ?, '[]', 'keen', ?, 0, 'you', 1, 1, datetime('now'), datetime('now'))",
    )
    .bind(id)
    .bind(&pond.amina)
    .bind(topic)
    .bind(topic)
    .bind(category)
    .bind(news)
    .execute(&pond.pool)
    .await
    .unwrap();
}

fn story(id: &str, member: &str, interest: &str, minutes_ago: i64) -> Story {
    Story {
        id: id.into(),
        profile_id: member.into(),
        kind: StoryKind::News,
        interest: InterestRef {
            id: interest.into(),
            topic: interest.into(),
        },
        headline: format!("Headline {id}"),
        dek: "Dek.".into(),
        summary: "Summary in Goose's words.".into(),
        why_you: "You follow this.".into(),
        url: format!("https://news.example/{id}"),
        site: "Nation".into(),
        source_title: "The page's own title".into(),
        further_sources: vec![],
        picture_key: Some("abcdef0123456789".into()),
        found_by: Some("query".into()),
        published_at: None,
        written_at: Utc::now() - Duration::minutes(minutes_ago),
        read_at: None,
        vote: None,
    }
}

#[tokio::test]
async fn settings_start_at_the_defaults_and_change_only_for_their_member() {
    let pond = pond(false).await;
    let (status, body) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news/settings",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"digest": true, "digest_time": "07:00", "spoken_on_speaker": false, "tiles": true,
               "timezone": body["timezone"]})
    );
    let household = body["timezone"].clone();
    let (status, body) = call(
        &pond,
        "amina-token",
        Method::PUT,
        "/api/v1/me/news/settings",
        Some(
            json!({"digest_time": "6:30", "spoken_on_speaker": true, "timezone": "Africa/Nairobi"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["digest_time"], "06:30");
    assert_eq!(body["spoken_on_speaker"], true);
    assert_eq!(body["timezone"], "Africa/Nairobi");
    let (_, body) = call(
        &pond,
        "amina-token",
        Method::PUT,
        "/api/v1/me/news/settings",
        Some(json!({"timezone": null})),
    )
    .await;
    assert_eq!(body["timezone"], household);
    assert_eq!(body["digest_time"], "06:30");
    let (status, _) = call(
        &pond,
        "amina-token",
        Method::PUT,
        "/api/v1/me/news/settings",
        Some(json!({"digest_time": "seven"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &pond,
        "amina-token",
        Method::PUT,
        "/api/v1/me/news/settings",
        Some(json!({"timezone": "Mars/Olympus"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, theirs) = call(
        &pond,
        "otieno-token",
        Method::GET,
        "/api/v1/me/news/settings",
        None,
    )
    .await;
    assert_eq!(theirs["digest_time"], "07:00");
}

#[tokio::test]
async fn no_interests_or_no_search_is_said_and_a_refresh_waits_for_neither() {
    let bare = pond(false).await;
    let (_, body) = call(&bare, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    assert_eq!(body["blocked"], "no_interests");
    assert_eq!(body["stories"], json!([]));
    assert_eq!(body["digest"], Value::Null);
    assert_eq!(body["state"], "idle");
    let (status, body) = call(
        &bare,
        "amina-token",
        Method::POST,
        "/api/v1/me/news/refresh",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, json!({"state": "idle", "blocked": "no_interests"}));
    interest(&bare, "mcp", "MCP", "tech", "both").await;
    let (_, body) = call(&bare, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    assert_eq!(body["blocked"], "web_search");

    let keyed = pond(true).await;
    interest(&keyed, "mcp", "MCP", "tech", "both").await;
    let (_, body) = call(&keyed, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    assert_eq!(body["blocked"], Value::Null);
    assert!(body["next_digest_at"].is_string());
    let (status, body) = call(
        &keyed,
        "amina-token",
        Method::POST,
        "/api/v1/me/news/refresh",
        None,
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({"state": "writing"}))
    );
    let (_, body) = call(&keyed, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    assert_eq!(body["state"], "writing");
    assert!(keyed
        .news
        .book(&keyed.amina)
        .await
        .unwrap()
        .refresh_requested_at
        .is_some());
}

#[tokio::test]
async fn tiles_show_what_feeds_them_and_reading_and_feedback_are_kept() {
    let pond = pond(true).await;
    interest(&pond, "mcp", "MCP", "tech", "both").await;
    interest(
        &pond,
        "politics",
        "Kenyan civic politics",
        "politics",
        "digest",
    )
    .await;
    for s in [
        story("s1", &pond.amina, "mcp", 30),
        story("s2", &pond.amina, "politics", 20),
        story("s3", &pond.amina, "area:mcp-dev-summit", 10),
        story("s4", &pond.amina, "mcp", 5),
    ] {
        pond.news.add_story(&s).await.unwrap();
    }
    let (_, body) = call(&pond, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    let ids: Vec<&str> = body["stories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["s4", "s3", "s1"],
        "digest-only interests stay off the tiles"
    );
    let tile = &body["stories"][0];
    assert_eq!(tile["kind"], "news");
    assert_eq!(tile["picture"], "/api/v1/report-images/abcdef0123456789");
    assert_eq!(
        tile["source"],
        json!({"name": "Nation", "url": "https://news.example/s4"})
    );
    assert_eq!(
        tile["interest"],
        json!({"id": "mcp", "topic": "mcp", "category": "tech"})
    );
    assert_eq!(tile["read"], false);
    assert!(tile.get("summary").is_none(), "a tile carries no summary");
    assert_eq!(body["stories"][1]["interest"]["category"], "other");

    let (_, page) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news?interest=politics",
        None,
    )
    .await;
    assert_eq!(page["stories"].as_array().unwrap().len(), 1);
    let (_, theirs) = call(&pond, "otieno-token", Method::GET, "/api/v1/me/news", None).await;
    assert_eq!(theirs["stories"], json!([]));

    let (status, detail) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news/stories/s1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["summary"], "Summary in Goose's words.");
    assert_eq!(detail["why_you"], "You follow this.");
    assert_eq!(
        detail["sources"],
        json!([{"title": "The page's own title", "url": "https://news.example/s1", "site": "Nation"}])
    );
    let (status, _) = call(
        &pond,
        "otieno-token",
        Method::GET,
        "/api/v1/me/news/stories/s1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &pond,
        "amina-token",
        Method::POST,
        "/api/v1/me/news/stories/s1/read",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &pond,
        "otieno-token",
        Method::POST,
        "/api/v1/me/news/stories/s4/read",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, history) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news/history",
        None,
    )
    .await;
    assert_eq!(history["stories"].as_array().unwrap().len(), 1);
    assert_eq!(history["stories"][0]["read"], true);
    assert!(history["stories"][0]["read_at"].is_string());

    let feedback = |id: &str, vote: Value| (format!("/api/v1/me/news/stories/{id}/feedback"), vote);
    let (uri, vote) = feedback(
        "s4",
        json!({"vote": "wrong", "note": "That is not about MCP."}),
    );
    let (status, _) = call(&pond, "amina-token", Method::POST, &uri, Some(vote)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (uri, vote) = feedback("s1", json!({"vote": "up"}));
    assert_eq!(
        call(&pond, "amina-token", Method::POST, &uri, Some(vote))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (uri, vote) = feedback("s1", json!({"vote": "meh"}));
    assert_eq!(
        call(&pond, "amina-token", Method::POST, &uri, Some(vote))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, body) = call(&pond, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    let ids: Vec<&str> = body["stories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["s3", "s1"], "a story called wrong is hidden");
    let votes = pond
        .news
        .voted_stories(&pond.amina, Utc::now() - Duration::days(1))
        .await
        .unwrap();
    assert_eq!(votes.len(), 1);
}

#[tokio::test]
async fn digests_are_listed_read_and_kept_from_other_members() {
    let pond = pond(true).await;
    interest(&pond, "mcp", "MCP", "tech", "both").await;
    let digest = Digest {
        id: "d1".into(),
        profile_id: pond.amina.clone(),
        day: NaiveDate::from_ymd_opt(2026, 11, 9).unwrap(),
        title: "MCP's new spec".into(),
        intro: "One thing today. It is about MCP.".into(),
        sections: vec![DigestSection {
            interest: InterestRef {
                id: "mcp".into(),
                topic: "MCP".into(),
            },
            heading: "The spec goes stateless".into(),
            body: "The core drops sessions [1](https://news.example/s1).".into(),
            pictures: vec![DigestPicture {
                key: "abcdef0123456789".into(),
                caption: "The spec".into(),
                source_url: "https://news.example/s1".into(),
            }],
            references: vec![pond_core::shared::domain::news::Reference {
                title: "The spec".into(),
                url: "https://news.example/s1".into(),
                site: "Nation".into(),
            }],
        }],
        written_at: Utc::now(),
    };
    pond.news.add_digest(&digest).await.unwrap();
    let (_, body) = call(&pond, "amina-token", Method::GET, "/api/v1/me/news", None).await;
    assert_eq!(body["digest"]["id"], "d1");
    assert_eq!(
        body["digest"]["listen"],
        "/api/v1/me/news/digests/d1/listen"
    );
    assert_eq!(
        body["digest"]["sections"][0]["pictures"][0],
        json!({"path": "/api/v1/report-images/abcdef0123456789", "caption": "The spec", "source_url": "https://news.example/s1"})
    );
    assert!(body["last_written_at"].is_string());
    let (_, list) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news/digests",
        None,
    )
    .await;
    assert_eq!(list["digests"][0]["title"], "MCP's new spec");
    let (status, one) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news/digests/d1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["intro"], "One thing today. It is about MCP.");
    let (status, _) = call(
        &pond,
        "otieno-token",
        Method::GET,
        "/api/v1/me/news/digests/d1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &pond,
        "amina-token",
        Method::GET,
        "/api/v1/me/news/digests/d1/listen",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no voice in this pond"
    );
    let (status, _) = call(
        &pond,
        "amina-token",
        Method::POST,
        "/api/v1/me/news/digests/d1/speak",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no speaker in this pond"
    );
    let (_, hub) = call(&pond, "amina-token", Method::GET, "/api/v1/me/hub", None).await;
    assert_eq!(hub["news"], true);
}
