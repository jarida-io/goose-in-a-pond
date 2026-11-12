//! Syncs connected accounts into the context corpus. Offline: `Paused`, never `Error`.
//! A 401 becomes `NeedsReauth` and is not retried; an unchanged ctag skips the REPORT.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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
use pond_core::shared::ports::hook_event_sink::{HookEventSink, NewHookEvent};
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
    events: Option<&ConnectorEvents>,
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

        let outcome = sync_one(&repo, &pipeline, &secrets, &mut source, events, now).await;
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
    events: Option<&ConnectorEvents>,
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
    // Read once per process even when unchanged, so the events about to start are known.
    let unread = events.is_some_and(|e| e.needs_read(source.id()));
    if !ctag.is_empty() && source.cursor() == Some(ctag.as_str()) && !unread {
        source.advance(Some(ctag), now, SourceStatus::Connected);
        return Ok(SyncOne::Unchanged);
    }

    let (from, to) = CalDavAdapter::default_window(now);
    let mut ingested = 0usize;
    let mut seen = Vec::new();
    for calendar in &calendars {
        let items = adapter.events_in_window(&calendar.url, from, to).await?;
        for item in items {
            let event = EventSeen {
                external_id: item.external_id.clone(),
                title: item.title.clone(),
                starts_at: item.occurred_at,
            };
            match pipeline.ingest(source, item, now).await {
                Ok(_) => {
                    ingested += 1;
                    seen.push(event);
                }
                Err(e) => {
                    // One malformed event must not abandon the rest of the calendar.
                    tracing::debug!(source = source.id(), error = %e, "one calendar event was not ingested");
                }
            }
        }
    }
    let _ = repo;
    if let Some(events) = events {
        events.calendar_read(source, &seen, now).await;
    }
    source.advance(Some(ctag), now, SourceStatus::Connected);
    Ok(SyncOne::Ingested(ingested))
}

// ── Mail ─────────────────────────────────────────────────────────────────────

