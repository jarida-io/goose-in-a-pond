//! Tracing initialisation for Goose In A Pond.
//!
//! Three sinks are registered on startup:
//!
//! 1. **stdout** — human-readable coloured output (same as before).
//! 2. **Rolling file** — plain-text log, one file per day under `<data_dir>/logs/`.
//!    Older files are kept on disk; the OS or the user prunes them as needed.
//! 3. **SQLite event log** — routed to the `event_log` table in `pond_logs.db`
//!    via an async drain task.  Two categories of events reach the DB:
//!    - WARN and above from any target (incident history).
//!    - INFO from targets starting with `giap::trace` (correlated turn events:
//!      turn start/end, tool calls, inference, provider swaps, outbound HTTP).
//!    Structured key-value fields are serialised as JSON into the `metadata` column,
//!    enabling `WHERE json_extract(metadata,'$.session_id') = ?` queries.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::field::Visit;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use pond_core::security::ports::event_log::EventLogRepository;

// ── Internal channel entry ────────────────────────────────────────────────────

struct ChannelEntry {
    level: String,
    /// tracing target (e.g. `"giap::trace"`, `"pond_server"`)
    source: String,
    message: String,
    /// JSON blob of all structured key-value fields except `message`.
    metadata: Option<String>,
}

// ── Visitor: captures message + all key-value fields ─────────────────────────

#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.insert(field.name().to_string(), value.into());
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}").into());
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_string(), value.into());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.insert(field.name().to_string(), value.into());
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.insert(field.name().to_string(), value.into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name().to_string(), value.into());
    }
}

// ── Custom per-layer filter ───────────────────────────────────────────────────

/// Routes events to the SQLite event log when:
/// - Level is WARN or above (incident/error history), OR
/// - Level is INFO and the tracing target starts with `giap::trace`
///   (structured turn-level event stream for session correlation).
struct TraceFilter;

impl<S: Subscriber> tracing_subscriber::layer::Filter<S> for TraceFilter {
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        *meta.level() <= Level::WARN
            || (*meta.level() == Level::INFO && meta.target().starts_with("giap::trace"))
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::INFO)
    }
}

// ── Custom tracing Layer ──────────────────────────────────────────────────────

struct EventLogLayer {
    tx: UnboundedSender<ChannelEntry>,
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        if visitor.message.is_empty() && visitor.fields.is_empty() {
            return;
        }
        let metadata = if visitor.fields.is_empty() {
            None
        } else {
            serde_json::to_string(&visitor.fields).ok()
        };
        let meta = event.metadata();
        let _ = self.tx.send(ChannelEntry {
            level: meta.level().to_string(),
            source: meta.target().to_string(),
            message: visitor.message,
            metadata,
        });
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Keeps the rolling-file background writer thread alive.
/// Dropped at server shutdown to flush buffered bytes to disk.
pub struct FileWriterGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Returned by [`init_tracing`].
///
/// Keep this value alive for the duration of the process.  Call
/// [`drain_into`](Self::drain_into) after the database is ready to start
/// persisting events to the SQLite event log.
pub struct LogDrainHandle {
    rx: UnboundedReceiver<ChannelEntry>,
    file_guard: tracing_appender::non_blocking::WorkerGuard,
}

impl LogDrainHandle {
    /// Spawn the async drain task that writes buffered log events into the
    /// SQLite event log.  If `repo` is `None` the channel is closed and events
    /// are discarded (file logging still works).
    ///
    /// Returns a [`FileWriterGuard`] that must be held until the process exits.
    pub fn drain_into(self, repo: Option<Arc<dyn EventLogRepository>>) -> FileWriterGuard {
        let LogDrainHandle { rx, file_guard } = self;
        if let Some(repo) = repo {
            let mut rx = rx;
            tokio::spawn(async move {
                while let Some(entry) = rx.recv().await {
                    let _ = repo
                        .insert(
                            &entry.level,
                            &entry.source,
                            &entry.message,
                            entry.metadata.as_deref(),
                        )
                        .await;
                }
            });
        }
        FileWriterGuard { _guard: file_guard }
    }
}

/// Initialise the global tracing subscriber.
///
/// **Call this once**, early in `main`, before any tracing macros fire.
/// The returned [`LogDrainHandle`] must be kept alive; call
/// [`drain_into`](LogDrainHandle::drain_into) once the database pool is ready.
pub fn init_tracing(debug: bool, data_dir: &Path) -> LogDrainHandle {
    let filter_str = if debug {
        "debug,sqlx=warn,hyper=warn,tower=warn,reqwest=warn,hyper_util=warn,rustls=warn"
    } else {
        "info"
    };
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter_str.into());

    // ── Rolling file appender ────────────────────────────────────────────
    // Produces daily files: <data_dir>/logs/pond.log.YYYY-MM-DD
    let log_dir = data_dir.join("logs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "pond.log");
    let (non_blocking_file, file_guard) = tracing_appender::non_blocking(file_appender);

    // ── Event-log channel (with selective filter) ────────────────────────
    let (tx, rx) = mpsc::unbounded_channel();
    let db_layer = EventLogLayer { tx }.with_filter(TraceFilter);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_file)
                .with_ansi(false),
        )
        .with(db_layer)
        .init();

    LogDrainHandle { rx, file_guard }
}
