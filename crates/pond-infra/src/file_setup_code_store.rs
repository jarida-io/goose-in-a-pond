//! The printed card's setup code, kept out of `Settings` (PAI-2) and the database because the card
//! belongs to the hardware: the file is never regenerated behind the operator's back.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::network::domain::discriminator::Discriminator;
use pond_core::network::domain::setup_code::{SetupCode, SETUP_CODE_RANDOM_DIGITS};
use pond_core::network::domain::setup_record::{SetupRecord, SETUP_RECORD_VERSION};
use pond_core::network::ports::setup_code_store::SetupCodeStore;
use rand::rngs::OsRng;
use rand::RngCore;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

pub const SETUP_DIR: &str = "setup";
pub const SETUP_FILE: &str = "setup.json";

/// The largest multiple of ten a byte holds: drawing above it would favour the digits 0 to 5.
const DIGIT_REJECTION_BOUND: u8 = 250;

/// Restoring is the only fix this message offers: a new code would void the printed card.
const RESTORE_ADVICE: &str = "Restore it from a backup of this pond's data directory. \
                              Do not delete it: a new code would void the printed setup card.";

/// File-backed [`SetupCodeStore`]. Directory `0700`, file `0600`, written atomically.
#[derive(Debug)]
pub struct FileSetupCodeStore {
    dir: PathBuf,
    path: PathBuf,
    // Serialises read-modify-write within this process. Another process writing the same file is
    // not covered, which is why creation links rather than renames.
    write_lock: Mutex<()>,
}

impl FileSetupCodeStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let dir = data_dir.into().join(SETUP_DIR);
        let path = dir.join(SETUP_FILE);
        Self {
            dir,
            path,
            write_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `None` when the directory or the file does not exist; reading never creates either.
    fn read(&self) -> Result<Option<SetupRecord>> {
        match fs::symlink_metadata(&self.dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
            Ok(meta) => check_dir(&self.dir, &meta)?,
        }
        let meta = match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
            Ok(meta) => meta,
        };
        check_file(&self.path, &meta)?;
        let bytes =
            fs::read(&self.path).with_context(|| format!("reading {}", self.path.display()))?;
        parse_record(&self.path, &bytes).map(Some)
    }

    fn ensure_dir(&self) -> Result<()> {
        match fs::symlink_metadata(&self.dir) {
            Ok(meta) => return check_dir(&self.dir, &meta),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        }
        if let Some(parent) = self.dir.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&self.dir) {
            // Only a directory this store made is set outright: the umask could have narrowed it.
            #[cfg(unix)]
            Ok(()) => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("setting {} to 0700", self.dir.display()))?;
            }
            #[cfg(not(unix))]
            Ok(()) => {}
            // Another process made it first; the check below still applies to what it made.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("creating {}", self.dir.display())),
        }
        let meta = fs::symlink_metadata(&self.dir)
            .with_context(|| format!("reading {}", self.dir.display()))?;
        check_dir(&self.dir, &meta)
    }

    /// A `0600` temp file beside the record, flushed to disk. Named by pid, so two processes never
    /// share one; within a process the write lock serialises.
    fn write_temp(&self, record: &SetupRecord) -> Result<PathBuf> {
        let tmp = self
            .dir
            .join(format!("{SETUP_FILE}.{}.tmp", std::process::id()));
        // A crashed run whose pid was reused may have left one, and it would fail `create_new`.
        let _ = fs::remove_file(&tmp);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        let json = serde_json::to_vec_pretty(record)?;
        let written = file
            .write_all(&json)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all());
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("writing {}", tmp.display()));
        }
        Ok(tmp)
    }

    /// Atomically replaces the record.
    fn replace(&self, record: &SetupRecord) -> Result<()> {
        let tmp = self.write_temp(record)?;
        if let Err(e) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(e).with_context(|| {
                format!("renaming {} onto {}", tmp.display(), self.path.display())
            });
        }
        self.sync_dir();
        Ok(())
    }

    /// Publishes a new record only if none exists. A hard link never replaces its target, so a
    /// second process creating at the same moment cannot overwrite a code already printed.
    fn create_exclusive(&self, record: &SetupRecord) -> Result<bool> {
        let tmp = self.write_temp(record)?;
        let linked = fs::hard_link(&tmp, &self.path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                self.sync_dir();
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e).with_context(|| format!("creating {}", self.path.display())),
        }
    }

    /// Best effort, like `secret_crypto::write_private`: some filesystems refuse a directory
    /// fsync, and the record is already written.
    fn sync_dir(&self) {
        if let Ok(dir) = fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
    }

    fn require(&self) -> Result<SetupRecord> {
        self.read()?.ok_or_else(|| {
            anyhow!(
                "there is no setup code at {}; make one before it can be claimed or re-armed",
                self.path.display()
            )
        })
    }
}

