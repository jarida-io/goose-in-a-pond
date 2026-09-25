//! Syncs connected accounts into the context corpus. Offline: `Paused`, never `Error`.
//! A 401 becomes `NeedsReauth` and is not retried; an unchanged ctag skips the REPORT.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use pond_adapters_caldav::{CalDavAdapter, CalDavConfig, CalDavProvider};
use pond_adapters_imap::{ImapAdapter, ImapConfig, ImapProvider};
use pond_core::context::domain::{secret_key_for, ContextSource, SourceKind, SourceStatus};
use pond_core::context::ingest::IngestPipeline;
use pond_core::context::ports::{
    AccountSync, AccountSyncSummary, ContextRepository, SourceSyncOutcome,
};
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::services::egress::{network_mode, NetworkMode};
use pond_core::user_data::domain::profile::ProfileScope;

/// What one sweep did, for the log line and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountSyncReport {
    /// Calendar sources considered.
    pub sources: usize,
    /// Sources whose ctag matched, so nothing was fetched.
    pub unchanged: usize,
    /// Items handed to the pipeline.
    pub ingested: usize,
    /// Sources that ended in `NeedsReauth`.
    pub needs_reauth: usize,
    /// Sources that ended in `Error`.
    pub failed: usize,
    /// Sources skipped because the pond is offline.
    pub paused: usize,
    /// Each source's outcome, by name, so a two-account household can tell which failed.
    pub per_source: Vec<SourceSyncOutcome>,
}

/// Record one source's result on the report it belongs to.
fn note(report: &mut AccountSyncReport, source: &ContextSource, outcome: &str, ingested: usize) {
    report.per_source.push(SourceSyncOutcome {
        source_id: source.id().to_string(),
        provider: source.provider().to_string(),
        kind: source.kind().as_str().to_string(),
        outcome: outcome.to_string(),
        ingested,
    });
}

/// The credential blob behind a source's `secret_ref`. Holds the self-hosted URL too: a
/// household's own server address identifies it, so it belongs in the encrypted store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountCredentials {
    pub username: String,
    pub password: String,
    /// Self-hosted server: a CalDAV base URL or IMAP `host:port`; `None` for named presets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Rebuild the adapter a stored source needs.
fn adapter_for(source: &ContextSource, creds: &AccountCredentials) -> Result<CalDavAdapter> {
    let provider = CalDavProvider::from_stored(source.provider(), creds.base_url.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this source names a calendar provider this pond does not know: {}",
                source.provider()
            )
        })?;
    CalDavAdapter::new(CalDavConfig {
        provider,
        username: creds.username.clone(),
        password: creds.password.clone(),
    })
}

/// True for the failure a household can fix; matches the adapter's pinned error sentence.
fn is_auth_failure(err: &anyhow::Error) -> bool {
    err.to_string().contains("app-specific password")
}

/// Sync every calendar source. Household scope, as a sessionless sweep; items still inherit
/// their source's `profile_id`, which migration 0044 keeps from moving.
pub async fn sync_calendars(
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
    now: DateTime<Utc>,
) -> Result<AccountSyncReport> {
    let mut report = AccountSyncReport::default();
    let offline = network_mode() == NetworkMode::Offline;

    let sources = repo.list_sources(&ProfileScope::Household).await?;
    for mut source in sources
        .into_iter()
        .filter(|s| s.kind() == SourceKind::Calendar)
    {
        report.sources += 1;

        if offline {
            report.paused += 1;
            note(&mut report, &source, "paused", 0);
            source.advance(
                source.cursor().map(str::to_string),
                now,
                SourceStatus::Paused,
            );
            let _ = repo.upsert_source(&source).await;
            continue;
        }

        let outcome = sync_one(&repo, &pipeline, &secrets, &mut source, now).await;
        match outcome {
            Ok(SyncOne::Unchanged) => {
                report.unchanged += 1;
                note(&mut report, &source, "unchanged", 0);
            }
            Ok(SyncOne::Ingested(n)) => {
                report.ingested += n;
                note(&mut report, &source, "ingested", n);
            }
            Err(e) => {
                if is_auth_failure(&e) {
                    report.needs_reauth += 1;
                    tracing::warn!(
                        source = source.id(),
                        "this calendar refused its credentials; it will not be retried until \
                         somebody reconnects it"
                    );
                    source.advance(
                        source.cursor().map(str::to_string),
                        now,
                        SourceStatus::NeedsReauth,
                    );
                } else {
                    report.failed += 1;
                    tracing::warn!(source = source.id(), error = %e, "calendar sync failed");
                    source.advance(
                        source.cursor().map(str::to_string),
                        now,
                        SourceStatus::Error,
                    );
                }
            }
        }
        if let Err(e) = repo.upsert_source(&source).await {
            tracing::warn!(source = source.id(), error = %e, "could not record the sync result");
        }
    }
    Ok(report)
}

