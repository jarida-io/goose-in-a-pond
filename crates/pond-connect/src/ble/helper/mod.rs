//! The peripheral that runs the radio in a child process and supervises it. On macOS a process
//! that touches CoreBluetooth without a Bluetooth grant for its responsible app is killed by TCC
//! (`docs/matter.md`), so only the helper ever does: its death switches BLE off, never the pond.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::peripheral::{Advertisement, BleError, BlePeripheral, PeripheralStatus};
use crate::setup_channel::{SessionEnd, SessionHandle, SessionWriter, SetupChannel};

pub mod launcher;
pub mod protocol;

pub use launcher::{HelperExit, HelperLauncher, HelperProcess, ProcessLauncher};
use protocol::{encode_line, Authorization, Bytes, Command, Event, RadioState, PROTOCOL_VERSION};

/// How TCC ends a process whose responsible app may not use Bluetooth: SIGKILL (measured with the
/// Matter controller) or an abort with a TCC payload (measured with this helper on macOS 27).
const SIGABRT: i32 = 6;
const SIGKILL: i32 = 9;

const UNAUTHORIZED: &str = "macOS has not allowed the pond to use Bluetooth; allow it in System \
     Settings > Privacy & Security > Bluetooth";

/// How often and how soon a dead helper is started again. A helper macOS kills for want of a
/// Bluetooth grant dies the same way every time, so the budget is small and never refills quickly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub first_backoff: Duration,
    pub max_backoff: Duration,
    /// A helper that ran this long died of something else; its death starts the budget afresh.
    pub healthy_uptime: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            first_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(30),
            healthy_uptime: Duration::from_secs(300),
        }
    }
}

impl RestartPolicy {
    /// 2 s, 8 s, then 30 s with the defaults.
    pub fn backoff(&self, restarts: u32) -> Duration {
        self.first_backoff
            .saturating_mul(4u32.saturating_pow(restarts))
            .min(self.max_backoff)
    }
}

/// What the helper last said about the radio.
#[derive(Debug, Default)]
struct Radio {
    authorization: Option<Authorization>,
    /// The helper said CoreBluetooth went quiet without ever reporting a state.
    silent: Option<String>,
    state: Option<RadioState>,
    service_added: bool,
    service_error: Option<String>,
    advertising: bool,
}

impl Radio {
    fn status(&self) -> PeripheralStatus {
        match self.state {
            None | Some(RadioState::Unknown) => match (&self.silent, self.authorization) {
                (None, _) => PeripheralStatus::Off,
                (Some(_), Some(Authorization::NotDetermined)) => PeripheralStatus::unavailable(
                    "macOS is waiting for someone to allow Bluetooth for the app the pond runs \
                     under",
                ),
                (Some(_), Some(Authorization::Denied | Authorization::Restricted)) => {
                    PeripheralStatus::unavailable(UNAUTHORIZED)
                }
                (Some(silence), _) => PeripheralStatus::unavailable(silence.clone()),
            },
            Some(RadioState::PoweredOn) => match &self.service_error {
                Some(error) => PeripheralStatus::unavailable(format!(
                    "macOS refused the setup service: {error}"
                )),
                None if self.advertising => PeripheralStatus::Advertising,
                None if self.service_added => PeripheralStatus::Serving,
                None => PeripheralStatus::Off,
            },
            Some(RadioState::PoweredOff) => {
                PeripheralStatus::unavailable("Bluetooth is turned off on this Mac")
            }
            Some(RadioState::Unauthorized) => PeripheralStatus::unavailable(UNAUTHORIZED),
            Some(RadioState::Unsupported) => {
                PeripheralStatus::unavailable("this Mac has no Bluetooth LE radio the pond can use")
            }
            Some(RadioState::Resetting) => PeripheralStatus::unavailable("Bluetooth is restarting"),
        }
    }
}

struct Shared {
    status: RwLock<PeripheralStatus>,
    /// The controller's last wish, so a restarted helper picks it up.
    wanted: Mutex<Option<String>>,
    /// The running helper's stdin, while there is one.
    commands: Mutex<Option<mpsc::UnboundedSender<Command>>>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl Shared {
    fn set_status(&self, status: PeripheralStatus) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = status;
    }

