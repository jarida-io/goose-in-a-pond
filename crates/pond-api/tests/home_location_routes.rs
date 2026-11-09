//! `PUT /location/device`: a household phone's fix replaces the Pond's guess, from the home
//! network only, never a place someone set, and rounded; `PUT /settings` records where the
//! coordinates came from.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::security::ports::remote_access::{RemoteMachines, TailnetMachine};
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::settings::SettingsRepository as _;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    settings: Arc<pond_infra::sqlite_settings::SqliteSettingsRepository>,
    _dir: tempfile::TempDir,
}

async fn a_pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let settings = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    state.settings_repo = settings.clone();
    let device_id = state
        .device_registry
        .register(RegisterDeviceRequest {
            id: None,
            name: "Phone".into(),
            device_type: "gotg".into(),
            hostname: None,
            capabilities: vec![],
            room_id: None,
        })
        .await
        .unwrap()
        .id;
    state.handshake = Arc::new(DeviceHandshake(device_id));
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        settings,
        _dir: built.dir,
    }
}

const MACHINE: &str = "mkey:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The phone enrolled this machine, so the middleware lets its tailnet request through to the
/// handler, as on a real Pond.
struct Enrolled;

#[async_trait::async_trait]
impl RemoteMachines for Enrolled {
    async fn machine_for(&self, _device: &str) -> anyhow::Result<Option<String>> {
        Ok(Some(MACHINE.to_string()))
    }
}

/// Where a request came from, as the server's listeners record it.
enum From {
    /// The Pond's own machine stands in for the LAN here: `require_lan` admits loopback, and a
    /// test cannot be on the LAN of the machine running it in a portable way.
    Home,
    /// The tailnet, as the embedded network's private socket hands a request over.
    Tailnet,
    /// An address on no network of the Pond's.
    Elsewhere,
}

async fn call(
    pond: &Pond,
    method: Method,
    uri: &str,
    body: Value,
    from: From,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let peer: SocketAddr = match from {
        From::Home => "127.0.0.1:50000",
        From::Tailnet => "100.64.0.7:50000",
        From::Elsewhere => "203.0.113.9:50000",
    }
    .parse()
    .unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    if matches!(from, From::Tailnet) {
        request
            .extensions_mut()
            .insert(TailnetMachine(MACHINE.to_string()));
        request
            .extensions_mut()
            .insert(Arc::new(Enrolled) as Arc<dyn RemoteMachines>);
    }
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn guessed(pond: &Pond) {
    let mut s = pond.settings.get().await.unwrap();
    s.weather_location_name = "Nairobi, Kenya".into();
    s.weather_latitude = -1.28333;
    s.weather_longitude = 36.81667;
    s.weather_location_source = "geocoded".into();
    pond.settings.update(&s).await.unwrap();
}

fn ndenderu(replace: bool) -> Value {
    json!({"latitude": -1.193_421, "longitude": 36.744_088, "accuracy_m": 14.0, "name": "Ndenderu", "replace": replace})
}

#[tokio::test]
async fn a_phone_at_home_replaces_a_guessed_town_with_its_fix_rounded() {
    let pond = a_pond().await;
    guessed(&pond).await;
    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/location/device",
        ndenderu(false),
        From::Home,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], true);
    assert_eq!(body["name"], "Ndenderu");

    let s = pond.settings.get().await.unwrap();
    assert_eq!(
        (s.weather_latitude, s.weather_longitude),
        (-1.193, 36.744),
        "about 110 m, not the house"
    );
    assert_eq!(s.weather_location_name, "Ndenderu");
    assert_eq!(s.weather_location_source, "device");
    assert!(s.weather_location_is_precise());
}

#[tokio::test]
async fn a_phone_away_from_home_moves_nothing() {
    let pond = a_pond().await;
    guessed(&pond).await;
    for from in [From::Tailnet, From::Elsewhere] {
        let (status, body) = call(
            &pond,
            Method::PUT,
            "/api/v1/location/device",
            ndenderu(true),
            from,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "not_on_home_network");
    }
    let s = pond.settings.get().await.unwrap();
    assert_eq!(s.weather_location_name, "Nairobi, Kenya");
    assert_eq!(s.weather_location_source, "geocoded");
}

#[tokio::test]
async fn a_place_someone_set_stays_unless_the_person_asks_for_the_phone_fix() {
    let pond = a_pond().await;
    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/settings",
        json!({"weather_latitude": -1.2, "weather_longitude": 36.75, "weather_location_name": "Home"}),
        From::Home,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        pond.settings.get().await.unwrap().weather_location_source,
        "manual"
    );

    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/location/device",
        ndenderu(false),
        From::Home,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], false);
    assert_eq!(body["reason"], "location_known");
    assert_eq!(
        pond.settings.get().await.unwrap().weather_location_name,
        "Home"
    );

    let (_, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/location/device",
        ndenderu(true),
        From::Home,
    )
    .await;
    assert_eq!(body["applied"], true, "asked for, it replaces");
    assert_eq!(
        pond.settings.get().await.unwrap().weather_location_name,
        "Ndenderu"
    );
}

#[tokio::test]
async fn a_fix_with_no_name_keeps_the_name_and_a_poor_fix_is_refused() {
    let pond = a_pond().await;
    guessed(&pond).await;
    let mut unnamed = ndenderu(false);
    unnamed.as_object_mut().unwrap().remove("name");
    let (_, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/location/device",
        unnamed,
        From::Home,
    )
    .await;
    assert_eq!(body["applied"], true);
    assert_eq!(
        pond.settings.get().await.unwrap().weather_location_name,
        "Nairobi, Kenya"
    );

    let mut vague = ndenderu(true);
    vague["accuracy_m"] = json!(5_000.0);
    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/location/device",
        vague,
        From::Home,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("too_imprecise"))
    );

    let mut nowhere = ndenderu(true);
    nowhere["latitude"] = json!(0.0);
    nowhere["longitude"] = json!(0.0);
    let (status, body) = call(
        &pond,
        Method::PUT,
        "/api/v1/location/device",
        nowhere,
        From::Home,
    )
    .await;
    assert_eq!(
        (status, body["error"].clone()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!("invalid_coordinates")
        )
    );
}
