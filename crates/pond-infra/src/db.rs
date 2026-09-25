//! Opens and migrates `pond_system.db`, `pond_logs.db` and the derived `pond_vectors.db`.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;

pub struct Database {
    pub system: Pool<Sqlite>,
    pub logs: Pool<Sqlite>,
    /// Derived personal-context vector index, rebuildable from `system`; a separate file so it
    /// never syncs to a phone. Its connections `ATTACH` `system` as `sys`.
    pub vectors: Pool<Sqlite>,
}

impl Database {
    pub async fn init(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;

        let system_path = data_dir.join("pond_system.db");
        let system = Self::connect(&system_path).await?;
        let logs = Self::connect(&data_dir.join("pond_logs.db")).await?;

        sqlx::migrate!("migrations/system").run(&system).await?;
        sqlx::migrate!("migrations/logs").run(&logs).await?;

        // Must follow the system migrations: vector connections ATTACH it and join its tables.
        let vectors = crate::sqlite_vector_index::SqliteVectorIndex::connect(
            &data_dir.join("pond_vectors.db"),
            &system_path,
        )
        .await?;
        sqlx::migrate!("migrations/vectors").run(&vectors).await?;

        tracing::info!("Databases ready at {}", data_dir.display());
        Ok(Self {
            system,
            logs,
            vectors,
        })
    }

    pub async fn connect(path: &Path) -> Result<Pool<Sqlite>> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", path.display()))?
            .create_if_missing(true)
            // SQLite defaults foreign keys OFF per connection, making ON DELETE CASCADE a no-op.
            .foreign_keys(true)
            .pragma("journal_mode", "WAL")
            .pragma("synchronous", "NORMAL")
            .pragma("cache_size", "2000") // 2000 pages × 4KB = 8MB shared cache
            .pragma("mmap_size", "33554432"); // 32MB mmap — friendly to ARM flash

        Ok(SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[tokio::test]
    async fn database_initializes_all_tables() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let system_tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(&db.system)
                .await
                .unwrap();
        let sys: Vec<&str> = system_tables.iter().map(|r| r.0.as_str()).collect();
        assert!(sys.contains(&"sessions"), "sessions missing");
        assert!(
            sys.contains(&"session_messages"),
            "session_messages missing"
        );
        assert!(
            sys.contains(&"onboarding_state"),
            "onboarding_state missing"
        );
        assert!(sys.contains(&"devices"), "devices missing");
        assert!(sys.contains(&"settings"), "settings missing");

        let log_tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(&db.logs)
                .await
                .unwrap();
        let logs: Vec<&str> = log_tables.iter().map(|r| r.0.as_str()).collect();
        assert!(logs.contains(&"event_log"), "event_log missing");
        assert!(logs.contains(&"system_info"), "system_info missing");

        // Verify WAL journal mode is active on both databases
        let (journal,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal", "system db should use WAL");

        let (journal,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&db.logs)
            .await
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal", "logs db should use WAL");
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let tmp = tempdir().unwrap();
        Database::init(tmp.path()).await.unwrap();
        Database::init(tmp.path()).await.unwrap(); // second run must not fail
    }

    /// Colliding versions appear only after a merge and kill fresh-DB startup with a UNIQUE error.
    #[test]
    fn migration_versions_are_unique_within_each_database() {
        for (name, migrator) in [
            ("system", sqlx::migrate!("migrations/system")),
            ("logs", sqlx::migrate!("migrations/logs")),
            ("vectors", sqlx::migrate!("migrations/vectors")),
        ] {
            let mut seen: HashMap<i64, &str> = HashMap::new();
            for migration in migrator.iter() {
                if let Some(previous) =
                    seen.insert(migration.version, migration.description.as_ref())
                {
                    panic!(
                        "{name} migrations {previous:?} and {:?} both claim version {}; \
                         renumber the one that merged last to the next free version",
                        migration.description, migration.version
                    );
                }
            }
        }
    }
}