    fn status(&self) -> PeripheralStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_commands(&self, commands: Option<mpsc::UnboundedSender<Command>>) {
        *self.commands.lock().unwrap_or_else(|e| e.into_inner()) = commands;
    }

    fn send(&self, command: Command) -> bool {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|commands| commands.send(command).is_ok())
    }
}

/// [`BlePeripheral`] over a helper process speaking [`protocol`].
pub struct HelperPeripheral {
    launcher: Arc<dyn HelperLauncher>,
    policy: RestartPolicy,
    shared: Arc<Shared>,
}

impl HelperPeripheral {
    pub fn new(launcher: Arc<dyn HelperLauncher>, policy: RestartPolicy) -> Self {
        Self {
            launcher,
            policy,
            shared: Arc::new(Shared {
                status: RwLock::new(PeripheralStatus::Off),
                wanted: Mutex::new(None),
                commands: Mutex::new(None),
                supervisor: Mutex::new(None),
            }),
        }
    }
}

#[async_trait]
impl BlePeripheral for HelperPeripheral {
    async fn start(&self, channel: Arc<SetupChannel>) -> Result<(), BleError> {
        let mut supervisor = self
            .shared
            .supervisor
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if supervisor.is_none() {
            *supervisor = Some(tokio::spawn(supervise(
                self.launcher.clone(),
                self.policy,
                self.shared.clone(),
                channel,
            )));
        }
        Ok(())
    }

    async fn advertise(&self, advertisement: Option<Advertisement>) -> Result<(), BleError> {
        let name = advertisement.map(|a| a.local_name);
        *self.shared.wanted.lock().unwrap_or_else(|e| e.into_inner()) = name.clone();
        let command = match name {
            Some(local_name) => Command::Advertise { local_name },
            None => Command::StopAdvertising,
        };
        if self.shared.send(command) {
            return Ok(());
        }
        let supervised = self
            .shared
            .supervisor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|supervisor| !supervisor.is_finished());
        if supervised {
            // Recorded in `wanted`: a helper that is starting or restarting picks it up.
            return Ok(());
        }
        Err(BleError::Unavailable(
            self.shared
                .status()
                .reason()
                .unwrap_or("the Bluetooth helper is not running")
                .to_string(),
        ))
    }

    async fn shutdown(&self) {
        let supervisor = self
            .shared
            .supervisor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(supervisor) = supervisor {
            // Dropping the helper's handle kills it.
            supervisor.abort();
            let _ = supervisor.await;
        }
        self.shared.set_commands(None);
        self.shared.set_status(PeripheralStatus::Off);
    }

    fn status(&self) -> PeripheralStatus {
        self.shared.status()
    }
}

async fn supervise(
    launcher: Arc<dyn HelperLauncher>,
    policy: RestartPolicy,
    shared: Arc<Shared>,
    channel: Arc<SetupChannel>,
) {
    let mut restarts = 0;
    loop {
        let launched = Instant::now();
        let reason = match launcher.launch() {
            Ok(process) => {
                tracing::info!(kind = "ble_helper_started", "started the Bluetooth helper");
                let (exit, radio_answered) = drive(process, &channel, &shared).await;
                describe_exit(&exit, radio_answered)
            }
            Err(error) => format!("could not start the Bluetooth helper: {error}"),
        };
        shared.set_status(PeripheralStatus::unavailable(reason.clone()));
        if launched.elapsed() >= policy.healthy_uptime {
            restarts = 0;
        }
        if restarts >= policy.max_restarts {
            tracing::error!(
                kind = "ble_unavailable",
                %reason,
                restarts,
                "the Bluetooth helper keeps stopping; BLE setup stays off until the pond restarts"
            );
            return;
        }
        let wait = policy.backoff(restarts);
        restarts += 1;
        tracing::warn!(
            kind = "ble_unavailable",
            %reason,
            attempt = restarts,
            retry_in_secs = wait.as_secs(),
            "the Bluetooth helper stopped; BLE setup is off until it starts again"
        );
        tokio::time::sleep(wait).await;
    }
}

