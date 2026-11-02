//! Opens and migrates `pond_system.db`, `pond_logs.db` and the derived `pond_vectors.db`.

use anyhow::{Context, Result};
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

    /// Every household-personal table in `pond_system.db`, erased by a factory reset. Kept
    /// explicit (not "everything except the keep-list") so a new migration's table must be
    /// placed deliberately — see `every_system_table_is_classified` below, which fails the build
    /// the moment a table is in neither list.
    const SYSTEM_ERASE_TABLES: &[&str] = &[
        "agent_recipes",
        "agent_suggestion_dismissals",
        "consolidation_runs",
        "context_items",
        "context_sources",
        "devices",
        "drafts",
        "engine_session_map",
        "face_embeddings",
        "face_profile_thresholds",
        "guest_accessible_rooms",
        "guest_device_access",
        "handshake_challenges",
        "lane_job_runs",
        "long_task_artifacts",
        "long_task_steps",
        "long_tasks",
        "mcp_servers",
        "memory_edges",
        "memory_events",
        "memory_fragments",
        "mesh_credit_balances",
        "mesh_trusted_peers",
        "mesh_usage_tally",
        "message_attachments",
        "model_role_assignments",
        "notifications",
        "pairing_codes",
        "profile_interviews",
        "profiles",
        // `0074_member_prompt_extras` let a row be one member's own (`profile_id` set), not just
        // the household's -- a factory reset must erase those, so the whole table goes with them
        // rather than splitting household-wide rows out as config.
        "prompt_extras",
        "push_tokens",
        "reminders",
        "rides",
        "rooms",
        "session_messages",
        "session_thinking",
        "session_tokens",
        "session_tool_groups",
        "sessions",
        // A key/value table: deleting every row is exactly "reset to defaults", since
        // `SettingsRepository::get()` already starts from `Settings::default()` and layers
        // whatever rows exist on top.
        "settings",
        "suggestion_queue",
        "user_skills",
    ];

    /// Infrastructure/config a factory reset leaves alone — none of it is household-personal.
    /// `onboarding_state` is reset separately by `OnboardingService::reset()`, not here, so it
    /// isn't erased twice by two different owners of the same table.
    const SYSTEM_KEEP_TABLES: &[&str] = &[
        "agent_prefills",   // compiled prefill cache, rebuildable, no household data
        "models",           // the model catalog, like the `models/` directory on disk
        "onboarding_state", // owned by `OnboardingService::reset()`, not this function
        "prompt_templates", // shipped + user-customized prompt text, treated as config
    ];

    /// Erases every household-personal row across `system` and the derived `vectors` index.
    /// Leaves `logs` (telemetry) and the keep-list above untouched — a factory reset undoes
    /// setup, not installation. `vectors` has no personal/infra split: it is wholly derived from
    /// `system`'s `context_items`/`context_sources`, so it is always erased in full.
    pub async fn wipe_personal_data(&self) -> Result<()> {
        let mut tx = self.system.begin().await?;
        // Lets the deletes below run in any order: FK checks still run, just once at COMMIT, by
        // which point every table either side of a reference is already empty.
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *tx)
            .await?;
        for table in Self::SYSTEM_ERASE_TABLES {
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

        // `vectors/0002_chunks.sql` folded the old `vectors_chunked` table back into `vectors`
        // (one `DROP` + `RENAME`, not two live tables) — one DELETE is the whole index now.
        sqlx::query("DELETE FROM vectors")
            .execute(&self.vectors)
            .await?;

        tracing::info!("factory reset: household data erased");
        Ok(())
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

    /// Guards against a new migration's table landing in neither `SYSTEM_ERASE_TABLES` nor
    /// `SYSTEM_KEEP_TABLES`: silence here would mean a factory reset quietly stops being
    /// complete, not that the table needs no decision.
    #[tokio::test]
    async fn every_system_table_is_classified() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'",
        )
        .fetch_all(&db.system)
        .await
        .unwrap();

        for (table,) in tables {
            assert!(
                Database::SYSTEM_ERASE_TABLES.contains(&table.as_str())
                    || Database::SYSTEM_KEEP_TABLES.contains(&table.as_str()),
                "table '{table}' is in neither the erase-list nor the keep-list -- \
                 a factory reset needs an explicit decision for every table"
            );
        }
    }

    #[tokio::test]
    async fn wipe_personal_data_empties_the_erase_list_and_keeps_the_rest() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        sqlx::query("INSERT INTO settings (key, value) VALUES ('home_name', 'The Kitchen Pond')")
            .execute(&db.system)
            .await
            .unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('p1', 'Owner')")
            .execute(&db.system)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO devices (id, name, device_type, created_at, is_online) \
             VALUES ('d1', 'Phone', 'gotg', datetime('now'), 1)",
        )
        .execute(&db.system)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO prompt_templates (name, content) VALUES ('system.md', 'Be helpful.')",
        )
        .execute(&db.system)
        .await
        .unwrap();

        db.wipe_personal_data().await.unwrap();

        let settings: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM settings")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(settings.0, 0, "settings must be fully cleared, not patched");

        let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(profiles.0, 0);

        let devices: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM devices")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(devices.0, 0);

        let templates: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM prompt_templates")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(templates.0, 1, "kept tables must survive the reset untouched");
    }

    #[tokio::test]
    async fn wipe_personal_data_clears_the_derived_vector_index() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        sqlx::query(
            "INSERT INTO vectors (corpus, row_id, model_id, dims, vector, embedded_at) \
             VALUES ('context', 'c1', 'test-model', 4, X'00000000', datetime('now'))",
        )
        .execute(&db.vectors)
        .await
        .unwrap();

        db.wipe_personal_data().await.unwrap();

        let vectors: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vectors")
            .fetch_one(&db.vectors)
            .await
            .unwrap();
        assert_eq!(vectors.0, 0);
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
