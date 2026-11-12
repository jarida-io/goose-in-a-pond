//! Adapter for `PersonalDataWipe`: erases every household-personal store a factory reset must
//! clear, by calling the three things that each own a slice of it. `db` is the one erase that
//! must land (it holds everything else the port's doc comment promises — profiles, sessions,
//! memories, devices, rooms, mesh credit/trust state); secrets and Wi-Fi are best-effort, same as
//! every other step `FactoryResetService::execute` takes past its refusal checks.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use pond_core::network::ports::personal_data_wipe::PersonalDataWipe;
use pond_core::network::services::wifi_connections::WifiConnections;
use pond_core::security::ports::secret::SecretRepository;

use crate::db::Database;

pub struct SystemDataWipe {
    db: Arc<Database>,
    secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
    wifi: Option<Arc<WifiConnections>>,
}

impl SystemDataWipe {
    pub fn new(
        db: Arc<Database>,
        secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
        wifi: Option<Arc<WifiConnections>>,
    ) -> Self {
        Self { db, secrets, wifi }
    }
}

#[async_trait]
impl PersonalDataWipe for SystemDataWipe {
    async fn wipe(&self) -> Result<()> {
        self.db.wipe_personal_data().await?;

        if let Some(secrets) = &self.secrets {
            if let Err(error) = secrets.clear_all().await {
                tracing::warn!(%error, "factory reset: could not clear stored secrets");
            }
        }

        if let Some(wifi) = &self.wifi {
            if let Err(error) = wifi.forget_all().await {
                tracing::warn!(%error, "factory reset: could not forget every saved Wi-Fi network");
            }
        }

        Ok(())
    }
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
        ) -> Result<Vec<pond_core::network::domain::wifi::WifiNetwork>, pond_core::network::domain::wifi::WifiScanError>
        {
            unreachable!()
        }
        async fn join(
            &self,
            _: &str,
            _: Option<&pond_core::network::domain::wifi::WifiPassphrase>,
            _: bool,
        ) -> Result<pond_core::network::domain::wifi::JoinedNetwork, pond_core::network::domain::wifi::WifiJoinError>
        {
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
            Some(Arc::new(FakeSecrets {
                cleared: cleared.clone(),
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
    async fn a_failed_wifi_forget_never_fails_the_wipe() {
        let tmp = tempdir().unwrap();
        let db = Arc::new(Database::init(tmp.path()).await.unwrap());
        let wipe = SystemDataWipe::new(
            db,
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

        let wipe = SystemDataWipe::new(db.clone(), None, None);
        wipe.wipe().await.unwrap();

        let profiles: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM profiles")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(profiles.0, 0);
    }
}
