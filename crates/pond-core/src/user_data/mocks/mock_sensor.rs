//! In-memory mock implementations of `SensorStorage` and `CameraStorage`.

use crate::user_data::domain::sensor::{CameraEvent, SensorReading};
use crate::user_data::ports::camera_storage::CameraStorage;
use crate::user_data::ports::sensor_storage::SensorStorage;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct MockSensorStorage {
    readings: Arc<RwLock<Vec<SensorReading>>>,
}

impl MockSensorStorage {
    pub fn new() -> Self {
        Self {
            readings: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for MockSensorStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SensorStorage for MockSensorStorage {
    async fn record(&self, reading: SensorReading) -> Result<()> {
        self.readings.write().await.push(reading);
        Ok(())
    }

    async fn get_latest(
        &self,
        device_id: &str,
        sensor_type: &str,
    ) -> Result<Option<SensorReading>> {
        let readings = self.readings.read().await;
        Ok(readings
            .iter()
            .filter(|r| r.device_id == device_id && r.sensor_type == sensor_type)
            .max_by_key(|r| r.recorded_at)
            .cloned())
    }

    async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>> {
        let readings = self.readings.read().await;
        let mut results: Vec<SensorReading> = readings
            .iter()
            .filter(|r| r.device_id == device_id)
            .cloned()
            .collect();
        results.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        results.truncate(limit);
        Ok(results)
    }

    async fn get_history(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<Vec<SensorReading>> {
        let readings = self.readings.read().await;
        let mut results: Vec<SensorReading> = readings
            .iter()
            .filter(|r| {
                r.device_id == device_id
                    && r.sensor_type == sensor_type
                    && since.map_or(true, |s| r.recorded_at >= s)
                    && until.map_or(true, |u| r.recorded_at < u)
            })
            .cloned()
            .collect();
        results.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        Ok(results)
    }

    async fn list_sensors(&self) -> Result<Vec<(String, String)>> {
        let readings = self.readings.read().await;
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for r in readings.iter() {
            let key = (r.device_id.clone(), r.sensor_type.clone());
            if seen.insert(key.clone()) {
                result.push(key);
            }
        }
        result.sort();
        Ok(result)
    }
}

pub struct MockCameraStorage {
    events: Arc<RwLock<Vec<CameraEvent>>>,
    next_id: Arc<RwLock<i64>>,
}

impl MockCameraStorage {
    pub fn new() -> Self {
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(RwLock::new(1)),
        }
    }
}

impl Default for MockCameraStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CameraStorage for MockCameraStorage {
    async fn record_event(&self, mut event: CameraEvent) -> Result<i64> {
        let mut id = self.next_id.write().await;
        event.id = Some(*id);
        event.created_at = Utc::now();
        self.events.write().await.push(event);
        let assigned = *id;
        *id += 1;
        Ok(assigned)
    }

    async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>> {
        let events = self.events.read().await;
        let mut results: Vec<CameraEvent> = events
            .iter()
            .filter(|e| e.camera_id == camera_id)
            .cloned()
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit);
        Ok(results)
    }

    async fn acknowledge(&self, event_id: i64) -> Result<()> {
        let mut events = self.events.write().await;
        if let Some(e) = events.iter_mut().find(|e| e.id == Some(event_id)) {
            e.acknowledged = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(device_id: &str, sensor_type: &str, value: f64) -> SensorReading {
        SensorReading {
            device_id: device_id.to_string(),
            sensor_type: sensor_type.to_string(),
            value,
            unit: "C".to_string(),
            recorded_at: Utc::now(),
        }
    }

    fn camera_event(camera_id: &str) -> CameraEvent {
        CameraEvent {
            id: None,
            camera_id: camera_id.to_string(),
            event_type: "motion".to_string(),
            confidence: Some(0.9),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn sensor_record_and_get_latest() {
        let storage = MockSensorStorage::new();
        storage
            .record(reading("room1", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("room1", "temperature", 22.5))
            .await
            .unwrap();
        let latest = storage.get_latest("room1", "temperature").await.unwrap();
        assert!(latest.is_some());
    }

    fn reading_at(value: f64, hours: i64) -> SensorReading {
        SensorReading {
            device_id: "room1".to_string(),
            sensor_type: "temperature".to_string(),
            value,
            unit: "C".to_string(),
            recorded_at: DateTime::from_timestamp(1_800_000_000 + hours * 3600, 0).unwrap(),
        }
    }

    async fn seeded(values: &[(f64, i64)]) -> MockSensorStorage {
        let storage = MockSensorStorage::new();
        for (value, hours) in values {
            storage.record(reading_at(*value, *hours)).await.unwrap();
        }
        storage
    }

    // The three tests below exercise the port's default implementations.

    #[tokio::test]
    async fn aggregate_over_empty_window_is_all_none() {
        let storage = seeded(&[]).await;
        let agg = storage
            .aggregate("room1", "temperature", None, None)
            .await
            .unwrap();
        assert_eq!(agg.count, 0);
        assert_eq!(agg.min, None);
        assert_eq!(agg.max, None);
        assert_eq!(agg.avg, None);
        assert_eq!(agg.unit, None);
    }

    #[tokio::test]
    async fn aggregate_matches_manual_fold() {
        let storage = seeded(&[(10.0, 0), (20.0, 1), (30.0, 2)]).await;
        let agg = storage
            .aggregate("room1", "temperature", None, None)
            .await
            .unwrap();
        assert_eq!(agg.count, 3);
        assert_eq!(agg.min, Some(10.0));
        assert_eq!(agg.max, Some(30.0));
        assert_eq!(agg.avg, Some(20.0));
        assert_eq!(agg.unit.as_deref(), Some("C"));
    }

    #[tokio::test]
    async fn get_history_limited_truncates_to_newest() {
        let storage = seeded(&[(10.0, 0), (20.0, 1), (30.0, 2)]).await;
        let rows = storage
            .get_history_limited("room1", "temperature", None, None, 2)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].value, 30.0);
        assert_eq!(rows[1].value, 20.0);
    }

    #[tokio::test]
    async fn camera_record_and_acknowledge() {
        let storage = MockCameraStorage::new();
        let id = storage.record_event(camera_event("front")).await.unwrap();
        assert_eq!(id, 1);
        storage.acknowledge(id).await.unwrap();
        let events = storage.list_events("front", 10).await.unwrap();
        assert!(events[0].acknowledged);
    }
}
