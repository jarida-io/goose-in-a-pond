//! File-backed [`AdminCardStore`]: `<data>/setup/admin.json`, beside the setup card, holding a
//! PBKDF2 verifier of the admin password and never the password itself.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use pond_core::security::domain::admin_card::{AdminPassword, ADMIN_PASSWORD_LEN};
use pond_core::security::ports::admin_card::{AdminCardStore, CardCheck};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::file_setup_code_store::SETUP_DIR;

pub const ADMIN_CARD_FILE: &str = "admin.json";

/// The only record version this build reads.
const ADMIN_CARD_VERSION: u32 = 1;

/// OWASP's figure for PBKDF2-HMAC-SHA256. The password carries 80 random bits, so this is
/// defence in depth for a leaked file, not what keeps a guesser out; the throttle does that.
pub const PBKDF2_ROUNDS: u32 = 600_000;

const SCHEME: &str = "pbkdf2-sha256";

#[derive(Clone, Serialize, Deserialize)]
struct AdminCardRecord {
    version: u32,
    card_id: String,
    /// `pbkdf2-sha256$<rounds>$<salt>$<hash>`, base64 without padding.
    verifier: String,
    created_at: DateTime<Utc>,
}

pub struct FileAdminCardStore {
    dir: PathBuf,
    path: PathBuf,
}

impl FileAdminCardStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let dir = data_dir.into().join(SETUP_DIR);
        let path = dir.join(ADMIN_CARD_FILE);
        Self { dir, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Make a card and return its password, which exists nowhere else afterwards. Refuses to
    /// replace a card in force unless `rotate`, so a re-run never voids a printed card.
    pub fn issue(&self, rotate: bool) -> Result<AdminPassword> {
        self.issue_with_rounds(rotate, PBKDF2_ROUNDS)
    }

    pub fn issue_with_rounds(&self, rotate: bool, rounds: u32) -> Result<AdminPassword> {
        if !rotate && self.read()?.is_some() {
            bail!(
                "{} already holds this pond's admin card; rotate it to make a new one",
                self.path.display()
            );
        }
        let mut rng = rand::rngs::OsRng;
        let mut symbols = [0u8; ADMIN_PASSWORD_LEN];
        for symbol in &mut symbols {
            // 256 is a multiple of 32, so masking draws each symbol evenly.
            *symbol = (rng.next_u32() & 31) as u8;
        }
        let password = AdminPassword::from_symbols(symbols).map_err(|e| anyhow!("{e}"))?;
        let mut salt = [0u8; 16];
        rng.fill_bytes(&mut salt);
        let mut id = [0u8; 8];
        rng.fill_bytes(&mut id);
        let record = AdminCardRecord {
            version: ADMIN_CARD_VERSION,
            card_id: id.iter().map(|b| format!("{b:02x}")).collect(),
            verifier: verifier(&password, &salt, rounds),
            created_at: Utc::now(),
        };
        self.ensure_dir()?;
        self.replace(&record)?;
        Ok(password)
    }

    fn read(&self) -> Result<Option<AdminCardRecord>> {
        let meta = match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
            Ok(meta) => meta,
        };
        if !meta.file_type().is_file() {
            bail!("{} is not a regular file", self.path.display());
        }
        let bytes =
            fs::read(&self.path).with_context(|| format!("reading {}", self.path.display()))?;
        let record: AdminCardRecord = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow!("{} is not an admin card ({e})", self.path.display()))?;
        if record.version != ADMIN_CARD_VERSION {
            bail!(
                "{} is admin card version {}; this build reads {ADMIN_CARD_VERSION}",
                self.path.display(),
                record.version
            );
        }
        Ok(Some(record))
    }

    fn ensure_dir(&self) -> Result<()> {
        match fs::symlink_metadata(&self.dir) {
            Ok(meta) if meta.file_type().is_dir() => return Ok(()),
            Ok(_) => bail!("{} is not a directory", self.dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))
    }

    fn replace(&self, record: &AdminCardRecord) -> Result<()> {
        let tmp = self
            .dir
            .join(format!("{ADMIN_CARD_FILE}.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options
            .open(&tmp)
            .and_then(|mut file| {
                file.write_all(&serde_json::to_vec_pretty(record)?)?;
                file.write_all(b"\n")?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&tmp, &self.path));
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("writing {}", self.path.display()));
        }
        if let Ok(dir) = fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD_NO_PAD
}

fn derive(password: &AdminPassword, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_str().as_bytes(), salt, rounds, &mut out);
    out
}

fn verifier(password: &AdminPassword, salt: &[u8], rounds: u32) -> String {
    let hash = derive(password, salt, rounds);
    format!(
        "{SCHEME}${rounds}${}${}",
        b64().encode(salt),
        b64().encode(hash)
    )
}

