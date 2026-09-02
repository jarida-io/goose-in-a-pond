//! The Matter bridge task: keeps GIAP's view of the fabric current.
//!
//! On start it sends `subscribe`, which returns every commissioned device and
//! every reading they currently hold, and subscribes the connection to changes.
//! Devices are synced into the [`DeviceRegistry`] under stable `matter-<node_id>`
//! ids. From then on:
//!
//! - a `reading` event → [`BusEvent::Sensor`] on the EventBus, so #92 rules, the
//!   activity feed and notifications react to Matter sensors exactly like any
//!   other sensor source;
//! - device lifecycle events refresh the registry.
//!
//! On connection loss `run_matter_bridge` returns. [`run_matter_supervisor`]
//! wraps it in a reconnect loop: when the connection drops it re-establishes the
//! WebSocket with backoff, swaps the new client into the shared handle the
//! control port reads, and re-runs the bridge — which subscribes again and
//! resyncs the fabric. A controller restart does not need a pond-server restart.
//!
//! Reconnecting only recovers a controller that is up. When the controller
//! *process* has died there is nothing to reconnect to, so after
//! [`RESPAWN_AFTER`] consecutive failures the supervisor re-runs the local
//! controller setup before the next attempt, and keeps doing so on that cadence
//! until it is back.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::ports::device_registry::{DeviceRegistry, RegisterDeviceRequest};
use serde_json::json;
use tokio::sync::mpsc;

use crate::client::{MatterClient, MatterEvent};
use crate::control::SharedMatterClient;
use crate::notify::MatterNotifier;
use crate::protocol::{
    describe, AvailabilityEvent, DeviceEvent, DeviceRemovedEvent, Snapshot, WireDevice, WireReading,
};
use crate::server_setup::{revive_local_controller, Revival, SharedServerChild};

/// Reconnect backoff bounds. Exponential from `RECONNECT_BASE` doubling to
/// `RECONNECT_MAX`, with equal jitter so several Ponds pointed at one restarted
/// controller don't reconnect in lockstep.
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Consecutive reconnect failures before the supervisor stops assuming the
/// controller is merely unreachable and tries to restart it. Three is past the
/// blip a restarting controller causes (~3.5s of backoff) while still well
/// inside the time a user would wait before reaching for the toggle themselves.
const RESPAWN_AFTER: u32 = 3;

/// How long a revived controller gets to start listening. Shorter than the
/// startup budget: by the time the supervisor runs, the dependencies are already
/// installed, so this waits on a process start rather than on an `npm ci`.
const RESPAWN_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The last value published per `(device, sensor)`.
///
/// The reason this exists rather than publishing whatever `subscribe` returns:
/// the bridge re-subscribes on every reconnect, and the rules engine (#92) is
/// LEVEL-based — republishing a steady "motion = true" is indistinguishable to
/// it from motion starting again. A controller that reconnects a few times would
/// re-fire every automation attached to every Matter sensor with nothing in the
/// house having changed, and the reconnect-often case is exactly the one the
/// supervisor exists to handle.
///
/// First sight still publishes everything, which is the behaviour that makes a
/// steady sensor knowable at all; the cache is what distinguishes the two.
pub(crate) type ReadingCache = HashMap<(String, String), f64>;

/// Where the supervisor reconnects to, and what it needs to bring the controller
/// back when reconnecting is not enough.
pub struct SupervisorConfig {
    /// The controller's WebSocket URL. Also decides whether the controller is
    /// GIAP's to restart: only a loopback URL is.
    pub url: String,
    /// GIAP's data dir — where the controller and its fabric live.
    pub data_dir: PathBuf,
    /// The controller GIAP started, if any. A respawn replaces the dead handle
    /// here, so the reconciler's teardown still kills the live process.
    pub child: SharedServerChild,
    /// Whether the respawned controller should be asked for BLE again.
    ///
    /// Carried rather than re-read: a respawn that quietly dropped a transport
    /// the user asked for would leave a Pond that pairs new devices until the
    /// first reconnect and then silently stops.
    pub ble: bool,
}