#[async_trait]
impl SetupCodeStore for FileSetupCodeStore {
    async fn load(&self) -> Result<Option<SetupRecord>> {
        self.read()
    }

    async fn create_if_missing(&self) -> Result<SetupRecord> {
        let _guard = self.write_lock.lock().await;
        if let Some(existing) = self.read()? {
            return Ok(existing);
        }
        self.ensure_dir()?;
        let record = SetupRecord::new(draw_code(&mut OsRng)?, draw_id(&mut OsRng)?, Utc::now());
        if !self.create_exclusive(&record)? {
            return self.require();
        }
        tracing::info!(
            path = %self.path.display(),
            discriminator = %record.discriminator,
            "created the pond's setup code"
        );
        Ok(record)
    }

    async fn mark_claimed(&self, at: DateTime<Utc>, by: Option<&str>) -> Result<SetupRecord> {
        let _guard = self.write_lock.lock().await;
        let mut record = self.require()?;
        if record.claimed_at.is_some() {
            return Ok(record);
        }
        record.claimed_at = Some(at);
        record.claimed_by = by.map(str::to_string);
        self.replace(&record)?;
        Ok(record)
    }

    async fn rearm(&self) -> Result<SetupRecord> {
        let _guard = self.write_lock.lock().await;
        let mut record = self.require()?;
        let was_claimed = record.claimed_at.take().is_some();
        let had_claimant = record.claimed_by.take().is_some();
        if !was_claimed && !had_claimant {
            return Ok(record);
        }
        self.replace(&record)?;
        tracing::info!(path = %self.path.display(), "re-armed the pond's setup code");
        Ok(record)
    }
}

/// The TLS identity loader's rule: a symlink, or any group or other bit, is refused, not repaired.
fn check_dir(dir: &Path, meta: &fs::Metadata) -> Result<()> {
    if meta.file_type().is_symlink() {
        bail!(
            "{} is a symlink; the setup directory must be a real directory",
            dir.display()
        );
    }
    if !meta.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "{} has mode {mode:o}; the setup directory must be 0700 (chmod 700 it)",
                dir.display()
            );
        }
    }
    Ok(())
}

fn check_file(path: &Path, meta: &fs::Metadata) -> Result<()> {
    if meta.file_type().is_symlink() {
        bail!(
            "{} is a symlink; the setup code must be a regular file. {RESTORE_ADVICE}",
            path.display()
        );
    }
    if !meta.is_file() {
        bail!("{} is not a regular file. {RESTORE_ADVICE}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "{} has mode {mode:o}; the setup code must be 0600 (chmod 600 it)",
                path.display()
            );
        }
    }
    Ok(())
}

