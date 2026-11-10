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
/// ngrok's agent endpoint, which the session dials; checked against the network mode first.
const NGROK_AGENT: &str = "https://connect.ngrok-agent.com";
const LONGEST_ERROR: usize = 300;

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
        }
    }

    /// The router tunnel requests are served by, from [`public_companion`]. Set once at startup.
    pub fn serve_with(&self, router: Router) {
        let _ = self.router.set(router);
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
        self.stop().await;
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
            if let Err(error) = tunnel.run(&token, &config, router).await {
                tunnel.fail(redact(&format!("{error:#}"), &token));
            }
        });
        *self.running.lock().await = Some(task);
    }

    pub async fn stop(&self) {
        if let Some(task) = self.running.lock().await.take() {
            task.abort();
            let _ = task.await;
            tracing::info!(target: "giap::trace", kind = "remote_tunnel_stopped", "ngrok tunnel stopped");
        }
        self.set_status(Status::Off);
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
        match config.endpoint {
            Endpoint::Tcp => {
                let mut builder = session.tcp_endpoint();
                if let Some(addr) = &config.remote_addr {
                    builder.remote_addr(addr.clone());
                }
                let tunnel = builder.listen().await?;
                let url = tunnel.url().to_string();
                self.serve(tunnel, &url, router).await
            }
            Endpoint::Tls => {
                let mut builder = session.tls_endpoint();
                if let Some(domain) = &config.domain {
                    builder.domain(domain.clone());
                }
                let tunnel = builder.listen().await?;
                let url = tunnel.url().to_string();
                self.serve(tunnel, &url, router).await
            }
        }
    }

    async fn serve<T, C>(&self, mut tunnel: T, url: &str, router: Router) -> Result<()>
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
        self.set_status(Status::Running { address });
        while let Some(conn) = tunnel.next().await {
            let conn = conn.map_err(|e| anyhow!("ngrok stopped accepting connections: {e}"))?;
            let peer = public_peer(conn.remote_addr());
            tokio::spawn(serve_connection(
                conn,
                peer,
                acceptor.clone(),
                router.clone(),
            ));
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
    let stream = match acceptor.accept(conn).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(%error, "tunnel TLS handshake failed");
            return;
        }
    };
    let service = tower::ServiceExt::map_request(router, move |mut request: Request<_>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    });
    let served = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(
            hyper_util::rt::TokioIo::new(stream),
            hyper_util::service::TowerToHyperService::new(service),
        )
        .await;
    if let Err(error) = served {
        tracing::debug!(%error, "tunnel connection ended with an error");
    }
}

/// The caller's address as ngrok reports it, kept only when it is a public one. Anything else
/// becomes `0.0.0.0`, which is never loopback, LAN or tailnet: a tunnel caller must never pass
/// for the host or for the household's own network.
fn public_peer(reported: SocketAddr) -> SocketAddr {
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
    if public {
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
        }))
        .into_response(),
        Err(error) => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_unreadable",
            format!("{error:#}"),
        ),
    }
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
        tunnel.start(config).await;
    } else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_public_caller_keeps_its_address() {
        let public: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        assert_eq!(public_peer(public), public);
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
            let peer = public_peer(hidden.parse().unwrap());
            assert!(peer.ip().is_unspecified(), "{hidden} kept as {peer}");
        }
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
