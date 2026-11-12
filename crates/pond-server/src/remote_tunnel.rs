//! Remote access through the household's own ngrok account, the alternative to the embedded
//! tailnet. See `docs/remote-access.md`, "ngrok".
//!
//! The tunnel is a TCP or TLS endpoint with no termination at ngrok, so a phone's TLS arrives
//! untouched and the pond terminates it here with its pinned key, under a certificate naming only
//! the tunnel's host. Connections are accepted in this process and never forwarded to a local
//! port: forwarded to `127.0.0.1` every caller on the internet would look like the host. Each
//! request carries [`RemoteTunnel`], so the auth middleware answers only a few routes without a
//! token and requires the token's device to have been home within the presence window.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, bail, ensure, Context, Result};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use futures::StreamExt;
use ngrok::prelude::*;
use pond_api::network::{TunnelAddress, NGROK_AUTHTOKEN};
use pond_core::security::ports::remote_access::{DevicePresence, RemoteTunnel};
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::services::egress::{check_egress_as, EgressPurpose};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tls_identity::TunnelCertifier;

const DIRECTORY: &str = "remote-tunnel";
const CONFIG_FILE: &str = "ngrok.json";
/// The tunnel address paired phones were last able to learn at home.
const LEARNED_FILE: &str = "learned.json";
/// How long a switch to the built-in network waits before stopping the tunnel, so the answer
/// reaches a phone that asked through it.
const HANDOVER_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
/// Waits before each new attempt when ngrok refuses or drops a session: it refuses a session
/// opened right after the last one closed, and a dropped tunnel should come back on its own.
const RETRY_AFTER: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(3),
    std::time::Duration::from_secs(10),
    std::time::Duration::from_secs(30),
];
/// How long a tunnel has to run before a drop no longer counts against its attempts.
const STEADY_FOR: std::time::Duration = std::time::Duration::from_secs(120);
/// ngrok's agent endpoint, which the session dials; checked against the network mode first.
const NGROK_AGENT: &str = "https://connect.ngrok-agent.com";
const LONGEST_ERROR: usize = 300;
/// Connections served at once; more are dropped, so scanners cannot exhaust an 8 GB board.
const MAX_CONNECTIONS: usize = 64;
/// How long a caller has to finish the TLS handshake, and to send a request's headers.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

type Secrets = Arc<dyn SecretRepository + Send + Sync>;

/// Which kind of ngrok endpoint carries the tunnel. Both pass TLS through untouched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endpoint {
    /// A TCP address, on every ngrok plan once a payment method is on the account.
    #[default]
    Tcp,
    /// A TLS endpoint on a reserved domain, on paid plans.
    Tls,
}

/// What the household chose, kept in `<data>/remote-tunnel/ngrok.json`. Never the authtoken,
/// which is in the encrypted secret store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    pub enabled: bool,
    #[serde(default)]
    pub endpoint: Endpoint,
    /// The reserved domain for a TLS endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// A reserved TCP address (`host:port`); without one ngrok assigns a new one each start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_addr: Option<String>,
}

impl Config {
    /// Whether the address survives a restart: a reserved TCP address, or a TLS endpoint, which
    /// always has its reserved domain. Without one ngrok hands out a new address each start.
    pub fn reserved(&self) -> bool {
        match self.endpoint {
            Endpoint::Tcp => self.remote_addr.is_some(),
            Endpoint::Tls => self.domain.is_some(),
        }
    }

    fn validate(&self) -> Result<()> {
        if let Some(domain) = &self.domain {
            ensure!(is_dns_name(domain), "the domain is not a host name");
        }
        if let Some(addr) = &self.remote_addr {
            let (host, port) = addr
                .rsplit_once(':')
                .ok_or_else(|| anyhow!("the reserved address needs a port, as host:port"))?;
            ensure!(
                is_dns_name(host) && port.parse::<u16>().is_ok_and(|p| p != 0),
                "the reserved address is not host:port"
            );
        }
        if self.enabled && self.endpoint == Endpoint::Tls {
            ensure!(
                self.domain.is_some(),
                "a TLS endpoint needs its reserved domain"
            );
        }
        Ok(())
    }
}

fn is_dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// The built-in network (the embedded tailnet), which the tunnel replaces while it runs and falls
/// back to when its address changes before phones can learn it.
#[async_trait::async_trait]
pub trait BuiltInNetwork: Send + Sync {
    fn is_on(&self) -> bool;
    async fn turn_on(&self) -> Result<()>;
    async fn turn_off(&self) -> Result<()>;
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Learned {
    address: String,
}

/// Where the tunnel stands, as the dashboard shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Off,
    Starting,
    Running { address: String },
    Failed { reason: String },
}

