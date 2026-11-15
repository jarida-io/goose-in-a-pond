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

    /// Every household-personal table in `pond_system.db`, erased by a factory reset. A table in
    /// neither this nor the keep-list fails `every_system_table_is_classified` and the wipe.
    const SYSTEM_ERASE_TABLES: &[&str] = &[
        "admin_device_keys",
        "agent_notes",
        "agent_recipes",
        "agent_suggestion_dismissals",
        "cart_items",
        "cart_prices",
        "connector_webhooks",
        "consolidation_runs",
        "context_items",
        "context_sources",
        "devices",
        "drafts",
        "engine_session_map",
        "face_embeddings",
        "face_profile_thresholds",
        "goose_activity",
        "guest_accessible_rooms",
        "guest_device_access",
        "handshake_challenges",
        "hook_events",
        "idea_runs",
        "ideas",
        "interest_interviews",
        "interest_sightings",
        "interests",
        "lane_job_runs",
        "last_device_actions",
        "long_task_artifacts",
        "long_task_steps",
        "long_tasks",
        "mcp_servers",
        "member_interviews",
        "member_places",
        "member_portraits",
        "member_topics",
        "memory_edges",
        "memory_events",
        "memory_fragments",
        "mesh_credit_balances",
        "mesh_trusted_peers",
        "mesh_usage_tally",
        "message_attachments",
        "model_role_assignments",
        // Kept beside `model_role_assignments`, same category: a role's chosen model is a
        // household choice, not shipped config.
        "model_role_history",
        "news_digests",
        "news_settings",
        "news_stories",
        "notification_history",
        "notification_history_reads",
        "notification_history_recipients",
        "notifications",
        "pairing_codes",
        // Its one row is seeded again below, so the old main member and recovery PIN go.
        "pairing_lock",
        "pairing_requests",
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
        "saved_items",
        "screenshot_notes",
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
        "todo_hooks",
        "todos",
        "user_skills",
        // Biometric, same as `face_embeddings`/`face_profile_thresholds` above.
        "voice_prints",
        "voice_wake_takes",
        "web_form_submissions",
        "web_watch_changes",
        "web_watches",
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

    /// What the household did, in `pond_logs.db`: its audit trail, the operational log the Logs
    /// screen shows, its sensors and cameras, and the turns it took.
    const LOGS_ERASE_TABLES: &[&str] = &[
        "camera_events",
        "event_log",
        "events",
        "sensor_readings",
        "turn_metrics",
    ];

    /// What `pond_logs.db` records about the host itself.
    const LOGS_KEEP_TABLES: &[&str] = &["system_info"];

    /// Erases every household-personal row across `system`, `logs` and the derived `vectors`
    /// index, leaving the keep-lists: a factory reset undoes setup, not installation. `system`
    /// goes last, so a failure before it leaves the pond's name for the reset to run again.
    pub async fn wipe_personal_data(&self) -> Result<()> {
        // Refused before anything is erased: a reset that skipped a table would look complete.
        for (name, pool, erase, keep) in [
            (
                "pond_system.db",
                &self.system,
                Self::SYSTEM_ERASE_TABLES,
                Self::SYSTEM_KEEP_TABLES,
            ),
            (
                "pond_logs.db",
                &self.logs,
                Self::LOGS_ERASE_TABLES,
                Self::LOGS_KEEP_TABLES,
            ),
        ] {
            let tables = table_names(pool).await?;
            let unknown = unclassified(&tables, erase, keep);
            if !unknown.is_empty() {
                anyhow::bail!(
                    "{name} has tables a factory reset has no rule for: {}",
                    unknown.join(", ")
                );
            }
        }

        let mut tx = self.logs.begin().await?;
        for table in Self::LOGS_ERASE_TABLES {
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

        // Wholly derived from `system`, so every table goes, whatever a later migration adds.
        let mut tx = self.vectors.begin().await?;
        for table in table_names(&self.vectors).await? {
            sqlx::query(&format!("DELETE FROM \"{}\"", table.replace('"', "\"\"")))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

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
        sqlx::query("INSERT OR IGNORE INTO pairing_lock (id) VALUES (1)")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

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

/// The tables of `pool`'s main schema, leaving out SQLite's own and sqlx's migration record.
async fn table_names(pool: &Pool<Sqlite>) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?)
}

/// The tables in neither list.
fn unclassified<'a>(tables: &'a [String], erase: &[&str], keep: &[&str]) -> Vec<&'a str> {
    tables
        .iter()
        .map(String::as_str)
        .filter(|table| !erase.contains(table) && !keep.contains(table))
        .collect()
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

    /// A new migration's table must land in `SYSTEM_ERASE_TABLES` or `SYSTEM_KEEP_TABLES`, or
    /// every factory reset refuses to run.
    #[tokio::test]
    async fn every_system_table_is_classified() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let tables = table_names(&db.system).await.unwrap();
        let unknown = unclassified(
            &tables,
            Database::SYSTEM_ERASE_TABLES,
            Database::SYSTEM_KEEP_TABLES,
        );
        assert!(
            unknown.is_empty(),
            "{unknown:?} in neither the erase-list nor the keep-list -- \
             a factory reset needs an explicit decision for every table"
        );
    }

    #[tokio::test]
    async fn every_logs_table_is_classified() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let tables = table_names(&db.logs).await.unwrap();
        let unknown = unclassified(
            &tables,
            Database::LOGS_ERASE_TABLES,
            Database::LOGS_KEEP_TABLES,
        );
        assert!(unknown.is_empty(), "{unknown:?} in neither logs list");
    }

    #[tokio::test]
    async fn the_wipe_empties_the_households_logs_and_keeps_the_hosts() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for statement in [
            "INSERT INTO events (timestamp, category, action, privacy_sensitivity) \
             VALUES ('2026-11-01T08:00:00Z', 'auth', 'admin.unlocked', 'sensitive')",
            "INSERT INTO camera_events (camera_id, event_type) VALUES ('front', 'person')",
            "INSERT INTO sensor_readings (device_id, sensor_type, value, unit) \
             VALUES ('bedroom', 'temperature', 21.5, 'C')",
            "INSERT INTO event_log (source, message) VALUES ('giap::trace', 'Amina asked')",
            "INSERT INTO turn_metrics (session_id, turn_number, prompt_tokens, \
             completion_tokens, ttft_ms, total_latency_ms, context_utilization_pct, \
             model_name, timestamp) VALUES ('s1', 1, 10, 10, 5, 5, 1.0, 'm', 'now')",
            "INSERT INTO system_info (key, value) VALUES ('board', 'orin-nano')",
        ] {
            sqlx::query(statement).execute(&db.logs).await.unwrap();
        }

        db.wipe_personal_data().await.unwrap();

        for table in Database::LOGS_ERASE_TABLES {
            let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&db.logs)
                .await
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_info")
            .fetch_one(&db.logs)
            .await
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[tokio::test]
    async fn a_table_with_no_rule_stops_the_wipe_before_it_erases_anything() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('p1', 'Owner')")
            .execute(&db.system)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE household_diary (entry TEXT)")
            .execute(&db.system)
            .await
            .unwrap();

        let error = db.wipe_personal_data().await.unwrap_err();

        assert!(error.to_string().contains("household_diary"), "{error}");
        let profiles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profiles")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(profiles, 1, "nothing is erased when a table has no rule");
    }

    #[tokio::test]
    async fn the_wipe_forgets_the_main_member_and_the_recovery_pin() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('p1', 'Owner')")
            .execute(&db.system)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE pairing_lock SET owner_profile_id = 'p1', locked = 0, \
             pin_verifier = 'scrypt$old', pin_failures = 3 WHERE id = 1",
        )
        .execute(&db.system)
        .await
        .unwrap();

        db.wipe_personal_data().await.unwrap();

        let (owner, locked, pin, failures): (Option<String>, i64, Option<String>, i64) =
            sqlx::query_as(
                "SELECT owner_profile_id, locked, pin_verifier, pin_failures \
                 FROM pairing_lock WHERE id = 1",
            )
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(owner, None);
        assert_eq!(locked, 1, "a fresh install's lock is on");
        assert_eq!(
            pin, None,
            "the previous household's recovery PIN must not survive"
        );
        assert_eq!(failures, 0);
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
        assert_eq!(
            templates.0, 1,
            "kept tables must survive the reset untouched"
        );
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
