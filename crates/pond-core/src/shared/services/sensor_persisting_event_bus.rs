//! [`EventBus`] decorator that records sensor readings to a [`SensorStorage`] before forwarding.
//! Only for publishers that don't persist themselves: `record_sensor` keeps the plain bus,
//! or every POSTed reading would be written twice.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tokio::sync::mpsc;

use crate::shared::ports::event_bus::{BusEvent, BusStream, EventBus};
use crate::user_data::ports::sensor_storage::SensorStorage;

/// Only fills while the database stalls; deep enough to ride out a stall of several seconds.
const DEFAULT_QUEUE_CAPACITY: usize = 1024;

pub struct SensorPersistingEventBus {
    tx: mpsc::Sender<BusEvent>,
    inner: Arc<dyn EventBus>,
    /// Latched while the queue is full, so a sustained stall logs once.
    overloaded: AtomicBool,
}

impl SensorPersistingEventBus {
    pub fn new(inner: Arc<dyn EventBus>, storage: Arc<dyn SensorStorage + Send + Sync>) -> Self {
        Self::with_capacity(inner, storage, DEFAULT_QUEUE_CAPACITY)
    }

    /// Requires a Tokio runtime: spawns the drain itself so no caller can forget to.
    pub fn with_capacity(
        inner: Arc<dyn EventBus>,
        storage: Arc<dyn SensorStorage + Send + Sync>,
        capacity: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel(capacity);

        let drain_inner = inner.clone();
        tokio::spawn(async move {
            // One sequential consumer: order is kept and each record lands before its event.
            while let Some(event) = rx.recv().await {
                if let BusEvent::Sensor(reading) = &event {
                    if let Err(e) = storage.record(reading.clone()).await {
                        tracing::warn!(
                            error = %e,
                            device_id = %reading.device_id,
                            sensor_type = %reading.sensor_type,
                            "failed to persist bus sensor reading; forwarding it anyway"
                        );
                    }
                }
                drain_inner.publish(event);
            }
        });

        Self {
            tx,
            inner,
            overloaded: AtomicBool::new(false),
        }
    }
}

impl EventBus for SensorPersistingEventBus {
    fn publish(&self, event: BusEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {
                self.overloaded.store(false, Ordering::Relaxed);
            }
            // Full or closed: forward unrecorded; a history gap beats a rule that never fires.
            Err(mpsc::error::TrySendError::Full(event))
            | Err(mpsc::error::TrySendError::Closed(event)) => {
                if !self.overloaded.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "sensor persistence queue is not draining; forwarding events \
                         without storing them until it recovers"
                    );
                }
                self.inner.publish(event);
            }
        }
    }

    fn subscribe(&self) -> BusStream {
        self.inner.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::services::in_process_event_bus::InProcessEventBus;
    use crate::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
    use crate::user_data::domain::sensor::SensorReading;
    use crate::user_data::mocks::mock_sensor::MockSensorStorage;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use futures::StreamExt;
    use std::time::Duration;
    use tokio::sync::Notify;

    fn reading(value: f64) -> SensorReading {
        SensorReading {
            device_id: "bedroom".into(),
            sensor_type: "temperature".into(),
            value,
            unit: "C".into(),
            recorded_at: Utc::now(),
        }
    }

    fn make_bus(
        storage: Arc<dyn SensorStorage + Send + Sync>,
        capacity: usize,
    ) -> (SensorPersistingEventBus, Arc<InProcessEventBus>) {
        let inner = Arc::new(InProcessEventBus::new());
        let bus = SensorPersistingEventBus::with_capacity(inner.clone(), storage, capacity);
        (bus, inner)
    }

    #[tokio::test]
    async fn sensor_event_is_recorded_and_forwarded() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage.clone(), 8);
        let mut sub = inner.subscribe();

        bus.publish(BusEvent::Sensor(reading(21.5)));

        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event within timeout")
            .expect("a bus event");
        assert!(matches!(event, BusEvent::Sensor(_)));

        let stored = storage
            .get_latest("bedroom", "temperature")
            .await
            .unwrap()
            .expect("the reading to have been persisted");
        assert_eq!(stored.value, 21.5);
    }

    #[tokio::test]
    async fn record_completes_before_subscribers_see_the_event() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage.clone(), 8);
        let mut sub = inner.subscribe();

        bus.publish(BusEvent::Sensor(reading(19.0)));

        tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event within timeout")
            .expect("a bus event");

        // Deterministic: the drain awaits the record before forwarding.
        assert!(storage
            .get_latest("bedroom", "temperature")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn non_sensor_events_are_forwarded_without_recording() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage.clone(), 8);
        let mut sub = inner.subscribe();

        bus.publish(BusEvent::Device(DeviceStateChanged {
            device_id: "lamp".into(),
            key: "on".into(),
            value: DeviceStateValue::Bool(true),
            changed_at: Utc::now(),
        }));

        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event within timeout")
            .expect("a bus event");
        assert!(matches!(event, BusEvent::Device(_)));
        assert!(storage.list_sensors().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn publish_order_is_preserved() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage, 8);
        let mut sub = inner.subscribe();

        for i in 0..5 {
            bus.publish(BusEvent::Sensor(reading(f64::from(i))));
        }

        for expected in 0..5 {
            let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
                .await
                .expect("event within timeout")
                .expect("a bus event");
            match event {
                BusEvent::Sensor(r) => assert_eq!(r.value, f64::from(expected)),
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    /// Blocks in `record` until released, to wedge the drain and fill the queue.
    struct StallingStorage {
        release: Arc<Notify>,
    }

    #[async_trait]
    impl SensorStorage for StallingStorage {
        async fn record(&self, _reading: SensorReading) -> Result<()> {
            self.release.notified().await;
            Ok(())
        }
        async fn get_latest(&self, _d: &str, _s: &str) -> Result<Option<SensorReading>> {
            Ok(None)
        }
        async fn get_recent(&self, _d: &str, _l: usize) -> Result<Vec<SensorReading>> {
            Ok(vec![])
        }
        async fn get_history(
            &self,
            _d: &str,
            _s: &str,
            _since: Option<DateTime<Utc>>,
            _until: Option<DateTime<Utc>>,
        ) -> Result<Vec<SensorReading>> {
            Ok(vec![])
        }
        async fn list_sensors(&self) -> Result<Vec<(String, String)>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn overflow_forwards_without_persisting() {
        let release = Arc::new(Notify::new());
        let storage = Arc::new(StallingStorage {
            release: release.clone(),
        });
        let (bus, inner) = make_bus(storage, 1);
        let mut sub = inner.subscribe();

        // The wedged drain takes the first, the second fills the queue, the third overflows.
        for i in 0..3 {
            bus.publish(BusEvent::Sensor(reading(f64::from(i))));
        }

        // Overflow guarantees delivery, not order: it jumps the stalled queue.
        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("the overflowed event to be delivered while the drain is stalled")
            .expect("a bus event");
        assert!(matches!(event, BusEvent::Sensor(_)));

        release.notify_waiters();
    }
}
