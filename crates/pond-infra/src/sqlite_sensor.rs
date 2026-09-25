//! SQLite `SensorStorage` and `CameraStorage`. Both tables are in `pond_logs.db`: pass `db.logs`.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::sensor::{CameraEvent, SensorAggregate, SensorReading};
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::sensor_storage::SensorStorage;
use sqlx::{Pool, Sqlite};

// ─────────────────────────────────────────────────────────────────────────────
// SensorStorage
// ─────────────────────────────────────────────────────────────────────────────

pub struct SqliteSensorStorage {
    pool: Pool<Sqlite>,
}

impl SqliteSensorStorage {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct SensorRow {
    device_id: String,
    sensor_type: String,
    value: f64,
    unit: String,
    created_at: String,
}

/// Formats a bound like stored `created_at`: the column is TEXT and compared as text.
fn format_bound(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Unreadable timestamps parse as the epoch, not now, so staleness checks see them as stale.
fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or(DateTime::UNIX_EPOCH)
}

fn sensor_row_to_reading(row: SensorRow) -> SensorReading {
    SensorReading {
        device_id: row.device_id,
        sensor_type: row.sensor_type,
        value: row.value,
        unit: row.unit,
        recorded_at: parse_dt(&row.created_at),
    }
}

#[async_trait]
impl SensorStorage for SqliteSensorStorage {
    async fn record(&self, reading: SensorReading) -> Result<()> {
        sqlx::query(
            // The reading's own time, not `datetime('now')`, in `format_bound`'s TEXT format;
            // byte-compatible with `datetime('now')`, so older rows still order correctly.
            "INSERT INTO sensor_readings (device_id, sensor_type, value, unit, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&reading.device_id)
        .bind(&reading.sensor_type)
        .bind(reading.value)
        .bind(&reading.unit)
        .bind(format_bound(reading.recorded_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_latest(
        &self,
        device_id: &str,
        sensor_type: &str,
    ) -> Result<Option<SensorReading>> {
        let row: Option<SensorRow> = sqlx::query_as(
            "SELECT device_id, sensor_type, value, unit, created_at \
             FROM sensor_readings \
             WHERE device_id = ? AND sensor_type = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(device_id)
        .bind(sensor_type)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(sensor_row_to_reading))
    }

    async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>> {
        let rows: Vec<SensorRow> = sqlx::query_as(
            "SELECT device_id, sensor_type, value, unit, created_at \
             FROM sensor_readings \
             WHERE device_id = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT ?",
        )
        .bind(device_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(sensor_row_to_reading).collect())
    }

    async fn get_history(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<Vec<SensorReading>> {
        let since_str = since.map(format_bound);
        let until_str = until.map(format_bound);
        let rows: Vec<SensorRow> = sqlx::query_as(
            "SELECT device_id, sensor_type, value, unit, created_at \
             FROM sensor_readings \
             WHERE device_id = ? AND sensor_type = ? \
               AND (? IS NULL OR created_at >= ?) \
               AND (? IS NULL OR created_at < ?) \
             ORDER BY created_at DESC, rowid DESC",
        )
        .bind(device_id)
        .bind(sensor_type)
        .bind(&since_str)
        .bind(&since_str)
        .bind(&until_str)
        .bind(&until_str)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(sensor_row_to_reading).collect())
    }

    async fn get_history_limited(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<SensorReading>> {
        let since_str = since.map(format_bound);
        let until_str = until.map(format_bound);
        let rows: Vec<SensorRow> = sqlx::query_as(
            "SELECT device_id, sensor_type, value, unit, created_at \
             FROM sensor_readings \
             WHERE device_id = ? AND sensor_type = ? \
               AND (? IS NULL OR created_at >= ?) \
               AND (? IS NULL OR created_at < ?) \
             ORDER BY created_at DESC, rowid DESC LIMIT ?",
        )
        .bind(device_id)
        .bind(sensor_type)
        .bind(&since_str)
        .bind(&since_str)
        .bind(&until_str)
        .bind(&until_str)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(sensor_row_to_reading).collect())
    }

    async fn aggregate(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<SensorAggregate> {
        #[derive(sqlx::FromRow)]
        struct AggregateRow {
            count: i64,
            min_value: Option<f64>,
            max_value: Option<f64>,
            avg_value: Option<f64>,
            unit: Option<String>,
        }
        let since_str = since.map(format_bound);
        let until_str = until.map(format_bound);
        // MAX(unit) just picks one (unit is fixed per device/type); an empty window is all NULL.
        let row: AggregateRow = sqlx::query_as(
            "SELECT COUNT(value) AS count, \
                    MIN(value)   AS min_value, \
                    MAX(value)   AS max_value, \
                    AVG(value)   AS avg_value, \
                    MAX(unit)    AS unit \
             FROM sensor_readings \
             WHERE device_id = ? AND sensor_type = ? \
               AND (? IS NULL OR created_at >= ?) \
               AND (? IS NULL OR created_at < ?)",
        )
        .bind(device_id)
        .bind(sensor_type)
        .bind(&since_str)
        .bind(&since_str)
        .bind(&until_str)
        .bind(&until_str)
        .fetch_one(&self.pool)
        .await?;
        Ok(SensorAggregate {
            count: row.count.max(0) as u64,
            min: row.min_value,
            max: row.max_value,
            avg: row.avg_value,
            unit: row.unit,
        })
    }

    async fn list_sensors(&self) -> Result<Vec<(String, String)>> {
        #[derive(sqlx::FromRow)]
        struct PairRow {
            device_id: String,
            sensor_type: String,
        }
        let rows: Vec<PairRow> = sqlx::query_as(
            "SELECT DISTINCT device_id, sensor_type FROM sensor_readings \
             ORDER BY device_id, sensor_type",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.device_id, r.sensor_type))
            .collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CameraStorage
// ─────────────────────────────────────────────────────────────────────────────

pub struct SqliteCameraStorage {
    pool: Pool<Sqlite>,
}

impl SqliteCameraStorage {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct CameraRow {
    id: i64,
    camera_id: String,
    event_type: String,
    confidence: Option<f64>,
    snapshot_path: Option<String>,
    metadata: Option<String>,
    acknowledged: i64,
    created_at: String,
}

fn camera_row_to_event(row: CameraRow) -> CameraEvent {
    CameraEvent {
        id: Some(row.id),
        camera_id: row.camera_id,
        event_type: row.event_type,
        confidence: row.confidence,
        snapshot_path: row.snapshot_path,
        metadata: row.metadata,
        acknowledged: row.acknowledged != 0,
        created_at: parse_dt(&row.created_at),
    }
}

#[async_trait]
impl CameraStorage for SqliteCameraStorage {
    async fn record_event(&self, event: CameraEvent) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO camera_events \
             (camera_id, event_type, confidence, snapshot_path, metadata, acknowledged, created_at) \
             VALUES (?, ?, ?, ?, ?, 0, datetime('now'))",
        )
        .bind(&event.camera_id)
        .bind(&event.event_type)
        .bind(event.confidence)
        .bind(&event.snapshot_path)
        .bind(&event.metadata)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>> {
        let rows: Vec<CameraRow> = sqlx::query_as(
            "SELECT id, camera_id, event_type, confidence, snapshot_path, metadata, \
             acknowledged, created_at \
             FROM camera_events WHERE camera_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(camera_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(camera_row_to_event).collect())
    }

    async fn acknowledge(&self, event_id: i64) -> Result<()> {
        sqlx::query("UPDATE camera_events SET acknowledged = 1 WHERE id = ?")
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::user_data::domain::sensor::{CameraEvent, SensorReading};
    use tempfile::tempdir;

    async fn make_logs_pool() -> (Pool<Sqlite>, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (db.logs, tmp)
    }

    fn reading(device_id: &str, t: &str, v: f64) -> SensorReading {
        SensorReading {
            device_id: device_id.to_string(),
            sensor_type: t.to_string(),
            value: v,
            unit: "C".to_string(),
            recorded_at: Utc::now(),
        }
    }

    /// Seeds a reading at a chosen time, bypassing the port.
    async fn insert_at(pool: &Pool<Sqlite>, device_id: &str, t: &str, v: f64, created_at: &str) {
        sqlx::query(
            "INSERT INTO sensor_readings (device_id, sensor_type, value, unit, created_at) \
             VALUES (?, ?, ?, 'C', ?)",
        )
        .bind(device_id)
        .bind(t)
        .bind(v)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    fn at(s: &str) -> DateTime<Utc> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc()
    }

    fn cam_event(camera_id: &str) -> CameraEvent {
        CameraEvent {
            id: None,
            camera_id: camera_id.to_string(),
            event_type: "motion".to_string(),
            confidence: Some(0.95),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn a_readings_own_timestamp_survives_the_round_trip() {
        let (pool, _tmp) = make_logs_pool().await;
        let store = SqliteSensorStorage::new(pool);

        let when = at("2026-08-30 14:05:09");
        store
            .record(SensorReading {
                device_id: "matter-5".to_string(),
                sensor_type: "flow".to_string(),
                value: 197.8,
                unit: "m3/h".to_string(),
                recorded_at: when,
            })
            .await
            .unwrap();

        let stored = store.get_latest("matter-5", "flow").await.unwrap().unwrap();
        assert_eq!(stored.recorded_at, when);
    }

    #[test]
    fn a_timestamp_nobody_can_read_is_ancient_rather_than_now() {
        assert_eq!(parse_dt("not a timestamp"), DateTime::UNIX_EPOCH);
        assert_eq!(parse_dt(""), DateTime::UNIX_EPOCH);
        assert_eq!(parse_dt("2026-08-30 14:05:09"), at("2026-08-30 14:05:09"));
    }

    #[tokio::test]
    async fn sensor_record_and_get_latest() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteSensorStorage::new(pool);
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
        assert_eq!(latest.unwrap().value, 22.5);
    }

    #[tokio::test]
    async fn sensor_get_recent_respects_limit() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteSensorStorage::new(pool);
        for i in 0..5 {
            storage
                .record(reading("dev1", "humidity", i as f64))
                .await
                .unwrap();
        }
        let recent = storage.get_recent("dev1", 3).await.unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[tokio::test]
    async fn sensor_get_history_honours_since_and_until() {
        let (pool, _tmp) = make_logs_pool().await;
        for (v, ts) in [
            (1.0, "2026-01-01 10:00:00"),
            (2.0, "2026-01-01 11:00:00"),
            (3.0, "2026-01-01 12:00:00"),
        ] {
            insert_at(&pool, "room1", "temperature", v, ts).await;
        }
        let storage = SqliteSensorStorage::new(pool);

        // `since` is inclusive and `until` exclusive: only the 11:00 reading.
        let rows = storage
            .get_history(
                "room1",
                "temperature",
                Some(at("2026-01-01 11:00:00")),
                Some(at("2026-01-01 12:00:00")),
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, 2.0);
    }

    #[tokio::test]
    async fn sensor_get_history_limited_caps_rows_newest_first() {
        let (pool, _tmp) = make_logs_pool().await;
        for (v, ts) in [
            (1.0, "2026-01-01 10:00:00"),
            (2.0, "2026-01-01 11:00:00"),
            (3.0, "2026-01-01 12:00:00"),
            (4.0, "2026-01-01 13:00:00"),
            (5.0, "2026-01-01 14:00:00"),
        ] {
            insert_at(&pool, "room1", "temperature", v, ts).await;
        }
        let storage = SqliteSensorStorage::new(pool);

        let rows = storage
            .get_history_limited("room1", "temperature", None, None, 2)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].value, 5.0);
        assert_eq!(rows[1].value, 4.0);
    }

    #[tokio::test]
    async fn sensor_aggregate_matches_seeded_values() {
        let (pool, _tmp) = make_logs_pool().await;
        for (v, ts) in [
            (10.0, "2026-01-01 10:00:00"),
            (20.0, "2026-01-01 11:00:00"),
            (30.0, "2026-01-01 12:00:00"),
        ] {
            insert_at(&pool, "room1", "temperature", v, ts).await;
        }
        let storage = SqliteSensorStorage::new(pool);

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
    async fn sensor_aggregate_over_empty_window_is_none() {
        let (pool, _tmp) = make_logs_pool().await;
        insert_at(&pool, "room1", "temperature", 10.0, "2026-01-01 10:00:00").await;
        let storage = SqliteSensorStorage::new(pool);

        let agg = storage
            .aggregate(
                "room1",
                "temperature",
                Some(at("2026-02-01 00:00:00")),
                None,
            )
            .await
            .unwrap();
        assert_eq!(agg.count, 0);
        assert_eq!(agg.min, None);
        assert_eq!(agg.max, None);
        assert_eq!(agg.avg, None);
        assert_eq!(agg.unit, None);
    }

    #[tokio::test]
    async fn sensor_aggregate_respects_time_bounds() {
        let (pool, _tmp) = make_logs_pool().await;
        for (v, ts) in [
            (5.0, "2026-01-01 10:00:00"),
            (15.0, "2026-01-02 10:00:00"),
            (25.0, "2026-01-02 12:00:00"),
        ] {
            insert_at(&pool, "room1", "temperature", v, ts).await;
        }
        let storage = SqliteSensorStorage::new(pool);

        let agg = storage
            .aggregate(
                "room1",
                "temperature",
                Some(at("2026-01-02 00:00:00")),
                None,
            )
            .await
            .unwrap();
        assert_eq!(agg.count, 2);
        assert_eq!(agg.min, Some(15.0));
        assert_eq!(agg.max, Some(25.0));
        assert_eq!(agg.avg, Some(20.0));
    }

    #[tokio::test]
    async fn camera_record_and_acknowledge() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteCameraStorage::new(pool);
        let id = storage.record_event(cam_event("front")).await.unwrap();
        assert_eq!(id, 1);
        storage.acknowledge(id).await.unwrap();
        let events = storage.list_events("front", 10).await.unwrap();
        assert!(events[0].acknowledged);
    }

    #[tokio::test]
    async fn camera_list_respects_camera_id_filter() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteCameraStorage::new(pool);
        storage.record_event(cam_event("front")).await.unwrap();
        storage.record_event(cam_event("back")).await.unwrap();
        let front_events = storage.list_events("front", 10).await.unwrap();
        assert_eq!(front_events.len(), 1);
    }

    #[tokio::test]
    async fn sensor_get_history_no_bounds() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteSensorStorage::new(pool);
        for v in [21.0_f64, 22.5, 23.0] {
            storage
                .record(reading("room1", "temperature", v))
                .await
                .unwrap();
        }
        let history = storage
            .get_history("room1", "temperature", None, None)
            .await
            .unwrap();
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    async fn sensor_list_sensors_returns_distinct_pairs() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteSensorStorage::new(pool);
        storage
            .record(reading("bedroom", "temperature", 20.0))
            .await
            .unwrap();
        storage
            .record(reading("bedroom", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("kitchen", "humidity", 55.0))
            .await
            .unwrap();
        let pairs = storage.list_sensors().await.unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&("bedroom".to_string(), "temperature".to_string())));
        assert!(pairs.contains(&("kitchen".to_string(), "humidity".to_string())));
    }
}
