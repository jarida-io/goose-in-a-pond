//! Keyfile encryption for the on-disk secret store.
//!
//! Threat model: this makes the file alone unreadable, nothing more. It does not protect a
//! running pond (secrets live decrypted in memory, so zeroizing the key is moot) or, by
//! default, a stolen device (the key sits beside the ciphertext unless [`KEY_PATH_ENV`] moves
//! it). Never claim more in user-facing copy. No escrow: losing `master.key` loses every
//! secret. Full-DB encryption (SQLCipher) is deferred on purpose; this must not grow into it.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    Key, XChaCha20Poly1305, XNonce,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Envelope `format` magic. Detection keys off this: a legacy secret could be named `ciphertext`.
pub const ENVELOPE_FORMAT: &str = "giap-secret-envelope-v1";

/// Recorded on disk so a future cipher change is detectable.
const CIPHER_LABEL: &str = "xchacha20poly1305";

/// Associated data (not secret) binding envelopes to this store.
const AAD: &[u8] = b"giap-secrets-v1";

/// Key file location override; point it at a separate mount to survive whole-device theft.
pub const KEY_PATH_ENV: &str = "POND_SECRET_KEY_FILE";

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    format: String,
    cipher: String,
    nonce: String,
    ciphertext: String,
}

/// The store is encrypted and this process cannot open it. A distinct type so callers can
/// tell a missing key from a broken disk.
#[derive(Debug)]
pub struct SecretStoreLocked {
    pub store_path: PathBuf,
    pub key_path: PathBuf,
    pub reason: String,
}

impl std::fmt::Display for SecretStoreLocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "secret store at {} is encrypted but cannot be opened: {}. \
             The key is expected at {} (override with {}). \
             If that key is gone the stored secrets are UNRECOVERABLE -- there is \
             no passphrase and no escrow copy.",
            self.store_path.display(),
            self.reason,
            self.key_path.display(),
            KEY_PATH_ENV,
        )
    }
}

impl std::error::Error for SecretStoreLocked {}

pub fn key_path(data_dir: &Path) -> PathBuf {
    key_path_with_override(std::env::var(KEY_PATH_ENV).ok().as_deref(), data_dir)
}

/// Pure half of [`key_path`], testable without mutating the process environment.
pub(crate) fn key_path_with_override(override_path: Option<&str>, data_dir: &Path) -> PathBuf {
    if let Some(p) = override_path {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    data_dir.join("secrets").join("master.key")
}

/// True when `raw` is one of our envelopes rather than a legacy plaintext map.
pub fn looks_encrypted(raw: &str) -> bool {
    serde_json::from_str::<Envelope>(raw)
        .map(|e| e.format == ENVELOPE_FORMAT)
        .unwrap_or(false)
}

/// `Ok(None)` only when the file is absent (first run); an unreadable key is an error.
pub fn load_key(path: &Path) -> Result<Option<Key>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("reading key file {}", path.display()));
        }
    };
    let bytes = B64
        .decode(text.trim())
        .with_context(|| format!("key file {} is not base64", path.display()))?;
    if bytes.len() != 32 {
        return Err(anyhow!(
            "key file {} holds {} bytes, expected 32",
            path.display(),
            bytes.len()
        ));
    }
    Ok(Some(Key::clone_from_slice(&bytes)))
}

/// Generate and write a key as base64: a password-manager copy is the only backup path.
pub fn create_key(path: &Path) -> Result<Key> {
    let key = XChaCha20Poly1305::generate_key(&mut OsRng);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating key directory {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("tightening {} to 0700", dir.display()))?;
        }
    }
    let mut encoded = B64.encode(key.as_slice());
    encoded.push('\n');
    write_private(path, encoded.as_bytes())
        .with_context(|| format!("writing key file {}", path.display()))?;
    Ok(key)
}

pub fn create_key_if_absent(path: &Path) -> Result<Key> {
    match load_key(path)? {
        Some(key) => Ok(key),
        None => create_key(path),
    }
}

/// Encrypt into a JSON envelope. XChaCha20, so a random 192-bit nonce per write is safe.
pub fn encrypt(key: &Key, plaintext: &str) -> Result<String> {
    let cipher = XChaCha20Poly1305::new(key);
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: AAD,
            },
        )
        // aead::Error has no detail and, without its `std` feature, is not a std::error::Error.
        .map_err(|_| anyhow!("secret store encryption failed"))?;
    let envelope = Envelope {
        format: ENVELOPE_FORMAT.to_string(),
        cipher: CIPHER_LABEL.to_string(),
        nonce: B64.encode(nonce.as_slice()),
        ciphertext: B64.encode(&ciphertext),
    };
    Ok(serde_json::to_string_pretty(&envelope)?)
}

