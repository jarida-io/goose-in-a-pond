//! Proposals over HTTP: owner-only, expired on read, and never a back door for deciding drafts.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::draft::{Draft, DraftStatus};
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::proposal::{
    BusEventRef, Proposal, ProposalAudience, PROPOSAL_SESSION_ID,
};
use pond_core::user_data::domain::schedule::TaskKind;
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::draft::DraftRepository;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::proposal::ProposalRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_draft::SqliteDraftRepository;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_proposal::SqliteProposalRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use serde_json::Value;
use tower::ServiceExt;

// ── Stubs ────────────────────────────────────────────────────────────────────

struct CompletedOnboarding;

#[async_trait::async_trait]
impl OnboardingRepository for CompletedOnboarding {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct NoDevices;

#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
        anyhow::bail!("not used")
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Harness {
    app: axum::Router,
    storage: Arc<SqliteSessionStorage>,
    profiles: Arc<SqliteProfileRepository>,
    proposals: SqliteProposalRepository,
    drafts: SqliteDraftRepository,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let storage = Arc::new(SqliteSessionStorage::new(pool.clone()));
    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));
    let hs = MockHandshake::new();
    hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        warmup: Default::default(),
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage: storage.clone(),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: profiles.clone(),
        device_registry: Arc::new(NoDevices),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });

    Harness {
        app: build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        storage,
        profiles,
        proposals: SqliteProposalRepository::new(pool.clone()),
        drafts: SqliteDraftRepository::new(pool),
        _tmp: tmp,
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

async fn member(h: &Harness, name: &str) -> String {
    h.profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id
}

/// An unidentified session: `Household` on a one-member pond, `Guest` with two or more.
async fn unidentified_session(h: &Harness, id: &str) -> String {
    h.storage.create_session(id.to_string()).await.unwrap();
    id.to_string()
}

/// A session bound at `Explicit` strength, as `PUT /sessions/{id}/user` writes it.
async fn session_of(h: &Harness, id: &str, profile_id: &str) -> String {
    h.storage.create_session(id.to_string()).await.unwrap();
    h.storage
        .set_session_identity(
            id,
            &SessionIdentity {
                profile_id: Some(profile_id.to_string()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();
    id.to_string()
}

async fn save_proposal(h: &Harness, id: &str, owner: &str, ttl: Duration) -> String {
    let created = Utc::now();
    let proposal = Proposal::expiring_after(
        id,
        BusEventRef::new(
            "camera",
            Some("front-door".to_string()),
            Some("person".to_string()),
            created,
        )
        .unwrap(),
        "a parcel has been at the door for an hour",
        TaskKind::AgentPrompt {
            prompt: "remind me about the parcel".to_string(),
        },
        ProposalAudience::for_member(owner).unwrap(),
        0.72,
        created,
        ttl,
    )
    .unwrap();
    h.proposals.save(&proposal).await.unwrap();
    id.to_string()
}

/// A user-staged draft as `save_draft` writes it (`origin` NULL, no expiry).
async fn save_plain_draft(h: &Harness, id: &str, owner: &str) {
    h.drafts
        .save(Draft {
            id: id.to_string(),
            session_id: "engine-sess-1".to_string(),
            kind: "shell_command".to_string(),
            summary: "rm -rf the logs".to_string(),
            payload: "{}".to_string(),
            profile_id: Some(owner.to_string()),
            identification_source: Some(IdentificationSource::Explicit),
            status: DraftStatus::Pending,
            created_at: Utc::now(),
            expires_at: None,
        })
        .await
        .unwrap();
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn decide(app: &axum::Router, id: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/proposals/{id}/decide"))
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn ids(body: &Value) -> Vec<String> {
    body["proposals"]
        .as_array()
        .unwrap_or_else(|| panic!("no `proposals` array in {body}"))
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

// ── Invariant 4: one member's proposals, and nobody else's ───────────────────

#[tokio::test]
async fn a_member_sees_their_own_live_proposals_and_no_one_elses() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let jerry = member(&h, "Jerry").await;

    save_proposal(&h, "p-liz-1", &liz, Duration::hours(2)).await;
    save_proposal(&h, "p-liz-2", &liz, Duration::hours(3)).await;
    save_proposal(&h, "p-jerry", &jerry, Duration::hours(2)).await;
    // Invariant 7: expiry is enforced on read, with no sweeper.
    save_proposal(&h, "p-liz-stale", &liz, Duration::seconds(1)).await;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let session = session_of(&h, "s-liz", &liz).await;
    let (status, body) = get_json(&h.app, &format!("/api/v1/proposals?session_id={session}")).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["profile_id"], liz, "body: {body}");

    let mut seen = ids(&body);
    seen.sort();
    assert_eq!(
        seen,
        vec!["p-liz-1".to_string(), "p-liz-2".to_string()],
        "Liz must see exactly her two live proposals: not Jerry's (invariant 4 \
         is that a proposal is addressed to one member) and not her expired one \
         (invariant 7 holds on the read, because nothing sweeps the table). \
         Body: {body}"
    );

    // Invariant 2: the reason is always there to be shown, as its own field.
    for p in body["proposals"].as_array().unwrap() {
        assert!(
            p["rationale"]
                .as_str()
                .is_some_and(|r| !r.trim().is_empty()),
            "every proposal carries a rationale the user can read: {p}"
        );
        assert!(
            !p["summary"]
                .as_str()
                .unwrap()
                .contains("parcel has been at the door"),
            "the summary describes the ACTION; folding the rationale into it \
             would let a surface that renders only the summary look like it \
             honours invariant 2: {p}"
        );
    }
}

/// Invariant 5; two members plus an unidentified session make a `Guest`.
#[tokio::test]
async fn a_guest_receives_nothing_and_is_told_why() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let _jerry = member(&h, "Jerry").await;
    save_proposal(&h, "p-liz-1", &liz, Duration::hours(2)).await;

    let session = unidentified_session(&h, "s-visitor").await;
    let (status, body) = get_json(&h.app, &format!("/api/v1/proposals?session_id={session}")).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "invariant 5: a Guest session generates no proposals and receives none. Body: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unidentified speaker"),
        "the refusal must name the scope it refused; body: {body}"
    );
}

/// `Household` is the broadcast, so it cannot hold a proposal even on a one-member pond.
#[tokio::test]
async fn an_unidentified_session_on_a_one_member_pond_is_still_refused() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    save_proposal(&h, "p-liz-1", &liz, Duration::hours(2)).await;

    let session = unidentified_session(&h, "s-anon").await;
    let (status, body) = get_json(&h.app, &format!("/api/v1/proposals?session_id={session}")).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a Household scope IS the broadcast, and invariant 4 forbids one. Body: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("whole household"),
        "the refusal must name the broadcast it refused; body: {body}"
    );

    // Vacuity control: the same proposal is visible once the session names its member.
    let bound = session_of(&h, "s-liz", &liz).await;
    let (status, body) = get_json(&h.app, &format!("/api/v1/proposals?session_id={bound}")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(ids(&body), vec!["p-liz-1".to_string()], "body: {body}");
}

#[tokio::test]
async fn the_route_will_not_guess_which_session_is_asking() {
    let h = make_app().await;
    let (status, _body) = get_json(&h.app, "/api/v1/proposals").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a missing session_id must be refused, not defaulted: the session is how \
         this edge learns who the caller is, and a default resolves to the \
         broadcast for everybody"
    );
}