/// Should the reconnect about to be made (1-based `attempt`) re-run controller
/// setup first? True on the attempt following every [`RESPAWN_AFTER`] failures,
/// so a controller that stays dead keeps being retried for as long as the outage
/// lasts rather than once and never again. Pure, so the schedule is
/// unit-testable without sleeping.
fn should_respawn_controller(attempt: u32) -> bool {
    attempt > 1 && (attempt - 1).is_multiple_of(RESPAWN_AFTER)
}

/// Backoff delay for reconnect attempt `attempt` (1-based), with equal jitter.
/// Pure so the schedule is unit-testable without sleeping.
fn reconnect_backoff(attempt: u32, rand_unit: f64) -> Duration {
    let base = RECONNECT_BASE.as_millis() as u64;
    let cap = RECONNECT_MAX.as_millis() as u64;
    // Exponential, saturating, capped — shift caps at 63 to avoid overflow.
    let exp = base.saturating_mul(
        1u64.checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u64::MAX),
    );
    let capped = exp.min(cap);
    // Equal jitter: half fixed, half random in [0, half].
    let half = capped / 2;
    let jitter = (half as f64 * rand_unit.clamp(0.0, 1.0)) as u64;
    Duration::from_millis(half + jitter)
}

/// Publish a reading unless it is one we have already published at that value.
fn publish_reading(reading: &WireReading, cache: &mut ReadingCache, bus: &Arc<dyn EventBus>) {
    let key = (reading.device_id.clone(), reading.sensor_type.clone());
    if cache.get(&key) == Some(&reading.value) {
        return;
    }
    cache.insert(key, reading.value);
    bus.publish(BusEvent::Sensor(reading.to_reading()));
}

/// How often a device the controller can still see is touched in the registry.
///
/// `is_online` is derived from `last_seen` being fresher than five minutes, so
/// something has to say "still here" or every device eventually reads offline. The
/// bridge only ever said it when an event arrived, and a Matter device that is
/// simply idle sends none: a washer nobody touched went offline five minutes after
/// the server started, and its "last seen" stayed frozen at the moment it was
/// synced -- which is why the card read like a commissioning timestamp.
///
/// A fifth of the threshold, so four ticks can be missed before a device that is
/// genuinely present is called absent.
const LIVENESS_TICK: Duration = Duration::from_secs(60);

/// Sync one device into the registry (register if new, heartbeat if known).
async fn sync_device(
    wire: &WireDevice,
    registry: &Arc<dyn DeviceRegistry + Send + Sync>,
    notifier: &MatterNotifier,
) {
    let device = wire.to_device();

    match registry.get_device(&device.id).await {
        Ok(Some(existing)) => {
            // Only for a device the controller can actually see. This runs for every
            // device in the snapshot, including the ones it reports as offline, so an
            // unconditional heartbeat handed each of those a fresh five minutes of
            // looking present at every connect and reconnect — a second mechanism
            // vouching for a device the first one had already given up on.
            if device.is_online {
                if let Err(e) = registry.heartbeat(&device.id).await {
                    tracing::warn!(device = %device.id, error = %e, "matter: heartbeat failed");
                }
            }
            // Re-derived typing has to reach a device that already exists, or it
            // only ever applies to devices commissioned after the improvement
            // shipped. Registration was the sole writer of these two fields, so
            // every fan and sensor already on the fabric kept `device_type:
            // "matter"` and no capabilities through every restart.
            //
            // Guarded on a real difference because this runs on the initial sync
            // and on every reconnect — an unconditional UPDATE would be a write
            // per device per reconnect for a value that almost never changes.
            if existing.device_type != device.device_type
                || existing.capabilities != device.capabilities
            {
                if let Err(e) = registry
                    .set_discovered_profile(&device.id, &device.device_type, &device.capabilities)
                    .await
                {
                    tracing::warn!(device = %device.id, error = %e, "matter: profile refresh failed");
                } else {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "matter_device_retyped",
                        device = %device.id,
                        from = %existing.device_type,
                        to = %device.device_type,
                        "matter: re-typed an already-registered device"
                    );
                }
            }
        }
        Ok(None) => {
            let request = RegisterDeviceRequest {
                id: Some(device.id.clone()),
                name: device.name.clone(),
                device_type: device.device_type.clone(),
                hostname: None,
                capabilities: device.capabilities.clone(),
                room: None,
            };
            match registry.register(request).await {
                Ok(_) => {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "matter_node_added",
                        device = %device.id,
                        name = %device.name,
                        device_type = %device.device_type,
                        "matter: device registered"
                    );
                    notifier
                        .device_paired(&device.id, &device.name, &device.device_type)
                        .await;
                }
                Err(e) => {
                    tracing::warn!(device = %device.id, error = %e, "matter: registration failed")
                }
            }
        }
        Err(e) => tracing::warn!(device = %device.id, error = %e, "matter: registry lookup failed"),
    }
}

