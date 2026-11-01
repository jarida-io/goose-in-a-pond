//! Bundled userspace networking lifecycle and its private companion boundary.
//! The public listeners never accept the embedded peer header.

use anyhow::{bail, ensure, Context, Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use pond_api::network::{is_tailnet, EmbeddedAddress};
use pond_core::security::domain::proven_device::DeviceRung;
use pond_core::security::ports::remote_access::{RemoteMachines, TailnetMachine};
use pond_core::user_data::domain::profile::{ProfileScope, Role};
use pond_core::user_data::domain::session::SessionIdentity;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::services::identity_resolution;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::Mutex,
};

mod recovery;
mod revocation;

const PEER_HEADER: &str = "x-pond-embedded-peer";
const MACHINE_HEADER: &str = "x-pond-embedded-machine";

/// Hosted coordinator for a household that names none. Applied on enable, never on read: an
/// empty control URL is what marks a household local-only.
pub const DEFAULT_CONTROL_URL: &str = "https://controlpond.jarida.io";
pub const DEFAULT_ENROLLMENT_URL: &str = "https://enrollpond.jarida.io";

/// Default both origins only when both are empty; validation rejects a half-configured pair.
fn with_default_coordinator(mut config: Config) -> Config {
    if config.control_url.is_empty() && config.enrollment_url.is_empty() {
        config.control_url = DEFAULT_CONTROL_URL.to_string();
        config.enrollment_url = DEFAULT_ENROLLMENT_URL.to_string();
    }
    config
}

/// Where the Pond records the coordination service its household is registered with.
///
/// An invite or a device certificate is needed only for a household's first registration, so the
/// dashboard asks for an invite only while this names no service, or another one than configured.
const REGISTERED_FILE: &str = "registered.json";

/// The contents of [`REGISTERED_FILE`]: the enrollment origin the household registered with.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registered {
    enrollment: String,
}

/// Non-secret enrollment settings. No auth key can be stored in this file.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Whether to restore the embedded node on startup.
    pub enabled: bool,
    /// Explicit Headscale HTTPS origin; an empty value never selects a hosted service.
    #[serde(default)]
    pub control_url: String,
    /// Enrollment service origin, configured locally by the operator.
    #[serde(default)]
    pub enrollment_url: String,
}

/// Private, loopback-only state. Enrollment URLs must never be logged.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Backend state, localized by the caller.
    pub state: String,
    /// Tailnet addresses allocated to this application instance.
    pub addresses: Vec<String>,
    /// One-time browser authorization URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    /// Public key of the pending network registration.
    #[serde(default)]
    pub node_key: String,
    /// Public machine identity available before coordinator authorization.
    #[serde(default)]
    pub machine_key: String,
}

struct Process {
    child: Child,
    _stdin: ChildStdin,
}

/// Owns an isolated local socket and the bundled Go networking process.
pub struct Runtime {
    directory: PathBuf,
    identity: PathBuf,
    socket: PathBuf,
    _socket_dir: tempfile::TempDir,
    port: u16,
    /// The bundled `pondnet` helper, resolved once at startup.
    helper: PathBuf,
    status: RwLock<Status>,
    process: Mutex<Option<Process>>,
    generation: AtomicU64,
    /// Held only across a read-modify-write of the revocation queue or enrollment record,
    /// never across a helper call.
    files: std::sync::Mutex<()>,
    /// Serialises helper calls about one device, so enrolling and revoking it cannot
    /// interleave, without making every other device wait for a slow coordinator.
    device_locks: std::sync::Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// When each device's sighting was last written, to throttle presence writes.
    sightings_written: std::sync::Mutex<BTreeMap<String, std::time::Instant>>,
    recovery: recovery::Queue,
    /// Address published by system information and pairing QR producers.
    pub address: EmbeddedAddress,
    /// Wake certificate renewal as soon as the node obtains an address.
    pub changed: tokio::sync::Notify,
}

impl Runtime {
    /// Create the private bridge; the short random socket path fits macOS's sockaddr_un limit.
    pub fn new(data: &Path, port: u16) -> Result<(Arc<Self>, tokio::net::UnixListener)> {
        // The helper holds the household's network identity, so which binary runs is not
        // something the environment decides in a release build.
        #[cfg(debug_assertions)]
        if let Some(helper) = std::env::var_os("POND_NETWORK_BINARY") {
            return Self::with_helper(data, port, PathBuf::from(helper));
        }
        Self::with_helper(
            data,
            port,
            std::env::current_exe()?.with_file_name("pondnet"),
        )
    }

    fn with_helper(
        data: &Path,
        port: u16,
        helper: PathBuf,
    ) -> Result<(Arc<Self>, tokio::net::UnixListener)> {
        let directory = data.join("embedded-network");
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o077 == 0,
            "embedded directory must be private and not a symlink"
        );
        let socket_dir = tempfile::Builder::new().prefix("pond-net-").tempdir()?;
        std::fs::set_permissions(socket_dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let socket = socket_dir.path().join("api.sock");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        Ok((
            Arc::new(Self {
                directory,
                identity: crate::tls_identity::tailnet_identity_path(data),
                socket,
                _socket_dir: socket_dir,
                port,
                helper,
                status: RwLock::new(Status {
                    state: "Stopped".into(),
                    addresses: vec![],
                    auth_url: None,
                    node_key: String::new(),
                    machine_key: String::new(),
                }),
                process: Mutex::new(None),
                generation: AtomicU64::new(0),
                files: std::sync::Mutex::new(()),
                device_locks: std::sync::Mutex::new(BTreeMap::new()),
                sightings_written: std::sync::Mutex::new(BTreeMap::new()),
                recovery: recovery::Queue::default(),
                address: EmbeddedAddress::default(),
                changed: tokio::sync::Notify::new(),
            }),
            listener,
        ))
    }