/// Validates in steps so no error quotes the code: serde's "invalid type" messages print the
/// value, which for a code written as a JSON number would be the code itself.
fn parse_record(path: &Path, bytes: &[u8]) -> Result<SetupRecord> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        anyhow!(
            "{} is not valid JSON ({e}). {RESTORE_ADVICE}",
            path.display()
        )
    })?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(SETUP_RECORD_VERSION) => {}
        Some(v) => bail!(
            "{} is setup record version {v}; this pond reads version {SETUP_RECORD_VERSION}. \
             {RESTORE_ADVICE}",
            path.display()
        ),
        None => bail!("{} has no record version. {RESTORE_ADVICE}", path.display()),
    }
    match value.get("code").and_then(serde_json::Value::as_str) {
        Some(code) => {
            SetupCode::parse(code).map_err(|e| {
                anyhow!(
                    "{} holds an invalid setup code: {e}. {RESTORE_ADVICE}",
                    path.display()
                )
            })?;
        }
        None => bail!(
            "{} has no setup code string. {RESTORE_ADVICE}",
            path.display()
        ),
    }
    serde_json::from_value(value).map_err(|e| {
        anyhow!(
            "{} is not a valid setup record ({e}). {RESTORE_ADVICE}",
            path.display()
        )
    })
}

/// Ten uniform digits by rejection sampling, then the check digit.
fn draw_code(rng: &mut impl RngCore) -> Result<SetupCode> {
    let mut digits = Zeroizing::new([0u8; SETUP_CODE_RANDOM_DIGITS]);
    for digit in digits.iter_mut() {
        *digit = draw_digit(rng)?;
    }
    Ok(SetupCode::from_digits(*digits)?)
}

fn draw_digit(rng: &mut impl RngCore) -> Result<u8> {
    let mut byte = Zeroizing::new([0u8; 1]);
    loop {
        rng.try_fill_bytes(&mut byte[..])
            .context("the OS random number generator failed")?;
        if byte[0] < DIGIT_REJECTION_BOUND {
            return Ok(byte[0] % 10);
        }
    }
}

