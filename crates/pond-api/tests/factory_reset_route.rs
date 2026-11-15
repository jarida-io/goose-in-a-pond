//! `POST /api/v1/admin/factory-reset` over the real pairing adapter: who may reset, the typed
//! name and held credit that refuse it, and the household it erases. `factory_reset.rs` in
//! pond-core covers the service's own sequencing.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use axum::{Extension, Router};
use pond_api::host_guard::{HostCredential, CREDENTIAL_HEADER};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger;
use pond_core::mesh::ports::credit_ledger::CreditLedger as _;
use pond_core::network::ports::personal_data_wipe::PersonalDataWipe;
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::personal_data_wipe::SystemDataWipe;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

const POND_NAME: &str = "The Kitchen Pond";

struct Pond {
    router: Router,
    state: Arc<AppState>,
    handshake: Arc<SqliteHandshakeAdapter>,
    credential: HostCredential,
    ledger: Arc<MockCreditLedger>,
    /// The main member's phone.
    main: String,
    _dir: tempfile::TempDir,
}

/// Pairing's verify limiter is per address and process-wide, so each pond pairs from its own.
fn host_peer() -> SocketAddr {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    SocketAddr::from(([127, 0, 1, NEXT.fetch_add(1, Ordering::Relaxed)], 40_000))
}

/// A named pond whose main member, Jerry, paired the first phone.
async fn pond() -> Pond {
    pond_wiping(None).await
}

/// As [`pond`], erasing through `wipe` instead of the real one.
async fn pond_wiping(wipe: Option<Arc<dyn PersonalDataWipe>>) -> Pond {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let settings = Arc::new(SqliteSettingsRepository::new(state.db.system.clone()));
    let mut named = settings.get().await.unwrap();
    named.home_name = POND_NAME.to_string();
    settings.update(&named).await.unwrap();
    state.settings_repo = settings;
    state.data_wipe = Some(wipe.unwrap_or_else(|| {
        Arc::new(SystemDataWipe::new(
            state.db.clone(),
            dir.path(),
            None,
            None,
        ))
    }));
    state
        .onboarding_repo
        .save_step(OnboardingStep::Completed)
        .await
        .unwrap();
    let ledger = Arc::new(MockCreditLedger::new());
    state.credit_ledger = ledger.clone();
    let jerry = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "J".into(),
        })
        .await
        .unwrap()
        .id;

    let state = Arc::new(state);
    let credential = HostCredential::generate();
    let router = build_router(state.clone(), "pond-desktop/dist".into())
        .layer(Extension(credential.clone()))
        .layer(MockConnectInfo(host_peer()));
    let main = paired(&router, &handshake, "jerry-phone", Some(&jerry)).await;
    let lock = handshake.pairing_lock().await.unwrap();
    assert_eq!(lock.owner_profile_id.as_deref(), Some(jerry.as_str()));
    Pond {
        router,
        state,
        handshake,
        credential,
        ledger,
        main,
        _dir: dir,
    }
}