// ── Disposal ─────────────────────────────────────────────────────────────────

async fn status_of(h: &Harness, id: &str) -> DraftStatus {
    h.drafts
        .get(id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no drafts row for {id}"))
        .status
}

#[tokio::test]
async fn a_member_disposes_of_their_own_proposal_and_nothing_is_executed() {
    for (decision, expected) in [
        ("approve", DraftStatus::Approved),
        ("reject", DraftStatus::Rejected),
    ] {
        let h = make_app().await;
        let liz = member(&h, "Liz").await;
        let id = save_proposal(&h, "p-1", &liz, Duration::hours(2)).await;
        let session = session_of(&h, "s-liz", &liz).await;

        let (status, body) = decide(
            &h.app,
            &id,
            serde_json::json!({"session_id": session, "decision": decision}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["status"], expected.to_string(), "body: {body}");
        assert_eq!(
            body["executed"], false,
            "approving stages nothing: invariant 1 is that GIAP proposes and the \
             user disposes, and the executor is a later phase. Body: {body}"
        );
        assert_eq!(
            status_of(&h, &id).await,
            expected,
            "the drafts row must actually move -- this is the drafts machinery \
             the design says to reuse, not a second confirmation flow"
        );
    }
}

/// Ownership comes from `is_draft_decision_permitted`, not a local copy.
#[tokio::test]
async fn one_member_may_not_dispose_of_anothers_proposal() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let jerry = member(&h, "Jerry").await;
    let id = save_proposal(&h, "p-liz", &liz, Duration::hours(2)).await;
    let session = session_of(&h, "s-jerry", &jerry).await;

    let (status, body) = decide(
        &h.app,
        &id,
        serde_json::json!({"session_id": session, "decision": "approve"}),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("different household member"),
        "the refusal must be the shared reason string, so the audit trail and \
         this route cannot drift apart; body: {body}"
    );
    assert_eq!(
        status_of(&h, &id).await,
        DraftStatus::Pending,
        "a refused decision must leave the row alone. A 403 that had already \
         written is the worst of both answers"
    );
}

/// Not an unaudited draft-decision path: `get_live` ignores rows that are not proactive.
#[tokio::test]
async fn a_user_staged_draft_is_not_decidable_through_the_proposal_route() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    save_plain_draft(&h, "d-1", &liz).await;
    let session = session_of(&h, "s-liz", &liz).await;

    let (status, body) = decide(
        &h.app,
        "d-1",
        serde_json::json!({"session_id": session, "decision": "approve"}),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a plain draft must not be decidable here: this route records none of \
         the policy tally or audit entry `DraftMcpServer::decide` does, which is \
         the telemetry the enforce flip is waiting on. Body: {body}"
    );
    assert_eq!(
        status_of(&h, "d-1").await,
        DraftStatus::Pending,
        "and the draft is untouched"
    );

    // Vacuity control: the same call on a real proposal succeeds.
    let id = save_proposal(&h, "p-1", &liz, Duration::hours(2)).await;
    let (status, body) = decide(
        &h.app,
        &id,
        serde_json::json!({"session_id": session, "decision": "approve"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
async fn an_expired_proposal_cannot_be_disposed_of_here() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let id = save_proposal(&h, "p-stale", &liz, Duration::seconds(1)).await;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let session = session_of(&h, "s-liz", &liz).await;

    for decision in ["approve", "reject"] {
        let (status, body) = decide(
            &h.app,
            &id,
            serde_json::json!({"session_id": session, "decision": decision}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "invariant 7: an assistant that surfaces yesterday's suggestion has \
             failed twice, and approving one is worse. Body for {decision}: {body}"
        );
    }
    assert_eq!(status_of(&h, &id).await, DraftStatus::Pending);
}

/// A decision this route cannot read is never an approval.
#[tokio::test]
async fn an_unreadable_decision_is_refused_rather_than_guessed() {
    let h = make_app().await;
    let liz = member(&h, "Liz").await;
    let id = save_proposal(&h, "p-1", &liz, Duration::hours(2)).await;
    let session = session_of(&h, "s-liz", &liz).await;

    for body in [
        serde_json::json!({"session_id": session, "decision": "maybe"}),
        serde_json::json!({"session_id": session}),
        serde_json::json!({"decision": "approve"}),
        serde_json::json!({"session_id": session, "decision": "approve", "force": true}),
    ] {
        let (status, resp) = decide(&h.app, &id, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "`{body}` must be refused; resp: {resp}"
        );
    }
    assert_eq!(
        status_of(&h, &id).await,
        DraftStatus::Pending,
        "not one of those bodies may move the row"
    );
}

/// A collision would let `list_drafts`, which scopes by session, leak a proposal to a stranger.
#[test]
fn the_proposal_sentinel_session_cannot_be_an_engine_session_id() {
    assert!(
        PROPOSAL_SESSION_ID.starts_with("giap:"),
        "the sentinel lost its namespace: {PROPOSAL_SESSION_ID}"
    );
}