fn draw_id(rng: &mut impl RngCore) -> Result<Discriminator> {
    let mut bytes = [0u8; 2];
    rng.try_fill_bytes(&mut bytes)
        .context("the OS random number generator failed")?;
    Ok(Discriminator::new(u16::from_le_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, hour, 0, 0).unwrap()
    }

    fn store_in(dir: &tempfile::TempDir) -> FileSetupCodeStore {
        FileSetupCodeStore::new(dir.path())
    }

    /// Writes a record file by hand with a safe directory and mode, for the corruption tests.
    fn write_raw(dir: &tempfile::TempDir, contents: &str) -> PathBuf {
        let store = store_in(dir);
        store.ensure_dir().unwrap();
        let path = store.path().to_path_buf();
        fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn a_fresh_data_dir_loads_none_and_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        assert_eq!(store.load().await.unwrap(), None);
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn create_if_missing_is_idempotent_across_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let made = store_in(&tmp).create_if_missing().await.unwrap();
        assert_eq!(made.version, SETUP_RECORD_VERSION);
        assert_eq!(made.claimed_at, None);

        let store = store_in(&tmp);
        assert_eq!(store.create_if_missing().await.unwrap(), made);
        assert_eq!(store.load().await.unwrap(), Some(made.clone()));
        assert_eq!(store_in(&tmp).create_if_missing().await.unwrap(), made);

        let leftovers: Vec<_> = fs::read_dir(tmp.path().join(SETUP_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![SETUP_FILE], "a temp file was left behind");
    }

    #[tokio::test]
    async fn the_file_has_the_documented_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        let made = store.create_if_missing().await.unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "claimed_at",
                "claimed_by",
                "code",
                "created_at",
                "discriminator",
                "version"
            ]
        );
        assert_eq!(value["code"], made.code.digits());
        assert_eq!(value["discriminator"], made.discriminator.to_string());
        assert!(value["claimed_at"].is_null());
        assert!(value["claimed_by"].is_null());
    }

    #[tokio::test]
    async fn a_file_written_before_claimed_by_still_loads_and_claims() {
        let tmp = tempfile::tempdir().unwrap();
        write_raw(
            &tmp,
            r#"{"version":1,"code":"79927398713","discriminator":"7f3a",
                "created_at":"2026-10-16T09:00:00Z","claimed_at":null}"#,
        );
        let store = store_in(&tmp);
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!((loaded.claimed_at, loaded.claimed_by), (None, None));

        let claimed = store.mark_claimed(at(10), Some("phone-a")).await.unwrap();
        assert_eq!(claimed.claimed_by.as_deref(), Some("phone-a"));
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(value["version"], 1, "the record stays version 1");
        assert_eq!(value["claimed_by"], "phone-a");
    }

    #[tokio::test]
    async fn two_ponds_draw_different_codes() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let first = store_in(&a).create_if_missing().await.unwrap();
        let second = store_in(&b).create_if_missing().await.unwrap();
        assert_ne!(
            first.code, second.code,
            "a ten-digit collision, or no randomness"
        );
    }

    #[tokio::test]
    async fn mark_claimed_keeps_the_first_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        let made = store.create_if_missing().await.unwrap();

        let claimed = store.mark_claimed(at(10), Some("phone-a")).await.unwrap();
        assert_eq!(claimed.claimed_at, Some(at(10)));
        assert_eq!(claimed.code, made.code);
        let again = store.mark_claimed(at(11), Some("phone-b")).await.unwrap();
        assert_eq!(again.claimed_at, Some(at(10)));
        assert_eq!(again.claimed_by.as_deref(), Some("phone-a"));
        let on_disk = store_in(&tmp).load().await.unwrap().unwrap();
        assert_eq!(
            (on_disk.claimed_at, on_disk.claimed_by.as_deref()),
            (Some(at(10)), Some("phone-a")),
            "the claim did not reach the disk"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_claims_agree_on_one_first_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(store_in(&tmp));
        store.create_if_missing().await.unwrap();

        let handles: Vec<_> = (1..=12)
            .map(|hour| {
                let store = store.clone();
                tokio::spawn(async move {
                    let device = format!("phone-{hour}");
                    store.mark_claimed(at(hour), Some(&device)).await.unwrap()
                })
            })
            .collect();
        let mut seen = Vec::new();
        for handle in handles {
            let record = handle.await.unwrap();
            seen.push((record.claimed_at.unwrap(), record.claimed_by.unwrap()));
        }
        let record = store.load().await.unwrap().unwrap();
        let on_disk = (record.claimed_at.unwrap(), record.claimed_by.unwrap());
        assert!(
            seen.iter().all(|t| *t == on_disk),
            "claims disagreed: {seen:?} vs {on_disk:?}"
        );
    }

    #[tokio::test]
    async fn mark_claimed_and_rearm_need_a_record() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        assert!(store.mark_claimed(at(10), None).await.is_err());
        assert!(store.rearm().await.is_err());
        assert!(!store.path().exists(), "a refused claim created a record");
    }

    #[tokio::test]
    async fn rearm_clears_the_claim_and_keeps_the_code() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        let made = store.create_if_missing().await.unwrap();
        store.mark_claimed(at(10), Some("phone-a")).await.unwrap();

        let rearmed = store.rearm().await.unwrap();
        assert_eq!((rearmed.claimed_at, rearmed.claimed_by), (None, None));
        assert_eq!(rearmed.code, made.code);
        assert_eq!(rearmed.discriminator, made.discriminator);
        assert_eq!(rearmed.created_at, made.created_at);
        assert_eq!(store_in(&tmp).load().await.unwrap(), Some(made));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_0600_inside_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        store.create_if_missing().await.unwrap();
        store.mark_claimed(at(10), None).await.unwrap();

        let file_mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(tmp.path().join(SETUP_DIR))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "file mode {file_mode:o}");
        assert_eq!(dir_mode, 0o700, "directory mode {dir_mode:o}");
    }

    #[tokio::test]
    async fn a_corrupt_file_is_an_error_and_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_raw(&tmp, "{ not json");
        let store = store_in(&tmp);

        let err = store.load().await.unwrap_err();
        assert!(format!("{err:#}").contains("Restore"), "{err:#}");
        assert!(
            store.create_if_missing().await.is_err(),
            "a corrupt card was regenerated"
        );
        assert!(store.mark_claimed(at(10), None).await.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[tokio::test]
    async fn an_unknown_version_is_refused_not_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let future = r#"{"version":2,"code":"79927398713","discriminator":"7f3a",
                         "created_at":"2026-10-16T09:00:00Z","claimed_at":null}"#;
        let path = write_raw(&tmp, future);
        let store = store_in(&tmp);

        let err = store.load().await.unwrap_err();
        assert!(format!("{err:#}").contains("version 2"), "{err:#}");
        assert!(store.create_if_missing().await.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), future);
    }

    #[tokio::test]
    async fn a_code_failing_its_check_digit_is_refused_not_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let typo = r#"{"version":1,"code":"79927398714","discriminator":"7f3a",
                       "created_at":"2026-10-16T09:00:00Z","claimed_at":null}"#;
        let path = write_raw(&tmp, typo);
        let store = store_in(&tmp);

        let err = format!("{:#}", store.load().await.unwrap_err());
        assert!(err.contains("check digit"), "{err}");
        assert!(
            !err.contains("7992739871"),
            "the error quotes the code: {err}"
        );
        assert!(store.create_if_missing().await.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), typo);
    }

    #[tokio::test]
    async fn no_error_quotes_a_code_stored_as_a_number() {
        let tmp = tempfile::tempdir().unwrap();
        write_raw(
            &tmp,
            r#"{"version":1,"code":79927398713,"discriminator":"7f3a",
                "created_at":"2026-10-16T09:00:00Z","claimed_at":null}"#,
        );
        let err = format!("{:#}", store_in(&tmp).load().await.unwrap_err());
        assert!(
            !err.contains("7992739871"),
            "the error quotes the code: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_file_or_directory_is_refused() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let real = store_in(&elsewhere);
        real.create_if_missing().await.unwrap();

        let store = store_in(&tmp);
        store.ensure_dir().unwrap();
        symlink(real.path(), store.path()).unwrap();
        let err = format!("{:#}", store.load().await.unwrap_err());
        assert!(err.contains("symlink"), "{err}");
        assert!(store.create_if_missing().await.is_err());

        let linked_dir = tempfile::tempdir().unwrap();
        symlink(
            elsewhere.path().join(SETUP_DIR),
            linked_dir.path().join(SETUP_DIR),
        )
        .unwrap();
        let err = format!("{:#}", store_in(&linked_dir).load().await.unwrap_err());
        assert!(err.contains("symlink"), "{err}");
        assert!(store_in(&linked_dir).create_if_missing().await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unsafe_permissions_are_refused_not_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(&tmp);
        store.create_if_missing().await.unwrap();

        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        let err = format!("{:#}", store.load().await.unwrap_err());
        assert!(err.contains("0600"), "{err}");
        assert!(store.mark_claimed(at(10), None).await.is_err());
        let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "the store repaired the mode instead of refusing"
        );

        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        let dir = tmp.path().join(SETUP_DIR);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let err = format!("{:#}", store.load().await.unwrap_err());
        assert!(err.contains("0700"), "{err}");
    }

    struct Bytes(Vec<u8>);

    impl RngCore for Bytes {
        fn next_u32(&mut self) -> u32 {
            unimplemented!()
        }
        fn next_u64(&mut self) -> u64 {
            unimplemented!()
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            self.try_fill_bytes(dest).unwrap()
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            for slot in dest {
                *slot = self.0.remove(0);
            }
            Ok(())
        }
    }

    #[test]
    fn digit_draws_reject_bytes_that_would_bias_toward_low_digits() {
        let mut rng = Bytes(vec![255, 250, 249, 7, 252]);
        assert_eq!(
            draw_digit(&mut rng).unwrap(),
            9,
            "250..=255 must be skipped"
        );
        assert_eq!(draw_digit(&mut rng).unwrap(), 7);
        assert_eq!(rng.0, vec![252]);
    }

    #[test]
    fn a_drawn_code_is_ten_rng_digits_and_their_check_digit() {
        let mut rng = Bytes(vec![7, 9, 9, 2, 251, 7, 3, 9, 8, 7, 1]);
        assert_eq!(draw_code(&mut rng).unwrap().digits(), "79927398713");
        let mut rng = Bytes(vec![0x3a, 0x7f]);
        assert_eq!(draw_id(&mut rng).unwrap(), Discriminator::new(0x7f3a));
    }
}