/// A phone paired with a host-approved code, so the pairing lock lets it straight in.
async fn paired(
    router: &Router,
    handshake: &SqliteHandshakeAdapter,
    client_id: &str,
    profile: Option<&str>,
) -> String {
    let code = handshake
        .issue_pairing_code_for(profile)
        .await
        .unwrap()
        .code;
    handshake.mark_pairing_code_approved(&code).await.unwrap();
    let (status, body) = pair_phone(router, &code, client_id, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session_token"].as_str().unwrap().to_string()
}

async fn reset(pond: &Pond, token: &str, host: bool, pond_name: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/factory-reset")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if host {
        request = request.header(CREDENTIAL_HEADER, pond.credential.as_str());
    }
    let response = pond
        .router
        .clone()
        .oneshot(
            request
                .body(Body::from(json!({ "pond_name": pond_name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn members(pond: &Pond) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM profiles")
        .fetch_one(&pond.state.db.system)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_main_member_typing_the_real_name_wipes_the_household() {
    let pond = pond().await;

    let (status, body) = reset(&pond, &pond.main, false, POND_NAME).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["routines"].as_array().unwrap().len(), 0, "{body}");
    assert_eq!(
        members(&pond).await,
        0,
        "the household's profiles must be gone"
    );
}

struct BrokenWipe;
#[async_trait::async_trait]
impl PersonalDataWipe for BrokenWipe {
    async fn wipe(&self) -> anyhow::Result<()> {
        anyhow::bail!("disk I/O error")
    }
}

#[tokio::test]
async fn a_failed_erase_answers_500_and_leaves_onboarding_as_it_was() {
    let pond = pond_wiping(Some(Arc::new(BrokenWipe))).await;

    let (status, body) = reset(&pond, &pond.main, false, POND_NAME).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "factory_reset_failed", "{body}");
    assert_eq!(body["step"], "erasing_data", "{body}");
    assert!(
        pond.state.onboarding_repo.is_complete().await.unwrap(),
        "a reset that erased nothing must not reopen setup over the household"
    );
    assert_eq!(members(&pond).await, 1);
}

#[tokio::test]
async fn an_unattributed_phone_cannot_reset_a_one_member_pond() {
    let pond = pond().await;
    let stranger = paired(&pond.router, &pond.handshake, "stranger-phone", None).await;

    let (status, body) = reset(&pond, &stranger, false, POND_NAME).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "factory_reset_main_member_only", "{body}");
    assert_eq!(
        members(&pond).await,
        1,
        "a refused reset must not touch the household"
    );
}

#[tokio::test]
async fn an_owner_by_role_who_is_not_the_main_member_cannot_reset() {
    let pond = pond().await;
    let amina = pond
        .state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Amina".into(),
            avatar_emoji: "A".into(),
        })
        .await
        .unwrap()
        .id;
    let hers = paired(&pond.router, &pond.handshake, "amina-phone", Some(&amina)).await;
    sqlx::query("UPDATE profiles SET role = 'owner' WHERE id = ?")
        .bind(&amina)
        .execute(&pond.state.db.system)
        .await
        .unwrap();

    let (status, body) = reset(&pond, &hers, false, POND_NAME).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "factory_reset_main_member_only", "{body}");
    assert_eq!(members(&pond).await, 2);
}

#[tokio::test]
async fn the_host_resets_with_its_credential() {
    let pond = pond().await;
    let stranger = paired(&pond.router, &pond.handshake, "desktop", None).await;

    let (status, body) = reset(&pond, &stranger, true, POND_NAME).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(members(&pond).await, 0);
}

#[tokio::test]
async fn an_unnamed_pond_refuses_rather_than_let_an_empty_typed_name_match() {
    let pond = pond().await;
    sqlx::query("DELETE FROM settings WHERE key = 'home_name'")
        .execute(&pond.state.db.system)
        .await
        .unwrap();

    let (status, body) = reset(&pond, &pond.main, false, "").await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "pond_has_no_name");
    assert_eq!(
        members(&pond).await,
        1,
        "a refused reset must not touch the household"
    );
}

#[tokio::test]
async fn a_mistyped_name_refuses_with_409_and_touches_nothing() {
    let pond = pond().await;

    let (status, body) = reset(&pond, &pond.main, false, "Not The Right Name").await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "pond_name_mismatch");
    assert_eq!(
        members(&pond).await,
        1,
        "a refused reset must not touch the household"
    );
}

#[tokio::test]
async fn a_held_mesh_credit_balance_refuses_with_409() {
    let pond = pond().await;
    pond.ledger
        .credit(PeerId::from([9u8; 32]), Millisats::new(500))
        .await
        .unwrap();

    let (status, body) = reset(&pond, &pond.main, false, POND_NAME).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "payments_held");
    assert_eq!(
        members(&pond).await,
        1,
        "a refused reset must not touch the household"
    );
}