/// The tunnel's lifecycle: one session at a time, started and stopped by the household.
pub struct Tunnel {
    directory: PathBuf,
    secrets: Option<Secrets>,
    certifier: TunnelCertifier,
    /// What paired phones are told while the tunnel runs.
    pub address: TunnelAddress,
    status: Mutex<Status>,
    running: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    router: OnceLock<Router>,
    built_in: OnceLock<Arc<dyn BuiltInNetwork>>,
    /// Set by a choice made at home, where phones learn the address: the next one is adopted.
    adopt: AtomicBool,
    /// The address phones have not learned yet, while the built-in network stands in.
    fallback: Mutex<Option<String>>,
}

impl Tunnel {
    pub fn new(data_dir: &Path, secrets: Option<Secrets>, certifier: TunnelCertifier) -> Self {
        Self {
            directory: data_dir.join(DIRECTORY),
            secrets,
            certifier,
            address: TunnelAddress::default(),
            status: Mutex::new(Status::Off),
            running: tokio::sync::Mutex::new(None),
            router: OnceLock::new(),
            built_in: OnceLock::new(),
            adopt: AtomicBool::new(false),
            fallback: Mutex::new(None),
        }
    }

    /// The router tunnel requests are served by, from [`public_companion`]. Set once at startup.
    pub fn serve_with(&self, router: Router) {
        let _ = self.router.set(router);
    }

    /// The built-in network to hand over to. Set once at startup.
    pub fn hand_over_to(&self, built_in: Arc<dyn BuiltInNetwork>) {
        let _ = self.built_in.set(built_in);
    }

    /// The tunnel address phones have not learned yet, while the built-in network stands in.
    pub fn fallback(&self) -> Option<String> {
        self.fallback.lock().ok().and_then(|f| f.clone())
    }

    fn set_fallback(&self, address: Option<String>) {
        if let Ok(mut current) = self.fallback.lock() {
            *current = address;
        }
    }

    /// The next address is one phones can learn: the choice was made on the home network.
    fn adopt_next_address(&self) {
        self.adopt.store(true, Ordering::SeqCst);
    }

    fn learned(&self) -> Option<String> {
        let bytes = std::fs::read(self.directory.join(LEARNED_FILE)).ok()?;
        serde_json::from_slice::<Learned>(&bytes)
            .ok()
            .map(|l| l.address)
    }

    /// Decide what a new tunnel address means for the phones. One the household chose at home,
    /// a reserved one, or the one phones already have replaces the built-in network. Any other
    /// is unknown to a phone away from home, so the built-in network stays or comes back on
    /// until the household switches to ngrok again at home.
    async fn settle_address(&self, address: &str, reserved: bool) {
        let adopt = self.adopt.swap(false, Ordering::SeqCst);
        let learned = self.learned();
        let built_in = self.built_in.get();
        let known = learned.is_none() || learned.as_deref() == Some(address);
        if adopt || reserved || known {
            if learned.as_deref() != Some(address) {
                let record = Learned {
                    address: address.to_string(),
                };
                if let Err(error) = write_private_json(&self.directory, LEARNED_FILE, &record) {
                    tracing::warn!(%error, "could not record the tunnel address phones learn");
                }
            }
            self.set_fallback(None);
            if let Some(built_in) = built_in.filter(|b| b.is_on()) {
                match built_in.turn_off().await {
                    Ok(()) => tracing::info!(
                        target: "giap::trace",
                        kind = "remote_route_ngrok",
                        "the tunnel replaced the built-in network"
                    ),
                    Err(error) => tracing::warn!(%error, "could not turn the built-in network off"),
                }
            }
            return;
        }
        self.set_fallback(Some(address.to_string()));
        tracing::warn!(
            target: "giap::trace",
            kind = "remote_route_fallback",
            %address,
            "the tunnel's address changed and it is not reserved; the built-in network stands in until phones learn it at home"
        );
        match built_in {
            Some(built_in) if !built_in.is_on() => {
                if let Err(error) = built_in.turn_on().await {
                    tracing::warn!(%error, "could not turn the built-in network back on");
                }
            }
            Some(_) => {}
            None => tracing::warn!("no built-in network to fall back to"),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status::Off)
    }

