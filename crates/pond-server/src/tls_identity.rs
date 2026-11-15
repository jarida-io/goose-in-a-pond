//! Persistent TLS identity. Certificates rotate; the QR-pinned public key does not.
//!
//! The key signs two certificates. `tls/identity.json` is served on the LAN and loopback
//! listeners and names the host and its LAN addresses, which LAN pairing and the phones'
//! hostname checks need. `tls/tailnet-identity.json` is what the embedded helper serves on
//! the tailnet, and names only the node's tailnet addresses and node name: every tailnet node
//! can read a certificate it is shown, so it must not learn the host name or LAN addresses
//! from it. Both carry the same public key, so the pin is unchanged. A public tunnel's
//! certificate ([`TunnelCertifier`]) is a third, held only in memory: it names only the
//! tunnel's host, which changes with each tunnel session.

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

const IDENTITY_FILE: &str = "identity.json";
const TAILNET_IDENTITY_FILE: &str = "tailnet-identity.json";

/// The node name the embedded helper registers on the tailnet, which its certificate names.
pub const TAILNET_HOSTNAME: &str = "goose-in-a-pond";

/// Where the tailnet certificate is kept, for the helper's `--identity`.
pub fn tailnet_identity_path(data_dir: &Path) -> PathBuf {
    data_dir.join("tls").join(TAILNET_IDENTITY_FILE)
}

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
        let path = dir.join(IDENTITY_FILE);
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
            persist(&dir, IDENTITY_FILE, &material)?;
            tracing::info!("created persistent TLS identity");
            material
        };
        let mut identity = Self {
            dir,
            material,
            _lock: lock,
        };
        validate(&identity.material)?;
        identity.renew_if_needed(names)?;
        Ok(identity)
    }

    /// Stable public-key pin, not a hash of the renewable certificate.
    pub fn pin(&self) -> Result<String> {
        let key = KeyPair::from_pem(&self.material.key_pem)?;
        Ok(format!(
            "sha256/{}",
            STANDARD.encode(Sha256::digest(key.subject_public_key_info()))
        ))
    }

    /// Issues certificates for a public tunnel's host from this identity's key.
    pub fn tunnel_certifier(&self) -> TunnelCertifier {
        TunnelCertifier {
            key_pem: self.material.key_pem.clone(),
        }
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
        if !needs_renewal(&self.material.cert_pem, names)? {
            return Ok(false);
        }
        let key = KeyPair::from_pem(&self.material.key_pem)?;
        let next = Material {
            key_pem: self.material.key_pem.clone(),
            cert_pem: certificate(&key, names)?,
        };
        persist(&self.dir, IDENTITY_FILE, &next)?;
        self.material = next;
        tracing::info!("renewed TLS certificate with the existing public key");
        Ok(true)
    }

    /// Write the tailnet certificate when it is missing, names other than `names`, is within
    /// thirty days of expiry, or cannot be trusted as this key's: anything else in the file
    /// is replaced, since it is derived from the key and holds nothing of its own. True when
    /// it was written.
    pub fn renew_tailnet_if_needed(&self, names: &[String]) -> Result<bool> {
        self.write_tailnet(names, false)
    }

    /// Write the tailnet certificate only when it is missing or cannot be trusted as this key's,
    /// so a usable one keeps the addresses it names until the renewal loop knows the current ones.
    pub fn ensure_tailnet(&self, names: &[String]) -> Result<bool> {
        self.write_tailnet(names, true)
    }

    fn write_tailnet(&self, names: &[String], only_if_unusable: bool) -> Result<bool> {
        let path = self.dir.join(TAILNET_IDENTITY_FILE);
        let replaced = match self.read_tailnet(&path) {
            Ok(Some(material)) => {
                if only_if_unusable || !needs_renewal(&material.cert_pem, names)? {
                    return Ok(false);
                }
                None
            }
            Ok(None) => None,
            Err(error) => Some(format!("{error:#}")),
        };
        let key = KeyPair::from_pem(&self.material.key_pem)?;
        let next = Material {
            key_pem: self.material.key_pem.clone(),
            cert_pem: certificate(&key, names)?,
        };
        persist(&self.dir, TAILNET_IDENTITY_FILE, &next)?;
        match replaced {
            Some(reason) => tracing::warn!(
                target: "giap::trace",
                kind = "tailnet_certificate_replaced",
                %reason,
                "the tailnet certificate was unusable; wrote a new one with the existing public key"
            ),
            None => tracing::info!(
                target: "giap::trace",
                kind = "tailnet_certificate_renewed",
                names = ?names,
                "renewed the tailnet certificate with the existing public key"
            ),
        }
        Ok(true)
    }

    /// The tailnet certificate on disk, `None` when there is none. One that is a symlink,
    /// readable by others, malformed, or not signed with this identity's key is an error.
    fn read_tailnet(&self, path: &Path) -> Result<Option<Material>> {
        let metadata = match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        ensure!(
            metadata.is_file(),
            "the tailnet certificate is not a regular file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "the tailnet certificate is readable by others"
            );
        }
        let material: Material = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            material.key_pem == self.material.key_pem,
            "the tailnet certificate carries another key"
        );
        validate(&material)?;
        Ok(Some(material))
    }
}

