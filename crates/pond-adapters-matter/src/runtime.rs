//! [`MatterRuntime`]: one reconciler task owns all Matter state and converges on the latest
//! [`MatterRuntimePort::apply`] via a `watch` channel, so toggles coalesce and never interleave.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::NotificationSender;
use pond_core::shared::ports::event_bus::EventBus;
use pond_core::user_data::ports::device_commissioning::DeviceCommissioningPort;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceDescription, DeviceState,
};
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::matter_runtime::{
    MatterConfig, MatterRuntimePort, MatterState, MatterStatus,
};
use tokio::sync::{watch, RwLock};
use tokio::task::JoinHandle;

use crate::bridge::{run_matter_supervisor, SupervisorConfig};
use crate::client::{MatterClient, MatterEvent};
use crate::commissioning::MatterCommissioner;
use crate::control::MatterDeviceControl;
use crate::notify::MatterNotifier;
use crate::protocol::{describe, is_matter_device_id};
use crate::server_setup::{ensure_running, local_port_from_ws_url, SharedServerChild};

/// How long a freshly installed controller gets to start listening.
const CONTROLLER_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Generous for local teardown; bounded only so a wedged reconciler can't hang process exit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Wait for a SIGKILLed controller to exit; inside [`SHUTDOWN_TIMEOUT`], so it must be shorter.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);

/// Live [`MatterDeviceControl`], `None` while Matter is off or unreachable.
type ControlCell = Arc<RwLock<Option<Arc<MatterDeviceControl>>>>;

/// Requested state. `nonce` makes each [`MatterRuntimePort::apply`] wake the reconciler.
#[derive(Clone, PartialEq, Eq)]
struct Desired {
    url: String,
    /// Request BLE; a spawn argument, so changing it restarts the controller.
    ble: bool,
    shutdown: bool,
    nonce: u64,
}

/// Reconciles the Matter integration toward the requested state.
pub struct MatterRuntime {
    notifier: MatterNotifier,
    desired: watch::Sender<Desired>,
    status: Arc<RwLock<MatterStatus>>,
    commissioner: Arc<RwLock<Option<Arc<dyn DeviceCommissioningPort>>>>,
    control: ControlCell,
    /// Signalled by the reconciler once it has torn down and stopped.
    stopped: Arc<tokio::sync::Notify>,
    nonce: std::sync::atomic::AtomicU64,
}

impl MatterRuntime {
    /// Starts the reconciler, inert until the first [`apply`](MatterRuntimePort::apply).
    pub fn new(
        data_dir: PathBuf,
        registry: Arc<dyn DeviceRegistry + Send + Sync>,
        bus: Arc<dyn EventBus>,
    ) -> Arc<Self> {
        let (desired, desired_rx) = watch::channel(Desired {
            url: String::new(),
            ble: false,
            shutdown: false,
            nonce: 0,
        });

        let notifier = MatterNotifier::new();
        let runtime = Arc::new(Self {
            notifier: notifier.clone(),
            desired,
            status: Arc::new(RwLock::new(MatterStatus::disabled())),
            commissioner: Arc::new(RwLock::new(None)),
            control: Arc::new(RwLock::new(None)),
            stopped: Arc::new(tokio::sync::Notify::new()),
            nonce: std::sync::atomic::AtomicU64::new(0),
        });

        tokio::spawn(reconcile_loop(
            desired_rx,
            Reconciler {
                data_dir,
                registry,
                bus,
                notifier,
                status: runtime.status.clone(),
                commissioner: runtime.commissioner.clone(),
                control: runtime.control.clone(),
                stopped: runtime.stopped.clone(),
            },
        ));

        runtime
    }

