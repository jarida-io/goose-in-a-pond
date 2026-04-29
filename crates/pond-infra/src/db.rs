//! Database initialization for Goose In A Pond
//!
//! GIAP uses two SQLite databases:
//! - `pond_system.db` — Sessions, devices, onboarding, settings
//! - `pond_logs.db`   — Event log, telemetry, system info
//!
//! Migrations live in `migrations/system/` and `migrations/logs/` and are
//! applied automatically on startup via `sqlx::migrate!()`.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;

pub struct Database {
    pub system: Pool<Sqlite>,
    pub logs:   Pool<Sqlite>,
}

impl Database {
    pub async fn init(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;

        let system = Self::connect(&data_dir.join("pond_system.db")).await?;
        let logs   = Self::connect(&data_dir.join("pond_logs.db")).await?;

        sqlx::migrate!("migrations/system").run(&system).await?;
        sqlx::migrate!("migrations/logs").run(&logs).await?;

        tracing::info!("Databases ready at {}", data_dir.display());
        Ok(Self { system, logs })
    }

    pub async fn connect(path: &Path) -> Result<Pool<Sqlite>> {
        let opts = SqliteConnectOptions::from_str(
            &format!("sqlite:{}?mode=rwc", path.display()),
        )?
        .create_if_missing(true)
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "NORMAL")
        .pragma("cache_size", "2000")       // 2000 pages × 4KB = 8MB shared cache
        .pragma("mmap_size", "33554432");   // 32MB mmap — friendly to ARM flash

        Ok(SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn database_initializes_all_tables() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let system_tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
        .fetch_all(&db.system)
        .await
        .unwrap();
        let sys: Vec<&str> = system_tables.iter().map(|r| r.0.as_str()).collect();
        assert!(sys.contains(&"sessions"),         "sessions missing");
        assert!(sys.contains(&"session_messages"), "session_messages missing");
        assert!(sys.contains(&"onboarding_state"), "onboarding_state missing");
        assert!(sys.contains(&"devices"),          "devices missing");
        assert!(sys.contains(&"settings"),         "settings missing");

        let log_tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
        .fetch_all(&db.logs)
        .await
        .unwrap();
        let logs: Vec<&str> = log_tables.iter().map(|r| r.0.as_str()).collect();
        assert!(logs.contains(&"event_log"),   "event_log missing");
        assert!(logs.contains(&"system_info"), "system_info missing");

        // Verify WAL journal mode is active on both databases
        let (journal,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&db.system).await.unwrap();
        assert_eq!(journal.to_lowercase(), "wal", "system db should use WAL");

        let (journal,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&db.logs).await.unwrap();
        assert_eq!(journal.to_lowercase(), "wal", "logs db should use WAL");
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let tmp = tempdir().unwrap();
        Database::init(tmp.path()).await.unwrap();
        Database::init(tmp.path()).await.unwrap(); // second run must not fail
    }
}
