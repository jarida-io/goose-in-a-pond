//! My Pond over HTTP: who reaches it, the admin card's unlock, and what an unlocked phone may
//! read and delete. The host is the real adapter over this test's data directory.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::build_router;
use pond_api::pond_admin::PondAdmin;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::security::services::admin_elevation::AdminElevation;
use pond_core::shared::domain::long_task::{
    Artifact, ArtifactKind, LongTask, LongTaskBudget, LongTaskState,
};
use pond_core::shared::ports::long_task::LongTaskRepository;
use pond_core::shared::services::pond_admin::PondAdminService;
use pond_core::user_data::domain::profile::{CreateProfileRequest, ProfileScope};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_infra::file_admin_card::FileAdminCardStore;
use pond_infra::host_pond_admin::{HostPondAdministrator, PowerHost};
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_long_task::SqliteLongTaskRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

const MEMBER: &str = "member-token";
const GUEST: &str = "guest-token";

/// Each token names its own device, as pairing would issue them.
struct Devices(HashMap<String, String>);

#[async_trait]
impl Handshake for Devices {
    async fn revoke_device(&self, _: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used here")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(self.0.contains_key(token))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        Ok(self.0.get(token).map(|device| TokenCaller {
            client_id: device.clone(),
            device_id: device.clone(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used here")
    }
}

struct Pond {
    router: axum::Router,
    password: Option<String>,
    member: String,
    other_member: String,
    tasks: Arc<SqliteLongTaskRepository>,
    dir: tempfile::TempDir,
}

async fn register(state: &pond_api::AppState, name: &str) -> String {
    state
        .device_registry
        .register(RegisterDeviceRequest {
            id: None,
            name: name.into(),
            device_type: "gotg".into(),
            hostname: None,
            capabilities: vec![],
            room_id: None,
        })
        .await
        .unwrap()
        .id
}

/// A member's phone and an unattributed tablet; an admin card unless `with_card` is false.
async fn pond(with_card: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let dir = built.dir;
    let member = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "duck".into(),
        })
        .await
        .unwrap()
        .id;
    let other_member = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Liz".into(),
            avatar_emoji: "heron".into(),
        })
        .await
        .unwrap()
        .id;
    let phone = register(&state, "Jerry's phone").await;
    let tablet = register(&state, "Kitchen tablet").await;
    SqliteDeviceAttribution::new(state.db.system.clone())
        .set_device_profile(&phone, Some(&member))
        .await
        .unwrap();
    state.handshake = Arc::new(Devices(HashMap::from([
        (MEMBER.to_string(), phone),
        (GUEST.to_string(), tablet),
    ])));

    let cards = Arc::new(FileAdminCardStore::new(dir.path()));
    let password = with_card.then(|| cards.issue_with_rounds(false, 10).unwrap().grouped());
    let missing = dir.path().join("no-such-sys");
    let host = Arc::new(HostPondAdministrator::new(
        dir.path(),
        state.db.system.clone(),
        None,
        PowerHost {
            nvpmodel_conf: missing.join("nvpmodel.conf"),
            nvpmodel: None,
            sudo: "/usr/bin/false".into(),
            systemctl: "/usr/bin/false".into(),
            hwmon: missing.join("hwmon"),
            power_supply: missing.join("power_supply"),
        },
    ));
    let tasks = Arc::new(SqliteLongTaskRepository::new(state.db.system.clone()));
    state.pond_admin = Some(Arc::new(PondAdmin::new(
        PondAdminService::new(host, tasks.clone()),
        AdminElevation::new(cards),
    )));
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        password,
        member,
        other_member,
        tasks,
        dir,
    }
}

async fn send(
    pond: &Pond,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
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
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, retry_after)
}

async fn unlock(pond: &Pond) {
    let password = pond.password.clone().expect("this pond has a card");
    let (status, body, _) = send(
        pond,
        Method::POST,
        "/api/v1/admin/unlock",
        Some(MEMBER),
        Some(json!({ "password": password })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unlocked"], true);
}

/// Every route that needs a live unlock, with a body where it takes one.
fn locked_routes() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (Method::GET, "/api/v1/admin/overview", None),
        (Method::GET, "/api/v1/admin/files?path=", None),
        (Method::DELETE, "/api/v1/admin/files?path=logs", None),
        (Method::GET, "/api/v1/admin/caches", None),
        (
            Method::POST,
            "/api/v1/admin/caches/kv_snapshots/clear",
            None,
        ),
        (Method::GET, "/api/v1/admin/tasks", None),
        (Method::DELETE, "/api/v1/admin/tasks/x", None),
        (Method::DELETE, "/api/v1/admin/tasks/x/artifacts/y", None),
        (Method::GET, "/api/v1/admin/health", None),
        (Method::GET, "/api/v1/admin/power", None),
        (
            Method::PUT,
            "/api/v1/admin/power/mode",
            Some(json!({ "id": 0 })),
        ),
        (Method::POST, "/api/v1/admin/power/restart", None),
    ]
}

