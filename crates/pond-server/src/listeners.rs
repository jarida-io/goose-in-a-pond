//! Which routes and extensions each listener serves, kept apart from `serve()` so the
//! composition can be tested without starting the server.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{Extension, Router};
use pond_api::host_guard::HostCredential;
use pond_api::insecure_dev::InsecureDevLan;
use pond_api::network::CompanionTransport;
use pond_api::AppState;

#[cfg(unix)]
use crate::embedded_network::{self, Runtime};
#[cfg(unix)]
use crate::remote_tunnel::{self, Tunnel};
#[cfg(unix)]
use pond_core::security::ports::remote_access::{DevicePresence, RemoteRevocation};

/// The routers for every listener `serve()` binds.
pub struct Listeners {
    /// Plain HTTP on `127.0.0.1`: the dashboard, its API, and remote-access management,
    /// answering only loopback names and first-party pages.
    pub dashboard: Router,
    /// HTTPS on every interface: the companion API, without desktop assets.
    pub companion: Router,
    /// The private Unix socket the embedded node forwards tailnet requests to.
    #[cfg(unix)]
    pub embedded: Router,
    /// What the household's ngrok tunnel serves, accepted in process by `remote_tunnel`.
    #[cfg(unix)]
    pub tunnel: Router,
    /// Plain HTTP for Expo Go, bound only when a debug build is asked for it: the companion
    /// API without remote-access enrollment or presence. See `pond_api::insecure_dev`.
    pub insecure: Router,
}

/// Build the routers for every listener from the shared application state.
pub fn compose(
    state: Arc<AppState>,
    static_dir: PathBuf,
    transport: CompanionTransport,
    credential: HostCredential,
    insecure_dev: Option<InsecureDevLan>,
    #[cfg(unix)] embedded: Arc<Runtime>,
    #[cfg(unix)] tunnel: Arc<Tunnel>,
) -> Listeners {
    let companion = pond_api::insecure_dev::advertise(
        pond_api::build_companion_router(state.clone()).layer(Extension(transport.clone())),
        insecure_dev,
    );
    let dashboard = pond_api::insecure_dev::advertise(
        pond_api::build_router(state.clone(), static_dir).layer(Extension(transport)),
        insecure_dev,
    );
    #[cfg(unix)]
    let companion = companion
        .layer(Extension(embedded.clone() as Arc<dyn RemoteRevocation>))
        .layer(Extension(embedded.address.clone()))
        .layer(Extension(tunnel.address.clone()));
    // Taken before remote-access enrollment is merged in and before presence is layered: a
    // node's enrollment never crosses plaintext, and a plaintext request never renews remote
    // access. Revocation stays, so signing out over this listener still reaches the tailnet.
    let insecure = pond_api::insecure_dev::router(companion.clone());
    #[cfg(unix)]
    let companion = companion
        .merge(embedded_network::companion_management(
            embedded.clone(),
            state.clone(),
        ))
        .merge(
            remote_tunnel::phone_management(tunnel.clone())
                .route_layer(axum::extract::DefaultBodyLimit::max(4096))
                .route_layer(Extension(state.handshake.clone()))
                .route_layer(axum::middleware::from_fn_with_state(
                    state,
                    pond_api::middleware::auth_middleware,
                )),
        );
    // Taken before presence is added: a request over the tailnet must never count as the
    // phone being at home. The middleware's LAN check says the same; this makes it structural.
    #[cfg(unix)]
    let tailnet = embedded_network::private_companion(companion.clone(), embedded.clone());
    // Taken here for the same reason: a request through the tunnel is never the phone at home.
    #[cfg(unix)]
    let public_tunnel = remote_tunnel::public_companion(
        companion.clone(),
        embedded.clone() as Arc<dyn DevicePresence>,
    );
    // Phones reach the Pond over HTTPS, so that is where a LAN sighting renews remote access.
    #[cfg(unix)]
    let companion = companion.layer(Extension(embedded.clone() as Arc<dyn DevicePresence>));
    #[cfg(unix)]
    let dashboard = dashboard
        .layer(Extension(embedded.clone() as Arc<dyn RemoteRevocation>))
        .layer(Extension(embedded.address.clone()))
        .layer(Extension(tunnel.address.clone()))
        .merge(embedded_network::management(
            embedded.clone(),
            credential.clone(),
        ))
        .merge(remote_tunnel::management(tunnel, credential.clone()));
    // Outermost, so no route, merged router or fallback answers a rebound or cross-site request.
    let dashboard = dashboard
        .layer(Extension(credential))
        .layer(axum::middleware::from_fn(
            pond_api::host_guard::loopback_only,
        ));
    Listeners {
        dashboard,
        companion,
        #[cfg(unix)]
        embedded: tailnet,
        #[cfg(unix)]
        tunnel: public_tunnel,
        insecure,
    }
}
