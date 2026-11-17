//! `/me/facts`, `/me/dont` and the portrait the fact sheet gives, over HTTP: each member
//! reaches only their own lines and rules, a removed line is never learned again, and a
//! rebuild is asked for by queueing an engine job.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use pond_api::{build_router, EngineStores};
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::domain::engine_job::{JobClass, JobKind, JobState};
use pond_core::user_data::domain::fact_sheet::{
    Confidence, FactCategory, FactLine, FactSource, FactSourceKind, FactState,
};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use serde_json::{json, Value};
use tower::ServiceExt;

const JERRY: &str = "jerry-token";
const EMMANUEL: &str = "emmanuel-token";

/// `jerry-token` is Jerry's phone and `emmanuel-token` Emmanuel's.
struct TwoPhones {
    jerry: String,
    emmanuel: String,
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
        Ok(matches!(token, JERRY | EMMANUEL))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let device = match token {
            JERRY => &self.jerry,
            EMMANUEL => &self.emmanuel,
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

struct Pond {
    router: axum::Router,
    jerry: String,
    stores: EngineStores,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    state.skip_onboarding = true;
    let mut members = Vec::new();
    for name in ["Jerry", "Emmanuel"] {
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
        members.push((member, device));
    }
    state.handshake = Arc::new(TwoPhones {
        jerry: members[0].1.clone(),
        emmanuel: members[1].1.clone(),
    });
    let stores = state.engine.clone();
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        jerry: members[0].0.clone(),
        stores,
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
        .uri(format!("/api/v1{uri}"))
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

async fn add(pond: &Pond, token: &str, category: &str, fact: &str) -> Value {
    let (status, sheet) = call(
        pond,
        token,
        Method::POST,
        "/me/facts",
        Some(json!({"category": category, "fact": fact})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sheet}");
    sheet
}

fn facts(sheet: &Value) -> Vec<&str> {
    sheet["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["fact"].as_str().unwrap())
        .collect()
}

/// A line a rebuild would write for Jerry, not one he wrote.
async fn learned(pond: &Pond, category: FactCategory, fact: &str) -> FactLine {
    let line = FactLine::new(
        &pond.jerry,
        category,
        fact,
        Confidence::Likely,
        FactSource::new(FactSourceKind::Memory, Some("m1")),
        Utc::now(),
    )
    .unwrap();
    pond.stores.facts.write(&line).await.unwrap().unwrap()
}

#[tokio::test]
async fn a_member_writes_rewords_and_pins_their_own_lines() {
    let p = pond().await;
    let (status, empty) = call(&p, JERRY, Method::GET, "/me/facts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty, json!({"facts": [], "limit": 100}));

    let sheet = add(&p, JERRY, "home", "Lives in  Kilimani").await;
    let line = &sheet["facts"][0];
    assert_eq!(line["fact"], "Lives in Kilimani");
    assert_eq!(line["category"], "home");
    assert_eq!(line["confidence"], "sure");
    assert_eq!(line["source"]["kind"], "you");
    assert_eq!(line["state"], "active");
    assert!(line.get("profile_id").is_none(), "{line}");
    let id = line["id"].as_str().unwrap().to_string();

    let again = add(&p, JERRY, "home", "lives in kilimani!").await;
    assert_eq!(
        again["facts"].as_array().unwrap().len(),
        1,
        "one key, one line"
    );
    add(&p, JERRY, "work", "Runs Jarida").await;

    for refused in [
        json!({"category": "work", "fact": "  ... "}),
        json!({"category": "astrology", "fact": "A Leo"}),
        json!({"fact": "No category"}),
    ] {
        let (status, body) = call(&p, JERRY, Method::POST, "/me/facts", Some(refused)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    let (status, changed) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/facts/{id}"),
        Some(json!({"fact": "Lives in Kileleshwa", "pinned": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["fact"], "Lives in Kileleshwa");
    assert_eq!(changed["pinned"], true);
    assert_eq!(changed["id"], id.as_str());
    let (_, sheet) = call(&p, JERRY, Method::GET, "/me/facts", None).await;
    assert_eq!(
        facts(&sheet),
        ["Lives in Kileleshwa", "Runs Jarida"],
        "pinned first"
    );

    let (status, taken) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/facts/{id}"),
        Some(json!({"fact": "runs jarida"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{taken}");
    let (status, _) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/facts/{id}"),
        Some(json!({"fact": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &p,
        JERRY,
        Method::PATCH,
        "/me/facts/nope",
        Some(json!({"pinned": true})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rewording_a_learned_line_makes_it_the_members_own() {
    let p = pond().await;
    let guess = learned(&p, FactCategory::Work, "Works at a bank").await;
    let (status, changed) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/facts/{}", guess.id),
        Some(json!({"fact": "Works at Jarida", "category": "work"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["source"]["kind"], "you");
    assert_eq!(changed["confidence"], "sure");

    // A rebuild that still believes the old wording leaves the member's line alone.
    let mut stale = guess.clone();
    stale.id = "another".into();
    stale.fact = "works at jarida".into();
    let active = p
        .stores
        .facts
        .replace(&p.jerry, &[stale], Utc::now())
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].fact, "Works at Jarida");
    assert_eq!(active[0].source.kind, FactSourceKind::You);
}

#[tokio::test]
async fn a_removed_line_is_never_learned_again_and_goes_on_the_dont_list() {
    let p = pond().await;
    let duck = learned(&p, FactCategory::Food, "Likes duck recipes").await;
    let (status, _) = call(
        &p,
        JERRY,
        Method::DELETE,
        &format!("/me/facts/{}", duck.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, sheet) = call(&p, JERRY, Method::GET, "/me/facts", None).await;
    assert!(facts(&sheet).is_empty());

    let (status, list) = call(&p, JERRY, Method::GET, "/me/dont", None).await;
    assert_eq!(status, StatusCode::OK);
    let rules = list["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1, "{list}");
    assert_eq!(rules[0]["rule"], "Likes duck recipes");
    assert_eq!(rules[0]["strength"], "avoid");
    assert_eq!(rules[0]["origin"], "fact");
    assert_eq!(rules[0]["origin_ref"], duck.id.as_str());

    let mut relearned = duck.clone();
    relearned.id = "relearned".into();
    relearned.fact = "likes  duck recipes".into();
    let active = p
        .stores
        .facts
        .replace(&p.jerry, &[relearned], Utc::now())
        .await
        .unwrap();
    assert!(active.is_empty(), "a rebuild brought a removed line back");

    let (status, _) = call(
        &p,
        JERRY,
        Method::DELETE,
        &format!("/me/facts/{}", duck.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "already removed");
    let (status, _) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/facts/{}", duck.id),
        Some(json!({"pinned": true})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a removed line is not on the sheet"
    );

    // Only the member can bring it back, and the rule its removal left goes with it.
    let sheet = add(&p, JERRY, "food", "Likes duck recipes").await;
    assert_eq!(facts(&sheet), ["Likes duck recipes"]);
    assert_eq!(sheet["facts"][0]["id"], duck.id.as_str());
    assert_eq!(sheet["facts"][0]["source"]["kind"], "you");
    let (_, list) = call(&p, JERRY, Method::GET, "/me/dont", None).await;
    assert_eq!(list, json!({"rules": []}));
}

#[tokio::test]
async fn a_rebuild_is_asked_for_with_one_queued_job_and_no_task() {
    let p = pond().await;
    for _ in 0..2 {
        let (status, asked) = call(&p, JERRY, Method::POST, "/me/facts/rebuild", None).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(asked, json!({"state": "queued"}));
    }
    let open = p
        .stores
        .jobs
        .open_of_kind(&p.jerry, JobKind::FactSheet)
        .await
        .unwrap();
    assert_eq!(open.len(), 1, "two taps, one job");
    let mut job = open.into_iter().next().unwrap();
    assert_eq!(job.class, JobClass::Asked);
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.task_id, None);
    assert_eq!(job.profile_id.as_deref(), Some(p.jerry.as_str()));

    job.start(None, Utc::now()).unwrap();
    p.stores.jobs.save(&job).await.unwrap();
    let (_, asked) = call(&p, JERRY, Method::POST, "/me/facts/rebuild", None).await;
    assert_eq!(asked, json!({"state": "running"}), "the open job answers");

    job.settle(JobState::Passed, "12 lines kept", Utc::now())
        .unwrap();
    p.stores.jobs.save(&job).await.unwrap();
    let (_, asked) = call(&p, JERRY, Method::POST, "/me/facts/rebuild", None).await;
    assert_eq!(asked, json!({"state": "queued"}));
    let recent = p.stores.jobs.recent(&p.jerry, 10).await.unwrap();
    assert_eq!(recent.len(), 2, "a settled job does not block the next ask");
}

#[tokio::test]
async fn the_dont_list_takes_rules_by_hand_and_keeps_the_stronger_of_two_sayings() {
    let p = pond().await;
    let (status, list) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/dont",
        Some(json!({"rule": "Never:  celebrity gossip"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let rule = &list["rules"][0];
    assert_eq!(rule["rule"], "celebrity gossip");
    assert_eq!(rule["strength"], "never");
    assert_eq!(rule["origin"], "you");
    assert!(rule.get("profile_id").is_none(), "{rule}");
    let id = rule["id"].as_str().unwrap().to_string();

    let (_, list) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/dont",
        Some(json!({"rule": "Celebrity gossip", "strength": "avoid"})),
    )
    .await;
    let rules = list["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1, "one key, one rule");
    assert_eq!(rules[0]["strength"], "never", "an avoid does not weaken it");

    for refused in [
        json!({"rule": "  ", "strength": "never"}),
        json!({"rule": "duck recipes", "strength": "sometimes"}),
        json!({"strength": "never"}),
    ] {
        let (status, body) = call(&p, JERRY, Method::POST, "/me/dont", Some(refused)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }

    let (status, list) = call(&p, JERRY, Method::DELETE, &format!("/me/dont/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!({"rules": []}));
    let (status, _) = call(&p, JERRY, Method::DELETE, &format!("/me/dont/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_member_never_reads_or_changes_anothers_sheet_or_rules() {
    let p = pond().await;
    let sheet = add(&p, JERRY, "home", "Lives in Kilimani").await;
    let id = sheet["facts"][0]["id"].as_str().unwrap().to_string();
    let (_, list) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/dont",
        Some(json!({"rule": "celebrity gossip"})),
    )
    .await;
    let rule = list["rules"][0]["id"].as_str().unwrap().to_string();

    let (_, his) = call(&p, EMMANUEL, Method::GET, "/me/facts", None).await;
    assert!(facts(&his).is_empty());
    let (_, his) = call(&p, EMMANUEL, Method::GET, "/me/dont", None).await;
    assert_eq!(his, json!({"rules": []}));
    for (method, uri, body) in [
        (
            Method::PATCH,
            format!("/me/facts/{id}"),
            Some(json!({"fact": "Lives in Karen"})),
        ),
        (Method::DELETE, format!("/me/facts/{id}"), None),
        (Method::DELETE, format!("/me/dont/{rule}"), None),
    ] {
        let (status, body) = call(&p, EMMANUEL, method, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
    }
    call(&p, EMMANUEL, Method::POST, "/me/facts/rebuild", None).await;
    assert!(p
        .stores
        .jobs
        .open_of_kind(&p.jerry, JobKind::FactSheet)
        .await
        .unwrap()
        .is_empty());

    let (_, mine) = call(&p, JERRY, Method::GET, "/me/facts", None).await;
    assert_eq!(facts(&mine), ["Lives in Kilimani"]);
    let (_, mine) = call(&p, JERRY, Method::GET, "/me/dont", None).await;
    assert_eq!(mine["rules"].as_array().unwrap().len(), 1);
    let unauthorised = Request::builder()
        .uri("/api/v1/me/facts")
        .body(Body::empty())
        .unwrap();
    let response = p.router.clone().oneshot(unauthorised).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_portrait_is_read_from_the_fact_sheet() {
    let p = pond().await;
    let (status, none) = call(&p, JERRY, Method::GET, "/me/portrait", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{none}");

    add(&p, JERRY, "work", "Runs Jarida").await;
    add(&p, JERRY, "interest", "Formula 1").await;
    add(&p, JERRY, "plan", "Speaking at the MCP Dev Summit").await;
    add(&p, JERRY, "style", "Teal").await;
    let (status, portrait) = call(&p, JERRY, Method::GET, "/me/portrait", None).await;
    assert_eq!(status, StatusCode::OK, "{portrait}");
    assert_eq!(portrait["summary"], "Runs Jarida.");
    assert_eq!(portrait["into"], json!(["Formula 1"]));
    assert_eq!(
        portrait["working_on"],
        json!(["Speaking at the MCP Dev Summit"])
    );
    assert_eq!(portrait["likes"], json!(["Teal"]));
    assert!(portrait["updated_at"].is_string());

    let retired = p
        .stores
        .facts
        .list(&p.jerry, FactState::Retired)
        .await
        .unwrap();
    assert!(
        retired.is_empty(),
        "the member's own lines are never retired"
    );
    let (status, _) = call(&p, EMMANUEL, Method::GET, "/me/portrait", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another member's sheet is not his"
    );
}