fn matches(verifier: &str, password: &AdminPassword) -> Result<bool> {
    let mut parts = verifier.split('$');
    let (Some(SCHEME), Some(rounds), Some(salt), Some(hash), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        bail!("the admin card's verifier is not one this build reads");
    };
    let rounds: u32 = rounds.parse().context("the admin card's rounds")?;
    if rounds == 0 {
        bail!("the admin card's verifier has no rounds");
    }
    let salt = b64().decode(salt).context("the admin card's salt")?;
    let expected = b64().decode(hash).context("the admin card's hash")?;
    let derived = derive(password, &salt, rounds);
    Ok(derived.ct_eq(expected.as_slice()).into())
}

#[async_trait]
impl AdminCardStore for FileAdminCardStore {
    async fn current(&self) -> Result<Option<String>> {
        Ok(self.read()?.map(|record| record.card_id))
    }

    async fn check(&self, password: &AdminPassword) -> Result<CardCheck> {
        let Some(record) = self.read()? else {
            return Ok(CardCheck::Missing);
        };
        let password = password.clone();
        let verifier = record.verifier.clone();
        let matched = tokio::task::spawn_blocking(move || matches(&verifier, &password))
            .await
            .context("checking the admin card")??;
        Ok(if matched {
            CardCheck::Matches {
                card_id: record.card_id,
            }
        } else {
            CardCheck::Mismatch
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: u32 = 10;

    #[tokio::test]
    async fn an_issued_card_checks_and_nothing_else_does() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAdminCardStore::new(tmp.path());
        assert_eq!(store.current().await.unwrap(), None);
        let password = store.issue_with_rounds(false, FAST).unwrap();
        let card_id = store.current().await.unwrap().unwrap();
        assert_eq!(
            store.check(&password).await.unwrap(),
            CardCheck::Matches {
                card_id: card_id.clone()
            }
        );
        let typed = AdminPassword::parse(&password.grouped().to_lowercase()).unwrap();
        assert!(matches!(
            store.check(&typed).await.unwrap(),
            CardCheck::Matches { .. }
        ));
        let other = AdminPassword::parse("0000-0000-0000-0000").unwrap();
        assert_eq!(store.check(&other).await.unwrap(), CardCheck::Mismatch);
    }

    #[tokio::test]
    async fn a_missing_card_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAdminCardStore::new(tmp.path());
        let any = AdminPassword::parse("0000-0000-0000-0000").unwrap();
        assert_eq!(store.check(&any).await.unwrap(), CardCheck::Missing);
    }

    #[test]
    fn the_file_never_holds_the_password_and_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAdminCardStore::new(tmp.path());
        let password = store.issue_with_rounds(false, FAST).unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains(password.as_str()));
        assert!(!text.contains(&password.grouped()));
        assert!(text.contains("pbkdf2-sha256$10$"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            let dir = fs::metadata(tmp.path().join(SETUP_DIR))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!((file, dir), (0o600, 0o700));
        }
        let leftovers = fs::read_dir(tmp.path().join(SETUP_DIR)).unwrap().count();
        assert_eq!(leftovers, 1, "a temporary file was left behind");
    }

    #[tokio::test]
    async fn a_card_is_replaced_only_on_purpose() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAdminCardStore::new(tmp.path());
        let first = store.issue_with_rounds(false, FAST).unwrap();
        let first_id = store.current().await.unwrap();
        assert!(store.issue_with_rounds(false, FAST).is_err());
        assert!(matches!(
            store.check(&first).await.unwrap(),
            CardCheck::Matches { .. }
        ));
        let second = store.issue_with_rounds(true, FAST).unwrap();
        assert_ne!(store.current().await.unwrap(), first_id);
        assert_eq!(store.check(&first).await.unwrap(), CardCheck::Mismatch);
        assert!(matches!(
            store.check(&second).await.unwrap(),
            CardCheck::Matches { .. }
        ));
    }

    #[tokio::test]
    async fn a_damaged_card_is_an_error_not_a_missing_one() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAdminCardStore::new(tmp.path());
        store.issue_with_rounds(false, FAST).unwrap();
        fs::write(store.path(), "{not json").unwrap();
        assert!(store.current().await.is_err());
        let any = AdminPassword::parse("0000-0000-0000-0000").unwrap();
        assert!(store.check(&any).await.is_err());

        let record = serde_json::json!({
            "version": 1, "card_id": "x", "created_at": Utc::now(),
            "verifier": "md5$1$aa$bb",
        });
        fs::write(store.path(), record.to_string()).unwrap();
        assert!(
            store.check(&any).await.is_err(),
            "an unknown scheme never matches by accident"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_planted_symlink_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileAdminCardStore::new(tmp.path());
        fs::create_dir_all(tmp.path().join(SETUP_DIR)).unwrap();
        let elsewhere = tmp.path().join("elsewhere.json");
        fs::write(&elsewhere, "{}").unwrap();
        std::os::unix::fs::symlink(&elsewhere, store.path()).unwrap();
        assert!(store.current().await.is_err());
    }
}
