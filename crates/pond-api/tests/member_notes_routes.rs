//! A member's own notes and setup interview: reached only from that member's phone, carried only
//! on that member's turns, and saved only once the member approves them.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::services::profile_interview::InterviewService;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_profile_interview::SqliteProfileInterviewRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

/// `amina-token` is Amina's phone and `otieno-token` Otieno's.
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

struct Pond {
    router: axum::Router,
    amina: String,
    otieno: String,
    agent: Arc<pond_core::shared::mocks::mock_agent::MockAgent>,
    extras: Arc<SqlitePromptExtraRepository>,
    interviews: InterviewService,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    let agent = Arc::new(pond_core::shared::mocks::mock_agent::MockAgent::new());
    state.agent = agent.clone();
    state.skip_onboarding = true;
    let extras = Arc::new(SqlitePromptExtraRepository::new(pool.clone()));
    state.prompt_extra_repo = Some(extras.clone());

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
        members.push((member, device));
    }
    let (amina, amina_phone) = members[0].clone();
    let (otieno, otieno_phone) = members[1].clone();
    state.handshake = Arc::new(TwoPhones {
        amina: amina_phone,
        otieno: otieno_phone,
    });
    let interviews = InterviewService::new(
        Arc::new(SqliteProfileInterviewRepository::new(pool.clone())),
        extras.clone(),
        state.session_storage.clone(),
        state.profile_repo.clone(),
    );
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        amina,
        otieno,
        agent,
        extras,
        interviews,
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_str(&text).unwrap_or_else(|_| {
        // A chat stream: the frames, so a caller can find the session it ran in.
        Value::Array(
            text.lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .filter_map(|d| serde_json::from_str(d).ok())
                .collect(),
        )
    });
    (status, value)
}

fn note(instruction: &str) -> Option<Value> {
    Some(json!({ "instruction": instruction }))
}