pub fn decrypt(key: &Key, raw: &str) -> Result<String> {
    let envelope: Envelope =
        serde_json::from_str(raw).context("secret store envelope is not valid JSON")?;
    if envelope.format != ENVELOPE_FORMAT {
        return Err(anyhow!(
            "unknown secret store format {:?} (this build understands {:?})",
            envelope.format,
            ENVELOPE_FORMAT
        ));
    }
    if envelope.cipher != CIPHER_LABEL {
        return Err(anyhow!(
            "unknown cipher {:?} (this build understands {:?})",
            envelope.cipher,
            CIPHER_LABEL
        ));
    }
    let nonce_bytes = B64
        .decode(&envelope.nonce)
        .context("envelope nonce is not base64")?;
    if nonce_bytes.len() != 24 {
        return Err(anyhow!(
            "envelope nonce is {} bytes, expected 24",
            nonce_bytes.len()
        ));
    }
    let ciphertext = B64
        .decode(&envelope.ciphertext)
        .context("envelope ciphertext is not base64")?;
    let cipher = XChaCha20Poly1305::new(key);
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| anyhow!("authentication failed -- wrong key, or the file was modified"))?;
    String::from_utf8(plaintext).context("decrypted secret store is not UTF-8")
}

/// Write `bytes` to `path` atomically, never existing world-readable.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut tmp_os = path.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);
    // A crashed run's leftover may have the wrong mode, and would make create_new fail.
    let _ = std::fs::remove_file(&tmp);

    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flushing {}", tmp.display()))?;
    }

    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} onto {}", tmp.display(), path.display()))?;

    // fsync the directory so the rename survives a power cut. Best effort: some filesystems
    // refuse, and the secret is already stored.
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_key() -> Key {
        XChaCha20Poly1305::generate_key(&mut OsRng)
    }

    #[test]
    fn round_trip_recovers_the_plaintext() {
        let key = a_key();
        let envelope = encrypt(&key, r#"{"A":"1"}"#).unwrap();
        assert_eq!(decrypt(&key, &envelope).unwrap(), r#"{"A":"1"}"#);
    }

    #[test]
    fn a_different_key_does_not_decrypt() {
        let envelope = encrypt(&a_key(), "payload").unwrap();
        assert!(decrypt(&a_key(), &envelope).is_err());
    }

    #[test]
    fn a_flipped_ciphertext_byte_is_detected() {
        let key = a_key();
        let envelope = encrypt(&key, "payload").unwrap();
        let mut parsed: Envelope = serde_json::from_str(&envelope).unwrap();
        let mut bytes = B64.decode(&parsed.ciphertext).unwrap();
        bytes[0] ^= 0x01;
        parsed.ciphertext = B64.encode(&bytes);
        let tampered = serde_json::to_string(&parsed).unwrap();
        assert!(
            decrypt(&key, &tampered).is_err(),
            "a modified ciphertext authenticated"
        );
    }

    #[test]
    fn a_legacy_map_is_never_mistaken_for_an_envelope() {
        assert!(!looks_encrypted(r#"{"GNEWS_API_KEY":"abc"}"#));
        // A legacy store whose secret is literally named `ciphertext`.
        assert!(!looks_encrypted(
            r#"{"format":"x","cipher":"y","nonce":"z","ciphertext":"w"}"#
        ));
        // ...and the positive case, so this cannot pass by always returning false.
        assert!(looks_encrypted(&encrypt(&a_key(), "x").unwrap()));
    }

    #[test]
    fn the_key_path_defaults_beside_the_data_dir_and_honours_the_override() {
        let dir = Path::new("/var/pond");
        assert_eq!(
            key_path_with_override(None, dir),
            PathBuf::from("/var/pond/secrets/master.key")
        );
        assert_eq!(
            key_path_with_override(Some("/mnt/usb/pond.key"), dir),
            PathBuf::from("/mnt/usb/pond.key")
        );
        // A blank override is a mis-set variable, not a path.
        assert_eq!(
            key_path_with_override(Some("   "), dir),
            PathBuf::from("/var/pond/secrets/master.key")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_created_key_is_0600_inside_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secrets").join("master.key");
        let key = create_key(&path).unwrap();

        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "key file mode");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "key directory mode");
        assert_eq!(
            load_key(&path).unwrap().unwrap(),
            key,
            "the key did not round-trip through the file"
        );
    }

    #[test]
    fn a_truncated_key_file_is_an_error_not_a_short_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("master.key");
        std::fs::write(&path, B64.encode([0u8; 16])).unwrap();
        assert!(
            load_key(&path).is_err(),
            "a 16-byte key file must not be accepted"
        );
    }

    #[test]
    fn an_absent_key_file_is_none_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_key(&tmp.path().join("nope.key")).unwrap().is_none());
    }
}