#[tokio::test]
async fn nobody_but_a_members_phone_reaches_my_pond() {
    let pond = pond(true).await;
    let (status, _, _) = send(&pond, Method::GET, "/api/v1/admin/status", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let password = pond.password.clone().unwrap();
    for (method, uri, body) in [
        (Method::GET, "/api/v1/admin/status", None),
        (
            Method::POST,
            "/api/v1/admin/unlock",
            Some(json!({ "password": password })),
        ),
        (Method::GET, "/api/v1/admin/overview", None),
    ] {
        let (status, body, _) = send(&pond, method.clone(), uri, Some(GUEST), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        assert_eq!(body["error"], "admin_members_only", "{method} {uri}");
    }
}

#[tokio::test]
async fn every_admin_route_stays_locked_until_the_card_unlocks_it() {
    let pond = pond(true).await;
    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/status",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["card"], "present");
    assert_eq!(body["unlocked"], false);
    assert_eq!(body["idle_minutes"], 15);
    assert!(body["expires_at"].is_null());

    for (method, uri, body) in locked_routes() {
        let (status, answer, _) = send(&pond, method.clone(), uri, Some(MEMBER), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {answer}");
        assert_eq!(answer["error"], "admin_locked", "{method} {uri}");
    }

    let (status, body, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/unlock",
        Some(MEMBER),
        Some(json!({ "password": "0000-0000-0000-0000" })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("admin_password_rejected"))
    );
    let (status, body, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/unlock",
        Some(MEMBER),
        Some(json!({ "password": "1234" })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("admin_password_malformed"))
    );

    unlock(&pond).await;
    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/status",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["unlocked"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert!(body["expires_at"].is_string());
    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/overview",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["areas"].as_array().is_some_and(|a| !a.is_empty()));
    assert!(body["disk"]["total_bytes"].as_u64().is_some_and(|b| b > 0));

    let (status, _, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/lock",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/overview",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("admin_locked"))
    );
}

#[tokio::test]
async fn wrong_cards_are_slowed_and_the_wait_is_announced() {
    let pond = pond(true).await;
    for _ in 0..5 {
        let (status, _, _) = send(
            &pond,
            Method::POST,
            "/api/v1/admin/unlock",
            Some(MEMBER),
            Some(json!({ "password": "0000-0000-0000-0000" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let password = pond.password.clone().unwrap();
    let (status, body, retry_after) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/unlock",
        Some(MEMBER),
        Some(json!({ "password": password })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "after five wrong cards even the right one waits"
    );
    assert_eq!(body["error"], "admin_cooling");
    let secs = body["retry_after"].as_u64().unwrap();
    assert!((1..=60).contains(&secs), "{secs}");
    assert_eq!(retry_after, Some(secs.to_string()));
    let (_, status_body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/status",
        Some(MEMBER),
        None,
    )
    .await;
    assert!(status_body["retry_after"].as_u64().is_some());
}

#[tokio::test]
async fn a_pond_without_a_card_says_so() {
    let pond = pond(false).await;
    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/status",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["card"].as_str()),
        (StatusCode::OK, Some("missing"))
    );
    let (status, body, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/unlock",
        Some(MEMBER),
        Some(json!({ "password": "0000-0000-0000-0000" })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("admin_card_missing"))
    );
}

#[tokio::test]
async fn files_are_listed_and_deleted_inside_the_data_directory_only() {
    let pond = pond(true).await;
    unlock(&pond).await;
    let logs = pond.dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("pond.log.2026-07-12"), vec![b'x'; 40]).unwrap();

    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/files?path=",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"logs") && names.contains(&"pond_system.db"),
        "{names:?}"
    );
    assert!(
        !names.contains(&"setup"),
        "the admin card's directory is listed: {names:?}"
    );

    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/files?path=logs",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"][0]["path"], "logs/pond.log.2026-07-12");
    assert_eq!(body["entries"][0]["deletable"], true);

    let (status, body, _) = send(
        &pond,
        Method::DELETE,
        "/api/v1/admin/files?path=logs%2Fpond.log.2026-07-12",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["freed_bytes"].as_u64()),
        (StatusCode::OK, Some(40))
    );
    assert!(!logs.join("pond.log.2026-07-12").exists());

    for (uri, status, error) in [
        (
            "/api/v1/admin/files?path=..%2Fx",
            StatusCode::BAD_REQUEST,
            "invalid_path",
        ),
        (
            "/api/v1/admin/files?path=setup%2Fadmin.json",
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "/api/v1/admin/files?path=pond_system.db",
            StatusCode::FORBIDDEN,
            "protected_path",
        ),
        (
            "/api/v1/admin/files?path=logs",
            StatusCode::FORBIDDEN,
            "protected_path",
        ),
        (
            "/api/v1/admin/files?path=",
            StatusCode::BAD_REQUEST,
            "invalid_path",
        ),
    ] {
        let (got, body, _) = send(&pond, Method::DELETE, uri, Some(MEMBER), None).await;
        assert_eq!(
            (got, body["error"].as_str()),
            (status, Some(error)),
            "{uri}"
        );
    }
    assert!(pond.dir.path().join("setup/admin.json").exists());
    assert!(pond.dir.path().join("pond_system.db").exists());
    let (_, body, _) = send(
        &pond,
        Method::DELETE,
        "/api/v1/admin/files?path=pond_system.db",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(body["reason"], "in_use");
}

#[tokio::test]
async fn caches_health_and_power_answer_on_any_host() {
    let pond = pond(true).await;
    unlock(&pond).await;
    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/caches",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["caches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["kv_snapshots", "download_leftovers", "accelerator_caches"]
    );
    let (status, body, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/caches/target/clear",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_cache"))
    );
    let (status, body, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/caches/kv_snapshots/clear",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["freed_bytes"].as_u64()),
        (StatusCode::OK, Some(0))
    );

    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/health",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for key in [
        "version",
        "uptime_secs",
        "host",
        "memory",
        "disk",
        "temperatures",
        "process",
        "checks",
    ] {
        assert!(body.get(key).is_some(), "health lacks {key}: {body}");
    }

    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/power",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["control"].as_str()),
        (StatusCode::OK, Some("none"))
    );
    assert_eq!(body["can_change"], false);
    let (status, body, _) = send(
        &pond,
        Method::PUT,
        "/api/v1/admin/power/mode",
        Some(MEMBER),
        Some(json!({ "id": 0 })),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("power_control_unavailable")
        )
    );
    let (status, body, _) = send(
        &pond,
        Method::POST,
        "/api/v1/admin/power/restart",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("power_control_unavailable")
        )
    );
}