/// Sync every mail source. Not generic with [`sync_calendars`]: they share only shape.
pub async fn sync_mail(
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
    events: Option<&ConnectorEvents>,
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

        // The first sync reads the mailbox's backlog, which is not news.
        let first_sync = source.cursor().is_none();
        match sync_one_mailbox(&pipeline, &secrets, &source, now).await {
            Ok((n, cursor, mail)) => {
                if let (Some(events), false) = (events, first_sync) {
                    events.mail_arrived(&source, &mail, now).await;
                }
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
) -> Result<(usize, Option<String>, Vec<MailSeen>)> {
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
    let mut mail = Vec::new();
    for item in items {
        let seen = MailSeen {
            from: item.participants.first().cloned().unwrap_or_default(),
            subject: item.title.clone(),
        };
        match pipeline.ingest(source, item, now).await {
            Ok(_) => {
                ingested += 1;
                mail.push(seen);
            }
            Err(e) => {
                tracing::debug!(source = source.id(), error = %e, "one message was not ingested")
            }
        }
    }
    Ok((ingested, next_cursor, mail))
}

// ── Events for to-do hooks ───────────────────────────────────────────────────

/// A message a sync stored.
#[derive(Debug, Clone, PartialEq)]
pub struct MailSeen {
    pub from: String,
    pub subject: String,
}

/// A calendar event a sync read.
#[derive(Debug, Clone, PartialEq)]
pub struct EventSeen {
    pub external_id: String,
    pub title: String,
    pub starts_at: DateTime<Utc>,
}

#[derive(Default)]
struct CalendarSeen {
    known: HashSet<String>,
    upcoming: Vec<EventSeen>,
}

/// What syncs raise for to-do hooks: `mail:<source id>` / `new_mail`, and
/// `calendar:<source id>` / `event_added` and `event_starting`. A calendar's first read in this
/// process only learns what is there, so a restart or a new account raises no flood of "added".
pub struct ConnectorEvents {
    sink: Arc<dyn HookEventSink>,
    every: chrono::Duration,
    calendars: Mutex<HashMap<String, CalendarSeen>>,
}

impl ConnectorEvents {
    pub fn new(sink: Arc<dyn HookEventSink>, every: std::time::Duration) -> Self {
        Self {
            sink,
            every: chrono::Duration::from_std(every).unwrap_or(chrono::Duration::minutes(30)),
            calendars: Mutex::default(),
        }
    }

    fn needs_read(&self, source_id: &str) -> bool {
        !self
            .calendars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(source_id)
    }

    async fn raise(
        &self,
        source: &ContextSource,
        prefix: &str,
        event: &str,
        payload: serde_json::Value,
        now: DateTime<Utc>,
        once: Option<String>,
    ) {
        let mut row = NewHookEvent::new(
            Some(source.profile_id()),
            "connector",
            &format!("{prefix}:{}", source.id()),
            event,
            payload,
            now,
        );
        row.once_key = once;
        if let Err(e) = self.sink.record(row).await {
            tracing::warn!(source = source.id(), error = %e, "could not raise a connector event");
        }
    }

    pub async fn mail_arrived(
        &self,
        source: &ContextSource,
        mail: &[MailSeen],
        now: DateTime<Utc>,
    ) {
        for m in mail {
            let payload = serde_json::json!({"from": m.from, "subject": m.subject});
            self.raise(source, "mail", "new_mail", payload, now, None)
                .await;
        }
    }

    pub async fn calendar_read(
        &self,
        source: &ContextSource,
        events: &[EventSeen],
        now: DateTime<Utc>,
    ) {
        let added: Vec<EventSeen> = {
            let mut calendars = self.calendars.lock().unwrap_or_else(|e| e.into_inner());
            let first = !calendars.contains_key(source.id());
            let seen = calendars.entry(source.id().to_string()).or_default();
            let added = if first {
                Vec::new()
            } else {
                events
                    .iter()
                    .filter(|e| !seen.known.contains(&e.external_id))
                    .cloned()
                    .collect()
            };
            seen.known = events.iter().map(|e| e.external_id.clone()).collect();
            seen.upcoming = events
                .iter()
                .filter(|e| e.starts_at > now)
                .cloned()
                .collect();
            added
        };
        for event in added {
            let payload = serde_json::json!({"title": event.title, "starts_at": event.starts_at.to_rfc3339()});
            let once = format!("calendar:{}:{}:added", source.id(), event.external_id);
            self.raise(source, "calendar", "event_added", payload, now, Some(once))
                .await;
        }
    }

    /// Raises `event_starting` for each known event that begins before the next sync, once.
    pub async fn starting_soon(&self, now: DateTime<Utc>) {
        let soon: Vec<(String, EventSeen)> = {
            let mut calendars = self.calendars.lock().unwrap_or_else(|e| e.into_inner());
            let mut soon = Vec::new();
            for (source_id, seen) in calendars.iter_mut() {
                seen.upcoming.retain(|e| {
                    let starting = e.starts_at >= now && e.starts_at < now + self.every;
                    if starting {
                        soon.push((source_id.clone(), e.clone()));
                    }
                    e.starts_at >= now + self.every
                });
            }
            soon
        };
        for (source_id, event) in soon {
            let Some(profile) = self.owner_of(&source_id) else {
                continue;
            };
            let row = NewHookEvent::new(
                Some(&profile),
                "connector",
                &format!("calendar:{source_id}"),
                "event_starting",
                serde_json::json!({"title": event.title, "starts_at": event.starts_at.to_rfc3339()}),
                now,
            )
            .once(format!(
                "calendar:{source_id}:{}:starting:{}",
                event.external_id,
                event.starts_at.timestamp()
            ));
            if let Err(e) = self.sink.record(row).await {
                tracing::warn!(source = %source_id, error = %e, "could not raise a connector event");
            }
        }
    }

    /// The member a source id names: account sources are `<kind>:<provider>:<member>`.
    fn owner_of(&self, source_id: &str) -> Option<String> {
        source_id.splitn(3, ':').nth(2).map(str::to_string)
    }
}

// ── The port the route asks through ─────────────────────────────────────────

/// Both connectors in one pass; the timer and the "check now" button must both use this.
pub struct AccountSyncer {
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
    events: Option<ConnectorEvents>,
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
            events: None,
        }
    }

    /// Raises each sync's new mail and calendar events in `hook_events`; `every` is the time
    /// between syncs, which bounds "about to start".
    pub fn with_hook_events(
        mut self,
        sink: Arc<dyn HookEventSink>,
        every: std::time::Duration,
    ) -> Self {
        self.events = Some(ConnectorEvents::new(sink, every));
        self
    }

    /// Calendar then mail, summed; sequential, as both share one uplink and CPU on a Jetson.
    pub async fn run(&self, now: DateTime<Utc>) -> Result<AccountSyncSummary> {
        let calendars = sync_calendars(
            self.repo.clone(),
            self.pipeline.clone(),
            self.secrets.clone(),
            self.events.as_ref(),
            now,
        )
        .await?;
        let mail = sync_mail(
            self.repo.clone(),
            self.pipeline.clone(),
            self.secrets.clone(),
            self.events.as_ref(),
            now,
        )
        .await?;
        if let Some(events) = &self.events {
            events.starting_soon(now).await;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::context::domain::SourceParts;

    #[derive(Default)]
    struct Sink(Mutex<Vec<NewHookEvent>>);

    #[async_trait::async_trait]
    impl HookEventSink for Sink {
        async fn record(&self, event: NewHookEvent) -> Result<bool> {
            let mut rows = self.0.lock().unwrap();
            if event
                .once_key
                .as_ref()
                .is_some_and(|k| rows.iter().any(|r| r.once_key.as_ref() == Some(k)))
            {
                return Ok(false);
            }
            rows.push(event);
            Ok(true)
        }
    }

    fn source(kind: SourceKind, id: &str) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: id.into(),
            kind,
            provider: "icloud".into(),
            profile_id: "jerry".into(),
            scopes: Vec::new(),
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: Utc::now(),
        })
        .unwrap()
    }

    fn event(id: &str, starts_at: DateTime<Utc>) -> EventSeen {
        EventSeen {
            external_id: id.into(),
            title: format!("Event {id}"),
            starts_at,
        }
    }

    #[tokio::test]
    async fn a_calendar_raises_added_after_its_first_read_and_starting_once() {
        let sink = Arc::new(Sink::default());
        let events = ConnectorEvents::new(sink.clone(), std::time::Duration::from_secs(30 * 60));
        let calendar = source(SourceKind::Calendar, "calendar:icloud:jerry");
        let now = Utc::now();
        assert!(events.needs_read(calendar.id()));
        let summit = event("summit", now + chrono::Duration::days(10));
        events
            .calendar_read(&calendar, &[summit.clone()], now)
            .await;
        assert!(!events.needs_read(calendar.id()));
        assert!(
            sink.0.lock().unwrap().is_empty(),
            "the first read only learns"
        );

        let standup = event("standup", now + chrono::Duration::minutes(20));
        events
            .calendar_read(&calendar, &[summit, standup], now)
            .await;
        events.starting_soon(now).await;
        events
            .starting_soon(now + chrono::Duration::minutes(1))
            .await;
        let rows = sink.0.lock().unwrap();
        let seen: Vec<(String, String)> = rows
            .iter()
            .map(|r| (r.source_id.clone(), r.event.clone()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (
                    "calendar:calendar:icloud:jerry".into(),
                    "event_added".into()
                ),
                (
                    "calendar:calendar:icloud:jerry".into(),
                    "event_starting".into()
                ),
            ]
        );
        assert!(rows
            .iter()
            .all(|r| r.profile_id.as_deref() == Some("jerry")));
        assert_eq!(rows[1].payload["title"], "Event standup");
    }

    #[tokio::test]
    async fn new_mail_names_its_sender_and_subject() {
        let sink = Arc::new(Sink::default());
        let events = ConnectorEvents::new(sink.clone(), std::time::Duration::from_secs(60));
        let inbox = source(SourceKind::Mail, "mail:gmail:jerry");
        events
            .mail_arrived(
                &inbox,
                &[MailSeen {
                    from: "Edwin Sifuna".into(),
                    subject: "Linda Mwananchi town hall".into(),
                }],
                Utc::now(),
            )
            .await;
        let rows = sink.0.lock().unwrap();
        assert_eq!(rows[0].source_kind, "connector");
        assert_eq!(rows[0].source_id, "mail:mail:gmail:jerry");
        assert_eq!(rows[0].event, "new_mail");
        assert_eq!(
            rows[0].payload,
            serde_json::json!({"from": "Edwin Sifuna", "subject": "Linda Mwananchi town hall"})
        );
    }
}
