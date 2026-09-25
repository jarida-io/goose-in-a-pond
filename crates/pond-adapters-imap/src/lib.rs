//! Read-only IMAP context source: bodies via `BODY.PEEK[TEXT]` (plain `BODY[TEXT]` sets `\Seen`).
//! Implicit TLS on 993 only: STARTTLS starts in the clear, so a downgrade would be invisible.

mod body;
mod header;
mod provider;

pub use header::decode_rfc2047;
pub use provider::ImapProvider;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use futures::StreamExt;
use pond_core::context::domain::ItemKind;
use pond_core::context::ingest::RawItem;
use std::sync::Arc;

/// Whole-conversation timeout; generous because a first 30-day sync reads every body.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Per-message body cap, applied before the MIME parse (marketing mail can be a megabyte).
const MAX_BODY_BYTES: usize = 64 * 1024;

/// One member's mailbox account; `Debug` redacts the password.
#[derive(Clone)]
pub struct ImapConfig {
    pub provider: ImapProvider,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConfig")
            .field("provider", &self.provider)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

pub struct ImapAdapter {
    config: ImapConfig,
}

impl ImapAdapter {
    pub fn new(config: ImapConfig) -> Self {
        Self { config }
    }

    /// Default sync window: 30 days, short so mail volume stays in brute-force retrieval range.
    pub fn default_window(now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::days(30)
    }

    /// Recent messages as ingest items; connects per sync, deliberately no long-lived IDLE socket.
    pub async fn recent_messages(&self, since: DateTime<Utc>) -> Result<Vec<RawItem>> {
        Ok(self.fetch_since(since, None).await?.0)
    }

    /// Recent messages plus a `UIDVALIDITY:MAXUID` resume cursor; a changed `UIDVALIDITY` means the
    /// mailbox renumbered, so the whole window is read again.
    pub async fn messages_since_cursor(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> Result<(Vec<RawItem>, Option<String>)> {
        self.fetch_since(since, cursor).await
    }

    async fn fetch_since(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> Result<(Vec<RawItem>, Option<String>)> {
        let host = self.config.provider.host().to_string();
        let port = self.config.provider.port();

        // Gate before any packet, record after. The URL is synthetic; only its HOST is judged.
        let url = format!("imaps://{host}:{port}");
        pond_core::shared::services::egress::check_egress(&url)?;

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(TIMEOUT, self.fetch(&host, port, since, cursor)).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        let status = match &result {
            Ok(Ok(_)) => Some(200),
            Ok(Err(_)) => Some(500),
            Err(_) => None,
        };
        pond_core::shared::services::egress::record_egress(&url, "IMAP", status, latency_ms);

        match result {
            Ok(inner) => inner,
            Err(_) => Err(anyhow!("the mail server did not answer within {TIMEOUT:?}")),
        }
    }

    async fn fetch(
        &self,
        host: &str,
        port: u16,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> Result<(Vec<RawItem>, Option<String>)> {
        let tcp = tokio::net::TcpStream::connect((host, port))
            .await
            .with_context(|| format!("could not reach the mail server at {host}:{port}"))?;

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // Name the provider: `builder()` panics with both `aws_lc_rs` and `ring` enabled, as here.
        let tls_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .context("could not select TLS protocol versions")?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| anyhow!("{host} is not a usable server name"))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config))
            .connect(server_name, tcp)
            .await
            .context("the mail server's TLS handshake failed")?;

        let client = // No compat shim: async-imap's `runtime-tokio` takes tokio streams.
        async_imap::Client::new(tls);
        let mut session = client
            .login(&self.config.username, &self.config.password)
            .await
            .map_err(|(e, _)| {
                // Fixable by the user, and distinct so the scheduler doesn't retry it.
                anyhow!(
                    "the mail account refused these credentials -- most providers need an \
                     app-specific password rather than the account password ({e})"
                )
            })?;

        // `examine`, not `select`: it opens the mailbox READ-ONLY, so no flag can change.
        let mailbox = session
            .examine("INBOX")
            .await
            .context("could not open the INBOX")?;
        let uid_validity = mailbox.uid_validity.unwrap_or(0);

        // Resume only if UIDVALIDITY is unchanged; a stale UID would silently skip mail.
        let resume_from = cursor.and_then(|c| c.split_once(':')).and_then(|(v, u)| {
            match (v.parse::<u32>().ok(), u.parse::<u32>().ok()) {
                (Some(v), Some(u)) if v == uid_validity => Some(u),
                _ => None,
            }
        });

        let query = match resume_from {
            Some(max) => format!("UID {}:* SINCE {}", max + 1, since.format("%d-%b-%Y")),
            None => format!("SINCE {}", since.format("%d-%b-%Y")),
        };
        let uids = session
            .uid_search(&query)
            .await
            .context("the mail server refused the search")?;
        let highest = uids.iter().copied().max();

        let mut items = Vec::new();
        // Counted: both ways a message can vanish below are a silent `continue`.
        let mut unreadable = 0usize;
        let mut envelopeless = 0usize;
        // Batched to bound memory: one fetch of a 1,300-message window hit 769 MB and stalled the
        // health check until the watchdog restarted the pond.
        const BATCH: usize = 50;
        for window in uids.iter().copied().collect::<Vec<_>>().chunks(BATCH) {
            let set = window
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // PEEK: plain `BODY[TEXT]` would set \Seen and mark mail read.
            let mut stream = session
                .uid_fetch(set, "(ENVELOPE BODY.PEEK[TEXT])")
                .await
                .context("the mail server refused the fetch")?;
            while let Some(message) = stream.next().await {
                let message = match message {
                    Ok(m) => m,
                    Err(e) => {
                        unreadable += 1;
                        tracing::debug!(error = %e, "a fetch response could not be read");
                        continue;
                    }
                };
                let body = message
                    .text()
                    .map(|raw| {
                        let cut = raw.len().min(MAX_BODY_BYTES);
                        crate::body::body_to_text(&String::from_utf8_lossy(&raw[..cut]))
                    })
                    .unwrap_or_default();
                match envelope_to_item(message.envelope(), &body) {
                    Some(item) => items.push(item),
                    None => envelopeless += 1,
                }
            }
            drop(stream);
            // Yield between batches so a health check can run.
            tokio::task::yield_now().await;
        }
        // Best effort: a failed logout does not invalidate what was read.
        let _ = session.logout().await;

        let asked = uids.len();
        if items.len() < asked {
            tracing::warn!(
                asked,
                returned = items.len(),
                unreadable,
                envelopeless,
                "the mail server returned fewer messages than were searched for"
            );
        } else {
            tracing::info!(asked, returned = items.len(), "mail fetched");
        }

        // Highest UID searched; a fetch error returns early, so the old cursor stays.
        let next_cursor = match (highest, resume_from) {
            (Some(h), _) => Some(format!("{uid_validity}:{h}")),
            // Nothing new, but the mailbox was reachable: keep the cursor.
            (None, Some(prev)) => Some(format!("{uid_validity}:{prev}")),
            (None, None) => None,
        };
        Ok((items, next_cursor))
    }
}

/// `ENVELOPE` to item; skipped without a Message-ID (the re-sync key) or a date.
fn envelope_to_item(
    envelope: Option<&async_imap::imap_proto::Envelope<'_>>,
    body_text: &str,
) -> Option<RawItem> {
    let envelope = envelope?;
    let text = |field: &Option<std::borrow::Cow<'_, [u8]>>| -> Option<String> {
        field
            .as_ref()
            .map(|b| decode_rfc2047(&String::from_utf8_lossy(b)))
    };