/// Signs certificates for a public tunnel's host with the identity key, so a phone's pin holds.
/// No `Debug`: it holds the private key.
#[derive(Clone)]
pub struct TunnelCertifier {
    key_pem: String,
}

impl TunnelCertifier {
    /// A certificate naming only `host`, and the key, as PEM. Never log the key.
    pub fn issue(&self, host: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let key = KeyPair::from_pem(&self.key_pem)?;
        let cert = certificate(&key, &[host.to_string()])?;
        Ok((cert.into_bytes(), self.key_pem.as_bytes().to_vec()))
    }
}

/// The key is ECDSA P-256 and signed the certificate, which is already valid.
fn validate(material: &Material) -> Result<()> {
    let key = KeyPair::from_pem(&material.key_pem).context("invalid TLS private key")?;
    ensure!(
        key.is_compatible(&PKCS_ECDSA_P256_SHA256),
        "TLS key must be ECDSA P-256"
    );
    let (_, pem) = x509_parser::pem::parse_x509_pem(material.cert_pem.as_bytes())
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

/// The DNS names and IP addresses a certificate's SANs cover, sorted and deduplicated.
fn san_names(cert_pem: &str) -> Result<Vec<String>> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
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
                GeneralName::IPAddress(bytes) if bytes.len() == 4 => {
                    actual.push(std::net::Ipv4Addr::from(<[u8; 4]>::try_from(*bytes)?).to_string())
                }
                GeneralName::IPAddress(bytes) if bytes.len() == 16 => {
                    actual.push(std::net::Ipv6Addr::from(<[u8; 16]>::try_from(*bytes)?).to_string())
                }
                _ => {}
            }
        }
    }
    actual.sort();
    actual.dedup();
    Ok(actual)
}

/// Whether a certificate names other than exactly `names`, or expires within thirty days.
fn needs_renewal(cert_pem: &str, names: &[String]) -> Result<bool> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid TLS certificate PEM"))?;
    let cert = pem
        .parse_x509()
        .map_err(|_| anyhow::anyhow!("invalid TLS certificate"))?;
    let mut expected = names.to_vec();
    expected.sort();
    expected.dedup();
    Ok(san_names(cert_pem)? != expected
        || cert.validity().not_after.timestamp()
            <= (OffsetDateTime::now_utc() + Duration::days(30)).unix_timestamp())
}

fn certificate(key: &KeyPair, names: &[String]) -> Result<String> {
    let mut params = CertificateParams::new(names.to_vec())?;
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::minutes(5);
    params.not_after = now + Duration::days(365);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    Ok(params.self_signed(key)?.pem())
}

fn persist(dir: &Path, name: &str, material: &Material) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&serde_json::to_vec(material)?)?;
    file.as_file().sync_all()?;
    file.persist(dir.join(name))?;
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