    fn set_status(&self, status: Status) {
        let address = match &status {
            Status::Running { address } => Some(address.clone()),
            _ => None,
        };
        self.address.set(address);
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    pub fn config(&self) -> Result<Config> {
        let path = self.directory.join(CONFIG_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("ngrok.json is malformed")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn save_config(&self, config: &Config) -> Result<()> {
        write_private_json(&self.directory, CONFIG_FILE, config)
    }

    async fn authtoken(&self) -> Result<Option<String>> {
        let secrets = self
            .secrets
            .as_ref()
            .ok_or_else(|| anyhow!("no secret store"))?;
        Ok(secrets
            .get(NGROK_AUTHTOKEN)
            .await?
            .filter(|t| !t.trim().is_empty()))
    }

    pub async fn has_authtoken(&self) -> bool {
        matches!(self.authtoken().await, Ok(Some(_)))
    }

    /// Start the tunnel when the household turned it on and stored an authtoken.
    pub async fn start_if_enabled(self: &Arc<Self>) {
        match self.config() {
            Ok(config) if config.enabled => self.start(config).await,
            Ok(_) => {}
            Err(error) => self.fail(format!("{error:#}")),
        }
    }

    /// Replace any running session with one for `config`. A failure is recorded in the status.
    pub async fn start(self: &Arc<Self>, config: Config) {
        // Held from stopping the old session to storing the new one, so two overlapping starts
        // cannot both spawn and leave one running that no handle can stop.
        let mut running = self.running.lock().await;
        Self::abort(&mut running).await;
        self.set_status(Status::Starting);
        let token = match self.authtoken().await {
            Ok(Some(token)) => token,
            Ok(None) => return self.fail("no ngrok authtoken is stored".to_string()),
            Err(error) => return self.fail(format!("{error:#}")),
        };
        let Some(router) = self.router.get().cloned() else {
            return self.fail("the tunnel has no router".to_string());
        };
        let tunnel = self.clone();
        let task = tokio::spawn(async move {
            let mut attempt = 0;
            loop {
                let started = std::time::Instant::now();
                let Err(error) = tunnel.run(&token, &config, router.clone()).await else {
                    return;
                };
                let reason = redact(&format!("{error:#}"), &token);
                // A tunnel that ran a while has earned fresh attempts after it drops.
                if started.elapsed() >= STEADY_FOR {
                    attempt = 0;
                }
                let Some(wait) = RETRY_AFTER.get(attempt) else {
                    return tunnel.fail(reason);
                };
                attempt += 1;
                tracing::warn!(
                    target: "giap::trace",
                    kind = "remote_tunnel_retry",
                    attempt,
                    %reason,
                    "the ngrok session ended; trying again in {}s",
                    wait.as_secs()
                );
                tunnel.set_status(Status::Starting);
                tokio::time::sleep(*wait).await;
            }
        });
        *running = Some(task);
    }

    pub async fn stop(&self) {
        Self::abort(&mut *self.running.lock().await).await;
        self.set_status(Status::Off);
    }

    /// Aborting the session task also drops its connection set, which aborts every connection.
    async fn abort(running: &mut Option<tokio::task::JoinHandle<()>>) {
        if let Some(task) = running.take() {
            task.abort();
            let _ = task.await;
            tracing::info!(target: "giap::trace", kind = "remote_tunnel_stopped", "ngrok tunnel stopped");
        }
    }

    fn fail(&self, reason: String) {
        tracing::warn!(
            target: "giap::trace",
            kind = "remote_tunnel_failed",
            %reason,
            "the ngrok tunnel is not running"
        );
        self.set_status(Status::Failed { reason });
    }

    async fn run(&self, token: &str, config: &Config, router: Router) -> Result<()> {
        let session = connect(token).await?;
        let reserved = config.reserved();
        match config.endpoint {
            Endpoint::Tcp => {
                let mut builder = session.tcp_endpoint();
                if let Some(addr) = &config.remote_addr {
                    builder.remote_addr(addr.clone());
                }
                let tunnel = builder.listen().await?;
                let url = tunnel.url().to_string();
                self.serve(tunnel, &url, reserved, router).await
            }
            Endpoint::Tls => {
                let mut builder = session.tls_endpoint();
                if let Some(domain) = &config.domain {
                    builder.domain(domain.clone());
                }
                let tunnel = builder.listen().await?;
                let url = tunnel.url().to_string();
                self.serve(tunnel, &url, reserved, router).await
            }
        }
    }

    async fn serve<T, C>(
        &self,
        mut tunnel: T,
        url: &str,
        reserved: bool,
        router: Router,
    ) -> Result<()>
    where
        T: futures::Stream<Item = Result<C, ngrok::tunnel::AcceptError>> + Unpin,
        C: ngrok::conn::ConnInfo
            + tokio::io::AsyncRead
            + tokio::io::AsyncWrite
            + Unpin
            + Send
            + 'static,
    {
        let (host, port) = endpoint_address(url)?;
        let (cert, key) = self.certifier.issue(&host)?;
        let acceptor = acceptor(&cert, &key)?;
        let address = format!("{host}:{port}");
        tracing::info!(
            target: "giap::trace",
            kind = "remote_tunnel_running",
            %address,
            "ngrok tunnel is up"
        );
        self.set_status(Status::Running {
            address: address.clone(),
        });
        self.settle_address(&address, reserved).await;
        let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        // Dropped with this task when the tunnel stops, which aborts every open connection.
        let mut connections = tokio::task::JoinSet::new();
        while let Some(conn) = tunnel.next().await {
            let conn = conn.map_err(|e| anyhow!("ngrok stopped accepting connections: {e}"))?;
            while connections.try_join_next().is_some() {}
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                tracing::debug!("tunnel connection dropped: {MAX_CONNECTIONS} already open");
                continue;
            };
            let peer = public_peer(conn.remote_addr(), &lan_interfaces());
            let (acceptor, router) = (acceptor.clone(), router.clone());
            connections.spawn(async move {
                serve_connection(conn, peer, acceptor, router).await;
                drop(slot);
            });
        }
        bail!("ngrok closed the tunnel")
    }
}

/// Dial ngrok with the household's authtoken, after the network mode allows it.
async fn connect(token: &str) -> Result<ngrok::Session> {
    check_egress_as(NGROK_AGENT, EgressPurpose::RemoteTunnel)
        .map_err(|denied| anyhow!("the network mode does not allow the tunnel: {denied}"))?;
    ngrok::Session::builder()
        .authtoken(token.to_string())
        .connect()
        .await
        .map_err(|e| anyhow!("ngrok refused the session: {e}"))
}

/// The host and port phones dial, from ngrok's `tcp://host:port` or `tls://host` URL.
fn endpoint_address(url: &str) -> Result<(String, u16)> {
    let parsed = url::Url::parse(url).with_context(|| format!("ngrok gave no address: {url}"))?;
    let host = parsed
        .host_str()
        // The certificate names it as a DNS name, so an address literal is refused.
        .filter(|h| h.parse::<IpAddr>().is_err() && is_dns_name(h))
        .ok_or_else(|| anyhow!("ngrok's address has no host name: {url}"))?
        .to_string();
    let port = parsed.port().unwrap_or(443);
    Ok((host, port))
}

fn acceptor(cert_pem: &[u8], key_pem: &[u8]) -> Result<tokio_rustls::TlsAcceptor> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs = CertificateDer::pem_slice_iter(cert_pem).collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_slice(key_pem)?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

async fn serve_connection<C>(
    conn: C,
    peer: SocketAddr,
    acceptor: tokio_rustls::TlsAcceptor,
    router: Router,
) where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(conn)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            tracing::debug!(%error, "tunnel TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!("tunnel TLS handshake timed out");
            return;
        }
    };
    let service = tower::ServiceExt::map_request(router, move |mut request: Request<_>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    });
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(HANDSHAKE_TIMEOUT);
    let served = builder
        .serve_connection(
            hyper_util::rt::TokioIo::new(stream),
            hyper_util::service::TowerToHyperService::new(service),
        )
        .await;
    if let Err(error) = served {
        tracing::debug!(%error, "tunnel connection ended with an error");
    }
}