    /// Load explicit settings without silently replacing corrupt configuration.
    pub fn config(&self) -> Result<Config> {
        let path = self.directory.join("config.json");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Ok(meta) => ensure!(
                meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 4096,
                "invalid embedded configuration file"
            ),
            Err(e) => return Err(e.into()),
        }
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    fn persist(&self, config: &Config) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&serde_json::to_vec(config)?)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("config.json"))?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    fn publish(&self, status: Status) {
        let previous = self
            .status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone();
        if previous != status.state {
            tracing::info!(
                previous,
                state = status.state,
                "embedded networking state changed"
            );
            // A Pond's node is enrolled only after its household registered, so reaching Running
            // proves it. This is what records a household that registered before the record existed.
            if status.state == "Running" {
                self.record_registered();
            }
        }
        let address = status
            .addresses
            .iter()
            .find_map(|s| s.parse::<std::net::Ipv4Addr>().ok())
            .filter(|ip| is_tailnet((*ip).into()))
            .map(|ip| ip.to_string());
        if address.is_none() {
            *self.address.0.write().unwrap_or_else(|p| p.into_inner()) = None;
        }
        *self.status.write().unwrap_or_else(|p| p.into_inner()) = status;
        self.changed.notify_one();
    }

    /// Record that the household is registered with the configured coordination service.
    /// Failing to record it is logged and costs only an invite field the dashboard shows again.
    fn record_registered(&self) {
        let enrollment = match self.config() {
            Ok(config) => with_default_coordinator(config).enrollment_url,
            Err(error) => {
                tracing::warn!(%error, "could not read the configuration to record the household's registration");
                return;
            }
        };
        if self.registered_with().as_deref() == Some(enrollment.as_str()) {
            return;
        }
        match revocation::write_private_json(
            &self.directory,
            REGISTERED_FILE,
            &Registered {
                enrollment: enrollment.clone(),
            },
        ) {
            Ok(()) => tracing::info!(
                target: "giap::trace",
                kind = "household_registration_recorded",
                %enrollment,
                "remote access: recorded that this household is registered"
            ),
            Err(error) => tracing::warn!(
                %error,
                %enrollment,
                "remote access: could not record that this household is registered"
            ),
        }
    }

    /// Whether the household is registered with the coordination service now configured. It is
    /// recorded when the node first reaches `Running`, so it also says remote access has worked.
    pub fn registered(&self) -> bool {
        match self.config() {
            Ok(config) => {
                let enrollment = with_default_coordinator(config).enrollment_url;
                self.registered_with().as_deref() == Some(enrollment.as_str())
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the configuration to report the household's registration");
                false
            }
        }
    }

    /// The enrollment origin [`REGISTERED_FILE`] names, if it exists and can be read.
    fn registered_with(&self) -> Option<String> {
        match revocation::read_private_json::<Registered>(
            &self.directory.join(REGISTERED_FILE),
            4096,
        ) {
            Ok(registered) => registered.map(|registered| registered.enrollment),
            Err(error) => {
                tracing::warn!(%error, "the household registration record is unreadable");
                None
            }
        }
    }

    /// Publish only addresses covered by the certificate the helper serves on the tailnet,
    /// `tls/tailnet-identity.json`, so a phone that dials the published address can verify it.
    pub fn publish_ready(&self, certificate_names: &[String]) {
        let status = self.status.read().unwrap_or_else(|p| p.into_inner());
        let address = status.addresses.iter().find(|value| {
            certificate_names.contains(value) && value.parse::<std::net::Ipv4Addr>().is_ok()
        });
        *self.address.0.write().unwrap_or_else(|p| p.into_inner()) = if status.state == "Running" {
            address.cloned()
        } else {
            None
        };
    }

    /// The serial of the device certificate this Pond was imaged with, or `None` when it was
    /// never provisioned. Mirrors the helper's `LoadDevice`: the `device` directory must be
    /// private and hold a usable key beside the certificate. Anything else counts as absent and
    /// is logged, so the dashboard asks for an invite rather than claiming a provisioning the
    /// helper cannot present.
    fn provisioned_serial(&self) -> Option<String> {
        #[derive(Deserialize)]
        struct Certificate {
            payload: String,
        }
        #[derive(Deserialize)]
        struct Payload {
            serial: String,
        }
        #[derive(Deserialize)]
        struct Key {
            seed: String,
        }
        use base64::Engine;
        let device = self.directory.join("device");
        match std::fs::symlink_metadata(&device) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(error = %error, "device directory unreadable; ignoring it");
                return None;
            }
            Ok(metadata) if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 => {
                tracing::warn!("device directory is not private; ignoring it");
                return None;
            }
            Ok(_) => {}
        }
        let key = match revocation::read_private_json::<Key>(&device.join("key.json"), 8192) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(error = %error, "device key unreadable; ignoring the certificate");
                return None;
            }
        };
        let usable = key.is_some_and(|key| {
            base64::engine::general_purpose::STANDARD
                .decode(key.seed)
                .is_ok_and(|seed| seed.len() == 32)
        });
        if !usable {
            tracing::warn!("device key missing or invalid; ignoring the certificate");
            return None;
        }
        let path = device.join("certificate.json");
        // Opened once and checked on the open handle, so a file swapped in after a check is
        // never read: a symlink, a loose mode or more than 8 KiB counts as no certificate.
        let certificate = match revocation::read_private_json::<Certificate>(&path, 8192) {
            Ok(Some(certificate)) => certificate,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(error = %error, "device certificate unreadable; ignoring it");
                return None;
            }
        };
        let serial = base64::engine::general_purpose::STANDARD
            .decode(certificate.payload)
            .ok()
            .and_then(|payload| serde_json::from_slice::<Payload>(&payload).ok())
            .map(|payload| payload.serial)
            .filter(|serial| serial.len() == 32 && serial.bytes().all(|b| b.is_ascii_hexdigit()));
        if serial.is_none() {
            tracing::warn!("device certificate carries no serial; ignoring it");
        }
        serial
    }

    async fn authority(
        &self,
        action: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        use tokio::io::AsyncWriteExt;
        let config = self.config()?;
        let mut child = self
            .helper_command()?
            .arg("--authority-action")
            .arg(action)
            .arg("--state")
            .arg(self.directory.join("authority"))
            .arg("--enrollment")
            .arg(config.enrollment_url)
            .arg("--port")
            .arg(self.port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(&serde_json::to_vec(&payload)?).await?;
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(25), child.wait_with_output())
                .await??;
        // Exit 3: the coordinator refused the request, as opposed to being unreachable.
        if output.status.code() == Some(3) {
            #[derive(Deserialize)]
            struct Refusal {
                refused: String,
            }
            let reason =
                serde_json::from_slice::<Refusal>(&output.stdout[..output.stdout.len().min(1024)])
                    .ok()
                    .map(|refusal| refusal.refused);
            bail!(RefusedByCoordinator {
                complaint: helper_complaint(&output.stderr),
                reason,
            });
        }
        ensure!(
            output.status.success(),
            "the network helper exited {} during {action}: {}",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".to_string()),
            helper_complaint(&output.stderr),
        );
        ensure!(
            output.stdout.len() < 8192,
            "the network helper answered {action} with {} bytes, more than the 8192 allowed",
            output.stdout.len(),
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    async fn enroll(
        &self,
        device: &str,
        role: &str,
        registration: &Registration,
    ) -> Result<serde_json::Value> {
        let device = if role == "phone" {
            network_device(device)?
        } else {
            device.to_owned()
        };
        let lock = self.device_lock(&device);
        let _held = lock.lock().await;
        ensure!(
            !self.pending_revocations()?.contains_key(&device),
            "device revocation is pending"
        );
        let payload = self.registration_payload(&device, role, registration)?;
        let enrolled = self.authority("enroll", payload).await?;
        self.ensure_not_revoked_meanwhile(&device)?;
        Ok(enrolled)
    }

    /// A command for the helper, refusing one another account could have replaced, and
    /// passing it only the environment it needs. An inherited `TS_AUTHKEY` could join the
    /// node to another tailnet and an inherited `HTTPS_PROXY` could route its coordinator
    /// traffic, so nothing else is passed through.
    fn helper_command(&self) -> Result<Command> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&self.helper).with_context(|| {
            format!(
                "bundled pondnet helper is missing at {}; build native/pondnet first",
                self.helper.display()
            )
        })?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        ensure!(
            metadata.is_file()
                && (metadata.uid() == euid || metadata.uid() == 0)
                && metadata.mode() & 0o022 == 0,
            "refusing the pondnet helper at {}: it must be a file owned by this account or root \
             and writable by no one else",
            self.helper.display()
        );
        let mut command = Command::new(&self.helper);
        command.env_clear();
        for name in ["HOME", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        Ok(command)
    }

    /// The lock that serialises helper calls about `device`.
    fn device_lock(&self, device: &str) -> Arc<Mutex<()>> {
        self.device_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(device.to_owned())
            .or_default()
            .clone()
    }

    /// A revocation queued while a helper call was in flight wins: the call must not report a
    /// success that the queued revocation is about to undo.
    fn ensure_not_revoked_meanwhile(&self, device: &str) -> Result<()> {
        ensure!(
            !self.pending_revocations()?.contains_key(device),
            "a revocation was queued for this device while it was being enrolled"
        );
        Ok(())
    }

    /// Note that `device` holds an enrollment, or is about to, so a later revocation is sent
    /// without a check. True when the device was not recorded before.
    fn record_enrolled(&self, device: &str) -> Result<bool> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        // The enrollment may change the phone's machine; the next tailnet request asks again.
        self.forget_machine(device)?;
        let mut record = revocation::load_enrolled(&self.directory);
        let inserted = record.devices.insert(device.to_owned());
        if inserted {
            ensure!(
                record.devices.len() <= revocation::MAX_DEVICES,
                "enrollment record is full"
            );
            revocation::write_private_json(&self.directory, revocation::ENROLLED_FILE, &record)?;
        }
        Ok(inserted)
    }

    /// Begin a complete record for a household created just now; an existing one is kept.
    fn start_enrollment_record(&self) -> Result<()> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        if !self.directory.join(revocation::ENROLLED_FILE).exists() {
            revocation::write_private_json(
                &self.directory,
                revocation::ENROLLED_FILE,
                &revocation::Enrolled {
                    complete: true,
                    devices: Default::default(),
                },
            )?;
            tracing::info!("remote access: keeping a complete record of enrolled phones");
        }
        Ok(())
    }

    fn forget_enrolled(&self, device: &str) -> Result<()> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        self.forget_machine(device)?;
        let mut record = revocation::load_enrolled(&self.directory);
        if record.devices.remove(device) {
            revocation::write_private_json(&self.directory, revocation::ENROLLED_FILE, &record)?;
        }
        Ok(())
    }

    /// Callers hold `files`.
    fn forget_machine(&self, device: &str) -> Result<()> {
        let mut known = revocation::load_machines(&self.directory)?;
        if known.remove(device).is_some() {
            revocation::write_private_json(&self.directory, revocation::MACHINES_FILE, &known)?;
        }
        Ok(())
    }

    /// Record the machine `device_id`'s phone enrolled, as the coordinator holds it.
    pub fn remember_phone_machine(&self, device_id: &str, machine: &str) -> Result<()> {
        self.remember_machine(&network_device(device_id)?, machine)
    }

    fn remember_machine(&self, device: &str, machine: &str) -> Result<()> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let mut known = revocation::load_machines(&self.directory)?;
        known.insert(device.to_owned(), machine.to_owned());
        ensure!(
            known.len() <= revocation::MAX_DEVICES,
            "machine record is full"
        );
        revocation::write_private_json(&self.directory, revocation::MACHINES_FILE, &known)
    }

    fn known_machine(&self, device: &str) -> Result<Option<String>> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        Ok(revocation::load_machines(&self.directory)?.remove(device))
    }

    fn registration_payload(
        &self,
        device: &str,
        role: &str,
        registration: &Registration,
    ) -> Result<serde_json::Value> {
        let config = self.config()?;
        ensure!(config.enabled, "remote access needs local approval");
        let expected = url::Url::parse(&config.control_url)?;
        let supplied = url::Url::parse(&registration.auth_url)?;
        ensure!(
            expected.origin() == supplied.origin()
                && supplied.username().is_empty()
                && supplied.password().is_none()
                && supplied.query().is_none()
                && supplied.fragment().is_none(),
            "invalid enrollment origin"
        );
        let auth_id = supplied
            .path()
            .strip_prefix("/register/")
            .context("invalid registration path")?;
        ensure!(
            !auth_id.contains('/') && (16..=256).contains(&auth_id.len()),
            "invalid pending registration"
        );
        // The coordinator checks the same shapes; checked here too so a malformed key is
        // refused before anything is signed for it, and so an empty machine key can never
        // compare equal to an enrollment that lacks one.
        ensure!(
            hex_key(&registration.machine_key, "mkey:")
                && (registration.node_key.is_empty()
                    || hex_key(&registration.node_key, "nodekey:")),
            "invalid node or machine key"
        );
        Ok(
            serde_json::json!({"household":"", "device":device,"role":role,"action":"enroll","authId":auth_id,"nodeKey":registration.node_key,"machineKey":registration.machine_key,"nonce":"","expires":0}),
        )
    }

    fn pending_revocations(&self) -> Result<revocation::Queue> {
        revocation::load_queue(&self.directory, Utc::now())
    }

    fn save_revocations(&self, pending: &revocation::Queue) -> Result<()> {
        revocation::save_queue(&self.directory, pending)
    }

    /// Whether the household's signing key exists. The helper refuses every coordinator call
    /// without it, so with none there is nothing this Pond could revoke; it checks the same way.
    fn has_authority(&self) -> bool {
        std::fs::symlink_metadata(self.directory.join("authority").join("identity.json")).is_ok()
    }

    /// Retry durable revocations at a bounded rate, including while networking is disabled.
    /// Never returns: a fault is logged and retried, because ending this future would end
    /// `serve()` and take the dashboard down with it.
    pub async fn reconcile_revocations(&self) -> std::convert::Infallible {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            // Absent-device revocations are queued, so they survive an offline coordinator.
            if let Err(error) = self.sweep_absent_devices().await {
                tracing::warn!(%error, "could not check which devices have been away too long");
            }
            if let Err(error) = self.reconcile_due().await {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "remote_revocation_store_unavailable",
                    %error,
                    "the remote revocation queue could not be read or written; retrying"
                );
            }
        }
    }

    /// Send the revocations that are due, backing off each failure on its own.
    async fn reconcile_due(&self) -> Result<()> {
        let due = revocation::due(&self.pending_revocations()?, Utc::now());
        for (device, entry) in due {
            let outcome = self.send_revocation(&device, entry.verify).await;
            let unavailable = self.settle(&device, outcome)?;
            // Unreachable is unreachable for every device; a refusal is about this one.
            if unavailable {
                break;
            }
        }
        Ok(())
    }

    /// Record what became of one attempt; true when the coordinator could not be reached.
    fn settle(&self, device: &str, outcome: Sent) -> Result<bool> {
        let files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let mut queue = self.pending_revocations()?;
        let (reason, unavailable) = match outcome {
            Sent::Revoked | Sent::NeverEnrolled => {
                queue.remove(device);
                self.save_revocations(&queue)?;
                drop(files);
                self.forget_enrolled(device)?;
                if matches!(outcome, Sent::Revoked) {
                    tracing::info!(%device, "queued remote network revocation completed");
                } else {
                    tracing::info!(
                        %device,
                        "queued remote revocation dropped: the coordinator has no enrollment for this device"
                    );
                }
                return Ok(false);
            }
            Sent::Refused(reason) => (reason, false),
            Sent::Unavailable(reason) => (reason, true),
        };
        if let Some(entry) = queue.get_mut(device) {
            entry.back_off(Utc::now());
            tracing::warn!(
                %device,
                attempts = entry.attempts,
                next_attempt = %entry.next_attempt,
                unavailable,
                %reason,
                "remote network revocation remains queued"
            );
        }
        self.save_revocations(&queue)?;
        Ok(unavailable)
    }

    async fn send_revocation(&self, device: &str, verify: bool) -> Sent {
        if !self.has_authority() {
            // Without the signing key the helper exits 1 every time. A phone the record lists stays
            // queued, backing off, and goes out once the key is restored. An entry that needs a
            // check first cannot get one either, so it is dropped rather than retried forever.
            if !verify {
                return Sent::Unavailable("this Pond has no household authority".into());
            }
            tracing::warn!(
                target: "giap::trace",
                kind = "remote_revocation_without_authority",
                %device,
                "queued remote revocation dropped: this Pond has no household authority to check \
                 whether the device was ever enrolled"
            );
            return Sent::NeverEnrolled;
        }
        let lock = self.device_lock(device);
        let _held = lock.lock().await;
        let classify = |error: anyhow::Error| match error.downcast_ref::<RefusedByCoordinator>() {
            Some(refused) if refused.reason.as_deref() == Some("enrollment_missing") => {
                Sent::NeverEnrolled
            }
            Some(refused) => Sent::Refused(refused.to_string()),
            None => Sent::Unavailable(format!("{error:#}")),
        };
        if verify {
            if let Err(error) = self
                .authority(
                    "inspect",
                    serde_json::json!({"device": device, "role": "phone"}),
                )
                .await
            {
                return classify(error);
            }
        }
        match self
            .authority(
                "revoke",
                serde_json::json!({
                    "household":"", "device":device, "role":"phone", "action":"revoke",
                    "authId":"", "nodeKey":"", "nonce":"", "expires":0
                }),
            )
            .await
        {
            Ok(_) => Sent::Revoked,
            Err(error) => classify(error),
        }
    }

    /// The networking helper's state as it last reported it: `Running`, `Starting`, `NeedsLogin`,
    /// `Unavailable` when the helper died, `Stopped` when it was stopped.
    pub fn state(&self) -> String {
        self.status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone()
    }

    /// Current native addresses for certificate SAN renewal.
    pub fn addresses(&self) -> Vec<String> {
        self.status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .addresses
            .clone()
    }

    /// Start once, keeping stdin open so a normal shutdown stops the helper.
    pub async fn start(self: &Arc<Self>, config: Config) -> Result<()> {
        ensure!(config.enabled, "remote access must be explicitly enabled");
        let config = with_default_coordinator(config);
        ensure!(
            !config.enrollment_url.is_empty(),
            "enrollment service is required"
        );
        let enrollment = url::Url::parse(&config.enrollment_url)?;
        ensure!(
            enrollment.scheme() == "https"
                && enrollment.host_str().is_some()
                && enrollment.username().is_empty()
                && enrollment.password().is_none()
                && enrollment.query().is_none()
                && enrollment.fragment().is_none()
                && matches!(enrollment.path(), "" | "/"),
            "invalid enrollment origin"
        );
        ensure!(
            !config.control_url.is_empty(),
            "Headscale control server is required"
        );
        {
            let url = url::Url::parse(&config.control_url)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && matches!(url.path(), "" | "/"),
                "control server must be an HTTPS origin"
            );
        }
        let mut process = self.process.lock().await;
        if let Some(current) = process.as_mut() {
            if current.child.try_wait()?.is_none() {
                ensure!(
                    self.config()?.control_url == config.control_url,
                    "stop remote access before changing the control server"
                );
                return Ok(());
            }
            *process = None;
        }
        let mut child = self
            .helper_command()?
            .arg("--state")
            .arg(self.directory.join("node"))
            .arg("--hostname")
            .arg(crate::tls_identity::TAILNET_HOSTNAME)
            .arg("--control")
            .arg(&config.control_url)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--identity")
            .arg(&self.identity)
            .arg("--port")
            .arg(self.port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("start embedded network helper")?;
        let input = child.stdin.take().context("embedded stdin unavailable")?;
        let output = child.stdout.take().context("embedded status unavailable")?;
        self.persist(&config)?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *process = Some(Process {
            child,
            _stdin: input,
        });
        self.publish(Status {
            state: "Starting".into(),
            addresses: vec![],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        });
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = Vec::new();
                match (&mut reader).take(16385).read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => (),
                }
                if generation != runtime.generation.load(Ordering::SeqCst) {
                    return;
                }
                if line.len() > 16384 {
                    break;
                }
                let Ok(mut status) = serde_json::from_slice::<Status>(&line) else {
                    break;
                };
                if status.state.len() > 64
                    || status.addresses.len() > 8
                    || status.node_key.len() > 80
                    || status.machine_key.len() > 80
                {
                    break;
                }
                status
                    .addresses
                    .retain(|s| s.parse::<std::net::IpAddr>().is_ok_and(is_tailnet));
                status.auth_url = status.auth_url.filter(|s| {
                    url::Url::parse(s).is_ok_and(|u| {
                        u.scheme() == "https" && u.username().is_empty() && u.password().is_none()
                    })
                });
                runtime.publish(status);
            }
            let mut process = runtime.process.lock().await;
            if generation == runtime.generation.load(Ordering::SeqCst) {
                if let Some(mut failed) = process.take() {
                    let _ = failed.child.kill().await;
                }
                tracing::error!("embedded networking helper stopped unexpectedly");
                runtime.publish(Status {
                    state: "Unavailable".into(),
                    addresses: vec![],
                    auth_url: None,
                    node_key: String::new(),
                    machine_key: String::new(),
                });
            }
        });
        Ok(())
    }

    /// Stop and persist disablement without deleting the registered node identity.
    pub async fn disable(&self) -> Result<()> {
        let mut config = self.config()?;
        config.enabled = false;
        self.persist(&config)?;
        self.shutdown().await
    }

    /// Stop the helper when the Pond exits, retaining the startup preference.
    pub async fn shutdown(&self) -> Result<()> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.recovery.clear();
        if let Some(mut process) = self.process.lock().await.take() {
            drop(process._stdin);
            if tokio::time::timeout(std::time::Duration::from_secs(10), process.child.wait())
                .await
                .is_err()
            {
                process.child.kill().await?;
            }
        }
        self.publish(Status {
            state: "Stopped".into(),
            addresses: vec![],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        });
        Ok(())
    }
}

