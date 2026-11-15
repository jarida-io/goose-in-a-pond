//! Adapter for `PersonalDataWipe`: erases every household-personal store a factory reset must
//! clear: the household's files, the engine's copy of its conversations, stored secrets, and the
//! databases last, so any failure before them leaves the pond's name for the reset to run again.
//! Saved Wi-Fi networks are forgotten after that, best effort.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use pond_core::network::ports::personal_data_wipe::PersonalDataWipe;
use pond_core::network::services::wifi_connections::WifiConnections;
use pond_core::security::ports::secret::SecretRepository;

use crate::db::Database;
use crate::household_files;

pub struct SystemDataWipe {
    db: Arc<Database>,
    data_dir: PathBuf,
    secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
    wifi: Option<Arc<WifiConnections>>,
}

impl SystemDataWipe {
    pub fn new(
        db: Arc<Database>,
        data_dir: impl Into<PathBuf>,
        secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
        wifi: Option<Arc<WifiConnections>>,
    ) -> Self {
        Self {
            db,
            data_dir: data_dir.into(),
            secrets,
            wifi,
        }
    }
}

#[async_trait]
impl PersonalDataWipe for SystemDataWipe {
    async fn wipe(&self) -> Result<()> {
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || household_files::erase(&data_dir)).await??;
        erase_engine_sessions(&self.data_dir).await?;
        if let Some(secrets) = &self.secrets {
            secrets
                .clear_all()
                .await
                .map_err(|error| error.context("clearing stored secrets"))?;
        }
        self.db.wipe_personal_data().await?;

        if let Some(wifi) = &self.wifi {
            if let Err(error) = wifi.forget_all().await {
                tracing::warn!(%error, "factory reset: could not forget every saved Wi-Fi network");
            }
        }

        Ok(())
    }
}