/// The pond's interfaces, for [`public_peer`]; none when they cannot be read.
fn lan_interfaces() -> Vec<if_addrs::Interface> {
    pond_api::network::interfaces().unwrap_or_default()
}

/// The caller's address as ngrok reports it, kept only when it is public and outside every
/// subnet the pond is on. Anything else becomes `0.0.0.0`, which is never loopback, LAN or
/// tailnet: a tunnel caller must never pass for the host or for the household's own network,
/// including a pond on a public subnet.
fn public_peer(reported: SocketAddr, interfaces: &[if_addrs::Interface]) -> SocketAddr {
    let ip = match reported.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        ip => ip,
    };
    let public = match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.octets()[0] == 0
                || pond_api::network::is_tailnet(ip))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80)
        }
    };
    if public && !pond_api::network::is_lan_peer(ip, interfaces) {
        SocketAddr::new(ip, reported.port())
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    }
}

/// The companion API as the tunnel serves it. Built from the companion router before LAN
/// presence is layered, so a tunnel request never counts as the phone being home, and marked
/// with [`RemoteTunnel`] for the auth middleware.
pub fn public_companion(companion: Router, presence: Arc<dyn DevicePresence>) -> Router {
    companion.layer(axum::Extension(RemoteTunnel(presence)))
}

fn redact(message: &str, token: &str) -> String {
    let message = if token.len() >= 8 {
        message.replace(token, "<authtoken>")
    } else {
        message.to_string()
    };
    message.chars().take(LONGEST_ERROR).collect()
}

fn write_private_json<T: Serialize>(directory: &Path, name: &str, value: &T) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match std::fs::symlink_metadata(directory) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "{} is not a directory",
            directory.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file().sync_all()?;
    file.persist(directory.join(name))?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

// ── Management: loopback and the host credential only ──────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenBody {
    authtoken: String,
}

/// `/api/v1/remote-access/ngrok`: the tunnel's settings, status and token routes. Mounted on the
/// loopback dashboard listener behind the host credential, like the tailnet's management.
pub fn management(tunnel: Arc<Tunnel>, credential: pond_api::host_guard::HostCredential) -> Router {
    Router::new()
        .route("/api/v1/remote-access/ngrok", get(status).put(configure))
        .route(
            "/api/v1/remote-access/ngrok/token",
            put(set_token).delete(forget_token),
        )
        .route(
            "/api/v1/remote-access/route",
            get(host_route_status).put(host_choose_route),
        )
        .with_state(tunnel)
        .route_layer(middleware::from_fn_with_state(credential, host_only))
        .layer(middleware::from_fn(pond_api::middleware::log_requests))
        .layer(pond_api::cors_layer())
}