/// For the household: the likeliest cause first, since TCC's kill carries no message the pond sees.
fn describe_exit(exit: &HelperExit, radio_answered: bool) -> String {
    let said = exit
        .last_words
        .as_deref()
        .map(|words| format!(": {words}"))
        .unwrap_or_default();
    match (exit.signal, exit.code) {
        (Some(signal @ (SIGABRT | SIGKILL)), _) if !radio_answered => format!(
            "macOS stopped the Bluetooth helper (signal {signal}) before the radio answered, which \
             it does when the app the pond runs under has no Bluetooth permission or declares no \
             Bluetooth use; run the pond from the GIAP app, or from a terminal allowed Bluetooth"
        ),
        (Some(signal), _) => format!("the Bluetooth helper was stopped by signal {signal}{said}"),
        (None, Some(code)) => format!("the Bluetooth helper exited with status {code}{said}"),
        (None, None) => format!("the Bluetooth helper stopped{said}"),
    }
}

/// The session one link carries. The helper never reuses a link number, so a late command can
/// only ever name a link that is already gone.
struct Link {
    writer: SessionWriter,
}

/// Runs one helper to its end. Returns how it ended and whether the radio ever answered.
async fn drive(
    process: HelperProcess,
    channel: &SetupChannel,
    shared: &Shared,
) -> (HelperExit, bool) {
    let HelperProcess {
        stdin,
        stdout,
        mut exited,
    } = process;
    let (commands, outbox) = mpsc::unbounded_channel();
    let writer = tokio::spawn(write_commands(stdin, outbox));
    let resume = shared
        .wanted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(local_name) = resume {
        let _ = commands.send(Command::Advertise { local_name });
    }
    shared.set_commands(Some(commands.clone()));

    let mut radio = Radio::default();
    let mut answered = false;
    let mut links: HashMap<u64, Link> = HashMap::new();
    let (ended_tx, mut ended) = mpsc::unbounded_channel::<u64>();
    let mut lines = BufReader::new(stdout).lines();
    let exit = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => match serde_json::from_str::<Event>(&line) {
                    Ok(event) => {
                        if matches!(event, Event::State { .. }) {
                            answered = true;
                        }
                        on_event(event, channel, shared, &mut radio, &mut links, &commands, &ended_tx)
                            .await;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "an unreadable line from the Bluetooth helper")
                    }
                },
                Ok(None) | Err(_) => break None,
            },
            Some(link) = ended.recv() => {
                links.remove(&link);
            }
            exit = &mut exited => break Some(exit),
        }
    };
    shared.set_commands(None);
    for (_, link) in links.drain() {
        link.writer.phone_disconnected().await;
    }
    writer.abort();
    let exit = match exit {
        Some(exit) => exit,
        None => exited.await,
    };
    (exit, answered)
}

