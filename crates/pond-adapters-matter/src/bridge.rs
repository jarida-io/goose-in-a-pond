//! Matter bridge: syncs the fabric into the [`DeviceRegistry`] as `matter-<node_id>` devices and
//! turns `reading` events into [`BusEvent::Sensor`]s; [`run_matter_supervisor`] reconnects it.

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

/// Reconnect backoff bounds; equal jitter keeps Ponds sharing a controller out of lockstep.
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Failures before restarting the controller: past a normal restart's blip (~3.5 s of backoff).
const RESPAWN_AFTER: u32 = 3;

/// Time for a revived controller to listen; shorter than startup's, as `npm ci` is already done.
const RESPAWN_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Last value published per `(device, sensor)`, so a resubscribe can't re-fire level rules.
pub(crate) type ReadingCache = HashMap<(String, String), f64>;

/// Where to reconnect, and what reviving the controller needs.
pub struct SupervisorConfig {
    /// Controller WebSocket URL; only a loopback one is GIAP's to restart.
    pub url: String,
    /// GIAP's data dir — where the controller and its fabric live.
    pub data_dir: PathBuf,
    /// Controller GIAP started, if any; respawns replace it here so teardown kills the live one.
    pub child: SharedServerChild,
    /// Re-request BLE on respawn, or pairing silently stops after the first reconnect.
    pub ble: bool,
}

/// Whether to re-run controller setup before 1-based `attempt`: after every [`RESPAWN_AFTER`]
/// failures, so a controller that stays dead keeps being retried.
fn should_respawn_controller(attempt: u32) -> bool {
    attempt > 1 && (attempt - 1).is_multiple_of(RESPAWN_AFTER)
}

/// Backoff before 1-based `attempt`, with equal jitter from `rand_unit` in [0, 1].
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

/// Heartbeat for visible devices; `is_online` means seen within 5 min, so 4 ticks may be missed.
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
            // The snapshot includes offline devices; a heartbeat would fake 5 min of presence.
            if device.is_online {
                if let Err(e) = registry.heartbeat(&device.id).await {
                    tracing::warn!(device = %device.id, error = %e, "matter: heartbeat failed");
                }
            }
            // Update existing devices' typing, but only on a change: this runs every reconnect.
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

/// Run until the connection drops; `client` must be freshly connected, `events` its stream.
/// Starts with an empty [`ReadingCache`], unlike [`run_matter_bridge_with_cache`].
pub async fn run_matter_bridge(
    client: Arc<MatterClient>,
    events: mpsc::Receiver<MatterEvent>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
    notifier: MatterNotifier,
    // Production passes LIVENESS_TICK; a parameter so tests needn't wait a minute.
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

/// As [`run_matter_bridge`], with a caller-owned dedupe cache that must outlive reconnects.
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
    // `subscribe` returns the whole fabric and also streams later events here.
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

    // Devices the controller sees; it, not the registry, is the authority on presence.
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
    // Consume the immediate first tick (all just synced); `Delay` avoids a burst after a stall.
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
                    // Alert only if GIAP still has it: a user's own delete takes this path too.
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
                    // Repeated every controller tick, so log only changes; at info, because
                    // the tracing filter drops debug outside `pond_server`.
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
                        // Stop vouching; `last_seen` ages out, so no second offline mechanism.
                        present.remove(&device_id);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Run the bridge forever, reconnecting with backoff and swapping each new client into
/// `client_cell` so the control port keeps working. Never returns; spawn it.
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

    // Outlives each bridge run so a reconnect can't republish unchanged readings.
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

        // The next bridge run's `subscribe` resyncs the fabric.
        let mut attempt: u32 = 1;
        let (new_client, new_events) = loop {
            if should_respawn_controller(attempt) {
                // Only now is it more than a normal restart's blip, so only now tell the user.
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
                    // Reused: it is up, so keep backing off. NotLocal: not ours to restart.
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

    /// The next bus event, or `None` if nothing arrives within 100 ms.
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

        // Capped at 30 s, so half is 15 s, even for an attempt that would overflow.
        assert_eq!(reconnect_backoff(20, 0.0), Duration::from_secs(15));
        assert_eq!(reconnect_backoff(u32::MAX, 0.0), Duration::from_secs(15));

        // Full jitter adds up to another half: [15 s, 30 s] once capped.
        let full = reconnect_backoff(20, 1.0);
        assert!(full >= Duration::from_secs(15) && full <= Duration::from_secs(30));
    }

    #[test]
    fn controller_revival_waits_for_repeated_failures_then_keeps_retrying() {
        assert!(!should_respawn_controller(1));
        assert!(!should_respawn_controller(2));
        assert!(!should_respawn_controller(3));

        // Three failures in a row: try reviving before the fourth attempt.
        assert!(should_respawn_controller(4));

        // Still dead: keep reviving on the same cadence.
        assert!(!should_respawn_controller(5));
        assert!(!should_respawn_controller(6));
        assert!(should_respawn_controller(7));
        assert!(should_respawn_controller(10));
    }

    #[tokio::test]
    async fn a_steady_reading_is_published_once_however_often_the_bridge_resubscribes() {
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