async fn host_only(
    State(credential): State<pond_api::host_guard::HostCredential>,
    request: Request,
    next: Next,
) -> Response {
    let loopback = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
    if !loopback {
        return StatusCode::FORBIDDEN.into_response();
    }
    match pond_api::host_guard::require_credential(
        Some(&credential),
        request.headers(),
        "remote_access_management",
    ) {
        Ok(()) => next.run(request).await,
        Err(refusal) => refusal.into_response(),
    }
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into(), "code": code}))).into_response()
}

async fn describe(tunnel: &Tunnel) -> Response {
    match tunnel.config() {
        Ok(config) => Json(json!({
            "config": config,
            "tokenSet": tunnel.has_authtoken().await,
            "status": tunnel.status(),
            "reserved": config.reserved(),
            "fallback": tunnel.fallback(),
        }))
        .into_response(),
        Err(error) => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_unreadable",
            format!("{error:#}"),
        ),
    }
}

async fn host_route_status(State(tunnel): State<Arc<Tunnel>>) -> Response {
    describe_for_phone(&tunnel, true).await
}

/// The Pond's own screen switching routes: the phone's rules, from home.
async fn host_choose_route(
    State(tunnel): State<Arc<Tunnel>>,
    Json(choice): Json<RouteChoice>,
) -> Response {
    apply_route(&tunnel, choice, true, "pond").await
}

async fn status(State(tunnel): State<Arc<Tunnel>>) -> Response {
    describe(&tunnel).await
}

async fn configure(State(tunnel): State<Arc<Tunnel>>, Json(config): Json<Config>) -> Response {
    if let Err(error) = config.validate() {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            format!("{error:#}"),
        );
    }
    if config.enabled && !tunnel.has_authtoken().await {
        return refusal(
            StatusCode::CONFLICT,
            "no_authtoken",
            "Store your ngrok authtoken first",
        );
    }
    if let Err(error) = tunnel.save_config(&config) {
        tracing::warn!(%error, "could not save the ngrok settings");
        return refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_unwritable",
            "Could not save the tunnel settings",
        );
    }
    if config.enabled {
        tunnel.adopt_next_address();
        tunnel.start(config).await;
    } else {
        tunnel.set_fallback(None);
        tunnel.stop().await;
    }
    describe(&tunnel).await
}

async fn set_token(State(tunnel): State<Arc<Tunnel>>, Json(body): Json<TokenBody>) -> Response {
    let token = body.authtoken.trim().to_string();
    if token.is_empty() || token.len() > 200 || token.chars().any(|c| c.is_whitespace()) {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_authtoken",
            "That does not look like an ngrok authtoken",
        );
    }
    let Some(secrets) = tunnel.secrets.clone() else {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_secret_store",
            "This pond has no secret store",
        );
    };
    // One session proves the token before it is kept, so a typo is caught here, not at start.
    if let Err(error) = connect(&token).await {
        return refusal(
            StatusCode::BAD_REQUEST,
            "authtoken_refused",
            redact(&format!("{error:#}"), &token),
        );
    }
    if let Err(error) = secrets.set(NGROK_AUTHTOKEN, &token).await {
        tracing::warn!(%error, "could not store the ngrok authtoken");
        return refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret_unwritable",
            "Could not store the authtoken",
        );
    }
    tracing::info!(target: "giap::trace", kind = "remote_tunnel_token_stored", "ngrok authtoken stored");
    describe(&tunnel).await
}

async fn forget_token(State(tunnel): State<Arc<Tunnel>>) -> Response {
    tunnel.stop().await;
    let mut config = tunnel.config().unwrap_or_default();
    config.enabled = false;
    if let Err(error) = tunnel.save_config(&config) {
        tracing::warn!(%error, "could not turn the ngrok tunnel off");
    }
    if let Some(secrets) = &tunnel.secrets {
        if let Err(error) = secrets.delete(NGROK_AUTHTOKEN).await {
            tracing::warn!(%error, "could not forget the ngrok authtoken");
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "secret_unwritable",
                "Could not forget the authtoken",
            );
        }
    }
    tracing::info!(target: "giap::trace", kind = "remote_tunnel_token_forgotten", "ngrok authtoken forgotten");
    describe(&tunnel).await
}

// ── From a paired phone: choose the route, set the authtoken at home ──────────────────────────

/// Which way phones reach the Pond from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Route {
    BuiltIn,
    Ngrok,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RouteChoice {
    route: Route,
    /// A reserved TCP address (`host:port`); empty clears it, absent leaves it.
    #[serde(default)]
    reserved_address: Option<String>,
}