/// Empties the engine's own session store in place: the engine holds it open, so its file stays.
async fn erase_engine_sessions(data_dir: &std::path::Path) -> Result<()> {
    let Some(path) = household_files::beneath(data_dir, household_files::ENGINE_SESSIONS) else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }
    let pool = Database::connect(&path).await?;
    let erased = async {
        // `shadow` tables belong to a virtual table, which is emptied through itself.
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_list \
             WHERE schema = 'main' AND type IN ('table', 'virtual') \
             AND name NOT LIKE 'sqlite_%' AND name != 'schema_version'",
        )
        .fetch_all(&pool)
        .await?;
        let mut tx = pool.begin().await?;
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *tx)
            .await?;
        for table in &tables {
            sqlx::query(&format!("DELETE FROM \"{}\"", table.replace('"', "\"\"")))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        anyhow::Ok(tables.len())
    }
    .await;
    pool.close().await;
    let tables = erased?;
    tracing::info!(tables, "factory reset: the engine's sessions erased");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait as at;
    use pond_core::network::domain::wifi::WifiForgetError;
    use pond_core::network::ports::wifi_station::WifiStation;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::tempdir;

    struct FakeStation {
        forgot_all: Arc<AtomicBool>,
        fail: bool,
    }

    #[at]
    impl WifiStation for FakeStation {
        async fn capabilities(&self) -> pond_core::network::domain::wifi::WifiCapabilities {
            unreachable!()
        }
        async fn scan(
            &self,
        ) -> Result<
            Vec<pond_core::network::domain::wifi::WifiNetwork>,
            pond_core::network::domain::wifi::WifiScanError,
        > {
            unreachable!()
        }
        async fn join(
            &self,
            _: &str,
            _: Option<&pond_core::network::domain::wifi::WifiPassphrase>,
            _: bool,
        ) -> Result<
            pond_core::network::domain::wifi::JoinedNetwork,
            pond_core::network::domain::wifi::WifiJoinError,
        > {
            unreachable!()
        }
        async fn status(&self) -> pond_core::network::domain::wifi::NetworkStatus {
            unreachable!()
        }
        async fn current(&self) -> Option<pond_core::network::domain::wifi::CurrentNetwork> {
            None
        }
        async fn forget(&self, _: &str) -> Result<(), WifiForgetError> {
            unreachable!()
        }
        async fn forget_all(&self) -> Result<(), WifiForgetError> {
            if self.fail {
                return Err(WifiForgetError::Unsupported);
            }
            self.forgot_all.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeSecrets {
        cleared: Arc<AtomicBool>,
        fail: bool,
    }

    #[at]
    impl SecretRepository for FakeSecrets {
        async fn get(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn has(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        async fn clear_all(&self) -> Result<()> {
            if self.fail {
                anyhow::bail!("secrets.json is read-only");
            }
            self.cleared.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn wipe_clears_the_database_secrets_and_wifi() {
        let tmp = tempdir().unwrap();
        let db = Arc::new(Database::init(tmp.path()).await.unwrap());
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('p1', 'Owner')")
            .execute(&db.system)
            .await
            .unwrap();

        let cleared = Arc::new(AtomicBool::new(false));
        let forgotten = Arc::new(AtomicBool::new(false));
        let wipe = SystemDataWipe::new(
            db.clone(),
            tmp.path(),
            Some(Arc::new(FakeSecrets {
                cleared: cleared.clone(),
                fail: false,
            })),
            Some(Arc::new(WifiConnections::new(Arc::new(FakeStation {
                forgot_all: forgotten.clone(),
                fail: false,
            })))),
        );

        wipe.wipe().await.unwrap();

        let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(profiles.0, 0);
        assert!(cleared.load(Ordering::SeqCst));
        assert!(forgotten.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn secrets_that_will_not_clear_fail_the_wipe_before_the_databases() {
        let tmp = tempdir().unwrap();
        let db = Arc::new(Database::init(tmp.path()).await.unwrap());
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('p1', 'Owner')")
            .execute(&db.system)
            .await
            .unwrap();
        let wipe = SystemDataWipe::new(
            db.clone(),
            tmp.path(),
            Some(Arc::new(FakeSecrets {
                cleared: Arc::new(AtomicBool::new(false)),
                fail: true,
            })),
            None,
        );

        let error = wipe.wipe().await.unwrap_err();

        assert!(format!("{error:#}").contains("read-only"), "{error:#}");
        let profiles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profiles")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(
            profiles, 1,
            "the reset can run again: the household is still there"
        );
    }

    #[tokio::test]
    async fn a_failed_wifi_forget_never_fails_the_wipe() {
        let tmp = tempdir().unwrap();
        let db = Arc::new(Database::init(tmp.path()).await.unwrap());
        let wipe = SystemDataWipe::new(
            db,
            tmp.path(),
            None,
            Some(Arc::new(WifiConnections::new(Arc::new(FakeStation {
                forgot_all: Arc::new(AtomicBool::new(false)),
                fail: true,
            })))),
        );

        assert!(wipe.wipe().await.is_ok());
    }

    #[tokio::test]
    async fn missing_secrets_and_wifi_still_wipes_the_database() {
        let tmp = tempdir().unwrap();
        let db = Arc::new(Database::init(tmp.path()).await.unwrap());
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('p1', 'Owner')")
            .execute(&db.system)
            .await
            .unwrap();

        let wipe = SystemDataWipe::new(db.clone(), tmp.path(), None, None);
        wipe.wipe().await.unwrap();

        let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(profiles.0, 0);
    }

    #[tokio::test]
    async fn the_engines_conversations_and_the_households_files_go() {
        let tmp = tempdir().unwrap();
        let db = Arc::new(Database::init(tmp.path()).await.unwrap());
        let sessions_dir = tmp.path().join("engine/data/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let engine = Database::connect(&sessions_dir.join("sessions.db"))
            .await
            .unwrap();
        for statement in [
            "CREATE TABLE schema_version (version INTEGER)",
            "INSERT INTO schema_version VALUES (9)",
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT)",
            "CREATE TABLE messages (session_id TEXT REFERENCES sessions(id), content TEXT)",
            "INSERT INTO sessions VALUES ('s1', 'Dinner plans')",
            "INSERT INTO messages VALUES ('s1', 'my blood test results')",
        ] {
            sqlx::query(statement).execute(&engine).await.unwrap();
        }
        std::fs::create_dir_all(tmp.path().join("attachments/s1")).unwrap();
        std::fs::write(tmp.path().join("attachments/s1/scan.pdf"), b"x").unwrap();

        SystemDataWipe::new(db, tmp.path(), None, None)
            .wipe()
            .await
            .unwrap();

        for (table, left) in [("sessions", 0), ("messages", 0), ("schema_version", 1)] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&engine)
                .await
                .unwrap();
            assert_eq!(count, left, "{table}");
        }
        assert!(!tmp.path().join("attachments/s1").exists());
        assert!(tmp.path().join("pond_system.db").exists());
    }
}