#[tokio::test]
async fn each_member_reaches_only_their_own_notes() {
    let p = pond().await;
    let mine = format!("/api/v1/profiles/{}/notes", p.amina);

    let (status, body) = call(
        &p,
        "amina-token",
        Method::PUT,
        &format!("{mine}/me_food"),
        note("No pork."),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(&p, "amina-token", Method::GET, &mine, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["notes"][0]["key"], "me_food");
    assert_eq!(body["limits"]["max_total_chars"], 4000);

    for (method, uri, body) in [
        (Method::GET, mine.clone(), None),
        (Method::PUT, format!("{mine}/me_food"), note("Loves pork.")),
        (Method::DELETE, format!("{mine}/me_food"), None),
        (
            Method::GET,
            format!("/api/v1/profiles/{}/interview", p.amina),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/profiles/{}/interview", p.amina),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/profiles/{}/interview/approve", p.amina),
            None,
        ),
    ] {
        let (status, _) = call(&p, "otieno-token", method.clone(), &uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    assert_eq!(
        p.extras.list_for_profile(&p.amina).await.unwrap()[0].instruction,
        "No pork."
    );

    // The household list never shows a member's note.
    let (_, household) = call(&p, "amina-token", Method::GET, "/api/v1/agent/extras", None).await;
    assert!(!household.to_string().contains("No pork."), "{household}");
}

#[tokio::test]
async fn a_members_turn_carries_their_notes_and_never_anothers() {
    let p = pond().await;
    for (token, member, text) in [
        ("amina-token", &p.amina, "Call her Mama Amina."),
        ("otieno-token", &p.otieno, "Call him Baba Baraka."),
    ] {
        let uri = format!("/api/v1/profiles/{member}/notes/me_address");
        assert_eq!(
            call(&p, token, Method::PUT, &uri, note(text)).await.0,
            StatusCode::OK
        );
    }

    for (token, own, other) in [
        (
            "amina-token",
            "Call her Mama Amina.",
            "Call him Baba Baraka.",
        ),
        (
            "otieno-token",
            "Call him Baba Baraka.",
            "Call her Mama Amina.",
        ),
    ] {
        let (status, _) = call(
            &p,
            token,
            Method::POST,
            "/api/v1/chat/stream",
            Some(json!({"message": "Hello"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let request = p.agent.last_request().unwrap();
        let notes = request.profile_context.unwrap().member_notes;
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].instruction, own);
        assert_ne!(notes[0].instruction, other);
        assert!(request.interview.is_none());
    }
}

#[tokio::test]
async fn the_interview_runs_in_its_own_session_with_only_its_tools() {
    let p = pond().await;
    let start = format!("/api/v1/profiles/{}/interview", p.amina);

    let (status, body) = call(&p, "amina-token", Method::GET, &start, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "not_started");

    let (status, body) = call(&p, "amina-token", Method::POST, &start, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "in_progress");
    assert_eq!(body["topics"].as_array().unwrap().len(), 10);
    assert!(body.get("answers").is_none() && !body.to_string().contains("\"answer\""));
    let session = body["session_id"].as_str().unwrap().to_string();

    // A second start resumes the same session.
    let (_, again) = call(&p, "amina-token", Method::POST, &start, None).await;
    assert_eq!(again["session_id"], session);

    let (status, _) = call(
        &p,
        "amina-token",
        Method::POST,
        "/api/v1/chat/stream",
        Some(json!({"message": "Call me Mama Amina", "session_id": session})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let request = p.agent.last_request().unwrap();
    assert_eq!(
        request.tool_group_allowlist,
        Some(vec!["giap-interview".to_string()])
    );
    let context = request.interview.expect("an interview turn");
    assert!(
        context.contains("You opened this conversation by saying"),
        "{context}"
    );

    // Otieno speaking in Amina's interview session gets an ordinary turn.
    call(
        &p,
        "otieno-token",
        Method::POST,
        "/api/v1/chat/stream",
        Some(json!({"message": "Hi", "session_id": session})),
    )
    .await;
    let request = p.agent.last_request().unwrap();
    assert!(request.interview.is_none());
    assert_ne!(
        request.tool_group_allowlist,
        Some(vec!["giap-interview".to_string()])
    );
}

#[tokio::test]
async fn a_tap_saves_the_draft_and_without_one_there_is_nothing_to_approve() {
    let p = pond().await;
    let approve = format!("/api/v1/profiles/{}/interview/approve", p.amina);
    call(
        &p,
        "amina-token",
        Method::POST,
        &format!("/api/v1/profiles/{}/interview", p.amina),
        None,
    )
    .await;

    let (status, body) = call(&p, "amina-token", Method::POST, &approve, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "no_draft");

    let now = chrono::Utc::now();
    p.interviews
        .record(&p.amina, "address", "Mama Amina", now)
        .await
        .unwrap()
        .unwrap();
    p.interviews
        .propose(
            &p.amina,
            vec![("address".into(), "Call her Mama Amina.".into())],
            now,
        )
        .await
        .unwrap()
        .unwrap();
    let (_, body) = call(
        &p,
        "amina-token",
        Method::GET,
        &format!("/api/v1/profiles/{}/interview", p.amina),
        None,
    )
    .await;
    assert_eq!(body["status"], "awaiting_approval");
    assert_eq!(body["draft"][0]["key"], "me_address");

    let (status, body) = call(&p, "amina-token", Method::POST, &approve, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["saved"], json!(["me_address"]));
    let saved = p.extras.list_for_profile(&p.amina).await.unwrap();
    assert_eq!(saved[0].instruction, "Call her Mama Amina.");
    assert!(p
        .extras
        .list_for_profile(&p.otieno)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn household_and_member_notes_share_one_budget_from_either_side() {
    let p = pond().await;
    let long = "a".repeat(1_000);
    for key in ["h0", "h1", "h2"] {
        let (status, body) = call(
            &p,
            "amina-token",
            Method::POST,
            "/api/v1/agent/extras",
            Some(json!({"key": key, "instruction": long})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let mine = format!("/api/v1/profiles/{}/notes", p.amina);
    assert_eq!(
        call(
            &p,
            "amina-token",
            Method::PUT,
            &format!("{mine}/m0"),
            note(&long)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, body) = call(
        &p,
        "amina-token",
        Method::PUT,
        &format!("{mine}/m1"),
        note("x"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "extras_too_long");

    // A household note that would push Amina's turns past the cap is refused as well.
    let (status, body) = call(
        &p,
        "otieno-token",
        Method::POST,
        "/api/v1/agent/extras",
        Some(json!({"key": "h3", "instruction": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "extras_too_long");
}
