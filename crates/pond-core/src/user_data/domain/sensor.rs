//! Sensor and camera domain types for the data pipeline.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReading {
    pub device_id: String,
    /// Sensor category, e.g. "temperature", "humidity", "motion", "co2"
    pub sensor_type: String,
    pub value: f64,
    /// Unit string, e.g. "C", "F", "%", "ppm"
    pub unit: String,
    pub recorded_at: DateTime<Utc>,
}

/// Window stats computed by the store; the `Option` fields are `None` exactly when `count == 0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorAggregate {
    pub count: u64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
    /// Fixed per (device_id, sensor_type), so any row in the window gives it.
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraEvent {
    /// Set by the DB on insert; `None` before persisting
    pub id: Option<i64>,
    pub camera_id: String,
    /// Event category, e.g. "motion", "person", "vehicle", "package"
    pub event_type: String,
    pub confidence: Option<f64>,
    pub snapshot_path: Option<String>,
    /// Arbitrary JSON string for extra metadata
    pub metadata: Option<String>,
    pub acknowledged: bool,
    pub created_at: DateTime<Utc>,
}
