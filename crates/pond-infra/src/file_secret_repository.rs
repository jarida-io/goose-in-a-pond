use crate::secret_crypto::{self, SecretStoreLocked};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chacha20poly1305::Key;
use pond_core::security::ports::secret::SecretRepository;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

/// Encrypted file-based secret store at `$DATA_DIR/secrets.json`; env vars override it.
///
/// Protects the file only, not a running pond; see the threat model in [`crate::secret_crypto`].
pub struct FileSecretRepository {
    path: PathBuf,
    key: Key,
    cache: RwLock<HashMap<String, String>>,
}

impl FileSecretRepository {
    /// Open, creating the key eagerly on first run. A missing/wrong key is [`SecretStoreLocked`]
    /// and unparsable plaintext is an error, never an empty store (the next write would lose
    /// data). Legacy plaintext migrates with the key fsynced before any ciphertext is written.
    pub fn new(data_dir: &std::path::Path) -> Result<Self> {
        let path = data_dir.join("secrets.json");
        let key_path = secret_crypto::key_path(data_dir);

        let raw = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };

        match raw.as_deref().map(str::trim) {
            None | Some("") => {
                let key = secret_crypto::create_key_if_absent(&key_path)?;
                Ok(Self {
                    path,
                    key,
                    cache: RwLock::new(HashMap::new()),
                })
            }
            Some(text) if secret_crypto::looks_encrypted(text) => {
                let key = match secret_crypto::load_key(&key_path) {
                    Ok(Some(k)) => k,
                    Ok(None) => {
                        return Err(SecretStoreLocked {
                            store_path: path,
                            key_path,
                            reason: "the key file does not exist".to_string(),
                        }
                        .into())
                    }
                    Err(e) => {
                        return Err(SecretStoreLocked {
                            store_path: path,
                            key_path,
                            reason: format!("{e:#}"),
                        }
                        .into())
                    }
                };
                let json = match secret_crypto::decrypt(&key, text) {
                    Ok(j) => j,
                    Err(e) => {
                        return Err(SecretStoreLocked {
                            store_path: path,
                            key_path,
                            reason: format!("{e:#}"),
                        }
                        .into())
                    }
                };
                let cache: HashMap<String, String> = serde_json::from_str(&json)
                    .context("decrypted secret store is not a JSON object of strings")?;
                Ok(Self {
                    path,
                    key,
                    cache: RwLock::new(cache),
                })
            }
            Some(text) => {
                let cache: HashMap<String, String> =
                    serde_json::from_str(text).with_context(|| {
                        format!(
                            "{} is neither an encrypted store nor a readable plaintext one. \
                             Refusing to open it as an empty store, because the next write \
                             would overwrite whatever is in there. Move it aside by hand to \
                             start over.",
                            path.display()
                        )
                    })?;
                let key = secret_crypto::create_key_if_absent(&key_path)?;
                Self::write_store(&path, &key, &cache)?;
                tracing::info!(
                    keys = cache.len(),
                    store = %path.display(),
                    key_file = %key_path.display(),
                    "migrated the plaintext secret store to an encrypted one"
                );
                Ok(Self {
                    path,
                    key,
                    cache: RwLock::new(cache),
                })
            }
        }
    }

    /// Encrypt and atomically replace the store. Sync on purpose: `new` calls it outside async.
    fn write_store(
        path: &std::path::Path,
        key: &Key,
        cache: &HashMap<String, String>,
    ) -> Result<()> {
        let json = serde_json::to_string(cache)?;
        let envelope = secret_crypto::encrypt(key, &json)?;
        secret_crypto::write_private(path, envelope.as_bytes())
    }

    async fn persist(&self) -> Result<()> {
        let cache = self.cache.read().await;
        Self::write_store(&self.path, &self.key, &cache)
    }
}