fn network_device(id: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    ensure!(
        !id.is_empty() && id.len() <= 256,
        "invalid paired device identity"
    );
    let mut digest = Sha256::new();
    digest.update(b"goose-enrollment-device-v1\0");
    digest.update(id.as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

/// `prefix` followed by 64 lowercase hex digits, as the coordinator writes keys.
fn hex_key(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_device(id: &str) -> bool {
    (16..=80).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// When each device was last seen at home; kept with the remote-access files, not in `devices`.
/// Keyed by network-hashed id, like the queue and the enrollment record.
const PRESENCE_FILE: &str = "presence.json";
/// Bounds on the presence file; a household has far fewer devices.
const PRESENCE_MAX_BYTES: u64 = 256 * 1024;
const PRESENCE_MAX_DEVICES: usize = 1024;
/// A sighting is written at most this often per device: every authenticated request is one.
const PRESENCE_WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl Runtime {
    /// Sightings by network id. A corrupt file must not stop devices lapsing, which is how it
    /// failed open before: it is moved aside, and every enrolled phone is treated as last seen
    /// when the file was last written, so each still lapses within the window.
    fn presence(&self) -> Result<BTreeMap<String, String>> {
        let path = self.directory.join(PRESENCE_FILE);
        match revocation::read_private_json::<BTreeMap<String, String>>(&path, PRESENCE_MAX_BYTES) {
            Ok(None) => Ok(BTreeMap::new()),
            Ok(Some(seen)) if seen.len() <= PRESENCE_MAX_DEVICES => Ok(seen
                .into_iter()
                .map(|(device, at)| {
                    // Written before sightings were hashed.
                    let device = if valid_device(&device) && device.len() == 64 {
                        device
                    } else {
                        network_device(&device).unwrap_or(device)
                    };
                    (device, at)
                })
                .collect()),
            outcome => {
                let reason = match outcome {
                    Err(error) => format!("{error:#}"),
                    _ => "too many devices".to_owned(),
                };
                let written = std::fs::symlink_metadata(&path)
                    .and_then(|m| m.modified())
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now());
                let aside = self.directory.join(format!(
                    "{PRESENCE_FILE}.corrupt-{}",
                    Utc::now().timestamp()
                ));
                std::fs::rename(&path, &aside)?;
                let seen: BTreeMap<String, String> = revocation::load_enrolled(&self.directory)
                    .devices
                    .into_iter()
                    .map(|device| (device, written.to_rfc3339()))
                    .collect();
                tracing::warn!(
                    target: "giap::trace",
                    kind = "presence_record_replaced",
                    %reason,
                    aside = %aside.display(),
                    devices = seen.len(),
                    "the presence record was unreadable; enrolled phones now lapse from its last write"
                );
                self.save_presence(&seen)?;
                Ok(seen)
            }
        }
    }

    fn save_presence(&self, seen: &BTreeMap<String, String>) -> Result<()> {
        revocation::write_private_json(&self.directory, PRESENCE_FILE, seen)
    }

    /// Queue revocation for every device not home within the window; the retry loop sends them.
    async fn sweep_absent_devices(&self) -> Result<()> {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        // A local-only household must not contact coordination.
        if self.config()?.enrollment_url.is_empty() {
            return Ok(());
        }
        let cutoff = Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS);
        for device in self.absent_since(cutoff).await? {
            // Also forgets the sighting, or every sweep would re-queue this revocation. A device
            // that cannot be queued keeps it, is retried next sweep, and holds back no other.
            if let Err(error) = self.queue_network(&device) {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "remote_access_lapse_not_queued",
                    %device,
                    error = %format!("{error:#}"),
                    "could not queue the revocation of a device away too long; retrying"
                );
                continue;
            }
            tracing::warn!(
                target: "giap::trace",
                kind = "remote_access_lapsed",
                %device,
                window_days = LAN_PRESENCE_WINDOW_DAYS,
                "remote access lapsed: this device has not been on the household network inside \
                 the window, so its remote access is being revoked. Local pairing is untouched, \
                 and bringing it home restores it."
            );
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl pond_core::security::ports::remote_access::DevicePresence for Runtime {
    async fn seen_on_lan(&self, device_id: &str) {
        let Ok(device) = network_device(device_id) else {
            return;
        };
        {
            let mut written = self
                .sightings_written
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if written
                .get(&device)
                .is_some_and(|at| at.elapsed() < PRESENCE_WRITE_INTERVAL)
            {
                return;
            }
            written.insert(device.clone(), std::time::Instant::now());
        }
        let write = || -> Result<()> {
            let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
            let mut seen = self.presence()?;
            if !seen.contains_key(&device) && seen.len() >= PRESENCE_MAX_DEVICES {
                bail!("the presence record is full");
            }
            seen.insert(device.clone(), Utc::now().to_rfc3339());
            self.save_presence(&seen)
        };
        if let Err(error) = write() {
            // Never fails the request; a lost sighting only brings the device's reminder forward.
            self.sightings_written
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&device);
            tracing::warn!(%error, %device, "could not record a LAN sighting");
        }
    }

    async fn absent_since(&self, cutoff: DateTime<Utc>) -> Result<Vec<String>> {
        Ok(self
            .presence()?
            .into_iter()
            .filter(|(_, seen)| {
                DateTime::parse_from_rfc3339(seen)
                    .map(|at| at.with_timezone(&Utc) < cutoff)
                    // An unreadable timestamp is not evidence of absence.
                    .unwrap_or(false)
            })
            .map(|(device, _)| device)
            .collect())
    }

    async fn lapses_at(&self, device_id: &str) -> Result<Option<DateTime<Utc>>> {
        use pond_core::security::ports::remote_access::LAN_PRESENCE_WINDOW_DAYS;
        let device = network_device(device_id)?;
        Ok(self.presence()?.get(&device).and_then(|seen| {
            DateTime::parse_from_rfc3339(seen)
                .ok()
                .map(|at| at.with_timezone(&Utc) + chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS))
        }))
    }
}

#[async_trait::async_trait]
impl pond_core::security::ports::remote_access::RemoteRevocation for Runtime {
    async fn queue(&self, device_id: &str) -> Result<()> {
        self.queue_network(&network_device(device_id)?)
    }
}

impl Runtime {
    /// Queue the revocation of a network-hashed device, and forget its sightings.
    ///
    /// The sighting goes only once the revocation is saved, or once there is nothing to revoke:
    /// it is what makes the absence sweep queue the device again, so dropping it before a save
    /// that then failed left a phone whose remote access never lapsed.
    fn queue_network(&self, device: &str) -> Result<()> {
        // A local-only household has never registered a node and must not contact coordination.
        if self.config()?.enrollment_url.is_empty() {
            return Ok(());
        }
        let device = device.to_owned();
        self.recovery.revoke(&device);
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let record = revocation::load_enrolled(&self.directory);
        let plan = revocation::plan(&record, &device);
        if !self.has_authority() {
            // The helper cannot sign anything without the household key. A phone the record lists
            // is queued all the same and goes out once the key is restored from its backup; one it
            // does not list would need a check with the coordinator, so nothing is queued.
            if !matches!(plan, revocation::Plan::Revoke) {
                tracing::debug!(%device, "no remote revocation queued: this Pond has no household authority to check with");
                return self.forget_sighting(&device);
            }
            tracing::warn!(
                target: "giap::trace",
                kind = "remote_revocation_without_authority",
                %device,
                "remote revocation queued while the household authority is missing; it goes out \
                 once authority/identity.json is restored from its backup"
            );
        }
        let verify = match plan {
            revocation::Plan::Revoke => false,
            revocation::Plan::VerifyThenRevoke => true,
            revocation::Plan::NothingToRevoke => {
                tracing::debug!(%device, "no remote revocation needed: this device was never enrolled");
                return self.forget_sighting(&device);
            }
        };
        let mut pending = self.pending_revocations()?;
        ensure!(
            pending.contains_key(&device) || pending.len() < revocation::MAX_DEVICES,
            "remote revocation queue is full"
        );
        let entry = pending
            .entry(device.clone())
            .or_insert_with(|| revocation::Pending::new(verify, Utc::now()));
        // A device learned to be enrolled since it was queued needs no check any more.
        entry.verify &= verify;
        self.save_revocations(&pending)?;
        tracing::info!(%device, verify, "remote network revocation queued durably");
        self.forget_sighting(&device)
    }

    /// Drop `device`'s sighting, under the files lock, and its write throttle, so the next
    /// request from home records a fresh sighting instead of waiting out the hour.
    fn forget_sighting(&self, device: &str) -> Result<()> {
        self.sightings_written
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(device);
        let mut seen = self.presence()?;
        if seen.remove(device).is_some() {
            self.save_presence(&seen)?;
        }
        Ok(())
    }
}

fn local(
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<(), StatusCode> {
    if peer.is_ok_and(|p| p.0.ip().is_loopback()) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn status(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<Status>, StatusCode> {
    local(peer)?;
    Ok(Json(
        runtime
            .status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone(),
    ))
}

async fn enable(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    Json(config): Json<Config>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    runtime.start(config).await.map_err(|error| {
        tracing::warn!(%error, "could not enable embedded networking");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok(Json(serde_json::json!({"accepted":true})))
}

async fn disable(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    runtime.disable().await.map_err(|error| {
        tracing::warn!(%error, "could not disable embedded networking");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok(Json(serde_json::json!({"accepted":true})))
}

/// Refuse management without the host credential: these routes can re-point the Pond and
/// every paired phone at another coordinator, and a bearer token proves only that its holder
/// once had a pairing code.
async fn host_only(
    State(credential): State<pond_api::host_guard::HostCredential>,
    request: Request,
    next: Next,
) -> Response {
    match pond_api::host_guard::require_credential(
        Some(&credential),
        request.headers(),
        "remote_access_management",
    ) {
        Ok(()) => next.run(request).await,
        Err(refusal) => refusal.into_response(),
    }
}

/// Management is available only on the loopback listener, guarded by socket peer and the
/// host credential.
pub fn management(
    runtime: Arc<Runtime>,
    credential: pond_api::host_guard::HostCredential,
) -> Router {
    Router::new()
        .route(
            "/api/v1/remote-access",
            get(status).post(enable).delete(disable),
        )
        .route(
            "/api/v1/remote-access/identity",
            axum::routing::post(authority_identity),
        )
        .route(
            "/api/v1/remote-access/register",
            axum::routing::post(register_pond),
        )
        .route("/api/v1/remote-access/device", get(device_status))
        .merge(recovery::local_routes())
        .with_state(runtime)
        .route_layer(middleware::from_fn_with_state(credential, host_only))
        .layer(middleware::from_fn(pond_api::middleware::log_requests))
        // The desktop renderer calls these cross-origin, like every other dashboard route.
        .layer(pond_api::cors_layer())
}

async fn trusted_peer(mut request: Request, next: Next) -> Result<Response, StatusCode> {
    let peer = request
        .headers_mut()
        .remove(PEER_HEADER)
        .and_then(|v| v.to_str().ok().and_then(|s| s.parse::<SocketAddr>().ok()))
        .filter(|p| p.port() != 0 && is_tailnet(p.ip()))
        .ok_or(StatusCode::FORBIDDEN)?;
    let machine = request
        .headers_mut()
        .remove(MACHINE_HEADER)
        .and_then(|v| v.to_str().ok().map(str::to_owned))
        .filter(|key| hex_key(key, "mkey:"))
        .ok_or(StatusCode::FORBIDDEN)?;
    request.extensions_mut().insert(ConnectInfo(peer));
    request.extensions_mut().insert(TailnetMachine(machine));
    Ok(next.run(request).await)
}

/// Only use on the private Unix socket. Public listeners ignore peer headers.
pub fn private_companion(router: Router, machines: Arc<dyn RemoteMachines>) -> Router {
    router
        .layer(axum::Extension(machines))
        .layer(middleware::from_fn(trusted_peer))
}

/// Pending registration, with no authority or administration secret.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Registration {
    auth_url: String,
    node_key: String,
    machine_key: String,
}

/// A coordinator refusal rather than a fault, typed so the handler can pick a distinct status.
#[derive(Debug)]
pub struct RefusedByCoordinator {
    /// The helper's stderr, bounded and redacted.
    pub complaint: String,
    /// The coordinator's own error identifier, e.g. `enrollment_missing`.
    pub reason: Option<String>,
}

impl std::fmt::Display for RefusedByCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.complaint)
    }
}

/// What became of one attempt to send a queued revocation.
enum Sent {
    Revoked,
    /// The coordinator has no such enrollment: nothing to revoke, and no tombstone written.
    NeverEnrolled,
    Refused(String),
    Unavailable(String),
}

impl std::error::Error for RefusedByCoordinator {}

/// Whether the coordinator refused an enrollment because it holds one for another identity.
/// A 429 or 503 also exits 3, so the reason is what tells the two apart.
fn kept_existing_enrollment(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RefusedByCoordinator>()
        .is_some_and(|refused| refused.reason.as_deref() == Some("invalid_enrollment"))
}

/// Whether the coordinator refused before writing anything for the call, so nothing was enrolled.
/// `storage_unavailable` can come from the write itself and `enrollment_incomplete` after it, and
/// an answer without a reason may be a gateway's, so none of those count.
fn refused_before_writing(error: &anyhow::Error) -> bool {
    const BEFORE_ANY_WRITE: &[&str] = &[
        "approval_used",
        "busy",
        "capacity",
        "coordinator_unavailable",
        "identity_already_enrolled",
        "identity_already_registered",
        "identity_retired",
        "invalid_enrollment",
        "invalid_request",
        "pond_registration_required",
        "rate_limited",
        "replacement_conflict",
        "retired_capacity",
        "unauthorized",
    ];
    error
        .downcast_ref::<RefusedByCoordinator>()
        .and_then(|refused| refused.reason.as_deref())
        .is_some_and(|reason| BEFORE_ANY_WRITE.contains(&reason))
}

/// The coordinator's refusal of a replacement because the household's retired records are
/// at their bound. It passes on its own as old identities stay off the coordinator and their
/// records expire, so neither recovery nor a retry right away can help.
const RETIRED_CAPACITY: &str = "retired_capacity";

/// Whether `error` is the coordinator refusing with [`RETIRED_CAPACITY`].
fn retired_capacity(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RefusedByCoordinator>()
        .is_some_and(|refused| refused.reason.as_deref() == Some(RETIRED_CAPACITY))
}

/// How a phone's enrollment request is refused: a status, and for a refusal the phone should
/// be able to tell apart, its reason as `{"error": reason}`.
#[derive(Debug, PartialEq)]
struct PhoneRefusal {
    status: StatusCode,
    reason: Option<&'static str>,
}

impl PhoneRefusal {
    /// [`RETIRED_CAPACITY`], as 503: the household can do nothing about it now, and a 409
    /// would send a phone that reads only the status to recovery, which meets the same bound.
    fn retired_capacity() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: Some(RETIRED_CAPACITY),
        }
    }
}

impl From<StatusCode> for PhoneRefusal {
    fn from(status: StatusCode) -> Self {
        Self {
            status,
            reason: None,
        }
    }
}

impl IntoResponse for PhoneRefusal {
    fn into_response(self) -> Response {
        match self.reason {
            Some(reason) => {
                (self.status, Json(serde_json::json!({ "error": reason }))).into_response()
            }
            None => self.status.into_response(),
        }
    }
}

/// The helper's stderr, fit to log: bounded, one line, and with `/register/` URLs redacted
/// (a bearer capability to join the household's tailnet).
fn helper_complaint(stderr: &[u8]) -> String {
    const KEEP: usize = 400;
    let text = String::from_utf8_lossy(stderr);
    let redacted: Vec<&str> = text
        .split_whitespace()
        .map(|word| {
            if word.contains("/register/") {
                "<redacted enrolment URL>"
            } else {
                word
            }
        })
        .collect();
    let line = redacted.join(" ");
    if line.is_empty() {
        return "and said nothing".to_string();
    }
    match line.char_indices().nth_back(KEEP) {
        // Keep the tail: the helper prints its reason last.
        Some((at, _)) => format!("...{}", &line[at..]),
        None => line,
    }
}

async fn authority_identity(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    // No authority yet means no phone was ever enrolled, so the record can start complete.
    let first = !runtime.directory.join("authority").exists();
    let identity = runtime
        .authority("identity", serde_json::Value::Null)
        .await
        .map_err(|error| {
            tracing::warn!(%error, operation = "identity", "embedded enrollment failed");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    if first {
        runtime.start_enrollment_record().map_err(|error| {
            tracing::warn!(%error, operation = "enrollment_record", "embedded enrollment failed");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    }
    Ok(Json(identity))
}
/// Body of `POST /remote-access/register`. `deny_unknown_fields`, so a misspelt field fails
/// instead of registering without the invite it meant to carry.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RegisterPond {
    /// The operator's invite, needed once, for a household the coordinator has not seen.
    #[serde(default)]
    invite: Option<String>,
}

/// Loosely the coordinator's invite shape; it decides validity, this only refuses what
/// could never be one before it reaches the helper.
/// Whether this Pond was imaged with a device certificate, and the serial it was issued
/// under, so the dashboard can say why no invite is needed and an operator can quote the
/// serial to revoke a lost Pond. The helper presents the certificate itself; nothing here
/// signs or sends it.
async fn device_status(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    let registered = runtime.registered();
    Ok(Json(match runtime.provisioned_serial() {
        Some(serial) => {
            serde_json::json!({ "provisioned": true, "serial": serial, "registered": registered })
        }
        None => serde_json::json!({ "provisioned": false, "registered": registered }),
    }))
}

fn plausible_invite(invite: &str) -> bool {
    invite.len() <= 64
        && invite
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("giap-inv1-"))
        && invite
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b' ')
}

/// A refusal the client can act on: `status` with `{"error": error}`.
fn refusal(status: StatusCode, error: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": error })))
}

async fn register_pond(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    body: Option<Json<RegisterPond>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let refuse = refusal;
    local(peer).map_err(|status| refuse(status, "host_only"))?;
    let invite = body
        .map(|Json(body)| body.invite)
        .unwrap_or_default()
        .map(|invite| invite.trim().to_owned())
        .filter(|invite| !invite.is_empty());
    if invite
        .as_deref()
        .is_some_and(|invite| !plausible_invite(invite))
    {
        return Err(refuse(StatusCode::BAD_REQUEST, "invite_invalid"));
    }
    let current = runtime
        .status
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let registration = Registration {
        auth_url: current
            .auth_url
            .ok_or_else(|| refuse(StatusCode::CONFLICT, "no_pending_registration"))?,
        node_key: current.node_key,
        machine_key: current.machine_key,
    };
    // A fresh Pond has no household key until something creates one, and only the self-hosting
    // panel's "Prepare household identity" did, so the hosted Enable flow reached registration
    // without one and the helper refused it. The identity action creates the key once and
    // otherwise loads it, so asking for it on every registration is safe.
    if let Err(error) = runtime.authority("identity", serde_json::Value::Null).await {
        tracing::warn!(%error, operation = "identity", "embedded enrollment failed");
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "registration_unavailable",
        ));
    }
    // Register the household first; idempotent for one already registered, which needs no
    // invite. The invite goes to the helper on stdin and is never stored here.
    if let Err(error) = runtime
        .authority(
            "register",
            serde_json::json!({ "invite": invite.unwrap_or_default() }),
        )
        .await
    {
        let reason = error
            .downcast_ref::<RefusedByCoordinator>()
            .and_then(|refused| refused.reason.clone());
        tracing::warn!(%error, reason = ?reason, "household registration failed");
        return Err(match reason.as_deref() {
            Some(
                reason @ ("invite_required"
                | "invite_invalid"
                | "invite_expired"
                | "invite_used"
                | "device_certificate_invalid"
                | "device_revoked"
                | "device_used"),
            ) => refuse(StatusCode::FORBIDDEN, reason),
            _ => refuse(StatusCode::SERVICE_UNAVAILABLE, "registration_unavailable"),
        });
    }
    runtime.record_registered();
    runtime
        .enroll("pond000000000001", "pond", &registration)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, operation = "enroll", "embedded enrollment failed");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "enrollment_unavailable")
        })
}
async fn remote_configuration(
    State(runtime): State<Arc<Runtime>>,
    axum::Extension(principal): axum::Extension<pond_core::security::ports::policy::Principal>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let device =
        pond_core::security::domain::proven_device::ProvenDevice::from_principal(&principal);
    device.id().ok_or(StatusCode::FORBIDDEN)?;
    let config = runtime.config().map_err(|error| {
        tracing::warn!(%error, operation = "config", "embedded enrollment failed");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    // When remote access lapses unless the device comes home, so the app can warn first.
    let lapses_at = match device.id() {
        Some(id) => {
            use pond_core::security::ports::remote_access::DevicePresence;
            runtime
                .lapses_at(id)
                .await
                .unwrap_or_default()
                .map(|at| at.to_rfc3339())
        }
        None => None,
    };
    Ok(Json(serde_json::json!({
        "enabled": config.enabled,
        "controlUrl": config.control_url,
        "state": runtime.status.read().unwrap_or_else(|p| p.into_inner()).state,
        "lapsesAt": lapses_at,
    })))
}
/// Resolves a raw device id's role the same way `pond-api`'s `resolve_role_from_principal`
/// does for a chat turn: attribution first, the sole-member household fallback otherwise.
/// This module authenticates its own caller via `recovery::caller`, not `Principal`, so it has
/// a device id directly rather than something to run `ProvenDevice` over.
async fn resolve_role_for_device(
    device_id: &str,
    profile_repo: &dyn ProfileRepository,
    device_attribution: &dyn DeviceAttribution,
) -> Option<Role> {
    let device_rung = DeviceRung::from_attribution(device_attribution.device_profile(device_id).await);
    let household_has_multiple_members = match profile_repo.list().await {
        Ok(profiles) => profiles.len() > 1,
        Err(_) => true,
    };
    let resolved = identity_resolution::resolve(&identity_resolution::ResolutionInputs {
        paired_device_profile: device_rung.profile_id(),
        session: &SessionIdentity::unknown(),
        household_has_multiple_members,
    });
    match resolved.scope {
        ProfileScope::Owner(id) => profile_repo.get(&id).await.ok().flatten().map(|p| p.role),
        ProfileScope::Household => Some(Role::Owner),
        ProfileScope::Guest => None,
    }
}

async fn register_phone(
    State(runtime): State<Arc<Runtime>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    axum::Extension(handshake): axum::Extension<
        Arc<dyn pond_core::security::ports::handshake::Handshake>,
    >,
    axum::Extension(profile_repo): axum::Extension<
        Arc<dyn pond_core::user_data::ports::profile::ProfileRepository>,
    >,
    axum::Extension(device_attribution): axum::Extension<
        Arc<dyn pond_core::user_data::ports::device_attribution::DeviceAttribution>,
    >,
    headers: axum::http::HeaderMap,
    Json(registration): Json<Registration>,
) -> Result<Json<serde_json::Value>, Response> {
    pond_api::network::require_lan(Some(ConnectInfo(peer)))
        .map_err(|_| StatusCode::FORBIDDEN.into_response())?;
    let device = recovery::caller(&headers, handshake.as_ref())
        .await
        .map_err(IntoResponse::into_response)?
        .device;

    let role = resolve_role_for_device(&device, profile_repo.as_ref(), device_attribution.as_ref())
        .await;
    if let pond_core::user_data::services::household_permission::PermissionDecision::Denied {
        reason,
        grantable_by,
    } = pond_core::user_data::services::household_permission::may_enroll_remote_access(role)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": reason, "who_can_grant": grantable_by.as_str()})),
        )
            .into_response());
    }
    let lock = runtime.device_lock(&device);
    let _held = lock.lock().await;
    let store_failed = |error: anyhow::Error| {
        tracing::warn!(%error, %device, operation = "enrollment_store", "embedded enrollment failed");
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    };
    if runtime
        .pending_revocations()
        .map_err(store_failed)?
        .contains_key(&device)
    {
        return Err(StatusCode::CONFLICT.into_response());
    }
    let payload = runtime
        .registration_payload(&device, "phone", &registration)
        .map_err(|error| {
            tracing::warn!(%error, %device, operation = "registration_payload", "embedded enrollment failed");
            StatusCode::CONFLICT.into_response()
        })?;
    // Recorded before the helper is asked: a removal, a logout or a sweep while the enrollment is
    // in flight (up to 25 s), or a crash before it returns, must find this device and revoke it.
    // A record for an enrollment that never happened costs one unchecked revocation; a missing
    // record for one that did leaves remote access nothing ever revokes.
    let inserted = runtime.record_enrolled(&device).map_err(store_failed)?;

    // Skip re-enrolling a phone already active with the same identity (it would raise a
    // conflict); any other answer, or none, falls through to the enrollment below.
    let decided = match runtime.authority("inspect", payload.clone()).await {
        Ok(existing) => {
            let decided = phone_enrollment(&existing, &registration.machine_key);
            let status = existing
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            match &decided {
                PhoneEnrollment::AlreadyEnrolled => {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "remote_access_already_enrolled",
                        %device,
                        "remote access: already enrolled with this identity and active; nothing to do"
                    );
                    if let Err(error) = runtime.ensure_not_revoked_meanwhile(&device) {
                        tracing::warn!(%error, %device, "embedded enrollment superseded by a revocation");
                        return Err(StatusCode::CONFLICT.into_response());
                    }
                    remember_enrolled_machine(&runtime, &device, &registration.machine_key);
                    return Ok(Json(existing));
                }
                PhoneEnrollment::StaleIdentity => {
                    // Expected, not a fault: a lapsed or removed phone that was not re-paired
                    // still holds the key its stood-down record keeps as a tombstone, and the
                    // coordinator refuses both a replace and an enroll for that key. The app
                    // answers by making fresh node keys and enabling again.
                    tracing::info!(
                        target: "giap::trace",
                        kind = "remote_access_stale_identity",
                        %device, %status,
                        "remote access: the phone still holds the identity of its stood-down \
                         enrollment; it needs fresh node keys"
                    );
                    return Err(refusal(StatusCode::CONFLICT, "stale_identity").into_response());
                }
                PhoneEnrollment::Replace(_) => tracing::info!(
                    target: "giap::trace",
                    kind = "remote_access_reenrolling",
                    %device, %status,
                    "remote access: the phone's previous enrollment was stood down, so it is replaced"
                ),
                PhoneEnrollment::Enroll => tracing::info!(
                    %device, %status,
                    "remote access: an existing enrollment does not match, so enrolling again"
                ),
            }
            Some(decided)
        }
        Err(error) => {
            // Not a failure: the enrollment below is the authority.
            tracing::info!(%error, %device, "remote access: could not inspect the existing enrollment; enrolling");
            None
        }
    };
    // Only a plain enrollment over a record another identity holds is expected to be refused:
    // that refusal sends the phone to recovery. A refused replace is a fault.
    let refusal_expected = matches!(decided, Some(PhoneEnrollment::Enroll));
    // The coordinator reported an enrollment for this device, so the record stays whatever
    // becomes of this call: a later removal must revoke what it holds.
    let coordinator_holds_one = decided.is_some();
    let (operation, payload) = match decided {
        Some(PhoneEnrollment::Replace(revision)) => {
            let mut payload = payload;
            payload["expectedRevision"] = serde_json::Value::String(revision);
            ("replace", payload)
        }
        _ => ("enroll", payload),
    };

    let enrolled = match runtime.authority(operation, payload).await {
        Ok(enrolled) => enrolled,
        Err(error) => {
            if retired_capacity(&error) {
                tracing::warn!(
                    %error,
                    %device,
                    "remote access: the coordinator holds as many retired enrollments as it allows for this household, so the phone cannot be replaced until older ones expire"
                );
                return Err(PhoneRefusal::retired_capacity().into_response());
            }
            let refused = kept_existing_enrollment(&error);
            if refused && refusal_expected {
                // Expected, not a fault: the coordinator will not let a new identity take over an
                // enrollment it already holds. The 409 offers the phone recovery, which someone
                // approves on the home network.
                tracing::info!(
                    target: "giap::trace",
                    kind = "remote_access_recovery_required",
                    %error,
                    %device,
                    "remote access: the coordinator kept the existing enrollment; the phone must recover it"
                );
            } else {
                tracing::warn!(%error, %device, refused, operation = "enroll_phone", "embedded enrollment failed");
            }
            // Refused before the coordinator wrote anything, so nothing was enrolled: a device
            // recorded for this call alone is forgotten. Anything else may have enrolled it, so its
            // record stays and a later removal revokes it unchecked, the safe way to be wrong.
            if refused_before_writing(&error) && inserted && !coordinator_holds_one {
                match runtime.forget_enrolled(&device) {
                    Ok(()) => tracing::info!(
                        target: "giap::trace",
                        kind = "remote_access_enrollment_unrecorded",
                        %device,
                        "remote access: the coordinator refused the enrollment, so it is no longer recorded"
                    ),
                    Err(error) => tracing::warn!(
                        %error,
                        %device,
                        operation = "enrollment_store",
                        "could not forget a refused enrollment; a later removal revokes it unchecked"
                    ),
                }
            }
            return Err(if refused {
                StatusCode::CONFLICT.into_response()
            } else {
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            });
        }
    };
    // Recorded before the call, so a revocation that raced in goes out unchecked; the phone
    // must not be told it is enrolled when that revocation is about to undo it.
    if let Err(error) = runtime.ensure_not_revoked_meanwhile(&device) {
        tracing::warn!(%error, %device, "embedded enrollment superseded by a revocation");
        return Err(StatusCode::CONFLICT.into_response());
    }
    tracing::info!(%device, "remote access: phone enrolled");
    remember_enrolled_machine(&runtime, &device, &registration.machine_key);
    Ok(Json(enrolled))
}