enum SyncOne {
    Unchanged,
    Ingested(usize),
}

async fn sync_one(
    repo: &Arc<dyn ContextRepository>,
    pipeline: &Arc<IngestPipeline>,
    secrets: &Arc<dyn SecretRepository>,
    source: &mut ContextSource,
    now: DateTime<Utc>,
) -> Result<SyncOne> {
    let key = source
        .secret_ref()
        .map(str::to_string)
        .unwrap_or_else(|| secret_key_for(source.id()));
    let blob = secrets
        .get(&key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("this calendar's credentials are missing from the store"))?;
    let creds: AccountCredentials = serde_json::from_str(&blob)
        .map_err(|e| anyhow::anyhow!("this calendar's stored credentials are unreadable: {e}"))?;

    let adapter = adapter_for(source, &creds)?;
    let calendars = adapter.discover_calendars().await?;

    // All calendars' ctags joined: any change re-fetches all, which is cheap at a handful.
    let ctag: String = calendars
        .iter()
        .map(|c| c.ctag.clone().unwrap_or_else(|| c.url.clone()))
        .collect::<Vec<_>>()
        .join("|");
    if !ctag.is_empty() && source.cursor() == Some(ctag.as_str()) {
        source.advance(Some(ctag), now, SourceStatus::Connected);
        return Ok(SyncOne::Unchanged);
    }

    let (from, to) = CalDavAdapter::default_window(now);
    let mut ingested = 0usize;
    for calendar in &calendars {
        let items = adapter.events_in_window(&calendar.url, from, to).await?;
        for item in items {
            match pipeline.ingest(source, item, now).await {
                Ok(_) => ingested += 1,
                Err(e) => {
                    // One malformed event must not abandon the rest of the calendar.
                    tracing::debug!(source = source.id(), error = %e, "one calendar event was not ingested");
                }
            }
        }
    }
    let _ = repo;
    source.advance(Some(ctag), now, SourceStatus::Connected);
    Ok(SyncOne::Ingested(ingested))
}

// ── Mail ─────────────────────────────────────────────────────────────────────

/// Sync every mail source. Not generic with [`sync_calendars`]: they share only shape.
pub async fn sync_mail(
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
    now: DateTime<Utc>,
) -> Result<AccountSyncReport> {
    let mut report = AccountSyncReport::default();
    let offline = network_mode() == NetworkMode::Offline;

    let sources = repo.list_sources(&ProfileScope::Household).await?;
    for mut source in sources.into_iter().filter(|s| s.kind() == SourceKind::Mail) {
        report.sources += 1;

        if offline {
            report.paused += 1;
            note(&mut report, &source, "paused", 0);
            source.advance(
                source.cursor().map(str::to_string),
                now,
                SourceStatus::Paused,
            );
            let _ = repo.upsert_source(&source).await;
            continue;
        }

        match sync_one_mailbox(&pipeline, &secrets, &source, now).await {
            Ok((n, cursor)) => {
                report.ingested += n;
                note(
                    &mut report,
                    &source,
                    if n > 0 { "ingested" } else { "unchanged" },
                    n,
                );
                source.advance(cursor, now, SourceStatus::Connected);
            }
            Err(e) => {
                if is_auth_failure(&e) {
                    report.needs_reauth += 1;
                    note(&mut report, &source, "needs_reauth", 0);
                    tracing::warn!(
                        source = source.id(),
                        "this mailbox refused its credentials; it will not be retried until \
                         somebody reconnects it"
                    );
                    source.advance(None, now, SourceStatus::NeedsReauth);
                } else {
                    report.failed += 1;
                    note(&mut report, &source, "failed", 0);
                    tracing::warn!(source = source.id(), error = %e, "mail sync failed");
                    source.advance(None, now, SourceStatus::Error);
                }
            }
        }
        if let Err(e) = repo.upsert_source(&source).await {
            tracing::warn!(source = source.id(), error = %e, "could not record the sync result");
        }
    }
    Ok(report)
}