/// Hand-written so `{:?}` (or a test's `expect_err`) never prints the key or secret values.
impl std::fmt::Debug for FileSecretRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSecretRepository")
            .field("path", &self.path)
            .field("key", &"<redacted>")
            .field("cache", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl SecretRepository for FileSecretRepository {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        if let Ok(val) = std::env::var(key) {
            return Ok(Some(val));
        }
        let cache = self.cache.read().await;
        Ok(cache.get(key).cloned())
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        {
            let mut cache = self.cache.write().await;
            cache.insert(key.to_string(), value.to_string());
        }
        self.persist().await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        {
            let mut cache = self.cache.write().await;
            cache.remove(key);
        }
        self.persist().await
    }

    /// File store only (no env): [`crate::secret_migration`] relies on that to verify copies.
    async fn list_keys(&self) -> Result<Vec<String>> {
        let cache = self.cache.read().await;
        Ok(cache.keys().cloned().collect())
    }

    async fn has(&self, key: &str) -> Result<bool> {
        if std::env::var(key).is_ok() {
            return Ok(true);
        }
        let cache = self.cache.read().await;
        Ok(cache.contains_key(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_delete_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();

        assert!(repo.list_keys().await.unwrap().is_empty());
        assert!(!repo.has("MY_KEY").await.unwrap());
        assert!(repo.get("MY_KEY").await.unwrap().is_none());

        repo.set("MY_KEY", "secret_value").await.unwrap();
        assert!(repo.has("MY_KEY").await.unwrap());
        assert_eq!(
            repo.get("MY_KEY").await.unwrap().as_deref(),
            Some("secret_value")
        );
        assert_eq!(repo.list_keys().await.unwrap(), vec!["MY_KEY".to_string()]);

        repo.delete("MY_KEY").await.unwrap();
        assert!(!repo.has("MY_KEY").await.unwrap());
        assert!(repo.get("MY_KEY").await.unwrap().is_none());
        assert!(repo.list_keys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn overwrite_existing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();

        repo.set("TOKEN", "old").await.unwrap();
        repo.set("TOKEN", "new").await.unwrap();
        assert_eq!(repo.get("TOKEN").await.unwrap().as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn persists_to_disk() {
        let tmp = tempfile::tempdir().unwrap();

        {
            let repo = FileSecretRepository::new(tmp.path()).unwrap();
            repo.set("PERSIST_KEY", "persist_val").await.unwrap();
        }

        let repo2 = FileSecretRepository::new(tmp.path()).unwrap();
        assert_eq!(
            repo2.get("PERSIST_KEY").await.unwrap().as_deref(),
            Some("persist_val")
        );
    }

    #[tokio::test]
    async fn delete_nonexistent_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();
        repo.delete("DOES_NOT_EXIST").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();
        repo.set("KEY", "val").await.unwrap();

        let meta = std::fs::metadata(tmp.path().join("secrets.json")).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets.json should be owner-only rw");
    }

    // ── Encryption at rest ──────────────────────────────────────────────────
    // Secret names are distinctive because `get` reads the process environment first.

    #[tokio::test]
    async fn the_stored_value_is_not_in_the_file_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();
        repo.set("GIAP_TEST_CANARY", "plaintext-canary-value")
            .await
            .unwrap();

        let raw = std::fs::read(tmp.path().join("secrets.json")).unwrap();
        // Guard the vacuous pass: an absent or empty file trivially lacks the value.
        assert!(!raw.is_empty(), "secrets.json was never written");
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(
            crate::secret_crypto::looks_encrypted(&text),
            "secrets.json is not an envelope: {text}"
        );
        let needle = b"plaintext-canary-value";
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "the secret value is on disk in the clear"
        );
    }

    #[tokio::test]
    async fn a_legacy_plaintext_store_is_migrated_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("secrets.json");
        std::fs::write(&store, r#"{"GIAP_TEST_LEGACY":"legacy-value"}"#).unwrap();

        let repo = FileSecretRepository::new(tmp.path()).unwrap();
        assert_eq!(
            repo.get("GIAP_TEST_LEGACY").await.unwrap().as_deref(),
            Some("legacy-value"),
            "migration lost the value"
        );

        let raw = std::fs::read_to_string(&store).unwrap();
        assert!(
            crate::secret_crypto::looks_encrypted(&raw),
            "the store was left in plaintext: {raw}"
        );
        assert!(
            !raw.contains("legacy-value"),
            "the plaintext value survived the migration"
        );
        assert!(
            tmp.path().join("secrets").join("master.key").exists(),
            "migrated to ciphertext without writing a key"
        );
    }

    /// A directory at `secrets.json.tmp` makes `create_new` fail (a stand-in for ENOSPC/EIO),
    /// which makes the key-before-ciphertext ordering observable.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_key_is_durable_before_any_ciphertext_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("secrets.json");
        let key_path = tmp.path().join("secrets").join("master.key");
        let legacy = r#"{"GIAP_TEST_ORDER":"order-value"}"#;
        std::fs::write(&store, legacy).unwrap();

        std::fs::create_dir(tmp.path().join("secrets.json.tmp")).unwrap();

        let err = FileSecretRepository::new(tmp.path())
            .expect_err("the migration must fail when it cannot write the ciphertext");
        // Not `SecretStoreLocked`: that would send the operator hunting for a key that exists.
        assert!(
            err.downcast_ref::<SecretStoreLocked>().is_none(),
            "a failed write was reported as a locked store: {err:#}"
        );

        assert!(
            key_path.exists(),
            "the key was not durable before the ciphertext write was attempted -- \
             an interruption here would have produced an envelope with no key"
        );
        assert!(
            crate::secret_crypto::load_key(&key_path).unwrap().is_some(),
            "the key file exists but does not load, so it is not usable for a retry"
        );
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            legacy,
            "the plaintext store was damaged by a migration that did not complete"
        );
    }

    #[tokio::test]
    async fn an_unparseable_plaintext_store_is_refused_not_emptied() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("secrets.json");
        std::fs::write(&store, "this is not json").unwrap();

        assert!(
            FileSecretRepository::new(tmp.path()).is_err(),
            "a corrupt store must not open as an empty one -- the next set() would overwrite it"
        );
        assert_eq!(
            std::fs::read_to_string(&store).unwrap(),
            "this is not json",
            "the unreadable store was modified"
        );
    }

    #[tokio::test]
    async fn a_missing_key_locks_the_store_and_leaves_the_ciphertext_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("secrets.json");
        let key = tmp.path().join("secrets").join("master.key");

        {
            let repo = FileSecretRepository::new(tmp.path()).unwrap();
            repo.set("GIAP_TEST_GONE", "value-behind-a-lost-key")
                .await
                .unwrap();
        }
        let before = std::fs::read(&store).unwrap();
        std::fs::remove_file(&key).unwrap();

        let err = FileSecretRepository::new(tmp.path())
            .expect_err("a store whose key is gone must not open");
        assert!(
            err.downcast_ref::<SecretStoreLocked>().is_some(),
            "expected SecretStoreLocked, got: {err:#}"
        );
        assert_eq!(
            std::fs::read(&store).unwrap(),
            before,
            "the ciphertext was modified while the store was locked"
        );
    }

    #[tokio::test]
    async fn a_wrong_key_locks_rather_than_silently_reinitialising() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let repo = FileSecretRepository::new(tmp.path()).unwrap();
            repo.set("GIAP_TEST_WRONGKEY", "v").await.unwrap();
        }

        // A well-formed key that is simply not this store's key.
        let other = tempfile::tempdir().unwrap();
        let _ = FileSecretRepository::new(other.path()).unwrap();
        let wrong =
            std::fs::read_to_string(other.path().join("secrets").join("master.key")).unwrap();
        std::fs::write(tmp.path().join("secrets").join("master.key"), wrong).unwrap();

        let err =
            FileSecretRepository::new(tmp.path()).expect_err("a wrong key must not open the store");
        assert!(
            err.downcast_ref::<SecretStoreLocked>().is_some(),
            "expected SecretStoreLocked, got: {err:#}"
        );
    }
}