/// `/api/v1/remote-access/route`: what a paired phone may see and change. Mounted on the
/// companion behind the bearer middleware, so it is reachable at home and over either route.
/// The authtoken and the reserved address change only at home; switching to ngrok from away
/// needs a reserved address, since an unreserved one is new at every start and only a phone at
/// home can learn it.
pub fn phone_management(tunnel: Arc<Tunnel>) -> Router {
    Router::new()
        .route(
            "/api/v1/remote-access/route",
            get(phone_status).put(choose_route),
        )
        .route(
            "/api/v1/remote-access/route/token",
            put(phone_set_token).delete(phone_forget_token),
        )
        .with_state(tunnel)
}

/// The caller is on the Pond's own network, where phones learn the tunnel's address.
fn at_home(peer: Option<&ConnectInfo<SocketAddr>>) -> bool {
    let Some(ConnectInfo(peer)) = peer else {
        return false;
    };
    match pond_api::network::interfaces() {
        Ok(interfaces) => pond_api::network::is_lan_peer(peer.ip(), &interfaces),
        Err(error) => {
            tracing::warn!(%error, "cannot classify the caller's network; treating it as away");
            false
        }
    }
}

fn peer_of(request: &Request) -> Option<ConnectInfo<SocketAddr>> {
    // A tunnel request carries the caller's public address, so it is never at home either way.
    if request.extensions().get::<RemoteTunnel>().is_some() {
        return None;
    }
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .cloned()
}

async fn describe_for_phone(tunnel: &Tunnel, home: bool) -> Response {
    let config = match tunnel.config() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "could not read the ngrok settings");
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_unreadable",
                "Could not read the tunnel settings",
            );
        }
    };
    let built_in = tunnel.built_in.get().is_some_and(|b| b.is_on());
    Json(json!({
        "route": if config.enabled { "ngrok" } else { "built_in" },
        "atHome": home,
        "tokenSet": tunnel.has_authtoken().await,
        "endpoint": config.endpoint,
        "reservedAddress": config.remote_addr,
        "reserved": config.reserved(),
        "status": tunnel.status(),
        "fallback": tunnel.fallback(),
        "builtInRunning": built_in,
    }))
    .into_response()
}

async fn phone_status(State(tunnel): State<Arc<Tunnel>>, request: Request) -> Response {
    let home = at_home(peer_of(&request).as_ref());
    describe_for_phone(&tunnel, home).await
}

async fn choose_route(State(tunnel): State<Arc<Tunnel>>, request: Request) -> Response {
    let home = at_home(peer_of(&request).as_ref());
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), 4096).await else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "The request is too large",
        );
    };
    let Ok(choice) = serde_json::from_slice::<RouteChoice>(&bytes) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "Send {\"route\": \"built_in\" | \"ngrok\"}",
        );
    };
    apply_route(&tunnel, choice, home, "phone").await
}

/// Switch the way in, keeping one on: the same rules for a phone and for the Pond's own screen,
/// which is always at home.
async fn apply_route(
    tunnel: &Arc<Tunnel>,
    choice: RouteChoice,
    home: bool,
    chosen_on: &'static str,
) -> Response {
    let mut config = match tunnel.config() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "could not read the ngrok settings");
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_unreadable",
                "Could not read the tunnel settings",
            );
        }
    };
    if let Some(reserved) = choice.reserved_address {
        if !home {
            return refusal(
                StatusCode::FORBIDDEN,
                "at_home_only",
                "Change the reserved address on your home Wi-Fi",
            );
        }
        let reserved = reserved.trim();
        config.remote_addr = (!reserved.is_empty()).then(|| reserved.to_string());
    }
    match choice.route {
        Route::Ngrok => {
            if !tunnel.has_authtoken().await {
                return refusal(
                    StatusCode::CONFLICT,
                    "no_authtoken",
                    "Store your ngrok authtoken first",
                );
            }
            if !home && !config.reserved() {
                return refusal(
                    StatusCode::CONFLICT,
                    "needs_reserved_address",
                    "Away from home, ngrok needs a reserved address",
                );
            }
            config.enabled = true;
        }
        Route::BuiltIn => config.enabled = false,
    }
    if let Err(error) = config.validate() {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            format!("{error:#}"),
        );
    }
    if let Err(error) = tunnel.save_config(&config) {
        tracing::warn!(%error, "could not save the ngrok settings");
        return refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_unwritable",
            "Could not save the tunnel settings",
        );
    }
    tracing::info!(
        target: "giap::trace",
        kind = "remote_route_chosen",
        route = ?choice.route,
        at_home = home,
        reserved = config.reserved(),
        chosen_on,
        "the household chose how phones reach the Pond from outside"
    );
    match choice.route {
        Route::Ngrok => {
            if home {
                tunnel.adopt_next_address();
            }
            tunnel.start(config).await;
        }
        Route::BuiltIn => {
            tunnel.set_fallback(None);
            if let Some(built_in) = tunnel.built_in.get().filter(|b| !b.is_on()) {
                if let Err(error) = built_in.turn_on().await {
                    tracing::warn!(%error, "could not turn the built-in network on");
                    return refusal(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "built_in_unavailable",
                        "The built-in network could not start; the tunnel is left running",
                    );
                }
            }
            // Stopped after the answer, which may be travelling through the tunnel.
            let stopping = tunnel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(HANDOVER_DELAY).await;
                if stopping.config().is_ok_and(|c| !c.enabled) {
                    stopping.stop().await;
                }
            });
        }
    }
    describe_for_phone(tunnel, home).await
}