async fn on_event(
    event: Event,
    channel: &SetupChannel,
    shared: &Shared,
    radio: &mut Radio,
    links: &mut HashMap<u64, Link>,
    commands: &mpsc::UnboundedSender<Command>,
    ended: &mpsc::UnboundedSender<u64>,
) {
    let about_the_radio = !matches!(
        event,
        Event::Subscribed { .. }
            | Event::Mtu { .. }
            | Event::Write { .. }
            | Event::Unsubscribed { .. }
    );
    match event {
        Event::Ready { protocol } => {
            if protocol != PROTOCOL_VERSION {
                tracing::warn!(
                    protocol,
                    expected = PROTOCOL_VERSION,
                    "the Bluetooth helper speaks another protocol version"
                );
            }
            tracing::info!(
                kind = "ble_helper_ready",
                protocol,
                "the Bluetooth helper is up"
            );
        }
        Event::Authorization { authorization } => {
            radio.authorization = Some(authorization);
            tracing::info!(kind = "ble_authorization", authorization = ?authorization, "Bluetooth permission for the pond's helper");
        }
        Event::State { state } => {
            radio.silent = None;
            radio.state = Some(state);
            if state != RadioState::PoweredOn {
                // CoreBluetooth drops the service and the advertisement with the radio.
                radio.service_added = false;
                radio.service_error = None;
                radio.advertising = false;
            }
            tracing::info!(kind = "ble_radio_state", state = ?state, "Bluetooth radio state");
        }
        Event::ServiceAdded => {
            radio.service_added = true;
            radio.service_error = None;
            tracing::info!(
                kind = "ble_service_added",
                "the setup channel's GATT service is up"
            );
        }
        Event::Advertising { local_name } => {
            radio.advertising = true;
            tracing::info!(kind = "ble_advertising_started", %local_name, "advertising the setup channel");
        }
        Event::AdvertisingStopped => {
            radio.advertising = false;
            tracing::info!(
                kind = "ble_advertising_stopped",
                "stopped advertising the setup channel"
            );
        }
        Event::Subscribed { link, att_mtu, .. } => {
            if let Some(open) = links.get(&link) {
                let _ = open.writer.set_att_mtu(att_mtu).await;
                return;
            }
            match channel.open() {
                Ok(handle) => {
                    let writer = open_link(handle, link, att_mtu, commands, ended).await;
                    links.insert(link, Link { writer });
                    tracing::info!(
                        kind = "ble_phone_connected",
                        link,
                        att_mtu,
                        "a phone opened the setup channel over Bluetooth"
                    );
                }
                Err(_) => {
                    let _ = commands.send(Command::Disconnect { link });
                    tracing::info!(
                        kind = "ble_phone_refused",
                        link,
                        "refused a phone: the setup channel is busy or switched off"
                    );
                }
            }
        }
        Event::Mtu { link, att_mtu } => {
            if let Some(open) = links.get(&link) {
                let _ = open.writer.set_att_mtu(att_mtu).await;
            }
        }
        Event::Write { link, data } => {
            if let Some(open) = links.get(&link) {
                // A session that just ended refuses the write; its pump disconnects the link.
                let _ = open.writer.write(&data.0).await;
            }
        }
        Event::Unsubscribed { link } => {
            if let Some(open) = links.remove(&link) {
                open.writer.phone_disconnected().await;
            }
        }
        Event::Error { context, message } => {
            tracing::warn!(kind = "ble_helper_error", %context, %message, "the Bluetooth helper reported an error");
            match context.as_str() {
                "add_service" => radio.service_error = Some(message),
                "radio" => radio.silent = Some(message),
                _ => {}
            }
        }
    }
    if about_the_radio {
        shared.set_status(radio.status());
    }
}

/// Hands a session's notifications to its link, and ends the link when the session ends.
async fn open_link(
    handle: SessionHandle,
    link: u64,
    att_mtu: u16,
    commands: &mpsc::UnboundedSender<Command>,
    ended: &mpsc::UnboundedSender<u64>,
) -> SessionWriter {
    let SessionHandle {
        writer,
        mut notifications,
    } = handle;
    let _ = writer.set_att_mtu(att_mtu).await;
    let commands = commands.clone();
    let ended = ended.clone();
    tokio::spawn(async move {
        while let Some(chunk) = notifications.next().await {
            if commands
                .send(Command::Notify {
                    link,
                    data: Bytes(chunk),
                })
                .is_err()
            {
                break;
            }
        }
        let end = notifications.end();
        // A phone that left needs no refusal, and it may already be back on a new link.
        if end != Some(SessionEnd::PhoneDisconnected) {
            let _ = commands.send(Command::Disconnect { link });
        }
        tracing::info!(
            kind = "ble_session_ended",
            link,
            reason = end.map_or("helper_stopped", SessionEnd::as_str),
            "a Bluetooth setup session ended"
        );
        let _ = ended.send(link);
    });
    writer
}

async fn write_commands(
    mut stdin: Box<dyn AsyncWrite + Send + Unpin>,
    mut outbox: mpsc::UnboundedReceiver<Command>,
) {
    while let Some(command) = outbox.recv().await {
        let Ok(line) = encode_line(&command) else {
            continue;
        };
        if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