async fn sync_one_mailbox(
    pipeline: &Arc<IngestPipeline>,
    secrets: &Arc<dyn SecretRepository>,
    source: &ContextSource,
    now: DateTime<Utc>,
) -> Result<(usize, Option<String>)> {
    let key = source
        .secret_ref()
        .map(str::to_string)
        .unwrap_or_else(|| secret_key_for(source.id()));
    let blob = secrets
        .get(&key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("this mailbox's credentials are missing from the store"))?;
    let creds: AccountCredentials = serde_json::from_str(&blob)
        .map_err(|e| anyhow::anyhow!("this mailbox's stored credentials are unreadable: {e}"))?;

    let provider = ImapProvider::from_stored(source.provider(), creds.base_url.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this source names a mail provider this pond does not know: {}",
                source.provider()
            )
        })?;
    let adapter = ImapAdapter::new(ImapConfig {
        provider,
        username: creds.username.clone(),
        password: creds.password.clone(),
    });

    // Resume above the last seen UID: re-reading the window with bodies costs ~769 MB resident.
    let (items, next_cursor) = adapter
        .messages_since_cursor(ImapAdapter::default_window(now), source.cursor())
        .await?;

    let mut ingested = 0usize;
    for item in items {
        match pipeline.ingest(source, item, now).await {
            Ok(_) => ingested += 1,
            Err(e) => {
                tracing::debug!(source = source.id(), error = %e, "one message was not ingested")
            }
        }
    }
    Ok((ingested, next_cursor))
}

// ── The port the route asks through ─────────────────────────────────────────

/// Both connectors in one pass; the timer and the "check now" button must both use this.
pub struct AccountSyncer {
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
}

impl AccountSyncer {
    pub fn new(
        repo: Arc<dyn ContextRepository>,
        pipeline: Arc<IngestPipeline>,
        secrets: Arc<dyn SecretRepository>,
    ) -> Self {
        Self {
            repo,
            pipeline,
            secrets,
        }
    }

    /// Calendar then mail, summed; sequential, as both share one uplink and CPU on a Jetson.
    pub async fn run(&self, now: DateTime<Utc>) -> Result<AccountSyncSummary> {
        let calendars = sync_calendars(
            self.repo.clone(),
            self.pipeline.clone(),
            self.secrets.clone(),
            now,
        )
        .await?;
        let mail = sync_mail(
            self.repo.clone(),
            self.pipeline.clone(),
            self.secrets.clone(),
            now,
        )
        .await?;
        Ok(AccountSyncSummary {
            sources: calendars.sources + mail.sources,
            unchanged: calendars.unchanged + mail.unchanged,
            ingested: calendars.ingested + mail.ingested,
            needs_reauth: calendars.needs_reauth + mail.needs_reauth,
            failed: calendars.failed + mail.failed,
            paused: calendars.paused + mail.paused,
            per_source: calendars
                .per_source
                .into_iter()
                .chain(mail.per_source)
                .collect(),
        })
    }
}

#[async_trait::async_trait]
impl AccountSync for AccountSyncer {
    async fn sync_now(&self) -> Result<AccountSyncSummary> {
        self.run(Utc::now()).await
    }
}