#[tokio::test]
async fn tasks_hide_another_members_words_and_only_finished_ones_go() {
    let pond = pond(true).await;
    unlock(&pond).await;
    let budget = LongTaskBudget::new(10, None).unwrap();
    let theirs = LongTask::start(
        ProfileScope::Owner(pond.other_member.clone()),
        Some(pond.other_member.clone()),
        "a surprise",
        None,
        budget,
        chrono::Utc::now(),
    )
    .unwrap();
    let mine = LongTask::start(
        ProfileScope::Owner(pond.member.clone()),
        Some(pond.member.clone()),
        "plan the garden",
        None,
        budget,
        chrono::Utc::now(),
    )
    .unwrap();
    for task in [&theirs, &mine] {
        pond.tasks.create_task(task).await.unwrap();
        let artifact = Artifact::new(
            task.id(),
            None,
            ArtifactKind::Findings,
            "Findings",
            "summary",
            "body".into(),
            chrono::Utc::now(),
        )
        .unwrap();
        pond.tasks.insert_artifact(&artifact).await.unwrap();
    }

    let (status, body, _) = send(
        &pond,
        Method::GET,
        "/api/v1/admin/tasks",
        Some(MEMBER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["tasks"].as_array().unwrap();
    let find = |id: &str| listed.iter().find(|t| t["id"] == id).unwrap();
    assert_eq!(find(mine.id())["goal"], "plan the garden");
    assert!(find(theirs.id())["goal"].is_null());
    assert_eq!(find(theirs.id())["another_member"], true);
    assert!(find(theirs.id())["artifacts"][0]["title"].is_null());

    let uri = format!("/api/v1/admin/tasks/{}", theirs.id());
    let (status, body, _) = send(&pond, Method::DELETE, &uri, Some(MEMBER), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("task_active"))
    );
    pond.tasks
        .set_task_state(theirs.id(), LongTaskState::Cancelled, chrono::Utc::now())
        .await
        .unwrap();
    let (status, _, _) = send(&pond, Method::DELETE, &uri, Some(MEMBER), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = send(&pond, Method::DELETE, &uri, Some(MEMBER), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
}