    /// Wait up to `timeout` for a settled state, so Matter's startup lines precede the "listening"
    /// banner; a first-run install that takes longer continues in the background.
    pub async fn settle(&self, timeout: Duration) -> MatterStatus {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.status().await;
            // Require `enabled`: right after `apply` the status is still the initial `Disabled`.
            if status.enabled && !matches!(status.state, MatterState::Connecting) {
                return status;
            }
            if tokio::time::Instant::now() >= deadline {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Route notifications via `sender`; call before the first `apply` or setup goes unannounced.
    pub async fn attach_notifications(&self, sender: Arc<dyn NotificationSender>) {
        self.notifier.attach(sender).await;
    }

    /// A [`DeviceControlPort`] following this runtime: Matter while connected, else `fallback`.
    pub fn device_control(
        &self,
        fallback: Arc<dyn DeviceControlPort>,
    ) -> Arc<SwitchableDeviceControl> {
        Arc::new(SwitchableDeviceControl {
            matter: self.control.clone(),
            status: self.status.clone(),
            fallback,
        })
    }
}

#[async_trait]
impl MatterRuntimePort for MatterRuntime {
    fn apply(&self, config: MatterConfig) {
        let nonce = self
            .nonce
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        // Closed only after shutdown, when there is nothing left to converge.
        let _ = self.desired.send(Desired {
            url: config.url,
            ble: config.ble,
            shutdown: false,
            nonce,
        });
    }

    async fn status(&self) -> MatterStatus {
        self.status.read().await.clone()
    }

    async fn commissioner(&self) -> Option<Arc<dyn DeviceCommissioningPort>> {
        self.commissioner.read().await.clone()
    }

    async fn shutdown(&self) {
        let nonce = self
            .nonce
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        let notified = self.stopped.notified();
        if self
            .desired
            .send(Desired {
                url: String::new(),
                ble: false,
                shutdown: true,
                nonce,
            })
            .is_err()
        {
            return; // reconciler already stopped
        }
        // Bounded even though giving up can orphan the controller.
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, notified)
            .await
            .is_err()
        {
            tracing::warn!("matter: runtime did not stop within the shutdown timeout");
        }
    }
}

/// The reconciler's shared handles; its mutable state is `Running`.
struct Reconciler {
    data_dir: PathBuf,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
    notifier: MatterNotifier,
    status: Arc<RwLock<MatterStatus>>,
    commissioner: Arc<RwLock<Option<Arc<dyn DeviceCommissioningPort>>>>,
    control: ControlCell,
    stopped: Arc<tokio::sync::Notify>,
}

/// What is currently running, owned by the reconciler task.
#[derive(Default)]
struct Running {
    /// The controller GIAP started. Killed explicitly, as `kill_on_drop` misses signal exits;
    /// the supervisor parks respawned handles here.
    child: SharedServerChild,
    supervisor: Option<JoinHandle<()>>,
}

/// Converge toward the requested state until asked to shut down.
async fn reconcile_loop(mut rx: watch::Receiver<Desired>, r: Reconciler) {
    // What `running` was built from, compared against each request.
    let mut current = Desired {
        url: String::new(),
        ble: false,
        shutdown: false,
        nonce: 0,
    };
    let mut running = Running::default();

    loop {
        let want = rx.borrow_and_update().clone();

        if want.shutdown {
            teardown(&mut running, &r).await;
            r.stopped.notify_waiters();
            return;
        }

        // Nonce 0 is only the channel's initial value: nothing has been asked for yet.
        if want.nonce == 0 {
            if rx.changed().await.is_err() {
                teardown(&mut running, &r).await;
                r.stopped.notify_waiters();
                return;
            }
            continue;
        }

        // Same values are a no-op while connected but a retry while not (the UI's retry button).
        let healthy = r.status.read().await.state.is_connected();
        // BLE too: it's a controller spawn argument, so only a new process applies it.
        let changed = want.url != current.url || want.ble != current.ble;
        if changed || !healthy {
            teardown(&mut running, &r).await;
            current = want.clone();

            {
                *r.status.write().await = MatterStatus {
                    enabled: true,
                    url: want.url.clone(),
                    state: MatterState::Connecting,
                };

                // Cancellable: a disable or URL edit must not wait out a minutes-long
                // install. `kill_on_drop` reaps the half-built child.
                tokio::select! {
                    biased;
                    _ = rx.changed() => continue,
                    result = connect(&r, &want.url, want.ble) => match result {
                        Ok(connected) => {
                            running.child = connected.child;
                            running.supervisor = Some(connected.supervisor);
                            *r.commissioner.write().await = Some(connected.commissioner);
                            *r.control.write().await = Some(connected.control);
                            *r.status.write().await = MatterStatus {
                                enabled: true,
                                url: want.url.clone(),
                                state: MatterState::Connected,
                            };
                            tracing::info!(
                                target: "giap::trace",
                                kind = "matter_state_changed",
                                url = %want.url,
                                to = "connected",
                                "matter: controller connected"
                            );
                        }
                        Err(e) => {
                            // `describe`, not `to_string`: the user's only account of the
                            // failure, served redacted by `GET /api/v1/matter/status`.
                            let error = describe(&e);
                            tracing::warn!(
                                target: "giap::trace",
                                kind = "matter_state_changed",
                                url = %want.url,
                                to = "unreachable",
                                error = %error,
                                "matter: could not start or reach the controller"
                            );
                            *r.status.write().await = MatterStatus {
                                enabled: true,
                                url: want.url.clone(),
                                state: MatterState::Unreachable { error },
                            };
                        }
                    },
                }
            }
        }

        if rx.changed().await.is_err() {
            // The runtime handle is gone; leave nothing running.
            teardown(&mut running, &r).await;
            r.stopped.notify_waiters();
            return;
        }
    }
}

/// Everything a successful connect produced.
struct Connected {
    child: SharedServerChild,
    supervisor: JoinHandle<()>,
    commissioner: Arc<dyn DeviceCommissioningPort>,
    control: Arc<MatterDeviceControl>,
}

/// Start the controller if the URL is ours, connect, and supervise the bridge.
async fn connect(r: &Reconciler, url: &str, ble: bool) -> Result<Connected> {
    // Older installs can be enabled with no address; say so, not a WebSocket parse error.
    if url.trim().is_empty() {
        anyhow::bail!("no Matter controller address is set");
    }

    // Only a loopback URL is GIAP's to install and run; others are used as-is.
    let started = match local_port_from_ws_url(url) {
        Some(port) => {
            ensure_running(
                &r.data_dir,
                port,
                CONTROLLER_READY_TIMEOUT,
                &r.notifier,
                url,
                ble,
            )
            .await?
        }
        None => None,
    };
    let started_here = started.is_some();
    // Written here first, then by the supervisor on each controller restart.
    let child: SharedServerChild = Arc::new(tokio::sync::Mutex::new(started));

    // A controller GIAP spawned already has its stderr relayed; skip its duplicate log events.
    let (client, events): (Arc<MatterClient>, tokio::sync::mpsc::Receiver<MatterEvent>) =
        if started_here {
            MatterClient::connect_to_managed(url).await?
        } else {
            MatterClient::connect(url).await?
        };

    let control = Arc::new(MatterDeviceControl::new(client.clone()));
    let commissioner: Arc<dyn DeviceCommissioningPort> =
        Arc::new(MatterCommissioner::new(client.clone(), r.notifier.clone()));

    let supervisor = tokio::spawn(run_matter_supervisor(
        SupervisorConfig {
            url: url.to_string(),
            data_dir: r.data_dir.clone(),
            child: child.clone(),
            ble,
        },
        control.client_handle(),
        client,
        events,
        r.registry.clone(),
        r.bus.clone(),
        r.notifier.clone(),
    ));

    Ok(Connected {
        child,
        supervisor,
        commissioner,
        control,
    })
}

/// Stop everything, clearing the shared cells first so no caller reaches a dying controller.
async fn teardown(running: &mut Running, r: &Reconciler) {
    *r.commissioner.write().await = None;
    *r.control.write().await = None;

    if let Some(supervisor) = running.supervisor.take() {
        // Abort, not await (it reconnects forever), and before the kill so it can't revive it.
        supervisor.abort();
    }
    stop_controller(&running.child, &r.data_dir).await;
}

/// Kill the controller GIAP started, read from the shared cell since respawns replace it.
pub(crate) async fn stop_controller(child: &SharedServerChild, data_dir: &std::path::Path) {
    if let Some(mut running) = child.lock().await.take() {
        tracing::info!("matter: stopping the controller GIAP started");
        let _ = running.start_kill();

        // Await exit: a restart would see the dying process as `Occupant::Foreign` and refuse.
        if tokio::time::timeout(KILL_TIMEOUT, running.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_controller_kill_timeout",
                timeout_secs = KILL_TIMEOUT.as_secs(),
                "matter: the controller did not exit after being killed; its port may still be held"
            );
        }

        // Not required (the pid check copes), but a stale pidfile costs a `ps` on every start.
        crate::server_setup::clear_pidfile(data_dir);
    }
}

