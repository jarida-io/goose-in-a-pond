//! Short-lived local approval of an exact replacement, consumed by the LAN phone.
use super::*;
use axum::{extract::Path as RoutePath, http::HeaderMap, Extension};
use pond_core::security::ports::handshake::Handshake;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Queue(RwLock<BTreeMap<String, Pending>>);
struct Pending {
    device: String,
    bearer_hash: [u8; 32],
    payload: serde_json::Value,
    expires: Instant,
    generation: u64,
    approved: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Review {
    id: String,
    device: String,
    approved: bool,
}
impl Queue {
    pub(super) fn clear(&self) {
        self.0.write().unwrap_or_else(|p| p.into_inner()).clear();
    }
    pub(super) fn revoke(&self, device: &str) {
        self.0
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, p| p.device != device);
    }
    fn prune(&self, generation: u64) {
        let now = Instant::now();
        self.0
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, p| p.expires > now && p.generation == generation);
    }
    fn insert(
        &self,
        device: String,
        bearer_hash: [u8; 32],
        payload: serde_json::Value,
        generation: u64,
    ) -> Result<String, StatusCode> {
        self.prune(generation);
        let mut entries = self.0.write().unwrap_or_else(|p| p.into_inner());
        if entries.len() >= 32 && !entries.values().any(|p| p.device == device) {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        entries.retain(|_, p| p.device != device);
        let id = uuid::Uuid::new_v4().to_string();
        entries.insert(
            id.clone(),
            Pending {
                device,
                bearer_hash,
                payload,
                expires: Instant::now() + Duration::from_secs(120),
                generation,
                approved: false,
            },
        );
        Ok(id)
    }
    fn inspect(
        &self,
        id: &str,
        device: &str,
        bearer_hash: [u8; 32],
        generation: u64,
        consume: bool,
    ) -> Result<serde_json::Value, StatusCode> {
        self.prune(generation);
        let mut entries = self.0.write().unwrap_or_else(|p| p.into_inner());
        let entry = entries
            .get(id)
            .filter(|p| p.device == device && p.bearer_hash == bearer_hash)
            .ok_or(StatusCode::NOT_FOUND)?;
        if !consume {
            return Ok(serde_json::json!({"approved":entry.approved}));
        }
        if !entry.approved {
            return Err(StatusCode::CONFLICT);
        }
        Ok(entries.remove(id).ok_or(StatusCode::NOT_FOUND)?.payload)
    }
}