/// The coordinator has just accepted this machine, so its first tailnet request need not ask.
/// A failed write only costs that request one lookup.
fn remember_enrolled_machine(runtime: &Runtime, device: &str, machine: &str) {
    if let Err(error) = runtime.remember_machine(device, machine) {
        tracing::warn!(%error, %device, "could not record the phone's machine; its first remote request will look it up");
    }
}
/// What a phone enrolling from the home network asks the coordinator for, given the
/// enrollment the coordinator already holds for its device.
#[derive(Debug, PartialEq)]
enum PhoneEnrollment {
    /// Active with this identity: nothing to do.
    AlreadyEnrolled,
    /// A first enrollment. Also what an active enrollment held by another identity gets,
    /// which the coordinator refuses, sending the phone to recovery and its local approval.
    Enroll,
    /// A stood-down enrollment, replaced at the revision the coordinator reported.
    Replace(String),
    /// A stood-down (`revoked` or `failed`) enrollment that still holds the phone's current
    /// machine key. The coordinator keeps that key as a tombstone and refuses a `replace` or an
    /// `enroll` that presents it (`identity_already_enrolled`), so nothing is sent: the phone
    /// must make fresh node keys first.
    StaleIdentity,
}

/// Decides [`PhoneEnrollment`] from the coordinator's `inspect` answer.
///
/// A phone removed from the household, or whose remote access lapsed, leaves a `revoked`
/// enrollment under its device id, and a re-paired phone keeps that id. The coordinator
/// refuses a plain enrollment over any existing record, so without this the phone could
/// only come back through recovery, approved on the dashboard a second time after the
/// pairing that already needed it. Replacing a stood-down record grants nothing a first
/// enrollment would not: both need a LAN peer and a valid bearer, and there is no working
/// enrollment to take over. An active one stays behind the recovery approval, because
/// replacing it would drop remote access the household is using. So does a `failed` one:
/// a refused enrollment can be failed again at will, and every replacement retires the old
/// record against the household's bounded retirement budget, so replacing failed records
/// would let a loop of bad enrollments spend that budget without anyone approving it.
///
/// A stood-down record that still holds the phone's current machine key is a phone that
/// lapsed or was removed and was not re-paired. The coordinator refuses any
/// `replace` or `enroll` presenting that key, so it is [`PhoneEnrollment::StaleIdentity`].
fn phone_enrollment(existing: &serde_json::Value, machine_key: &str) -> PhoneEnrollment {
    let field = |name: &str| {
        existing
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    let same_identity = !machine_key.is_empty() && field("machineKey") == machine_key;
    match field("status") {
        "active" if field("machineKey") == machine_key => PhoneEnrollment::AlreadyEnrolled,
        "revoked" | "failed" if same_identity => PhoneEnrollment::StaleIdentity,
        "revoked" if valid_device(field("revision")) => {
            PhoneEnrollment::Replace(field("revision").to_owned())
        }
        _ => PhoneEnrollment::Enroll,
    }
}

#[async_trait::async_trait]
impl RemoteMachines for Runtime {
    /// The Pond's record, or the coordinator's the first time after a change. A pending
    /// revocation, or no active enrollment, is no machine.
    async fn machine_for(&self, device_id: &str) -> Result<Option<String>> {
        let device = network_device(device_id)?;
        if self.pending_revocations()?.contains_key(&device) {
            return Ok(None);
        }
        if let Some(machine) = self.known_machine(&device)? {
            return Ok(Some(machine));
        }
        // Held as enrollment holds it, so a lookup cannot record the machine an enrollment in
        // flight is about to replace.
        let lock = self.device_lock(&device);
        let _held = lock.lock().await;
        if let Some(machine) = self.known_machine(&device)? {
            return Ok(Some(machine));
        }
        let record = match self
            .authority(
                "inspect",
                serde_json::json!({"device": device, "role": "phone"}),
            )
            .await
        {
            Ok(record) => record,
            Err(error) if refused_as(&error, "enrollment_missing") => return Ok(None),
            Err(error) => return Err(error),
        };
        let field = |name: &str| record.get(name).and_then(serde_json::Value::as_str);
        let Some(machine) = field("machineKey")
            .filter(|key| field("status") == Some("active") && hex_key(key, "mkey:"))
        else {
            return Ok(None);
        };
        self.ensure_not_revoked_meanwhile(&device)?;
        self.remember_machine(&device, machine)?;
        Ok(Some(machine.to_owned()))
    }
}

fn refused_as(error: &anyhow::Error, reason: &str) -> bool {
    error
        .downcast_ref::<RefusedByCoordinator>()
        .is_some_and(|refused| refused.reason.as_deref() == Some(reason))
}

/// Companion enrollment uses the same bearer middleware and actual-peer LAN checks.
pub fn companion_management(runtime: Arc<Runtime>, state: Arc<pond_api::AppState>) -> Router {
    let limiter = Arc::new(pond_api::middleware::RateLimiter::new(
        20,
        std::time::Duration::from_secs(60),
    ));
    Router::new()
        .route(
            "/api/v1/remote-access/configuration",
            get(remote_configuration),
        )
        .route(
            "/api/v1/remote-access/enrollment",
            axum::routing::post(register_phone),
        )
        .merge(recovery::phone_routes(
            state.handshake.clone(),
            state.device_registry.clone(),
        ))
        .with_state(runtime)
        // route_layer, not layer: merged into the companion, a layer would also wrap its
        // fallback, charging every unknown path to this 20-a-minute budget.
        .route_layer(axum::extract::DefaultBodyLimit::max(4096))
        .route_layer(axum::Extension(state.handshake.clone()))
        .route_layer(axum::Extension(state.profile_repo.clone()))
        .route_layer(axum::Extension(Arc::new(
            pond_infra::sqlite_device_attribution::SqliteDeviceAttribution::new(
                state.db.system.clone(),
            ),
        )
            as Arc<dyn pond_core::user_data::ports::device_attribution::DeviceAttribution>))
        .route_layer(middleware::from_fn_with_state(
            state,
            pond_api::middleware::auth_middleware,
        ))
        .route_layer(middleware::from_fn(move |request: Request, next: Next| {
            let limiter = limiter.clone();
            async move {
                let peer = request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .ok_or(StatusCode::FORBIDDEN)?
                    .0
                    .ip()
                    .to_string();
                if !limiter.check_rate_limit(&peer).await {
                    return Err(StatusCode::TOO_MANY_REQUESTS);
                };
                Ok::<_, StatusCode>(next.run(request).await)
            }
        }))
}

#[cfg(test)]
mod tests {

    use super::helper_complaint;

    #[test]
    fn the_helper_complaint_never_carries_a_node_authorisation_url() {
        let noisy = "dial failed for https://controlpond.jarida.io/register/nodekey%3Aabc123 \
                     after 3 tries";
        let said = helper_complaint(noisy.as_bytes());
        assert!(!said.contains("/register/"), "{said}");
        assert!(!said.contains("nodekey"), "{said}");
        assert!(said.contains("<redacted enrolment URL>"), "{said}");
        assert!(said.contains("dial failed"), "{said}");
        assert!(said.contains("after 3 tries"), "{said}");
    }

    #[test]
    fn the_helper_complaint_is_one_bounded_line() {
        let long = format!("start {} end", "chatter ".repeat(400));
        let said = helper_complaint(long.as_bytes());
        assert!(said.len() <= 512, "unbounded: {} bytes", said.len());
        assert!(!said.contains('\n'), "a log line must be one line");
        // The tail is kept: the helper prints its reason last.
        assert!(said.ends_with("end"), "{said}");
        assert!(said.starts_with("..."), "{said}");
    }

    #[test]
    fn saying_nothing_is_reported_as_saying_nothing() {
        assert_eq!(helper_complaint(b""), "and said nothing");
        assert_eq!(helper_complaint(b"   \n  "), "and said nothing");
    }
    use super::*;

    /// No profile owns any device; paired with `MockProfileRepository` (no profiles at all),
    /// resolution falls through to the sole-member-household fallback — `Role::Owner` — the
    /// same as today's unrestricted "any paired device enrolls" behaviour these tests expect.
    struct NoAttribution;
    #[async_trait::async_trait]
    impl DeviceAttribution for NoAttribution {
        async fn set_device_profile(&self, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }
        async fn device_profile(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn devices_for_profile(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn push_tokens_for_profile(
            &self,
            _: &str,
        ) -> Result<Vec<pond_core::user_data::domain::push_token::PushToken>> {
            Ok(vec![])
        }
    }

    pub(crate) fn no_attribution() -> Arc<dyn DeviceAttribution> {
        Arc::new(NoAttribution)
    }

    pub(crate) fn no_profiles() -> Arc<dyn ProfileRepository> {
        Arc::new(pond_core::user_data::mocks::mock_profile::MockProfileRepository::new())
    }

    #[test]
    fn a_removed_phone_replaces_its_stood_down_enrollment() {
        let key = "mkey:aaaa";
        let revision = "rev-0123456789abcdef";
        let existing = serde_json::json!({
            "status": "revoked", "machineKey": "mkey:old", "revision": revision
        });
        assert_eq!(
            phone_enrollment(&existing, key),
            PhoneEnrollment::Replace(revision.into())
        );
    }

    /// The coordinator keeps a stood-down record's machine key as a tombstone and refuses any
    /// replace or enroll presenting it, `failed` as much as `revoked`.
    #[test]
    fn a_stood_down_record_holding_the_same_key_is_a_stale_identity() {
        let key = format!("mkey:{}", "a".repeat(64));
        let revision = "rev-0123456789abcdef";
        for status in ["revoked", "failed"] {
            let existing = serde_json::json!({
                "status": status, "machineKey": key, "revision": revision
            });
            assert_eq!(
                phone_enrollment(&existing, &key),
                PhoneEnrollment::StaleIdentity,
                "{status}"
            );
        }
        // A fresh key replaces a revoked record, and a failed one still waits for recovery.
        let other = format!("mkey:{}", "b".repeat(64));
        let revoked = serde_json::json!({
            "status": "revoked", "machineKey": key, "revision": revision
        });
        assert_eq!(
            phone_enrollment(&revoked, &other),
            PhoneEnrollment::Replace(revision.into())
        );
        let failed = serde_json::json!({
            "status": "failed", "machineKey": key, "revision": revision
        });
        assert_eq!(phone_enrollment(&failed, &other), PhoneEnrollment::Enroll);
        // An active one with the same key is the phone already enrolled.
        let active = serde_json::json!({"status": "active", "machineKey": key});
        assert_eq!(
            phone_enrollment(&active, &key),
            PhoneEnrollment::AlreadyEnrolled
        );
        // A record that names no key is never mistaken for the phone's.
        let keyless = serde_json::json!({"status": "revoked", "revision": revision});
        assert_eq!(
            phone_enrollment(&keyless, ""),
            PhoneEnrollment::Replace(revision.into())
        );
    }

    #[test]
    fn an_active_enrollment_is_never_replaced_without_approval() {
        let existing = serde_json::json!({
            "status": "active", "machineKey": "mkey:other", "revision": "rev-0123456789abcdef"
        });
        assert_eq!(
            phone_enrollment(&existing, "mkey:aaaa"),
            PhoneEnrollment::Enroll
        );
        let same = serde_json::json!({"status": "active", "machineKey": "mkey:aaaa"});
        assert_eq!(
            phone_enrollment(&same, "mkey:aaaa"),
            PhoneEnrollment::AlreadyEnrolled
        );
    }

    #[test]
    fn anything_else_enrolls_and_leaves_the_decision_to_the_coordinator() {
        for existing in [
            serde_json::json!({}),
            serde_json::json!({"status": "pending", "revision": "rev-0123456789abcdef"}),
            // Each replacement spends a retirement slot, and a refused enrollment can be
            // failed again at will, so a failed record waits for the recovery approval.
            serde_json::json!({"status": "failed", "machineKey": "mkey:old", "revision": "rev-0123456789abcdef"}),
            // A revision the helper would reject is not sent.
            serde_json::json!({"status": "revoked", "revision": "short"}),
            serde_json::json!({"status": "revoked", "revision": "has spaces in it, too many"}),
            serde_json::json!({"status": "revoked"}),
            serde_json::json!({"status": "active"}),
        ] {
            assert_eq!(
                phone_enrollment(&existing, "mkey:aaaa"),
                PhoneEnrollment::Enroll,
                "{existing}"
            );
        }
    }

    #[test]
    fn enabling_without_a_coordinator_uses_the_hosted_one() {
        let filled = with_default_coordinator(Config {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(filled.control_url, DEFAULT_CONTROL_URL);
        assert_eq!(filled.enrollment_url, DEFAULT_ENROLLMENT_URL);
    }

    #[test]
    fn a_configured_coordinator_is_never_replaced() {
        let chosen = Config {
            enabled: true,
            control_url: "https://control.example".into(),
            enrollment_url: "https://enroll.example".into(),
        };
        let filled = with_default_coordinator(chosen.clone());
        assert_eq!(filled.control_url, chosen.control_url);
        assert_eq!(filled.enrollment_url, chosen.enrollment_url);
    }

    #[test]
    fn a_half_configured_coordinator_is_not_quietly_completed() {
        let half = Config {
            enabled: true,
            control_url: "https://control.example".into(),
            enrollment_url: String::new(),
        };
        let filled = with_default_coordinator(half);
        assert_eq!(filled.control_url, "https://control.example");
        assert!(filled.enrollment_url.is_empty());
    }

    #[tokio::test]
    async fn a_household_that_never_enabled_remote_access_keeps_no_coordinator() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let stored = runtime.config().unwrap();
        assert!(!stored.enabled);
        assert!(stored.control_url.is_empty());
        assert!(stored.enrollment_url.is_empty());
    }
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    /// `management` as the dashboard reaches it, presenting the host credential on every
    /// request, for the tests that are about something else.
    fn management_with_credential(runtime: Arc<Runtime>) -> Router {
        let credential = pond_api::host_guard::HostCredential::generate();
        let presented = axum::http::HeaderValue::from_str(credential.as_str()).unwrap();
        management(runtime, credential).layer(axum::middleware::from_fn(
            move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                request
                    .headers_mut()
                    .insert(pond_api::host_guard::CREDENTIAL_HEADER, presented.clone());
                next.run(request)
            },
        ))
    }

    #[tokio::test]
    async fn a_device_that_stops_coming_home_loses_its_remote_access() {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        give_authority(&runtime);
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();

        runtime.seen_on_lan("phone000000000001").await;
        runtime.seen_on_lan("phone000000000002").await;
        // Nobody is absent yet, and a sweep must not revoke the household.
        runtime.sweep_absent_devices().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());

        // Age one device past the window by hand.
        let mut seen = runtime.presence().unwrap();
        let stale = Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS + 1);
        let key = seen.keys().next().unwrap().clone();
        seen.insert(key.clone(), stale.to_rfc3339());
        runtime.save_presence(&seen).unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        let queued = runtime.pending_revocations().unwrap();
        assert_eq!(
            queued.len(),
            1,
            "exactly the absent device, not the household"
        );
        // Presence and the queue are both keyed by the coordinator's (hashed) id.
        assert!(queued.contains_key(&key));

        assert!(!runtime.presence().unwrap().contains_key(&key));
        runtime.sweep_absent_devices().await.unwrap();
        assert_eq!(runtime.pending_revocations().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_device_that_cannot_be_queued_holds_back_no_other() {
        use pond_core::security::ports::remote_access::LAN_PRESENCE_WINDOW_DAYS;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        give_authority(&runtime);
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();
        // A full queue refuses a device it does not hold and still takes one it does. The
        // presence record is ordered, so the refused device comes first.
        let (refused, queued) = ("a".repeat(64), "b".repeat(64));
        let mut pending: revocation::Queue = (0..revocation::MAX_DEVICES - 1)
            .map(|n| {
                (
                    format!("{n:064}"),
                    revocation::Pending::new(false, Utc::now()),
                )
            })
            .collect();
        pending.insert(queued.clone(), revocation::Pending::new(false, Utc::now()));
        runtime.save_revocations(&pending).unwrap();
        let stale =
            (Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS + 1)).to_rfc3339();
        runtime
            .save_presence(&BTreeMap::from([
                (refused.clone(), stale.clone()),
                (queued.clone(), stale),
            ]))
            .unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        let seen = runtime.presence().unwrap();
        assert!(seen.contains_key(&refused), "kept for the next sweep");
        assert!(!seen.contains_key(&queued), "handled after the refusal");
    }

    #[tokio::test]
    async fn a_device_with_no_sighting_is_never_swept() {
        use pond_core::security::ports::remote_access::DevicePresence;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();
        assert!(runtime.absent_since(Utc::now()).await.unwrap().is_empty());
        runtime.sweep_absent_devices().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_local_only_household_is_never_swept() {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.seen_on_lan("phone000000000001").await;
        let mut seen = runtime.presence().unwrap();
        let key = seen.keys().next().unwrap().clone();
        let stale = Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS + 1);
        seen.insert(key, stale.to_rfc3339());
        runtime.save_presence(&seen).unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        assert!(!runtime.directory.join("revocations.json").exists());
    }

    #[tokio::test]
    async fn the_lapse_deadline_is_a_window_after_the_last_sighting() {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        assert_eq!(runtime.lapses_at("phone000000000001").await.unwrap(), None);

        let before = Utc::now();
        runtime.seen_on_lan("phone000000000001").await;
        let lapses = runtime
            .lapses_at("phone000000000001")
            .await
            .unwrap()
            .unwrap();
        let expected = before + chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS);
        assert!(
            (lapses - expected).num_seconds().abs() < 60,
            "lapses at {lapses}, expected about {expected}"
        );
    }

    #[tokio::test]
    async fn revocation_is_durable_idempotent_and_local_only_does_not_enroll() {
        use pond_core::security::ports::remote_access::{DevicePresence, RemoteRevocation};
        let data = tempfile::tempdir().unwrap();
        let (runtime, listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.queue("a").await.unwrap();
        assert!(!runtime.directory.join("revocations.json").exists());
        runtime
            .persist(&Config {
                enabled: false,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();
        give_authority(&runtime);
        runtime.queue("phone000000000001").await.unwrap();
        runtime.queue("phone000000000001").await.unwrap();
        assert_eq!(runtime.pending_revocations().unwrap().len(), 1);
        drop(listener);
        drop(runtime);
        let (restored, _) = Runtime::new(data.path(), 4443).unwrap();
        assert!(restored
            .pending_revocations()
            .unwrap()
            .contains_key(&network_device("phone000000000001").unwrap()));
        let phone = network_device("phone000000000002").unwrap();
        restored.seen_on_lan("phone000000000002").await;
        let queue = restored.directory.join("revocations.json");
        std::fs::write(&queue, b"corrupt").unwrap();
        assert!(restored.queue("phone000000000002").await.is_err());
        // The sighting is what makes the sweep try again, so a failed save must keep it.
        assert!(
            restored.presence().unwrap().contains_key(&phone),
            "a failed revocation forgot the sighting, so the phone would never lapse"
        );

        std::fs::remove_file(&queue).unwrap();
        restored.queue("phone000000000002").await.unwrap();
        assert!(!restored.presence().unwrap().contains_key(&phone));
        // Coming home straight after is recorded, not throttled for an hour.
        restored.seen_on_lan("phone000000000002").await;
        assert!(
            restored.presence().unwrap().contains_key(&phone),
            "the phone's return home was not recorded"
        );
    }

    /// Enabling remote access on a fresh Pond failed with `registration_unavailable`: nothing on
    /// the Enable path created the household key, and the helper refuses to register without
    /// one ("existing household authority is required").
    #[tokio::test]
    async fn registering_a_fresh_pond_creates_its_household_identity_first() {
        use std::os::unix::fs::PermissionsExt;
        let data = tempfile::tempdir().unwrap();
        // Stands in for the helper: records each authority action, answers with an empty object.
        let calls = data.path().join("calls");
        let helper = data.path().join("pondnet");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\necho \"$2\" >> '{}'\ncat > /dev/null\necho '{{}}'\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        // No other test in this module spawns the helper, so setting it process-wide is safe.
        std::env::set_var("POND_NETWORK_BINARY", &helper);

        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.status.write().unwrap().auth_url =
            Some("https://control.example/register/pending".into());
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/remote-access/register")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        management_with_credential(runtime)
            .oneshot(request)
            .await
            .unwrap();

        let recorded = std::fs::read_to_string(&calls).unwrap();
        let actions: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            actions.get(..2),
            Some(&["identity", "register"][..]),
            "registration must ensure the household identity first: {actions:?}"
        );
    }

    #[tokio::test]
    async fn local_management_ignores_forged_forwarding_identity() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let credential = pond_api::host_guard::HostCredential::generate();
        let router = management(runtime, credential.clone());
        for ip in ["100.64.0.2:1234", "192.168.1.2:1234"] {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access")
                .header("x-forwarded-for", "127.0.0.1")
                .header(PEER_HEADER, "127.0.0.1:1234")
                .header(pond_api::host_guard::CREDENTIAL_HEADER, credential.as_str())
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
            assert_eq!(
                router.clone().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let request = Request::builder()
            .uri("/api/v1/remote-access")
            .header(pond_api::host_guard::CREDENTIAL_HEADER, credential.as_str())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn local_management_requires_the_host_credential() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let credential = pond_api::host_guard::HostCredential::generate();
        let router = management(runtime, credential.clone());
        let request = |value: Option<&str>| {
            let mut request = Request::builder().uri("/api/v1/remote-access");
            if let Some(value) = value {
                request = request.header(pond_api::host_guard::CREDENTIAL_HEADER, value);
            }
            let mut request = request.body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            request
        };
        let stale = pond_api::host_guard::HostCredential::generate();
        for value in [None, Some(stale.as_str())] {
            assert_eq!(
                router
                    .clone()
                    .oneshot(request(value))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            router
                .oneshot(request(Some(credential.as_str())))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn embedded_pairing_remains_remote_even_with_local_forwarding_headers() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let router = private_companion(
            Router::new().route(
                "/pair",
                get(|peer: ConnectInfo<SocketAddr>| async move {
                    pond_api::network::require_lan(Some(peer)).map(|_| StatusCode::OK)
                }),
            ),
            runtime,
        );
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/pair")
                    .header(PEER_HEADER, "100.64.0.2:1234")
                    .header(MACHINE_HEADER, format!("mkey:{}", "a".repeat(64)))
                    .header("x-forwarded-for", "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn address_is_ready_only_after_certificate_coverage() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.publish(Status {
            state: "Running".into(),
            addresses: vec!["100.64.0.2".into()],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        });
        // The LAN certificate's names, or a tailnet certificate written before the address.
        runtime.publish_ready(&["pond.local".into()]);
        assert!(runtime.address.0.read().unwrap().is_none());
        runtime.publish_ready(&crate::tls_identity::tailnet_certificate_names(&[]));
        assert!(runtime.address.0.read().unwrap().is_none());
        runtime.publish_ready(&crate::tls_identity::tailnet_certificate_names(
            &runtime.addresses(),
        ));
        assert_eq!(
            runtime.address.0.read().unwrap().as_deref(),
            Some("100.64.0.2")
        );
    }

    /// A stand-in helper: records each call, and answers an action from `<action>.out` and
    /// `<action>.code` beside it (exit 0 with `{}` when neither exists). While `<action>.hold`
    /// exists, a call to that action is recorded and then held open. Shared with the recovery
    /// tests.
    pub(super) struct FakeHelper {
        dir: tempfile::TempDir,
    }

    impl FakeHelper {
        pub(super) fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("pondnet");
            std::fs::write(
                &script,
                "#!/bin/sh\n\
                 dir=$(dirname \"$0\")\n\
                 payload=$(cat)\n\
                 echo \"$2 $payload\" >> \"$dir/calls\"\n\
                 env > \"$dir/env\"\n\
                 while [ -f \"$dir/$2.hold\" ]; do sleep 0.05; done\n\
                 if [ -f \"$dir/$2.out\" ]; then cat \"$dir/$2.out\"; else echo '{}'; fi\n\
                 exit $(cat \"$dir/$2.code\" 2>/dev/null || echo 0)\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self { dir }
        }

        pub(super) fn answer(&self, action: &str, code: i32, stdout: &str) {
            std::fs::write(
                self.dir.path().join(format!("{action}.code")),
                code.to_string(),
            )
            .unwrap();
            std::fs::write(self.dir.path().join(format!("{action}.out")), stdout).unwrap();
        }

        /// Hold every call to `action` open until [`FakeHelper::release`].
        fn hold(&self, action: &str) {
            std::fs::write(self.dir.path().join(format!("{action}.hold")), "").unwrap();
        }

        fn release(&self, action: &str) {
            std::fs::remove_file(self.dir.path().join(format!("{action}.hold"))).unwrap();
        }

        pub(super) fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .map(|line| line.split(' ').next().unwrap().to_owned())
                .collect()
        }

        pub(super) fn runtime(&self, data: &Path) -> Arc<Runtime> {
            let (runtime, _listener) =
                Runtime::with_helper(data, 4443, self.dir.path().join("pondnet")).unwrap();
            runtime
                .persist(&Config {
                    enabled: true,
                    control_url: "https://coord.example".into(),
                    enrollment_url: "https://enroll.example".into(),
                })
                .unwrap();
            give_authority(&runtime);
            runtime
        }
    }

    /// The household signing key, as far as anything but the helper looks at it: a private file.
    fn give_authority(runtime: &Runtime) {
        let directory = runtime.directory.join("authority");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let identity = directory.join("identity.json");
        std::fs::write(&identity, b"{}").unwrap();
        std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn enrolled(runtime: &Runtime, complete: bool, devices: &[&str]) {
        revocation::write_private_json(
            &runtime.directory,
            revocation::ENROLLED_FILE,
            &revocation::Enrolled {
                complete,
                devices: devices.iter().map(|d| network_device(d).unwrap()).collect(),
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_enrolled_phone_is_revoked_and_a_device_that_never_enrolled_is_left_alone() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001"]);

        runtime.queue("phone000000000001").await.unwrap();
        // A desktop session or a Matter device: nothing enrolled, so nothing to revoke.
        runtime.queue("desktop0000000001").await.unwrap();
        let queued = runtime.pending_revocations().unwrap();
        assert_eq!(queued.len(), 1);
        assert!(!queued.values().next().unwrap().verify);

        runtime.reconcile_due().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        assert_eq!(helper.calls(), ["revoke"]);
        assert!(revocation::load_enrolled(&runtime.directory)
            .devices
            .is_empty());
    }

    #[tokio::test]
    async fn an_older_pond_checks_an_unrecorded_device_and_writes_no_tombstone() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        // No record at all: this Pond enrolled phones before it kept one.
        runtime.queue("matter-000000000001").await.unwrap();
        assert!(runtime
            .pending_revocations()
            .unwrap()
            .values()
            .all(|e| e.verify));

        helper.answer("inspect", 3, r#"{"refused":"enrollment_missing"}"#);
        runtime.reconcile_due().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        assert_eq!(
            helper.calls(),
            ["inspect"],
            "revoked a device that never enrolled"
        );
    }

    #[tokio::test]
    async fn a_refusal_backs_off_one_device_and_an_outage_waits_for_all() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001", "phone000000000002"]);
        runtime.queue("phone000000000001").await.unwrap();
        runtime.queue("phone000000000002").await.unwrap();

        helper.answer("revoke", 3, r#"{"refused":"revocation_conflict"}"#);
        runtime.reconcile_due().await.unwrap();
        assert_eq!(
            helper.calls().len(),
            2,
            "a refusal stopped the devices behind it"
        );
        let queued = runtime.pending_revocations().unwrap();
        assert!(queued
            .values()
            .all(|e| e.attempts == 1 && e.next_attempt > Utc::now()));
        // Backed off, so the next tick sends nothing.
        runtime.reconcile_due().await.unwrap();
        assert_eq!(helper.calls().len(), 2);

        let mut due_now = runtime.pending_revocations().unwrap();
        for entry in due_now.values_mut() {
            entry.next_attempt = Utc::now();
        }
        runtime.save_revocations(&due_now).unwrap();
        helper.answer("revoke", 1, "");
        runtime.reconcile_due().await.unwrap();
        assert_eq!(
            helper.calls().len(),
            3,
            "an unreachable coordinator was tried again"
        );
        let attempts: Vec<u32> = runtime
            .pending_revocations()
            .unwrap()
            .values()
            .map(|e| e.attempts)
            .collect();
        assert_eq!(attempts.iter().filter(|a| **a == 2).count(), 1);
        assert_eq!(attempts.iter().filter(|a| **a == 1).count(), 1);
    }

    #[tokio::test]
    async fn only_an_invalid_enrollment_counts_as_the_coordinator_keeping_one() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        for (code, stdout, kept) in [
            (3, r#"{"refused":"invalid_enrollment"}"#, true),
            (3, r#"{"refused":"rate_limited"}"#, false),
            (3, r#"{"refused":"with no explanation"}"#, false),
            (3, "", false),
            (1, "", false),
        ] {
            helper.answer("enroll", code, stdout);
            let error = runtime
                .authority("enroll", serde_json::json!({}))
                .await
                .unwrap_err();
            assert_eq!(kept_existing_enrollment(&error), kept, "{code} {stdout}");
        }
    }

    #[tokio::test]
    async fn a_new_household_records_every_enrollment_so_nothing_is_checked() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime.start_enrollment_record().unwrap();
        runtime.queue("desktop0000000001").await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        // A second start keeps what is recorded.
        runtime
            .record_enrolled(&network_device("phone000000000001").unwrap())
            .unwrap();
        runtime.start_enrollment_record().unwrap();
        assert_eq!(
            revocation::load_enrolled(&runtime.directory).devices.len(),
            1
        );
    }

    /// A paired phone's bearer, from a real handshake, and the handshake that checks it.
    async fn paired_phone(
        data: &Path,
    ) -> (
        Arc<dyn pond_core::security::ports::handshake::Handshake>,
        axum::http::HeaderMap,
    ) {
        use pond_core::security::ports::handshake::{Handshake, HandshakeRequest};
        use pond_infra::{db::Database, sqlite_handshake::SqliteHandshakeAdapter};
        let db = Database::init(data).await.unwrap();
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
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", paired.session_token.unwrap())
                .parse()
                .unwrap(),
        );
        let handshake: Arc<dyn Handshake> = handshake;
        (handshake, headers)
    }

    /// `POST /remote-access/enrollment` from the phone on the home network.
    async fn enroll_phone(
        runtime: &Arc<Runtime>,
        handshake: &Arc<dyn pond_core::security::ports::handshake::Handshake>,
        headers: &axum::http::HeaderMap,
    ) -> Result<Json<serde_json::Value>, StatusCode> {
        register_phone(
            State(runtime.clone()),
            ConnectInfo("127.0.0.1:5555".parse().unwrap()),
            axum::Extension(handshake.clone()),
            axum::Extension(no_profiles()),
            axum::Extension(no_attribution()),
            headers.clone(),
            Json(Registration {
                auth_url: "https://coord.example/register/abcdefghijklmnop".into(),
                node_key: format!("nodekey:{}", "b".repeat(64)),
                machine_key: format!("mkey:{}", "a".repeat(64)),
            }),
        )
        .await
        .map_err(|refused| refused.status())
    }

    /// A removal while the helper is still enrolling the phone must find the record and queue the
    /// revocation, or the enrollment the coordinator then completes has nothing to revoke it.
    #[tokio::test]
    async fn a_removal_while_an_enrollment_is_in_flight_revokes_it() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let (handshake, headers) = paired_phone(data.path()).await;
        let runtime = helper.runtime(data.path());
        // A household that has recorded every enrollment, and nothing yet.
        enrolled(&runtime, true, &[]);
        helper.answer("inspect", 3, r#"{"refused":"enrollment_missing"}"#);
        helper.hold("enroll");

        let enrolling = tokio::spawn({
            let (runtime, handshake, headers) = (runtime.clone(), handshake.clone(), headers);
            async move { enroll_phone(&runtime, &handshake, &headers).await }
        });
        let started = std::time::Instant::now();
        while !helper.calls().iter().any(|call| call == "enroll") {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "the enrollment never reached the helper: {:?}",
                helper.calls()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        runtime.queue("phone000000000001").await.unwrap();
        helper.release("enroll");

        assert_eq!(
            enrolling.await.unwrap().unwrap_err(),
            StatusCode::CONFLICT,
            "the phone was told it is enrolled while its removal is queued"
        );
        let queued = runtime.pending_revocations().unwrap();
        let entry = queued
            .get(&network_device("phone000000000001").unwrap())
            .expect("the removal queued nothing, so the enrollment would never be revoked");
        assert!(
            !entry.verify,
            "a recorded enrollment is revoked without a check"
        );
    }

    #[tokio::test]
    async fn only_a_refused_enrollment_is_forgotten() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let (handshake, headers) = paired_phone(data.path()).await;
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &[]);
        let device = network_device("phone000000000001").unwrap();
        let recorded = |runtime: &Runtime| {
            revocation::load_enrolled(&runtime.directory)
                .devices
                .contains(&device)
        };
        helper.answer("inspect", 3, r#"{"refused":"enrollment_missing"}"#);

        // Unreachable, or timed out: the coordinator may have enrolled it, so the record stays.
        helper.answer("enroll", 1, "");
        assert_eq!(
            enroll_phone(&runtime, &handshake, &headers)
                .await
                .unwrap_err(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(recorded(&runtime), "an ambiguous enrollment was forgotten");

        // A refusal of a device recorded before this call does not forget it. Only
        // `invalid_enrollment` sends the phone to recovery; any other refusal is a 503.
        helper.answer("enroll", 3, r#"{"refused":"identity_already_enrolled"}"#);
        assert_eq!(
            enroll_phone(&runtime, &handshake, &headers)
                .await
                .unwrap_err(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(recorded(&runtime), "an earlier record was forgotten");

        // A refusal of a device recorded for this call alone: nothing was enrolled.
        enrolled(&runtime, true, &[]);
        assert_eq!(
            enroll_phone(&runtime, &handshake, &headers)
                .await
                .unwrap_err(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(!recorded(&runtime), "a refused enrollment stayed recorded");

        // A refusal that can follow the coordinator's own write may have enrolled it.
        enrolled(&runtime, true, &[]);
        helper.answer("enroll", 3, r#"{"refused":"enrollment_incomplete"}"#);
        assert_eq!(
            enroll_phone(&runtime, &handshake, &headers)
                .await
                .unwrap_err(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            recorded(&runtime),
            "an enrollment the coordinator may have written was forgotten"
        );

        // A refusal is not forgotten when the coordinator reported an enrollment of its own.
        enrolled(&runtime, true, &[]);
        helper.answer("enroll", 3, r#"{"refused":"invalid_enrollment"}"#);
        helper.answer(
            "inspect",
            0,
            r#"{"status":"active","machineKey":"mkey:other","revision":"rev-0123456789abcdef"}"#,
        );
        assert_eq!(
            enroll_phone(&runtime, &handshake, &headers)
                .await
                .unwrap_err(),
            StatusCode::CONFLICT
        );
        assert!(
            recorded(&runtime),
            "an enrollment the coordinator holds was forgotten"
        );
    }

    /// A corrupt record, or one a restore left readable by others, is set aside rather than
    /// answering every removal, logout and enrollment 503 until someone repairs it by hand.
    #[tokio::test]
    async fn an_unreadable_enrollment_record_is_set_aside_and_removals_check_first() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        for (contents, mode) in [
            (&b"{not json"[..], 0o600),
            (&br#"{"complete":true,"devices":[]}"#[..], 0o644),
        ] {
            let helper = FakeHelper::new();
            let data = tempfile::tempdir().unwrap();
            let runtime = helper.runtime(data.path());
            let path = runtime.directory.join(revocation::ENROLLED_FILE);
            std::fs::write(&path, contents).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();

            runtime.queue("phone000000000001").await.unwrap();
            let queued = runtime.pending_revocations().unwrap();
            assert!(
                queued[&network_device("phone000000000001").unwrap()].verify,
                "mode {mode:o}: an unknown device must be checked before it is revoked"
            );
            let aside = std::fs::read_dir(&runtime.directory)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("enrolled.json.corrupt-")
                });
            assert!(
                aside,
                "mode {mode:o}: the record was not kept for inspection"
            );
            assert!(
                !path.exists(),
                "mode {mode:o}: the record was left in place"
            );
        }
    }

    /// Without the household key the helper exits 1 for every device, so a queued entry would
    /// back off to six hours forever and hold up the entries behind it.
    #[tokio::test]
    async fn without_a_household_authority_a_listed_phone_waits_for_it() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001"]);
        // Queued while the key was there, before it went missing.
        runtime.queue("phone000000000001").await.unwrap();
        std::fs::remove_file(runtime.directory.join("authority").join("identity.json")).unwrap();

        // Not on the record, and nothing to check it with: nothing is queued.
        runtime.queue("phone000000000002").await.unwrap();
        assert_eq!(runtime.pending_revocations().unwrap().len(), 1);
        // On the record: it stays queued until the key is back, rather than being dropped.
        runtime.reconcile_due().await.unwrap();
        let pending = runtime.pending_revocations().unwrap();
        let listed = network_device("phone000000000001").unwrap();
        assert_eq!(
            pending.get(&listed).map(|entry| entry.attempts),
            Some(1),
            "a listed phone's revocation was dropped"
        );
        assert!(
            revocation::load_enrolled(&runtime.directory)
                .devices
                .contains(&listed),
            "a listed phone was forgotten"
        );
        assert!(
            helper.calls().is_empty(),
            "the helper was asked without an authority"
        );
    }

    #[tokio::test]
    async fn a_corrupt_presence_record_still_lets_enrolled_phones_lapse() {
        use pond_core::security::ports::remote_access::LAN_PRESENCE_WINDOW_DAYS;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001"]);
        let path = runtime.directory.join(PRESENCE_FILE);
        std::fs::write(&path, b"{not json").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let written = std::time::SystemTime::now()
            - std::time::Duration::from_secs(60 * 60 * 24 * (LAN_PRESENCE_WINDOW_DAYS as u64 + 1));
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(written)
            .unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        let queued = runtime.pending_revocations().unwrap();
        assert!(queued.contains_key(&network_device("phone000000000001").unwrap()));
        let aside = std::fs::read_dir(&runtime.directory)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("presence.json.corrupt-")
            });
        assert!(aside, "the unreadable record was not kept for inspection");
    }

    #[tokio::test]
    async fn sightings_are_written_at_most_hourly_and_read_from_older_keys() {
        use pond_core::security::ports::remote_access::DevicePresence;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        // A record from before sightings were hashed.
        let old = (Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        runtime
            .save_presence(&BTreeMap::from([(
                "phone000000000001".to_owned(),
                old.clone(),
            )]))
            .unwrap();
        assert!(runtime
            .lapses_at("phone000000000001")
            .await
            .unwrap()
            .is_some());

        runtime.seen_on_lan("phone000000000001").await;
        let first = runtime.presence().unwrap();
        let device = network_device("phone000000000001").unwrap();
        assert_ne!(first[&device], old);
        // Put the old sighting back; within the hour a second request must not rewrite it.
        runtime
            .save_presence(&BTreeMap::from([(device.clone(), old.clone())]))
            .unwrap();
        runtime.seen_on_lan("phone000000000001").await;
        assert_eq!(runtime.presence().unwrap()[&device], old);
    }

    #[tokio::test]
    async fn the_helper_inherits_no_environment_and_must_not_be_replaceable() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime
            .authority("inspect", serde_json::json!({}))
            .await
            .unwrap();
        let env = std::fs::read_to_string(helper.dir.path().join("env")).unwrap();
        // Cargo sets these for every test process; none may reach the helper.
        for inherited in ["CARGO_PKG_NAME=", "RUST_TEST_THREADS=", "\nPATH="] {
            assert!(
                !format!("\n{env}").contains(inherited),
                "{inherited} reached the helper"
            );
        }

        let script = helper.dir.path().join("pondnet");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o720)).unwrap();
        let refused = runtime.authority("inspect", serde_json::json!({})).await;
        assert!(
            refused.is_err_and(|e| e.to_string().contains("writable by no one else")),
            "a group-writable helper was run"
        );
    }

    #[test]
    fn a_registration_names_well_formed_keys() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let registration = |node: &str, machine: &str| Registration {
            auth_url: "https://coord.example/register/abcdefghijklmnop".into(),
            node_key: node.into(),
            machine_key: machine.into(),
        };
        let machine = format!("mkey:{}", "a".repeat(64));
        let node = format!("nodekey:{}", "b".repeat(64));
        for (node_key, machine_key, ok) in [
            (node.as_str(), machine.as_str(), true),
            ("", machine.as_str(), true),
            (node.as_str(), "", false),
            (node.as_str(), "mkey:ABC", false),
            (node.as_str(), &format!("mkey:{}", "A".repeat(64)), false),
            ("nodekey:1", machine.as_str(), false),
        ] {
            let payload = runtime.registration_payload(
                "device0000000001",
                "phone",
                &registration(node_key, machine_key),
            );
            assert_eq!(payload.is_ok(), ok, "{node_key:?} {machine_key:?}");
        }
    }

    #[tokio::test]
    async fn the_dashboard_learns_whether_the_pond_was_provisioned() {
        use std::os::unix::fs::PermissionsExt;
        use tower::ServiceExt;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let ask = |runtime: Arc<Runtime>| async move {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access/device")
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = management_with_credential(runtime)
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };
        assert_eq!(
            ask(runtime.clone()).await,
            serde_json::json!({ "provisioned": false, "registered": false })
        );

        let device = runtime.directory.join("device");
        std::fs::create_dir(&device).unwrap();
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o700)).unwrap();
        use base64::Engine;
        let serial = "0f".repeat(16);
        let payload = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!({
                "v": 1, "serial": serial, "devicePublicKey": "k", "issued": 1
            }))
            .unwrap(),
        );
        let certificate = device.join("certificate.json");
        std::fs::write(
            &certificate,
            serde_json::to_vec(&serde_json::json!({ "payload": payload, "signature": "s" }))
                .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&certificate, std::fs::Permissions::from_mode(0o600)).unwrap();
        let not_provisioned = serde_json::json!({ "provisioned": false, "registered": false });
        // The helper cannot present a certificate without its key.
        assert_eq!(ask(runtime.clone()).await, not_provisioned);
        let key = device.join("key.json");
        std::fs::write(&key, r#"{"seed":"not base64"}"#).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(ask(runtime.clone()).await, not_provisioned);
        std::fs::write(
            &key,
            serde_json::to_vec(&serde_json::json!({
                "seed": base64::engine::general_purpose::STANDARD.encode([7u8; 32])
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            ask(runtime.clone()).await,
            serde_json::json!({ "provisioned": true, "serial": serial, "registered": false })
        );

        // A key, certificate or directory others can read is not shown as a provisioning.
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(ask(runtime.clone()).await, not_provisioned);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&certificate, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(ask(runtime.clone()).await, not_provisioned);
        std::fs::set_permissions(&certificate, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(ask(runtime).await, not_provisioned);
    }

    #[tokio::test]
    async fn a_refused_device_certificate_is_explained_to_the_dashboard() {
        use tower::ServiceExt;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime.publish(Status {
            state: "NeedsLogin".into(),
            addresses: vec![],
            auth_url: Some("https://coord.example/register/abcdefghijklmnop".into()),
            node_key: format!("nodekey:{}", "b".repeat(64)),
            machine_key: format!("mkey:{}", "a".repeat(64)),
        });
        for reason in [
            "device_certificate_invalid",
            "device_revoked",
            "device_used",
        ] {
            helper.answer("register", 3, &format!(r#"{{"refused":"{reason}"}}"#));
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/remote-access/register")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = management_with_credential(runtime.clone())
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{reason}");
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"], reason);
        }
    }

    #[tokio::test]
    async fn registration_carries_the_invite_and_explains_its_refusal() {
        use tower::ServiceExt;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime.publish(Status {
            state: "NeedsLogin".into(),
            addresses: vec![],
            auth_url: Some("https://coord.example/register/abcdefghijklmnop".into()),
            node_key: format!("nodekey:{}", "b".repeat(64)),
            machine_key: format!("mkey:{}", "a".repeat(64)),
        });
        let router = management_with_credential(runtime);
        let register = |body: &str| {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/remote-access/register")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            router.clone().oneshot(request)
        };
        let body = |response: axum::response::Response| async {
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };

        let response = register(r#"{"invite":"not an invite"}"#).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            helper.calls().is_empty(),
            "a malformed invite reached the helper"
        );

        helper.answer("register", 3, r#"{"refused":"invite_expired"}"#);
        let response = register(r#"{"invite":"giap-inv1-ABCD-EFGH"}"#)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body(response).await["error"], "invite_expired");
        let sent = std::fs::read_to_string(helper.dir.path().join("calls")).unwrap();
        assert!(
            sent.contains(r#"register {"invite":"giap-inv1-ABCD-EFGH"}"#),
            "{sent}"
        );

        // A household already registered sends no invite at all.
        assert!(
            !data
                .path()
                .join("embedded-network")
                .join(REGISTERED_FILE)
                .exists(),
            "a refusal was recorded as a registration"
        );
        helper.answer("register", 0, r#"{"household":"h"}"#);
        let response = register("{}").await.unwrap();
        assert_ne!(response.status(), StatusCode::FORBIDDEN);
        assert!(std::fs::read_to_string(helper.dir.path().join("calls"))
            .unwrap()
            .contains(r#"register {"invite":""}"#));
        let recorded: serde_json::Value = serde_json::from_slice(
            &std::fs::read(data.path().join("embedded-network").join(REGISTERED_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            recorded,
            serde_json::json!({ "enrollment": "https://enroll.example" })
        );
    }

    /// A phone whose remote access lapsed and that was not re-paired asks with the key its
    /// revoked record still holds. This drives the route, not only the decision.
    #[tokio::test]
    async fn a_lapsed_phone_with_its_old_keys_is_told_to_make_new_ones() {
        use pond_core::security::ports::handshake::{Handshake, HandshakeRequest};
        use pond_infra::{db::Database, sqlite_handshake::SqliteHandshakeAdapter};
        let helper = FakeHelper::new();
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
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", paired.session_token.unwrap())
                .parse()
                .unwrap(),
        );
        let runtime = helper.runtime(data.path());
        let machine_key = format!("mkey:{}", "a".repeat(64));
        helper.answer(
            "inspect",
            0,
            &serde_json::json!({
                "status": "revoked", "machineKey": machine_key, "revision": "rev-0123456789abcdef"
            })
            .to_string(),
        );

        let refused = register_phone(
            State(runtime),
            ConnectInfo("127.0.0.1:5555".parse().unwrap()),
            axum::Extension(handshake as Arc<dyn Handshake>),
            axum::Extension(no_profiles()),
            axum::Extension(no_attribution()),
            headers,
            Json(Registration {
                auth_url: "https://coord.example/register/abcdefghijklmnop".into(),
                node_key: format!("nodekey:{}", "b".repeat(64)),
                machine_key,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(refused.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, serde_json::json!({"error": "stale_identity"}));
        assert_eq!(
            helper.calls(),
            ["inspect"],
            "a replace or enroll the coordinator refuses was sent anyway"
        );
    }

    /// The dashboard asks for an invite only while the household has never registered with the
    /// coordination service configured now. A node that reached Running proves it registered,
    /// which is how a household that registered before the record existed gets one.
    #[tokio::test]
    async fn a_registered_household_is_not_asked_for_an_invite_again() {
        use std::os::unix::fs::PermissionsExt;
        use tower::ServiceExt;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let ask = |runtime: Arc<Runtime>| async move {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access/device")
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = management_with_credential(runtime)
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["registered"].clone()
        };
        let state = |state: &str| Status {
            state: state.into(),
            addresses: vec![],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        };
        assert_eq!(ask(runtime.clone()).await, false);

        runtime.publish(state("Starting"));
        assert_eq!(ask(runtime.clone()).await, false, "starting proves nothing");
        runtime.publish(state("Running"));
        assert_eq!(ask(runtime.clone()).await, true);
        let record = runtime.directory.join(REGISTERED_FILE);
        let mode = std::fs::metadata(&record).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "the record is readable by others: {mode:o}"
        );

        // Another coordination service has never seen this household.
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://elsewhere.example".into(),
                enrollment_url: "https://enroll.elsewhere.example".into(),
            })
            .unwrap();
        assert_eq!(ask(runtime.clone()).await, false);
        runtime.publish(state("Stopped"));
        runtime.publish(state("Running"));
        assert_eq!(ask(runtime.clone()).await, true);

        // An unreadable record is not taken as a registration.
        std::fs::write(&record, b"not json").unwrap();
        assert_eq!(ask(runtime).await, false);
    }

    fn active_on(machine: &str) -> String {
        serde_json::json!({"status": "active", "machineKey": machine, "role": "phone"}).to_string()
    }

    #[tokio::test]
    async fn a_phones_machine_is_learned_from_the_coordinator_once_then_recorded() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let machine = format!("mkey:{}", "a".repeat(64));
        helper.answer("inspect", 0, &active_on(&machine));
        assert_eq!(
            runtime.machine_for("phone").await.unwrap(),
            Some(machine.clone())
        );
        assert_eq!(runtime.machine_for("phone").await.unwrap(), Some(machine));
        assert_eq!(helper.calls(), ["inspect"]);
    }

    #[tokio::test]
    async fn enrolling_again_forgets_the_recorded_machine() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime
            .remember_phone_machine("phone", &format!("mkey:{}", "a".repeat(64)))
            .unwrap();
        runtime
            .record_enrolled(&network_device("phone").unwrap())
            .unwrap();
        let replaced = format!("mkey:{}", "b".repeat(64));
        helper.answer("inspect", 0, &active_on(&replaced));
        assert_eq!(runtime.machine_for("phone").await.unwrap(), Some(replaced));
    }

    #[tokio::test]
    async fn a_phone_without_an_active_enrollment_has_no_machine() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        helper.answer("inspect", 3, r#"{"refused":"enrollment_missing"}"#);
        assert_eq!(runtime.machine_for("phone").await.unwrap(), None);
        let revoked = serde_json::json!({"status": "revoked", "machineKey": format!("mkey:{}", "a".repeat(64))});
        helper.answer("inspect", 0, &revoked.to_string());
        assert_eq!(runtime.machine_for("phone").await.unwrap(), None);
        // Neither answer is recorded: the next request asks again.
        assert_eq!(helper.calls(), ["inspect", "inspect"]);
    }

    #[tokio::test]
    async fn a_phone_awaiting_revocation_has_no_machine_whatever_was_recorded() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime
            .remember_phone_machine("phone", &format!("mkey:{}", "a".repeat(64)))
            .unwrap();
        runtime.queue("phone").await.unwrap();
        assert_eq!(runtime.machine_for("phone").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unreachable_coordinator_is_an_error_not_a_machine() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        helper.answer("inspect", 1, "");
        assert!(runtime.machine_for("phone").await.is_err());
    }
}