/// A [`DeviceControlPort`]: Matter while connected, else the logging stub. One process-wide
/// `Arc`, cloned into the agent, MCP and tools at startup; the switch is a reconciler cell.
pub struct SwitchableDeviceControl {
    matter: ControlCell,
    status: Arc<RwLock<MatterStatus>>,
    fallback: Arc<dyn DeviceControlPort>,
}

impl SwitchableDeviceControl {
    /// The backend for `device_id`. A Matter id (by prefix) never falls back to the stub, which
    /// reports success for every verb though nothing was sent.
    async fn backend_for(&self, device_id: &str) -> Result<Arc<dyn DeviceControlPort>> {
        if let Some(matter) = self.matter.read().await.clone() {
            return Ok(matter);
        }
        if !is_matter_device_id(device_id) {
            return Ok(self.fallback.clone());
        }
        // Name the state: "off" and "unreachable" need different user actions.
        Err(match self.status.read().await.state.clone() {
            MatterState::Disabled => anyhow::anyhow!(
                "Matter is off, so '{device_id}' cannot be controlled — turn Matter on in the \
                 Devices tab"
            ),
            MatterState::Connecting => anyhow::anyhow!(
                "the Matter controller is still starting, so '{device_id}' cannot be controlled yet"
            ),
            MatterState::Unreachable { error } => anyhow::anyhow!(
                "the Matter controller is unreachable ({error}), so '{device_id}' cannot be \
                 controlled"
            ),
            // Connected with no control cell is a momentary gap during teardown.
            MatterState::Connected => anyhow::anyhow!(
                "the Matter controller is restarting, so '{device_id}' cannot be controlled yet"
            ),
        })
    }
}