pub(super) async fn caller(
    headers: &HeaderMap,
    handshake: &dyn Handshake,
) -> Result<(String, [u8; 32]), StatusCode> {
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
    Ok((device, Sha256::digest(token.as_bytes()).into()))
}
fn failed(error: anyhow::Error) -> StatusCode {
    // Reports the cause. It used to discard it, which is how a replacement that
    // failed on a stale revision read as "remote recovery operation failed" and
    // reached the user as "remote access is not set up on this Pond".
    tracing::warn!(%error, "remote recovery operation failed");
    StatusCode::SERVICE_UNAVAILABLE
}
async fn request(
    State(runtime): State<Arc<Runtime>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(handshake): Extension<Arc<dyn Handshake>>,
    headers: HeaderMap,
    Json(registration): Json<Registration>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    pond_api::network::require_lan(Some(ConnectInfo(peer))).map_err(|_| StatusCode::FORBIDDEN)?;
    let _guard = runtime.revocations.lock().await;
    let (device, hash) = caller(&headers, handshake.as_ref()).await?;
    if runtime
        .pending_revocations()
        .map_err(failed)?
        .contains(&device)
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
    // An active enrollment is no longer refused here. The coordinator will not
    // replace one, so it has to be stood down first -- but that is a step the
    // pond can take on the household's behalf, and `execute` takes it once a
    // person has approved. Asking the user to know that, and to perform it
    // through a sign-out, made the one case replacement exists for the one case
    // it could not serve.
    //
    // It is NOT done here. This route needs only a LAN peer and a bearer token,
    // so revoking at request time would let anyone who has both knock the
    // household's phone off the tailnet without approving anything.
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
    // Captured before any revocation, and still correct after one: revoking
    // preserves the revision, so the record this replacement supersedes is
    // pinned from the moment the user was asked about it.
    tracing::info!(%device, %status, "remote recovery awaiting local review");
    payload["expectedRevision"] = serde_json::Value::String(revision.to_owned());
    let id = runtime.recovery.insert(device, hash, payload, generation)?;
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
    Ok(Json(
        entries
            .iter()
            .map(|(id, p)| Review {
                id: id.clone(),
                device: p.device.clone(),
                approved: p.approved,
            })
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
    let _guard = runtime.revocations.lock().await;
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
) -> Result<Json<serde_json::Value>, StatusCode> {
    pond_api::network::require_lan(Some(ConnectInfo(peer))).map_err(|_| StatusCode::FORBIDDEN)?;
    let _guard = runtime.revocations.lock().await;
    let (device, hash) = caller(&headers, handshake.as_ref()).await?;
    if runtime
        .pending_revocations()
        .map_err(failed)?
        .contains(&device)
    {
        return Err(StatusCode::CONFLICT);
    }
    let generation = runtime.generation.load(Ordering::SeqCst);
    let payload = runtime
        .recovery
        .inspect(&id, &device, hash, generation, action == "execute")?;
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
    if action == "execute" {
        if !runtime.config().map_err(failed)?.enabled {
            return Err(StatusCode::CONFLICT);
        }
        // The coordinator replaces an enrollment that has been stood down, not a
        // live one, so a device whose record is still active needs revoking
        // first. Two steps either way; the difference is that the pond takes
        // them rather than telling the user to sign out and work it out.
        //
        // Here rather than at request time, because this runs only after a
        // person approved it at the pond. Revoking on request alone would hand
        // anyone with a LAN address and a token a way to drop the household's
        // remote access without approving anything.
        //
        // Revoking preserves the record's revision, so the `expectedRevision`
        // captured when the user was asked still pins the same record.
        let live = runtime
            .authority(
                "inspect",
                serde_json::json!({"device": device, "role": "phone"}),
            )
            .await
            .map_err(failed)?;
        let mut payload = payload;
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
                    // Nothing has changed yet, so the approval can simply be
                    // used again once coordination is reachable.
                    tracing::warn!(%error, %device, "remote recovery: could not stand down the existing enrollment");
                    StatusCode::SERVICE_UNAVAILABLE
                })?;

            // Re-read the revision rather than assuming the old one survived.
            // It does not: standing an enrollment down gives it a new revision,
            // so the `expectedRevision` captured when the user was asked is
            // stale by the time the replacement is sent, and the coordinator
            // refuses it -- after the old enrollment is already gone.
            let Some(revision) = stood_down["revision"].as_str().filter(|v| valid_device(v)) else {
                tracing::warn!(%device, "remote recovery: the stood-down enrollment reported no usable revision");
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            };
            payload["expectedRevision"] = serde_json::Value::String(revision.to_owned());
        }

        // The approved payload is consumed before a possibly ambiguous external write.
        // A failed response is never replayed. Credentials are checked after taking
        // the same lock used to queue revocation, and the coordinator checks revision.
        let result = runtime
            .authority("replace", payload)
            .await
            .map_err(failed)?;
        tracing::info!("remote recovery registration completed");
        return Ok(Json(result));
    }
    Ok(Json(payload))
}
macro_rules! phone_handler {
    ($name:ident,$action:literal) => {
        async fn $name(
            State(runtime): State<Arc<Runtime>>,
            ConnectInfo(peer): ConnectInfo<SocketAddr>,
            Extension(handshake): Extension<Arc<dyn Handshake>>,
            headers: HeaderMap,
            RoutePath(id): RoutePath<String>,
        ) -> Result<Json<serde_json::Value>, StatusCode> {
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
pub(super) fn phone_routes(handshake: Arc<dyn Handshake>) -> Router<Arc<Runtime>> {
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
        let (device, hash) = caller(&headers, handshake.as_ref()).await.unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let id = runtime
            .recovery
            .insert(device.clone(), hash, serde_json::Value::Null, 0)
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
            StatusCode::FORBIDDEN
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
            StatusCode::UNAUTHORIZED
        );
        // Neither denial consumed or contacted an enrollment helper.
        assert!(runtime
            .recovery
            .inspect(&id, &device, hash, 0, false)
            .is_ok());
    }

    #[test]
    fn approval_is_bound_consumed_expired_and_invalidated() {
        let queue = Queue::default();
        let id = queue
            .insert(
                "device".into(),
                [1; 32],
                serde_json::json!({"expectedRevision":"first"}),
                1,
            )
            .unwrap();
        assert_eq!(
            queue.inspect(&id, "device", [1; 32], 1, true).unwrap_err(),
            StatusCode::CONFLICT
        );
        queue.0.write().unwrap().get_mut(&id).unwrap().approved = true;
        assert_eq!(
            queue.inspect(&id, "other", [1; 32], 1, true).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            queue.inspect(&id, "device", [2; 32], 1, true).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            queue.inspect(&id, "device", [1; 32], 1, true).unwrap()["expectedRevision"],
            "first"
        );
        assert_eq!(
            queue.inspect(&id, "device", [1; 32], 1, true).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        let expired = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, 1)
            .unwrap();
        queue.0.write().unwrap().get_mut(&expired).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert!(queue
            .inspect(&expired, "device", [1; 32], 1, false)
            .is_err());
        let stale = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, 1)
            .unwrap();
        assert!(queue.inspect(&stale, "device", [1; 32], 2, false).is_err());
        let revoked = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, 2)
            .unwrap();
        queue.revoke("device");
        assert!(queue
            .inspect(&revoked, "device", [1; 32], 2, false)
            .is_err());
    }
    #[test]
    fn queue_is_bounded_and_a_new_request_invalidates_old_approval() {
        let queue = Queue::default();
        let old = queue
            .insert("device".into(), [1; 32], serde_json::Value::Null, 1)
            .unwrap();
        queue.0.write().unwrap().get_mut(&old).unwrap().approved = true;
        let new = queue
            .insert("device".into(), [2; 32], serde_json::Value::Null, 1)
            .unwrap();
        assert!(queue.inspect(&old, "device", [1; 32], 1, true).is_err());
        assert_eq!(
            queue.inspect(&new, "device", [2; 32], 1, false).unwrap()["approved"],
            false
        );
        for i in 1..32 {
            queue
                .insert(format!("device{i}"), [1; 32], serde_json::Value::Null, 1)
                .unwrap();
        }
        assert_eq!(
            queue
                .insert("extra".into(), [1; 32], serde_json::Value::Null, 1)
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
            .insert("device".into(), [1; 32], serde_json::Value::Null, 0)
            .unwrap();
        let router = management(runtime.clone());
        for peer in ["100.64.0.2:1234", "192.168.1.2:1234"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .header("content-type", "application/json")
                        .uri(format!("/api/v1/remote-access/recovery-requests/{id}"))
                        .header("x-forwarded-for", "127.0.0.1")
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
                    .extension(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            runtime
                .recovery
                .inspect(&id, "device", [1; 32], 0, false)
                .unwrap()["approved"],
            true
        );
    }
}
