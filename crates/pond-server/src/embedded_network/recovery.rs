//! Short-lived local approval of an exact replacement, consumed by the LAN phone.
use super::*;
use axum::{extract::Path as RoutePath, http::HeaderMap, Extension};
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// Recovery requests awaiting or carrying approval, and the approved ones the coordinator
/// then refused, kept briefly so the review that approved them can say why.
#[derive(Default)]
pub(super) struct Queue(
    RwLock<BTreeMap<String, Pending>>,
    RwLock<BTreeMap<String, Refused>>,
);
struct Pending {
    device: String,
    /// The name the household gave the phone, so the review is not of an opaque id.
    device_name: Option<String>,
    bearer_hash: [u8; 32],
    payload: serde_json::Value,
    expires: Instant,
    generation: u64,
    approved: bool,
}
/// An approved recovery the coordinator refused, under the id it was approved as.
struct Refused {
    device: String,
    device_name: Option<String>,
    key_preview: String,
    reason: &'static str,
    expires: Instant,
    generation: u64,
}
/// How long the review keeps showing a refused recovery: long enough for a household that
/// approved it and turned away to come back and read why.
const REFUSAL_SHOWN_FOR: Duration = Duration::from_secs(600);
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Review {
    id: String,
    device: String,
    device_name: Option<String>,
    /// The start of the replacement's machine key, which the phone shows too, so the
    /// household can check it is approving that phone and not one that asked first.
    key_preview: String,
    approved: bool,
    /// The coordinator's reason for refusing this approved recovery, e.g. `retired_capacity`.
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<&'static str>,
}

/// `mkey:` and the first sixteen hex digits, grouped in fours for reading aloud.
fn key_preview(machine_key: &str) -> String {
    let hex = machine_key.strip_prefix("mkey:").unwrap_or(machine_key);
    let groups: Vec<&str> = hex
        .get(..16)
        .unwrap_or(hex)
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect();
    format!("mkey:{}", groups.join(" "))
}
impl Queue {
    pub(super) fn clear(&self) {
        self.0.write().unwrap_or_else(|p| p.into_inner()).clear();
        self.1.write().unwrap_or_else(|p| p.into_inner()).clear();
    }
    pub(super) fn revoke(&self, device: &str) {
        self.0
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, p| p.device != device);
        self.forget_refusal(device);
    }
    fn prune(&self, generation: u64) {
        let now = Instant::now();
        self.0
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, p| p.expires > now && p.generation == generation);
        self.1
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, r| r.expires > now && r.generation == generation);
    }
    fn forget_refusal(&self, device: &str) {
        self.1
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, r| r.device != device);
    }
    /// Records that the coordinator refused `taken`, the approved recovery `id`, for `reason`.
    /// Only the review sees it: the phone's own request is spent and cannot be executed again.
    fn refused(&self, id: String, taken: Pending, reason: &'static str) {
        self.forget_refusal(&taken.device);
        self.1.write().unwrap_or_else(|p| p.into_inner()).insert(
            id,
            Refused {
                key_preview: key_preview(taken.payload["machineKey"].as_str().unwrap_or_default()),
                device: taken.device,
                device_name: taken.device_name,
                reason,
                expires: Instant::now() + REFUSAL_SHOWN_FOR,
                generation: taken.generation,
            },
        );
    }
    fn insert(
        &self,
        device: String,
        bearer_hash: [u8; 32],
        payload: serde_json::Value,
        device_name: Option<String>,
        generation: u64,
    ) -> Result<String, StatusCode> {
        self.prune(generation);
        let mut entries = self.0.write().unwrap_or_else(|p| p.into_inner());
        if entries.len() >= 32 && !entries.values().any(|p| p.device == device) {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        entries.retain(|_, p| p.device != device);
        self.forget_refusal(&device);
        let id = uuid::Uuid::new_v4().to_string();
        entries.insert(
            id.clone(),
            Pending {
                device,
                device_name,
                bearer_hash,
                payload,
                expires: Instant::now() + Duration::from_secs(120),
                generation,
                approved: false,
            },
        );
        Ok(id)
    }
    /// Whether the phone's own request `id` has been approved yet.
    fn inspect(
        &self,
        id: &str,
        device: &str,
        bearer_hash: [u8; 32],
        generation: u64,
    ) -> Result<serde_json::Value, StatusCode> {
        self.prune(generation);
        let entries = self.0.read().unwrap_or_else(|p| p.into_inner());
        let entry = entries
            .get(id)
            .filter(|p| p.device == device && p.bearer_hash == bearer_hash)
            .ok_or(StatusCode::NOT_FOUND)?;
        Ok(serde_json::json!({"approved":entry.approved}))
    }
    /// Consumes the phone's own request `id`, which must have been approved.
    fn take(
        &self,
        id: &str,
        device: &str,
        bearer_hash: [u8; 32],
        generation: u64,
    ) -> Result<Pending, StatusCode> {
        self.prune(generation);
        let mut entries = self.0.write().unwrap_or_else(|p| p.into_inner());
        let entry = entries
            .get(id)
            .filter(|p| p.device == device && p.bearer_hash == bearer_hash)
            .ok_or(StatusCode::NOT_FOUND)?;
        if !entry.approved {
            return Err(StatusCode::CONFLICT);
        }
        entries.remove(id).ok_or(StatusCode::NOT_FOUND)
    }
}