// Forward every verb, optional ones too, or colour, fan and covering control get dropped.
#[async_trait]
impl DeviceControlPort for SwitchableDeviceControl {
    async fn describe(&self, device_id: &str) -> Result<DeviceDescription> {
        self.backend_for(device_id).await?.describe(device_id).await
    }

    async fn state(&self, device_id: &str) -> Result<DeviceState> {
        self.backend_for(device_id).await?.state(device_id).await
    }

    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_power(device_id, on)
            .await
    }

    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_brightness(device_id, percent)
            .await
    }

    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_target_temp(device_id, celsius)
            .await
    }

    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_locked(device_id, locked)
            .await
    }

    async fn set_color(
        &self,
        device_id: &str,
        hue_degrees: u16,
        saturation_percent: u8,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_color(device_id, hue_degrees, saturation_percent)
            .await
    }

    async fn set_volume(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_volume(device_id, percent)
            .await
    }

    async fn set_color_temp(&self, device_id: &str, kelvin: u32) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_color_temp(device_id, kelvin)
            .await
    }

    async fn set_fan_speed(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_fan_speed(device_id, percent)
            .await
    }

    async fn set_mode(
        &self,
        device_id: &str,
        setting: &str,
        value: &str,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_mode(device_id, setting, value)
            .await
    }

    async fn set_operation(
        &self,
        device_id: &str,
        operation: &str,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_operation(device_id, operation)
            .await
    }

    async fn set_tilt(&self, device_id: &str, percent_open: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_tilt(device_id, percent_open)
            .await
    }

    async fn set_valve(&self, device_id: &str, open: bool) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_valve(device_id, open)
            .await
    }

    async fn set_position(
        &self,
        device_id: &str,
        percent_open: u8,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_position(device_id, percent_open)
            .await
    }
}

#[cfg(test)]
mod controller_lifetime_tests {
    use super::*;

    /// Whether the OS still knows `pid`; `kill -0` succeeds even on an unreaped zombie.
    fn pid_exists(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn stopping_the_controller_waits_for_it_to_actually_exit() {
        let spawned = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("sleep must be available");
        let pid = spawned.id().expect("a freshly spawned child has a pid");
        assert!(pid_exists(pid), "the child must be running to begin with");

        let child: SharedServerChild = Arc::new(tokio::sync::Mutex::new(Some(spawned)));
        stop_controller(&child, std::path::Path::new("/nonexistent")).await;

        assert!(
            !pid_exists(pid),
            "pid {pid} still exists after stop_controller returned — it was signalled \
             but not reaped, so its port is still held"
        );
        assert!(
            child.lock().await.is_none(),
            "the handle must be taken, so nothing kills a pid that has been reused"
        );
    }
}