async fn phone_set_token(State(tunnel): State<Arc<Tunnel>>, request: Request) -> Response {
    if !at_home(peer_of(&request).as_ref()) {
        return refusal(
            StatusCode::FORBIDDEN,
            "at_home_only",
            "Add the authtoken on your home Wi-Fi",
        );
    }
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), 4096).await else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_body",
            "The request is too large",
        );
    };
    let Ok(body) = serde_json::from_slice::<TokenBody>(&bytes) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid_authtoken",
            "That does not look like an ngrok authtoken",
        );
    };
    let stored = set_token(State(tunnel.clone()), Json(body)).await;
    if !stored.status().is_success() {
        return stored;
    }
    describe_for_phone(&tunnel, true).await
}

async fn phone_forget_token(State(tunnel): State<Arc<Tunnel>>, request: Request) -> Response {
    if !at_home(peer_of(&request).as_ref()) {
        return refusal(
            StatusCode::FORBIDDEN,
            "at_home_only",
            "Remove the authtoken on your home Wi-Fi",
        );
    }
    let was_on = tunnel.config().is_ok_and(|c| c.enabled);
    let forgotten = forget_token(State(tunnel.clone())).await;
    if !forgotten.status().is_success() {
        return forgotten;
    }
    tunnel.set_fallback(None);
    // Phones away still need a way in once the tunnel is gone.
    if was_on {
        if let Some(built_in) = tunnel.built_in.get().filter(|b| !b.is_on()) {
            if let Err(error) = built_in.turn_on().await {
                tracing::warn!(%error, "could not turn the built-in network on");
            }
        }
    }
    describe_for_phone(&tunnel, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_public_caller_keeps_its_address() {
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        assert_eq!(public_peer(public, &[]), public);
        for hidden in [
            "127.0.0.1:5000",
            "192.168.1.20:5000",
            "10.0.0.4:5000",
            "100.64.0.7:5000",
            "169.254.1.1:5000",
            "0.0.0.0:5000",
            "[::1]:5000",
            "[fd00::1]:5000",
            "[fe80::1]:5000",
            "[::ffff:192.168.1.20]:5000",
        ] {
            let peer = public_peer(hidden.parse().unwrap(), &[]);
            assert!(peer.ip().is_unspecified(), "{hidden} kept as {peer}");
        }
    }

    /// A pond on a public subnet: a caller inside it must not pass for the LAN.
    #[test]
    fn a_caller_on_the_ponds_own_public_subnet_is_hidden() {
        let interfaces = [if_addrs::Interface {
            name: "en0".into(),
            addr: if_addrs::IfAddr::V4(if_addrs::Ifv4Addr {
                ip: "203.0.113.2".parse().unwrap(),
                netmask: "255.255.255.0".parse().unwrap(),
                prefixlen: 24,
                broadcast: Some("203.0.113.255".parse().unwrap()),
            }),
            index: Some(1),
        }];
        let neighbour = public_peer("203.0.113.9:5000".parse().unwrap(), &interfaces);
        assert!(neighbour.ip().is_unspecified(), "kept as {neighbour}");
        let far: SocketAddr = "198.51.100.7:5000".parse().unwrap();
        assert_eq!(public_peer(far, &interfaces), far);
    }

    #[test]
    fn the_address_comes_from_ngroks_url() {
        assert_eq!(
            endpoint_address("tcp://4.tcp.eu.ngrok.io:12345").unwrap(),
            ("4.tcp.eu.ngrok.io".to_string(), 12345)
        );
        assert_eq!(
            endpoint_address("tls://pond.example.org").unwrap(),
            ("pond.example.org".to_string(), 443)
        );
        assert!(endpoint_address("tcp://203.0.113.9:1").is_err());
    }

    #[test]
    fn a_tls_endpoint_needs_its_domain_and_addresses_are_checked() {
        let tls = Config {
            enabled: true,
            endpoint: Endpoint::Tls,
            ..Config::default()
        };
        assert!(tls.validate().is_err());
        let tls = Config {
            domain: Some("pond.example.org".into()),
            ..tls
        };
        assert!(tls.validate().is_ok());
        let bad = Config {
            remote_addr: Some("1.tcp.ngrok.io".into()),
            ..Config::default()
        };
        assert!(bad.validate().is_err());
        let good = Config {
            remote_addr: Some("1.tcp.ngrok.io:20123".into()),
            ..Config::default()
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn the_config_file_never_holds_the_token_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            enabled: true,
            endpoint: Endpoint::Tcp,
            domain: None,
            remote_addr: Some("1.tcp.ngrok.io:20123".into()),
        };
        write_private_json(&dir.path().join(DIRECTORY), CONFIG_FILE, &config).unwrap();
        let path = dir.path().join(DIRECTORY).join(CONFIG_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("token"), "{text}");
        let read: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(read, config);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "ngrok.json is readable by others");
    }

    struct FakeBuiltIn {
        on: AtomicBool,
    }

    #[async_trait::async_trait]
    impl BuiltInNetwork for FakeBuiltIn {
        fn is_on(&self) -> bool {
            self.on.load(Ordering::SeqCst)
        }
        async fn turn_on(&self) -> Result<()> {
            self.on.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn turn_off(&self) -> Result<()> {
            self.on.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    fn tunnel_with(dir: &Path, built_in_on: bool) -> (Arc<Tunnel>, Arc<FakeBuiltIn>) {
        let identity =
            crate::tls_identity::TlsIdentity::load(dir, &["pond.local".to_string()]).unwrap();
        let tunnel = Arc::new(Tunnel::new(dir, None, identity.tunnel_certifier()));
        let built_in = Arc::new(FakeBuiltIn {
            on: AtomicBool::new(built_in_on),
        });
        tunnel.hand_over_to(built_in.clone());
        (tunnel, built_in)
    }

    #[test]
    fn only_a_reserved_address_or_a_tls_domain_survives_a_restart() {
        assert!(!Config::default().reserved());
        let tcp = Config {
            remote_addr: Some("1.tcp.ngrok.io:20123".into()),
            ..Config::default()
        };
        assert!(tcp.reserved());
        let tls = Config {
            endpoint: Endpoint::Tls,
            domain: Some("pond.example.org".into()),
            ..Config::default()
        };
        assert!(tls.reserved());
    }

    #[tokio::test]
    async fn the_first_address_is_adopted_and_replaces_the_built_in_network() {
        let dir = tempfile::tempdir().unwrap();
        let (tunnel, built_in) = tunnel_with(dir.path(), true);
        tunnel
            .settle_address("1.tcp.eu.ngrok.io:20001", false)
            .await;
        assert_eq!(tunnel.learned().as_deref(), Some("1.tcp.eu.ngrok.io:20001"));
        assert_eq!(tunnel.fallback(), None);
        assert!(!built_in.is_on());
    }

    #[tokio::test]
    async fn a_new_unreserved_address_falls_back_to_the_built_in_network() {
        let dir = tempfile::tempdir().unwrap();
        let (tunnel, built_in) = tunnel_with(dir.path(), true);
        tunnel
            .settle_address("1.tcp.eu.ngrok.io:20001", false)
            .await;
        assert!(!built_in.is_on());

        // A restart: ngrok hands out another address phones away from home cannot know.
        tunnel
            .settle_address("4.tcp.eu.ngrok.io:31337", false)
            .await;
        assert_eq!(
            tunnel.fallback().as_deref(),
            Some("4.tcp.eu.ngrok.io:31337")
        );
        assert!(
            built_in.is_on(),
            "phones away need the built-in network back"
        );
        assert_eq!(
            tunnel.learned().as_deref(),
            Some("1.tcp.eu.ngrok.io:20001"),
            "phones still know only the old address"
        );
    }

    #[tokio::test]
    async fn switching_to_ngrok_at_home_adopts_the_new_address() {
        let dir = tempfile::tempdir().unwrap();
        let (tunnel, built_in) = tunnel_with(dir.path(), false);
        tunnel
            .settle_address("1.tcp.eu.ngrok.io:20001", false)
            .await;
        tunnel
            .settle_address("4.tcp.eu.ngrok.io:31337", false)
            .await;
        assert!(built_in.is_on());

        tunnel.adopt_next_address();
        tunnel
            .settle_address("6.tcp.eu.ngrok.io:12000", false)
            .await;
        assert_eq!(tunnel.learned().as_deref(), Some("6.tcp.eu.ngrok.io:12000"));
        assert_eq!(tunnel.fallback(), None);
        assert!(!built_in.is_on());
    }

    #[tokio::test]
    async fn a_reserved_address_never_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let (tunnel, built_in) = tunnel_with(dir.path(), true);
        tunnel.settle_address("1.tcp.eu.ngrok.io:20001", true).await;
        tunnel.settle_address("2.tcp.eu.ngrok.io:20002", true).await;
        assert_eq!(tunnel.fallback(), None);
        assert!(!built_in.is_on());
    }

    #[test]
    fn an_error_never_carries_the_token() {
        let token = "test-authtoken-not-a-real-one";
        let said = redact(
            &format!("ERR_NGROK_105: authtoken {token} is invalid"),
            token,
        );
        assert!(!said.contains(token));
        assert!(said.contains("<authtoken>"));
    }
}
