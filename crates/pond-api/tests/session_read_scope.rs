//! A conversation is read back only by its own member's devices, or by anyone when it is nobody's:
//! another member's history and pictures read as missing, by id as much as by listing.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::build_router;
use pond_core::models::domain::message::{ChatMessage, ImageAttachment};
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::session::{
    IdentificationSource, SessionIdentity, SessionMessage,
};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use tower::ServiceExt;

/// A one-pixel PNG, so the stored attachment has real bytes to serve.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// Each token is one phone; `nobody-token`'s phone was paired to no member.
struct Phones {
    amina: String,
    otieno: String,
    nobody: String,
}

#[async_trait]
impl Handshake for Phones {
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used by this fixture")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(matches!(
            token,
            "amina-token" | "otieno-token" | "nobody-token"
        ))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let device = match token {
            "amina-token" => &self.amina,
            "otieno-token" => &self.otieno,
            "nobody-token" => &self.nobody,
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
    storage: Arc<SqliteSessionStorage>,
    _dir: tempfile::TempDir,
}

async fn register_phone(state: &pond_api::AppState, name: &str) -> String {
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

/// Two members with a phone each, a third phone paired to nobody, and one conversation each:
/// Amina's, Otieno's and a shared one, every one carrying a picture.
async fn pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    let storage = Arc::new(
        SqliteSessionStorage::new(pool.clone()).with_attachment_dir(built.dir.path().join("att")),
    );
    state.session_storage = storage.clone();
    state.skip_onboarding = true;

    let mut phones = Vec::new();
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
        let phone = register_phone(&state, &format!("{name}'s phone")).await;
        SqliteDeviceAttribution::new(pool.clone())
            .set_device_profile(&phone, Some(&member))
            .await
            .unwrap();
        phones.push((member, phone));
    }
    let nobody = register_phone(&state, "the hall tablet").await;
    let (amina, amina_phone) = phones[0].clone();
    let (otieno, otieno_phone) = phones[1].clone();

    for (session, owner, words) in [
        (
            "amina-chat",
            Some(&amina),
            "Amina asked about her biopsy results",
        ),
        (
            "otieno-chat",
            Some(&otieno),
            "Otieno asked about his overdraft",
        ),
        ("shared-chat", None, "Somebody asked when the bins go out"),
    ] {
        storage.create_session(session.into()).await.unwrap();
        let mut message = ChatMessage::user(words);
        message.images = vec![ImageAttachment {
            data: PIXEL_PNG.into(),
            mime_type: "image/png".into(),
        }];
        storage
            .add_message(
                session.into(),
                SessionMessage::new(format!("{session}-m1"), session.into(), message),
            )
            .await
            .unwrap();
        if let Some(member) = owner {
            storage
                .set_session_identity(
                    session,
                    &SessionIdentity {
                        profile_id: Some(member.clone()),
                        source: IdentificationSource::PairedDevice,
                        confidence: None,
                    },
                )
                .await
                .unwrap();
        }
    }

    state.handshake = Arc::new(Phones {
        amina: amina_phone,
        otieno: otieno_phone,
        nobody,
    });
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        storage,
        _dir: built.dir,
    }
}

async fn get(pond: &Pond, token: &str, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn attachment_of(pond: &Pond, session: &str) -> String {
    let attachments = pond
        .storage
        .list_session_attachments(session)
        .await
        .unwrap();
    assert_eq!(
        attachments.len(),
        1,
        "{session} lost its picture in the fixture"
    );
    attachments[0].id.clone()
}

#[tokio::test]
async fn a_member_reads_their_own_and_shared_conversations_never_anothers() {
    let p = pond().await;

    let (status, body) = get(&p, "amina-token", "/api/v1/sessions/amina-chat/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("biopsy"), "{body}");

    let (status, body) = get(&p, "amina-token", "/api/v1/sessions/shared-chat/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("bins go out"), "{body}");

    let (status, body) = get(&p, "amina-token", "/api/v1/sessions/otieno-chat/messages").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        !body.contains("overdraft"),
        "Amina's phone read Otieno's words: {body}"
    );

    // Refused exactly as a conversation that does not exist, so an id can't be probed.
    let (missing_status, missing_body) =
        get(&p, "amina-token", "/api/v1/sessions/no-such-chat/messages").await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert_eq!(
        body.replace("otieno-chat", "ID"),
        missing_body.replace("no-such-chat", "ID"),
        "a refusal must not tell another member's conversation from a missing one"
    );

    // Control: Otieno's own phone reads it, so the refusal above is Amina's scope.
    let (status, body) = get(&p, "otieno-token", "/api/v1/sessions/otieno-chat/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("overdraft"), "{body}");
}

#[tokio::test]
async fn a_phone_paired_to_nobody_reads_only_conversations_that_are_nobodys() {
    let p = pond().await;

    let (status, body) = get(&p, "nobody-token", "/api/v1/sessions/shared-chat/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for member_chat in ["amina-chat", "otieno-chat"] {
        let (status, body) = get(
            &p,
            "nobody-token",
            &format!("/api/v1/sessions/{member_chat}/messages"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{member_chat}: {body}");
        assert!(
            !body.contains("biopsy") && !body.contains("overdraft"),
            "{member_chat}: {body}"
        );
    }
}

#[tokio::test]
async fn a_conversations_pictures_follow_its_messages() {
    let p = pond().await;
    let otieno_picture = attachment_of(&p, "otieno-chat").await;
    let shared_picture = attachment_of(&p, "shared-chat").await;
    let url = |session: &str, id: &str| format!("/api/v1/sessions/{session}/attachments/{id}");

    let (status, _) = get(&p, "amina-token", &url("otieno-chat", &otieno_picture)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "Amina's phone fetched Otieno's picture"
    );
    let (status, _) = get(&p, "nobody-token", &url("otieno-chat", &otieno_picture)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unattributed phone fetched Otieno's picture"
    );

    // Controls: the owner and a shared conversation's readers get the bytes.
    let (status, body) = get(&p, "otieno-token", &url("otieno-chat", &otieno_picture)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = get(&p, "amina-token", &url("shared-chat", &shared_picture)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
