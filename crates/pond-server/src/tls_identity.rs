//! Persistent TLS identity. Certificates rotate; the QR-pinned public key does not.

use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use fs2::FileExt;
use rcgen::{
    CertificateParams, ExtendedKeyUsagePurpose, KeyPair, PublicKeyData, PKCS_ECDSA_P256_SHA256,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use time::{Duration, OffsetDateTime};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Material {
    key_pem: String,
    cert_pem: String,
}

/// Holds an exclusive lifetime lock, preventing competing identity writers.
pub struct TlsIdentity {
    dir: PathBuf,
    material: Material,
    _lock: File,
}

impl TlsIdentity {
    /// Load and validate the complete identity, or create it on a new installation.
    pub fn load(data_dir: &Path, names: &[String]) -> Result<Self> {
        let dir = data_dir.join("tls");
        let creating =
            fs::symlink_metadata(&dir).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
        if creating {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(&dir)
                .context("create TLS identity directory")?;
        }
        ensure!(
            !fs::symlink_metadata(&dir)?.file_type().is_symlink(),
            "TLS directory must not be a symlink"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                fs::metadata(&dir)?.permissions().mode() & 0o077 == 0,
                "TLS directory permissions must be 0700"
            );
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options.open(dir.join("identity.lock"))?;
        lock.try_lock_exclusive()
            .context("TLS identity is already in use by another server")?;
        let path = dir.join("identity.json");
        let material = if fs::symlink_metadata(&path).is_ok() {
            ensure!(
                !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "TLS identity must not be a symlink"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    fs::metadata(&path)?.permissions().mode() & 0o077 == 0,
                    "TLS identity permissions must be 0600"
                );
            }
            serde_json::from_slice(&fs::read(&path)?)
                .context("invalid TLS identity; restore it instead of changing trust silently")?
        } else {
            ensure!(
                creating,
                "TLS identity is missing from an existing directory; restore the saved identity"
            );
            // An identity directory containing old material is not a new installation.
            ensure!(
                fs::read_dir(&dir)?.all(|e| e.is_ok_and(|e| e.file_name() == "identity.lock")),
                "incomplete TLS identity directory; restore the saved identity"
            );
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
            let material = Material {
                cert_pem: certificate(&key, names)?,
                key_pem: key.serialize_pem(),
            };
            persist(&dir, &material)?;
            tracing::info!("created persistent TLS identity");
            material
        };
        let mut identity = Self {
            dir,
            material,
            _lock: lock,
        };
        identity.validate()?;
        identity.renew_if_needed(names)?;
        Ok(identity)
    }

    fn validate(&self) -> Result<()> {
        let key = KeyPair::from_pem(&self.material.key_pem).context("invalid TLS private key")?;
        ensure!(
            key.is_compatible(&PKCS_ECDSA_P256_SHA256),
            "TLS key must be ECDSA P-256"
        );
        let (_, pem) = x509_parser::pem::parse_x509_pem(self.material.cert_pem.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid TLS certificate PEM"))?;
        let cert = pem
            .parse_x509()
            .map_err(|_| anyhow::anyhow!("invalid TLS certificate"))?;
        ensure!(
            cert.public_key().raw == key.subject_public_key_info(),
            "TLS key and certificate do not match"
        );
        cert.verify_signature(None)
            .context("TLS certificate signature is invalid")?;
        ensure!(
            cert.validity().not_before.timestamp() <= OffsetDateTime::now_utc().unix_timestamp(),
            "TLS certificate is not yet valid; check the clock"
        );
        Ok(())
    }

    /// Stable public-key pin, not a hash of the renewable certificate.
    pub fn pin(&self) -> Result<String> {
        let key = KeyPair::from_pem(&self.material.key_pem)?;
        Ok(format!(
            "sha256/{}",
            STANDARD.encode(Sha256::digest(key.subject_public_key_info()))
        ))
    }

    /// PEM material for rustls. Never log or expose the returned private key.
    pub fn pem(&self) -> (Vec<u8>, Vec<u8>) {
        (
            self.material.cert_pem.as_bytes().to_vec(),
            self.material.key_pem.as_bytes().to_vec(),
        )
    }

    /// Renew when an address changes or expiry is less than thirty days away.
    pub fn renew_if_needed(&mut self, names: &[String]) -> Result<bool> {
        let (_, pem) = x509_parser::pem::parse_x509_pem(self.material.cert_pem.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid TLS certificate PEM"))?;
        let cert = pem
            .parse_x509()
            .map_err(|_| anyhow::anyhow!("invalid TLS certificate"))?;
        let mut actual = Vec::new();
        if let Some(san) = cert.subject_alternative_name()? {
            for name in &san.value.general_names {
                use x509_parser::extensions::GeneralName;
                match name {
                    GeneralName::DNSName(name) => actual.push(name.to_string()),
                    GeneralName::IPAddress(bytes) if bytes.len() == 4 => actual
                        .push(std::net::Ipv4Addr::from(<[u8; 4]>::try_from(*bytes)?).to_string()),
                    GeneralName::IPAddress(bytes) if bytes.len() == 16 => actual
                        .push(std::net::Ipv6Addr::from(<[u8; 16]>::try_from(*bytes)?).to_string()),
                    _ => {}
                }
            }
        }
        actual.sort();
        actual.dedup();
        let mut expected = names.to_vec();
        expected.sort();
        expected.dedup();
        let renew = actual != expected
            || cert.validity().not_after.timestamp()
                <= (OffsetDateTime::now_utc() + Duration::days(30)).unix_timestamp();
        if !renew {
            return Ok(false);
        }
        let key = KeyPair::from_pem(&self.material.key_pem)?;
        let next = Material {
            key_pem: self.material.key_pem.clone(),
            cert_pem: certificate(&key, names)?,
        };
        persist(&self.dir, &next)?;
        self.material = next;
        tracing::info!("renewed TLS certificate with the existing public key");
        Ok(true)
    }
}

fn certificate(key: &KeyPair, names: &[String]) -> Result<String> {
    let mut params = CertificateParams::new(names.to_vec())?;
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::minutes(5);
    params.not_after = now + Duration::days(365);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    Ok(params.self_signed(key)?.pem())
}

fn persist(dir: &Path, material: &Material) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec(material)?)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("identity.json"))?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Names and addresses covered by this process's certificate, refreshed periodically.
pub fn certificate_names(hostname: &str) -> Result<Vec<String>> {
    let mut names = vec![
        hostname.to_string(),
        format!("{hostname}.local"),
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ];
    for interface in pond_api::network::interfaces()? {
        if pond_api::network::is_lan_interface(&interface)
            || pond_api::network::is_tailnet(interface.ip())
        {
            names.push(interface.ip().to_string());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Encode the same v2 QR contract in startup output and the standalone CLI.
pub fn pairing_url(
    host: &str,
    port: u16,
    code: &str,
    pin: &str,
    lan: Option<&str>,
    tailnet: Option<&str>,
) -> String {
    let mut url = url::Url::parse("pond://pair").expect("static pairing URL");
    let mut query = url.query_pairs_mut();
    query
        .append_pair("v", "2")
        .append_pair("scheme", "https")
        .append_pair(
            "host",
            &format!("{}.local", host.trim_end_matches(".local")),
        )
        .append_pair("port", &port.to_string())
        .append_pair("code", code)
        .append_pair("pin", pin);
    if let Some(lan) = lan {
        query.append_pair("ip", lan);
    }
    if let Some(tailnet) = tailnet {
        query.append_pair("ts", tailnet);
    }
    drop(query);
    url.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_and_address_renewal_keep_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["pond.local".into()];
        let mut identity = TlsIdentity::load(dir.path(), &names).unwrap();
        let pin = identity.pin().unwrap();
        assert!(TlsIdentity::load(dir.path(), &names).is_err());
        assert!(!identity.renew_if_needed(&names).unwrap());
        assert!(identity
            .renew_if_needed(&["pond.local".into(), "192.168.1.99".into()])
            .unwrap());
        assert_eq!(identity.pin().unwrap(), pin);
        drop(identity);
        assert_eq!(
            TlsIdentity::load(dir.path(), &names)
                .unwrap()
                .pin()
                .unwrap(),
            pin
        );
    }
    #[test]
    fn corruption_is_not_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["pond.local".into()];
        drop(TlsIdentity::load(dir.path(), &names).unwrap());
        fs::write(dir.path().join("tls/identity.json"), b"{}").unwrap();
        assert!(TlsIdentity::load(dir.path(), &names).is_err());
        assert_eq!(
            fs::read(dir.path().join("tls/identity.json")).unwrap(),
            b"{}"
        );
    }
    #[test]
    fn missing_identity_is_not_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["pond.local".into()];
        drop(TlsIdentity::load(dir.path(), &names).unwrap());
        fs::remove_file(dir.path().join("tls/identity.json")).unwrap();
        assert!(TlsIdentity::load(dir.path(), &names).is_err());
        assert!(!dir.path().join("tls/identity.json").exists());
    }

    #[test]
    fn expired_certificate_renews_without_changing_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["pond.local".into()];
        let mut identity = TlsIdentity::load(dir.path(), &names).unwrap();
        let pin = identity.pin().unwrap();
        let key = KeyPair::from_pem(&identity.material.key_pem).unwrap();
        let mut params = CertificateParams::new(names.clone()).unwrap();
        params.not_before = OffsetDateTime::now_utc() - Duration::days(400);
        params.not_after = OffsetDateTime::now_utc() - Duration::days(1);
        identity.material.cert_pem = params.self_signed(&key).unwrap().pem();
        persist(&identity.dir, &identity.material).unwrap();
        drop(identity);
        let identity = TlsIdentity::load(dir.path(), &names).unwrap();
        assert_eq!(identity.pin().unwrap(), pin);
        let (_, pem) =
            x509_parser::pem::parse_x509_pem(identity.material.cert_pem.as_bytes()).unwrap();
        assert!(pem.parse_x509().unwrap().validity().is_valid());
    }

    #[test]
    fn mismatched_certificate_and_key_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["pond.local".into()];
        let mut identity = TlsIdentity::load(dir.path(), &names).unwrap();
        identity.material.key_pem = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .unwrap()
            .serialize_pem();
        persist(&identity.dir, &identity.material).unwrap();
        drop(identity);
        assert!(TlsIdentity::load(dir.path(), &names).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_permissions_and_symlinks_are_refused() {
        use std::os::unix::{fs::symlink, fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let names = vec!["pond.local".into()];
        drop(TlsIdentity::load(dir.path(), &names).unwrap());
        let path = dir.path().join("tls/identity.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(TlsIdentity::load(dir.path(), &names).is_err());
        fs::remove_file(&path).unwrap();
        symlink(dir.path().join("missing"), path).unwrap();
        assert!(TlsIdentity::load(dir.path(), &names).is_err());
    }
}