    let external_id = text(&envelope.message_id)?.trim().to_string();
    if external_id.is_empty() {
        return None;
    }
    let occurred_at = text(&envelope.date)
        .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|d| d.with_timezone(&Utc))?;

    let title = match text(&envelope.subject) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => "(no subject)".to_string(),
    };

    // Sender: display name if any, else the address.
    let sender = envelope.from.as_ref().and_then(|addrs| {
        addrs.first().map(|a| {
            let name = a
                .name
                .as_ref()
                .map(|b| decode_rfc2047(&String::from_utf8_lossy(b)))
                .filter(|n| !n.trim().is_empty());
            let address = match (&a.mailbox, &a.host) {
                (Some(m), Some(h)) => Some(format!(
                    "{}@{}",
                    String::from_utf8_lossy(m),
                    String::from_utf8_lossy(h)
                )),
                _ => None,
            };
            match (name, address) {
                (Some(n), Some(addr)) => (n, Some(addr)),
                (Some(n), None) => (n, None),
                (None, Some(addr)) => (addr.clone(), Some(addr)),
                (None, None) => ("someone".to_string(), None),
            }
        })
    });

    let (participants, from_line) = match sender {
        Some((name, address)) => {
            let line = match &address {
                Some(a) if a != &name => format!("From: {name} <{a}>"),
                _ => format!("From: {name}"),
            };
            (vec![name], line)
        }
        None => (Vec::new(), String::new()),
    };
    // Sender first, so even the shortest body says who wrote it; the subject rides in chunk 0.
    let body = match (from_line.is_empty(), body_text.trim().is_empty()) {
        (_, true) => from_line,
        (true, false) => body_text.trim().to_string(),
        (false, false) => format!("{from_line}\n\n{}", body_text.trim()),
    };

    Some(RawItem {
        external_id,
        kind: ItemKind::Message,
        occurred_at,
        title,
        body,
        participants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Production source minus whole-line comments (a code line with a trailing one still counts).
    fn production_code() -> String {
        include_str!("lib.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with("*"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn cfg() -> ImapConfig {
        ImapConfig {
            provider: ImapProvider::Fastmail,
            username: "jerry@example.org".into(),
            password: "app-specific-secret".into(),
        }
    }

    #[test]
    fn the_password_is_not_printable_by_accident() {
        let rendered = format!("{:?}", cfg());
        assert!(!rendered.contains("app-specific-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn the_body_fetch_never_marks_mail_as_read() {
        let production = production_code();
        assert!(
            production.contains("BODY.PEEK[TEXT]"),
            "the body fetch must use PEEK, or reading the mailbox marks it read"
        );
        for forbidden in ["RFC822", "\"BODY[", " BODY[", "BODYSTRUCTURE"] {
            assert!(
                !production.contains(forbidden),
                "a non-PEEK body fetch sets \\Seen: {forbidden}"
            );
        }
        // And it must never write to the mailbox either.
        for forbidden in ["APPEND", "STORE", "\"COPY\"", "EXPUNGE"] {
            assert!(
                !production.contains(forbidden),
                "this connector is read-only, but it issues {forbidden}"
            );
        }
    }

    #[test]
    fn the_mailbox_is_opened_read_only() {
        let production = production_code();
        assert!(
            production.contains(".examine("),
            "the INBOX must be opened read-only"
        );
        assert!(
            !production.contains(".select("),
            "`select` opens the mailbox for writing and marks mail read"
        );
    }

    #[test]
    fn the_mail_window_is_shorter_than_the_calendars() {
        let now = Utc::now();
        let since = ImapAdapter::default_window(now);
        assert!(since < now);
        assert!(
            now - since <= Duration::days(31),
            "mail volume is what decides whether this corpus stays brute-forceable"
        );
    }
}