/// Run until the connection drops. `client` must be freshly connected; `events`
/// is its event stream.
///
/// Starts a fresh [`ReadingCache`], so every reading in the first `subscribe`
/// snapshot is published. Right for a one-shot run; wrong for the supervisor's
/// reconnect loop, which calls [`run_matter_bridge_with_cache`] instead so the
/// cache outlives a single connection.
pub async fn run_matter_bridge(
    client: Arc<MatterClient>,
    events: mpsc::Receiver<MatterEvent>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
    notifier: MatterNotifier,
    // How often to vouch for the devices the controller can still see. A parameter
    // so the behaviour can be tested without waiting a minute for it; production
    // passes LIVENESS_TICK.
    liveness_tick: Duration,
) -> Result<()> {
    let mut cache = ReadingCache::new();
    run_matter_bridge_with_cache(
        client,
        events,
        registry,
        bus,
        notifier,
        liveness_tick,
        &mut cache,
    )
    .await
}

/// As [`run_matter_bridge`], but the caller owns the dedupe cache.
///
/// The cache has to outlive a single bridge run or it does nothing at all.
/// [`ReadingCache`] exists because the bridge re-subscribes on every reconnect
/// and the rules engine is level-based, so republishing a steady
/// "motion = true" re-fires every automation attached to it. A cache rebuilt
/// per run makes each reconnect's snapshot "first sight" again — which is
/// exactly the case it was written for, so it was inert precisely when it
/// mattered. Kept private: callers other than the supervisor have no reason to
/// hold one, and `run_matter_bridge` is the shape they want.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_matter_bridge_with_cache(
    client: Arc<MatterClient>,
    mut events: mpsc::Receiver<MatterEvent>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
    notifier: MatterNotifier,
    liveness_tick: Duration,
    cache: &mut ReadingCache,
) -> Result<()> {
    // Initial sync: `subscribe` returns the whole fabric AND subscribes this
    // connection to subsequent events.
    let snapshot: Snapshot = serde_json::from_value(
        client
            .send("subscribe", json!({}))
            .await
            .context("subscribing to the Matter controller failed")?,
    )
    .context("the controller sent a snapshot this version does not understand")?;

    tracing::info!(
        target: "giap::trace",
        kind = "matter_subscribed",
        devices = snapshot.devices.len(),
        readings = snapshot.readings.len(),
        "matter: fabric synced"
    );

    // Who the controller currently believes is on the fabric. Held here rather than
    // read back from the registry because the controller is the authority on it:
    // the registry only knows when someone last said so.
    let mut present: HashSet<String> = HashSet::new();
    for device in &snapshot.devices {
        sync_device(device, &registry, &notifier).await;
        if device.online {
            present.insert(device.id.clone());
        }
    }
    for reading in &snapshot.readings {
        publish_reading(reading, cache, &bus);
    }

    let mut liveness = tokio::time::interval(liveness_tick);
    // The first tick fires immediately and everything above has just been synced;
    // skipping a late tick rather than firing a burst of them keeps a bridge that
    // was starved from writing one UPDATE per device per missed minute.
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    liveness.tick().await;

    loop {
        let MatterEvent { event, payload } = tokio::select! {
            received = events.recv() => match received {
                Some(event) => event,
                // Channel closed: the connection is gone and the caller reconnects.
                None => break,
            },
            _ = liveness.tick() => {
                for device_id in &present {
                    if let Err(e) = registry.heartbeat(device_id).await {
                        tracing::warn!(device = %device_id, error = %e, "matter: heartbeat failed");
                    }
                }
                continue;
            }
        };

        match event.as_str() {
            "reading" => {
                if let Ok(reading) = serde_json::from_value::<WireReading>(payload) {
                    tracing::debug!(
                        device = %reading.device_id,
                        sensor = %reading.sensor_type,
                        value = reading.value,
                        "matter: sensor update"
                    );
                    publish_reading(&reading, cache, &bus);
                }
            }
            "device_added" | "device_updated" => {
                if let Ok(DeviceEvent { device }) = serde_json::from_value::<DeviceEvent>(payload) {
                    if device.online {
                        present.insert(device.id.clone());
                    } else {
                        present.remove(&device.id);
                    }
                    sync_device(&device, &registry, &notifier).await;
                }
            }
            "device_removed" => {
                if let Ok(DeviceRemovedEvent { device_id }) =
                    serde_json::from_value::<DeviceRemovedEvent>(payload)
                {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "matter_node_removed",
                        device = %device_id,
                        "matter: device removed from the fabric"
                    );
                    // Only news if GIAP still thinks it has this device: a user
                    // deleting one goes through the same removal, and telling
                    // them about the thing they just did is noise.
                    //
                    // The lookup already had the name; the alert used to be given
                    // the id and print it, so it named the device `"matter-18"`.
                    present.remove(&device_id);
                    if let Ok(Some(device)) = registry.get_device(&device_id).await {
                        notifier.device_dropped(&device_id, &device.name).await;
                    }
                }
            }
            "device_availability" => {
                if let Ok(AvailabilityEvent { device_id, online }) =
                    serde_json::from_value::<AvailabilityEvent>(payload)
                {
                    // A LEVEL report, repeated on the controller's tick, so most of
                    // these say what the last one said. Log the CHANGES, at info.
                    //
                    // It was `debug!`, and the tracing filter admits debug from
                    // `pond_server` only — so across every log file on the machine
                    // where a working device kept going offline, the string
                    // "availability changed" did not appear once. A state change the
                    // user sees on a card, and gets an OS notification for, left no
                    // trace anywhere. That is most of why this took three passes to
                    // find.
                    if online != present.contains(&device_id) {
                        tracing::info!(
                            target: "giap::trace",
                            kind = "matter_availability_changed",
                            device = %device_id,
                            online,
                            "matter: a device's reachability changed"
                        );
                    }
                    if online {
                        present.insert(device_id.clone());
                        if let Err(e) = registry.heartbeat(&device_id).await {
                            tracing::warn!(device = %device_id, error = %e, "matter: heartbeat failed");
                        }
                    } else {
                        // Stop vouching for it. Its `last_seen` then ages out on its
                        // own, so the card turns offline without a second mechanism
                        // that could disagree with this one.
                        present.remove(&device_id);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(()) // event channel closed = connection gone; caller reconnects
}

/// Run the bridge forever, reconnecting transparently when the controller
/// connection drops.
///
/// Owns the loop that [`run_matter_bridge`] documented as "caller reconnects":
/// on drop it re-establishes the WebSocket with capped, jittered backoff, swaps
/// the fresh client into `client_cell` (so the control port the MCP tool holds
/// keeps working without being rebuilt), and re-runs the bridge.
///
/// When reconnecting keeps failing it also revives the controller itself — see
/// [`should_respawn_controller`] — because a process that has exited will never
/// answer a reconnect, however long the loop runs.
///
/// This never returns while the process lives; it is expected to be
/// `tokio::spawn`ed.
#[allow(clippy::too_many_arguments)]
pub async fn run_matter_supervisor(
    config: SupervisorConfig,
    client_cell: SharedMatterClient,
    mut client: Arc<MatterClient>,
    mut events: mpsc::Receiver<MatterEvent>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
    notifier: MatterNotifier,
) {
    let SupervisorConfig {
        url,
        data_dir,
        child,
        ble,
    } = config;

    // Owned out here, not inside the bridge: a reconnect must not re-publish a
    // reading that has not changed. See `run_matter_bridge_with_cache`.
    let mut cache = ReadingCache::new();

    loop {
        match run_matter_bridge_with_cache(
            client.clone(),
            events,
            registry.clone(),
            bus.clone(),
            notifier.clone(),
            LIVENESS_TICK,
            &mut cache,
        )
        .await
        {
            Ok(()) => tracing::warn!(
                target: "giap::trace",
                kind = "matter_bridge_stopped",
                "matter: bridge stopped (connection closed); reconnecting"
            ),
            Err(e) => tracing::warn!(
                target: "giap::trace",
                kind = "matter_bridge_failed",
                error = %describe(&e),
                "matter: bridge failed; reconnecting"
            ),
        }

        // Reconnect with backoff until it succeeds; the fabric is resynced when
        // the next run_matter_bridge subscribes.
        let mut attempt: u32 = 1;
        let (new_client, new_events) = loop {
            // Enough failures in a row means the controller is probably gone
            // rather than busy — reconnecting cannot fix that, restarting can.
            if should_respawn_controller(attempt) {
                // The first revival attempt is also the point at which this stops
                // looking like a blip to a person: told any earlier, a user would
                // be notified every time the controller restarted normally.
                notifier.controller_unreachable(&url).await;

                match revive_local_controller(&data_dir, &url, &child, RESPAWN_READY_TIMEOUT, ble)
                    .await
                {
                    Ok(Revival::Restarted) => tracing::info!(
                        target: "giap::trace",
                        kind = "matter_controller_revived",
                        url = %url,
                        outcome = "restarted",
                        "matter: controller was not running; restarted it"
                    ),
                    // Reused: the controller is up, so the fault is in the
                    // connection and the backoff below is the right answer.
                    // NotLocal: another host's controller, not ours to restart.
                    Ok(Revival::Reused | Revival::NotLocal) => {}
                    Err(e) => tracing::warn!(
                        target: "giap::trace",
                        kind = "matter_controller_revive_failed",
                        error = %describe(&e),
                        "matter: controller restart failed; will retry with the next attempts"
                    ),
                }
            }

            let delay = reconnect_backoff(attempt, rand::random::<f64>());
            tokio::time::sleep(delay).await;
            match MatterClient::connect(&url).await {
                Ok(pair) => break pair,
                Err(e) => {
                    tracing::warn!(
                        target: "giap::trace",
                        kind = "matter_reconnect_attempt",
                        url = %url,
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        error = %describe(&e),
                        "matter: reconnect attempt failed; will retry"
                    );
                    attempt = attempt.saturating_add(1);
                }
            }
        };

        *client_cell.write().await = new_client.clone();
        client = new_client;
        events = new_events;
        tracing::info!(
            target: "giap::trace",
            kind = "matter_reconnected",
            url = %url,
            attempts = attempt,
            "matter: reconnected to the controller"
        );
        notifier.controller_recovered().await;
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;
    use futures::StreamExt;

    /// The next event on the bus, or `None` if nothing arrives promptly. The bus
    /// hands back a stream, so "nothing was published" is a short wait rather
    /// than an immediate answer.
    async fn next_event(
        stream: &mut pond_core::shared::ports::event_bus::BusStream,
    ) -> Option<BusEvent> {
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .ok()
            .flatten()
    }

    #[test]
    fn backoff_grows_then_caps_and_stays_within_jitter_bounds() {
        // With zero jitter the delay is exactly half the (capped) exponential.
        assert_eq!(reconnect_backoff(1, 0.0), Duration::from_millis(500)); // 1s/2
        assert_eq!(reconnect_backoff(2, 0.0), Duration::from_secs(1)); // 2s/2
        assert_eq!(reconnect_backoff(3, 0.0), Duration::from_secs(2)); // 4s/2

        // Caps at RECONNECT_MAX (30s): half = 15s regardless of attempt, and a
        // huge attempt must not overflow.
        assert_eq!(reconnect_backoff(20, 0.0), Duration::from_secs(15));
        assert_eq!(reconnect_backoff(u32::MAX, 0.0), Duration::from_secs(15));

        // Full jitter adds up to another half; a capped attempt lands in
        // [15s, 30s].
        let full = reconnect_backoff(20, 1.0);
        assert!(full >= Duration::from_secs(15) && full <= Duration::from_secs(30));
    }

    #[test]
    fn controller_revival_waits_for_repeated_failures_then_keeps_retrying() {
        // A controller merely restarting is back within a couple of attempts.
        // Reviving on those would race its own startup and, worse, treat every
        // ordinary blip as a dead process.
        assert!(!should_respawn_controller(1));
        assert!(!should_respawn_controller(2));
        assert!(!should_respawn_controller(3));

        // Three failures in a row: try reviving before the fourth attempt.
        assert!(should_respawn_controller(4));

        // Still dead: keep trying on the same cadence rather than giving up
        // after one go, which would leave Matter down for the whole outage.
        assert!(!should_respawn_controller(5));
        assert!(!should_respawn_controller(6));
        assert!(should_respawn_controller(7));
        assert!(should_respawn_controller(10));
    }

    #[tokio::test]
    async fn a_steady_reading_is_published_once_however_often_the_bridge_resubscribes() {
        // The regression this guards: the rules engine is LEVEL-based, so a
        // republished "motion = true" fires every automation attached to it. A
        // reconnecting controller would do that on every reconnect.
        let bus: Arc<dyn EventBus> =
            Arc::new(pond_core::shared::services::in_process_event_bus::InProcessEventBus::new());
        let mut received = bus.subscribe();
        let mut cache = ReadingCache::new();

        let reading = WireReading {
            device_id: "matter-4".to_string(),
            sensor_type: "occupancy".to_string(),
            value: 1.0,
            unit: "bool".to_string(),
            at: None,
        };

        publish_reading(&reading, &mut cache, &bus);
        publish_reading(&reading, &mut cache, &bus);
        publish_reading(&reading, &mut cache, &bus);

        assert!(
            next_event(&mut received).await.is_some(),
            "first sight must publish"
        );
        assert!(
            next_event(&mut received).await.is_none(),
            "an unchanged reading must not be republished"
        );

        // A real change still gets through, or the cache would silence the sensor.
        let changed = WireReading {
            value: 0.0,
            ..reading.clone()
        };
        publish_reading(&changed, &mut cache, &bus);
        assert!(
            next_event(&mut received).await.is_some(),
            "a changed reading must publish"
        );
    }

    #[tokio::test]
    async fn two_sensors_on_one_device_do_not_shadow_each_other() {
        // Keyed by (device, sensor) rather than device: an air purifier reports
        // several, and a device-keyed cache would let the first one seen
        // suppress all the rest.
        let bus: Arc<dyn EventBus> =
            Arc::new(pond_core::shared::services::in_process_event_bus::InProcessEventBus::new());
        let mut received = bus.subscribe();
        let mut cache = ReadingCache::new();

        let base = WireReading {
            device_id: "matter-7".to_string(),
            sensor_type: "pm2_5".to_string(),
            value: 12.0,
            unit: "ug/m3".to_string(),
            at: None,
        };
        publish_reading(&base, &mut cache, &bus);
        publish_reading(
            &WireReading {
                sensor_type: "carbon_dioxide".to_string(),
                ..base.clone()
            },
            &mut cache,
            &bus,
        );

        assert!(next_event(&mut received).await.is_some());
        assert!(
            next_event(&mut received).await.is_some(),
            "the second sensor was shadowed"
        );
    }
}