/// Who a phone request is from: its network id, the Pond's own id for it, and its bearer.
pub(super) struct Caller {
    pub device: String,
    pub device_id: String,
    pub bearer_hash: [u8; 32],
}

pub(super) async fn caller(
    headers: &HeaderMap,
    handshake: &dyn Handshake,
) -> Result<Caller, StatusCode> {
    let token = pond_api::middleware::extract_bearer_token(headers)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !handshake
        .validate_token(&token)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let caller = handshake
        .caller_for_token(&token)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let device = network_device(&caller.device_id).map_err(|_| StatusCode::FORBIDDEN)?;
    Ok(Caller {
        device,
        device_id: caller.device_id,
        bearer_hash: Sha256::digest(token.as_bytes()).into(),
    })
}
fn failed(error: anyhow::Error) -> StatusCode {
    tracing::warn!(%error, "remote recovery operation failed");
    StatusCode::SERVICE_UNAVAILABLE
}
async fn request(
    State(runtime): State<Arc<Runtime>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(handshake): Extension<Arc<dyn Handshake>>,
    Extension(registry): Extension<Arc<dyn DeviceRegistry + Send + Sync>>,
    headers: HeaderMap,
    Json(registration): Json<Registration>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    pond_api::network::require_lan(Some(ConnectInfo(peer))).map_err(|_| StatusCode::FORBIDDEN)?;
    let Caller {
        device,
        device_id,
        bearer_hash: hash,
    } = caller(&headers, handshake.as_ref()).await?;
    let lock = runtime.device_lock(&device);
    let _held = lock.lock().await;
    if runtime
        .pending_revocations()
        .map_err(failed)?
        .contains_key(&device)
    {
        return Err(StatusCode::CONFLICT);
    }
    let mut payload = runtime
        .registration_payload(&device, "phone", &registration)
        .map_err(failed)?;
    let generation = runtime.generation.load(Ordering::SeqCst);
    let old = runtime
        .authority(
            "inspect",
            serde_json::json!({"device":device,"role":"phone"}),
        )
        .await
        .map_err(failed)?;
    // Never stand down an active enrollment here; `execute` does it after approval. This route
    // needs only a LAN peer and a token, so revoking here would let anyone skip the approval.
    let status = old["status"].as_str().unwrap_or("unknown");
    if old["machineKey"].as_str() == Some(&registration.machine_key) {
        tracing::warn!(
            %device,
            "remote recovery refused: this device already holds the enrolled identity, so there is nothing to replace"
        );
        return Err(StatusCode::CONFLICT);
    }
    let Some(revision) = old["revision"].as_str().filter(|v| valid_device(v)) else {
        tracing::warn!(%device, %status, "remote recovery refused: the enrollment carries no usable revision to supersede");
        return Err(StatusCode::CONFLICT);
    };
    tracing::info!(%device, %status, "remote recovery awaiting local review");
    payload["expectedRevision"] = serde_json::Value::String(revision.to_owned());
    let device_name = match registry.get_device(&device_id).await {
        Ok(found) => found.map(|d| d.name),
        Err(error) => {
            tracing::warn!(%error, %device, "remote recovery: could not read the device's name for review");
            None
        }
    };
    let id = runtime
        .recovery
        .insert(device, hash, payload, device_name, generation)?;
    tracing::info!("remote recovery awaits local review");
    Ok(Json(serde_json::json!({"id":id})))
}
async fn review(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<Vec<Review>>, StatusCode> {
    local(peer)?;
    runtime
        .recovery
        .prune(runtime.generation.load(Ordering::SeqCst));
    let entries = runtime.recovery.0.read().unwrap_or_else(|p| p.into_inner());
    let refusals = runtime.recovery.1.read().unwrap_or_else(|p| p.into_inner());
    Ok(Json(
        entries
            .iter()
            .map(|(id, p)| Review {
                id: id.clone(),
                device: p.device.clone(),
                device_name: p.device_name.clone(),
                key_preview: key_preview(p.payload["machineKey"].as_str().unwrap_or_default()),
                approved: p.approved,
                refused: None,
            })
            .chain(refusals.iter().map(|(id, r)| Review {
                id: id.clone(),
                device: r.device.clone(),
                device_name: r.device_name.clone(),
                key_preview: r.key_preview.clone(),
                approved: true,
                refused: Some(r.reason),
            }))
            .collect(),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalApproval {}

async fn approve(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    RoutePath(id): RoutePath<String>,
    Json(_approval): Json<LocalApproval>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    runtime
        .recovery
        .prune(runtime.generation.load(Ordering::SeqCst));
    let mut entries = runtime
        .recovery
        .0
        .write()
        .unwrap_or_else(|p| p.into_inner());
    let entry = entries.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    entry.approved = true;
    tracing::info!("remote recovery approved locally; waiting for authenticated LAN phone");
    Ok(Json(serde_json::json!({"approved":true})))
}
async fn operation(
    runtime: Arc<Runtime>,
    peer: SocketAddr,
    handshake: Arc<dyn Handshake>,
    headers: HeaderMap,
    id: String,
    action: &str,
) -> Result<Json<serde_json::Value>, PhoneRefusal> {
    pond_api::network::require_lan(Some(ConnectInfo(peer))).map_err(|_| StatusCode::FORBIDDEN)?;
    let Caller {
        device,
        bearer_hash: hash,
        ..
    } = caller(&headers, handshake.as_ref()).await?;
    let lock = runtime.device_lock(&device);
    let _held = lock.lock().await;
    if runtime
        .pending_revocations()
        .map_err(failed)?
        .contains_key(&device)
    {
        return Err(StatusCode::CONFLICT.into());
    }
    let generation = runtime.generation.load(Ordering::SeqCst);
    if action == "execute" {
        let taken = runtime.recovery.take(&id, &device, hash, generation)?;
        if !runtime.config().map_err(failed)?.enabled {
            return Err(StatusCode::CONFLICT.into());
        }
        // Recorded before any coordinator call, as `register_phone` does: the coordinator holds
        // an enrollment for this device whatever becomes of the calls below, so a removal while
        // they are in flight, or after a crash, must revoke it.
        runtime.record_enrolled(&device).map_err(failed)?;
        // The coordinator replaces only a stood-down enrollment, so an active one is revoked first.
        let live = runtime
            .authority(
                "inspect",
                serde_json::json!({"device": device, "role": "phone"}),
            )
            .await
            .map_err(failed)?;
        let mut payload = taken.payload.clone();
        if live["status"].as_str() == Some("active") {
            tracing::info!(
                %device,
                "remote recovery: standing down the active enrollment so it can be replaced"
            );
            let stood_down = runtime
                .authority(
                    "revoke",
                    serde_json::json!({
                        "household":"", "device": device, "role":"phone", "action":"revoke",
                        "authId":"", "nodeKey":"", "nonce":"", "expires":0
                    }),
                )
                .await
                .map_err(|error| {
                    tracing::warn!(%error, %device, "remote recovery: could not stand down the existing enrollment");
                    StatusCode::SERVICE_UNAVAILABLE
                })?;

            // Standing down issues a new revision, so the one captured at request time is stale.
            let Some(revision) = stood_down["revision"].as_str().filter(|v| valid_device(v)) else {
                tracing::warn!(%device, "remote recovery: the stood-down enrollment reported no usable revision");
                return Err(StatusCode::SERVICE_UNAVAILABLE.into());
            };
            payload["expectedRevision"] = serde_json::Value::String(revision.to_owned());
        }

        // The payload was consumed before this ambiguous write and is never replayed; credentials
        // were checked under the revocation lock, and the coordinator checks the revision.
        let result = match runtime.authority("replace", payload).await {
            Ok(result) => result,
            Err(error) if retired_capacity(&error) => {
                tracing::warn!(
                    %error,
                    %device,
                    "remote recovery refused: the coordinator holds as many retired enrollments as it allows for this household, until older ones expire"
                );
                runtime.recovery.refused(id, taken, RETIRED_CAPACITY);
                return Err(PhoneRefusal::retired_capacity());
            }
            Err(error) => return Err(failed(error).into()),
        };
        runtime
            .ensure_not_revoked_meanwhile(&device)
            .map_err(|error| {
                tracing::warn!(%error, %device, "remote recovery superseded by a revocation");
                StatusCode::CONFLICT
            })?;
        tracing::info!("remote recovery registration completed");
        return Ok(Json(result));
    }
    let review = runtime.recovery.inspect(&id, &device, hash, generation)?;
    if action == "cancel" {
        runtime
            .recovery
            .0
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        tracing::info!("remote recovery cancelled");
        return Ok(Json(serde_json::json!({"cancelled":true})));
    }
    Ok(Json(review))
}
macro_rules! phone_handler {
    ($name:ident,$action:literal) => {
        async fn $name(
            State(runtime): State<Arc<Runtime>>,
            ConnectInfo(peer): ConnectInfo<SocketAddr>,
            Extension(handshake): Extension<Arc<dyn Handshake>>,
            headers: HeaderMap,
            RoutePath(id): RoutePath<String>,
        ) -> Result<Json<serde_json::Value>, PhoneRefusal> {
            operation(runtime, peer, handshake, headers, id, $action).await
        }
    };
}
phone_handler!(poll, "poll");
phone_handler!(execute, "execute");
phone_handler!(cancel, "cancel");
pub(super) fn local_routes() -> Router<Arc<Runtime>> {
    Router::new()
        .route("/api/v1/remote-access/recovery-requests", get(review))
        .route(
            "/api/v1/remote-access/recovery-requests/{id}",
            axum::routing::post(approve),
        )
}
pub(super) fn phone_routes(
    handshake: Arc<dyn Handshake>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
) -> Router<Arc<Runtime>> {
    Router::new()
        .route(
            "/api/v1/remote-access/recovery",
            axum::routing::post(request),
        )
        .route(
            "/api/v1/remote-access/recovery/{id}",
            get(poll).post(execute).delete(cancel),
        )
        .layer(Extension(handshake))
        .layer(Extension(registry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_revalidates_real_credentials_and_requires_lan() {
        use pond_core::security::ports::handshake::HandshakeRequest;
        use pond_infra::{db::Database, sqlite_handshake::SqliteHandshakeAdapter};
        let data = tempfile::tempdir().unwrap();
        let db = Database::init(data.path()).await.unwrap();
        let handshake = Arc::new(SqliteHandshakeAdapter::new(db.system.clone(), None));
        let code = handshake.issue_pairing_code().await.unwrap();
        let paired = handshake
            .handshake(HandshakeRequest {
                client_id: "phone000000000001".into(),
                client_type: "gotg".into(),
                client_version: "test".into(),
                pairing_code: Some(code.code),
            })
            .await
            .unwrap();
        let token = paired.session_token.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let Caller {
            device,
            bearer_hash: hash,
            ..
        } = caller(&headers, handshake.as_ref()).await.unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let id = runtime
            .recovery
            .insert(device.clone(), hash, serde_json::Value::Null, None, 0)
            .unwrap();
        runtime
            .recovery
            .0
            .write()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .approved = true;
        // Remote completion is rejected even with a genuine paired token.
        assert_eq!(
            operation(
                runtime.clone(),
                "100.64.0.2:1234".parse().unwrap(),
                handshake.clone(),
                headers.clone(),
                id.clone(),
                "execute"
            )
            .await
            .unwrap_err(),
            StatusCode::FORBIDDEN.into()
        );
        handshake.revoke_token(&token).await.unwrap();
        assert_eq!(
            operation(
                runtime.clone(),
                "127.0.0.1:1234".parse().unwrap(),
                handshake,
                headers,
                id.clone(),
                "execute"
            )
            .await
            .unwrap_err(),
            StatusCode::UNAUTHORIZED.into()
        );
        // Neither denial consumed or contacted an enrollment helper.
        assert!(runtime.recovery.inspect(&id, &device, hash, 0).is_ok());
    }

    #[test]
    fn approval_is_bound_consumed_expired_and_invalidated() {
        let queue = Queue::default();
        let id = queue
            .insert(
                "device".into(),
                [1; 32],
                serde_json::json!({"expectedRevision":"first"}),
                None,
                1,
            )
            .unwrap();
        assert_eq!(
            queue.take(&id, "device", [1; 32], 1).err(),
            Some(StatusCode::CONFLICT)
        );
        queue.0.write().unwrap().get_mut(&id).unwrap().approved = true;
        assert_eq!(
            queue.take(&id, "other", [1; 32], 1).err(),
            Some(StatusCode::NOT_FOUND)
        );
        assert_eq!(
            queue.take(&id, "device", [2; 32], 1).err(),
            Some(StatusCode::NOT_FOUND)
        );
        assert_eq!(
            queue.take(&id, "device", [1; 32], 1).unwrap().payload["expectedRevision"],
            "first"
        );
        assert_eq!(
            queue.take(&id, "device", [1; 32], 1).err(),
            Some(StatusCode::NOT_FOUND)
        );
        let expired = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, None, 1)
            .unwrap();
        queue.0.write().unwrap().get_mut(&expired).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert!(queue.inspect(&expired, "device", [1; 32], 1).is_err());
        let stale = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, None, 1)
            .unwrap();
        assert!(queue.inspect(&stale, "device", [1; 32], 2).is_err());
        let revoked = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, None, 2)
            .unwrap();
        queue.revoke("device");
        assert!(queue.inspect(&revoked, "device", [1; 32], 2).is_err());
    }
    #[test]
    fn queue_is_bounded_and_a_new_request_invalidates_old_approval() {
        let queue = Queue::default();
        let old = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, None, 1)
            .unwrap();
        queue.0.write().unwrap().get_mut(&old).unwrap().approved = true;
        let new = queue
            .insert("device".into(), [2; 32], serde_json::Value::Null, None, 1)
            .unwrap();
        assert!(queue.take(&old, "device", [1; 32], 1).is_err());
        assert_eq!(
            queue.inspect(&new, "device", [2; 32], 1).unwrap()["approved"],
            false
        );
        for i in 1..32 {
            queue
                .insert(
                    format!("device{i}"),
                    [1; 32],
                    serde_json::Value::Null,
                    None,
                    1,
                )
                .unwrap();
        }
        assert_eq!(
            queue
                .insert("extra".into(), [1; 32], serde_json::Value::Null, None, 1)
                .unwrap_err(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    #[tokio::test]
    async fn local_approval_rejects_remote_and_forwarded_requests() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let id = runtime
            .recovery
            .insert("device".into(), [1; 32], serde_json::Value::Null, None, 0)
            .unwrap();
        let credential = pond_api::host_guard::HostCredential::generate();
        let router = management(runtime.clone(), credential.clone());
        for peer in ["100.64.0.2:1234", "192.168.1.2:1234"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .header("content-type", "application/json")
                        .uri(format!("/api/v1/remote-access/recovery-requests/{id}"))
                        .header("x-forwarded-for", "127.0.0.1")
                        .header(pond_api::host_guard::CREDENTIAL_HEADER, credential.as_str())
                        .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()))
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .header("content-type", "application/json")
                    .uri(format!("/api/v1/remote-access/recovery-requests/{id}"))
                    .header(pond_api::host_guard::CREDENTIAL_HEADER, credential.as_str())
                    .extension(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            runtime.recovery.inspect(&id, "device", [1; 32], 0).unwrap()["approved"],
            true
        );
    }

    #[test]
    fn the_review_shows_a_readable_key_start() {
        assert_eq!(
            key_preview(&format!("mkey:0123456789abcdef{}", "0".repeat(48))),
            "mkey:0123 4567 89ab cdef"
        );
        assert_eq!(key_preview(""), "mkey:");
    }

    /// A phone paired with a real handshake, and the bearer it presents.
    async fn paired_phone(data: &std::path::Path) -> (Arc<dyn Handshake>, HeaderMap) {
        use pond_core::security::ports::handshake::HandshakeRequest;
        use pond_infra::{db::Database, sqlite_handshake::SqliteHandshakeAdapter};
        let db = Database::init(data).await.unwrap();
        let handshake = Arc::new(SqliteHandshakeAdapter::new(db.system.clone(), None));
        let code = handshake.issue_pairing_code().await.unwrap();
        let paired = handshake
            .handshake(HandshakeRequest {
                client_id: "phone000000000002".into(),
                client_type: "gotg".into(),
                client_version: "test".into(),
                pairing_code: Some(code.code),
            })
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", paired.session_token.unwrap())
                .parse()
                .unwrap(),
        );
        (handshake, headers)
    }

    fn registration() -> Registration {
        Registration {
            auth_url: "https://coord.example/register/abcdefghijklmnop".into(),
            node_key: String::new(),
            machine_key: format!("mkey:{}", "a".repeat(64)),
        }
    }

    const STOOD_DOWN: &str = r#"{"status":"revoked","role":"phone","machineKey":"mkey:old","revision":"rev-0123456789abcdef"}"#;
    const AT_CAPACITY: &str = r#"{"refused":"retired_capacity"}"#;

    async fn error_of(refusal: impl IntoResponse) -> (StatusCode, serde_json::Value) {
        let response = refusal.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn a_phone_whose_household_is_out_of_retired_records_is_told_so() {
        let helper = super::super::tests::FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let (handshake, headers) = paired_phone(data.path()).await;
        helper.answer("inspect", 0, STOOD_DOWN);
        helper.answer("replace", 3, AT_CAPACITY);
        let refused = register_phone(
            State(runtime),
            ConnectInfo("192.168.1.20:1234".parse().unwrap()),
            Extension(handshake),
            Extension(super::super::tests::no_profiles()),
            Extension(super::super::tests::no_attribution()),
            headers,
            Json(registration()),
        )
        .await
        .unwrap_err();
        assert_eq!(helper.calls(), ["inspect", "replace"]);
        // 503, not the 409 a phone reading only the status would take to mean "recover".
        let (status, body) = error_of(refused).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "retired_capacity");
    }

    #[tokio::test]
    async fn an_approved_recovery_refused_for_retired_records_says_why_on_the_review() {
        let helper = super::super::tests::FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let (handshake, headers) = paired_phone(data.path()).await;
        let Caller {
            device,
            bearer_hash: hash,
            ..
        } = caller(&headers, handshake.as_ref()).await.unwrap();
        let payload = runtime
            .registration_payload(&device, "phone", &registration())
            .unwrap();
        let id = runtime
            .recovery
            .insert(device.clone(), hash, payload, Some("A57".into()), 0)
            .unwrap();
        let reviewed = || async {
            let Json(entries) = review(
                State(runtime.clone()),
                Ok(ConnectInfo("127.0.0.1:1234".parse().unwrap())),
            )
            .await
            .unwrap();
            serde_json::to_value(entries).unwrap()
        };
        let waiting = reviewed().await;
        assert!(waiting[0].get("refused").is_none(), "{waiting}");
        runtime
            .recovery
            .0
            .write()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .approved = true;

        helper.answer("inspect", 0, STOOD_DOWN);
        helper.answer("replace", 3, AT_CAPACITY);
        let refused = operation(
            runtime.clone(),
            "192.168.1.20:1234".parse().unwrap(),
            handshake,
            headers,
            id.clone(),
            "execute",
        )
        .await
        .unwrap_err();
        assert_eq!(refused, PhoneRefusal::retired_capacity());
        assert_eq!(
            error_of(refused).await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({"error": "retired_capacity"})
            )
        );

        let shown = reviewed().await;
        assert_eq!(shown.as_array().unwrap().len(), 1, "{shown}");
        assert_eq!(shown[0]["id"], id.as_str());
        assert_eq!(shown[0]["deviceName"], "A57");
        assert_eq!(shown[0]["refused"], "retired_capacity");
        // The phone's request was spent; the notice is for the review alone.
        assert_eq!(
            runtime.recovery.inspect(&id, &device, hash, 0).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        // A fresh request from the same phone replaces the notice.
        runtime
            .recovery
            .insert(device, hash, serde_json::Value::Null, None, 0)
            .unwrap();
        let fresh = reviewed().await;
        assert_eq!(fresh.as_array().unwrap().len(), 1, "{fresh}");
        assert!(fresh[0].get("refused").is_none(), "{fresh}");
    }
}