/// What the tailnet certificate names: the embedded node's tailnet addresses and its node name,
/// and nothing about the LAN. A phone away from home dials the tailnet IPv4 address.
pub fn tailnet_certificate_names(addresses: &[String]) -> Vec<String> {
    let mut names = vec![TAILNET_HOSTNAME.to_string()];
    names.extend(addresses.iter().cloned());
    names.sort();
    names.dedup();
    names
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
    /// Anyone reaching the tunnel can read its certificate, so it names only the tunnel's host,
    /// and it carries the pinned key so a phone's pin check passes.
    #[test]
    fn a_tunnel_certificate_names_only_its_host_and_keeps_the_pin() {
        let dir = tempfile::tempdir().unwrap();
        let identity =
            TlsIdentity::load(dir.path(), &["pond.local".into(), "192.168.1.20".into()]).unwrap();
        let (cert, key) = identity
            .tunnel_certifier()
            .issue("3.tcp.eu.ngrok.io")
            .unwrap();
        let material = Material {
            key_pem: String::from_utf8(key).unwrap(),
            cert_pem: String::from_utf8(cert).unwrap(),
        };
        validate(&material).unwrap();
        assert_eq!(
            san_names(&material.cert_pem).unwrap(),
            ["3.tcp.eu.ngrok.io"]
        );
        assert_eq!(material.key_pem, identity.material.key_pem);
    }

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
    /// Every tailnet node can read the certificate the helper shows it, so it must carry the
    /// same key the phones pinned and nothing about the LAN.
    #[test]
    fn the_tailnet_certificate_shares_the_key_and_names_only_the_tailnet() {
        let dir = tempfile::tempdir().unwrap();
        let lan = vec![
            "pond".to_string(),
            "pond.local".into(),
            "192.168.1.20".into(),
            "100.64.0.7".into(),
        ];
        let identity = TlsIdentity::load(dir.path(), &lan).unwrap();
        let names = tailnet_certificate_names(&["100.64.0.7".into()]);
        assert_eq!(names, ["100.64.0.7", TAILNET_HOSTNAME]);
        assert!(identity.renew_tailnet_if_needed(&names).unwrap());
        assert!(
            !identity.renew_tailnet_if_needed(&names).unwrap(),
            "an unchanged certificate was rewritten"
        );

        let path = tailnet_identity_path(dir.path());
        let tailnet: Material = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        validate(&tailnet).unwrap();
        let spki = |pem: &str| {
            let (_, pem) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
            pem.parse_x509().unwrap().public_key().raw.to_vec()
        };
        assert_eq!(
            spki(&tailnet.cert_pem),
            spki(&identity.material.cert_pem),
            "the tailnet certificate does not match the pin"
        );
        assert_eq!(san_names(&tailnet.cert_pem).unwrap(), names);
        let mut sorted = lan.clone();
        sorted.sort();
        assert_eq!(
            san_names(&identity.material.cert_pem).unwrap(),
            sorted,
            "the LAN certificate changed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }

        // A new address renews it; a damaged or foreign file is replaced, not trusted.
        assert!(identity
            .renew_tailnet_if_needed(&tailnet_certificate_names(&["100.64.0.8".into()]))
            .unwrap());
        fs::write(&path, b"{}").unwrap();
        assert!(identity.renew_tailnet_if_needed(&names).unwrap());
        let other = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let foreign = Material {
            cert_pem: certificate(&other, &names).unwrap(),
            key_pem: other.serialize_pem(),
        };
        persist(&identity.dir, TAILNET_IDENTITY_FILE, &foreign).unwrap();
        assert!(identity.renew_tailnet_if_needed(&names).unwrap());
        let restored: Material = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.key_pem, identity.material.key_pem);
    }

    /// At startup the tailnet addresses are not known yet; a usable certificate must keep the
    /// ones it names, or a phone dialing the tailnet address fails its check until the loop runs.
    #[test]
    fn a_restart_keeps_the_tailnet_certificates_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let identity = TlsIdentity::load(dir.path(), &["pond.local".to_string()]).unwrap();
        let path = tailnet_identity_path(dir.path());
        let at_startup = tailnet_certificate_names(&[]);

        assert!(
            identity.ensure_tailnet(&at_startup).unwrap(),
            "missing: written"
        );
        let with_address = tailnet_certificate_names(&["100.64.0.7".into()]);
        assert!(identity.renew_tailnet_if_needed(&with_address).unwrap());

        assert!(!identity.ensure_tailnet(&at_startup).unwrap());
        let kept: Material = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(san_names(&kept.cert_pem).unwrap(), with_address);

        fs::write(&path, b"{}").unwrap();
        assert!(
            identity.ensure_tailnet(&at_startup).unwrap(),
            "unusable: replaced"
        );
        fs::remove_file(&path).unwrap();
        assert!(
            identity.ensure_tailnet(&at_startup).unwrap(),
            "removed: written"
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
        persist(&identity.dir, IDENTITY_FILE, &identity.material).unwrap();
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
        persist(&identity.dir, IDENTITY_FILE, &identity.material).unwrap();
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
