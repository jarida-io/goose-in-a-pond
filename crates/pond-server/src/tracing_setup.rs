//! Sinks: console, daily files in `<data_dir>/logs/` (never pruned), and the SQLite event log.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::field::Visit;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use pond_core::security::ports::event_log::OperationalLogRepository;

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

/// Event-log filter: WARN+ from anywhere, plus INFO from `giap::trace*` targets.
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

/// Keeps the file writer thread alive; dropping it flushes buffered logs.
pub struct FileWriterGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Keep alive for the process; call [`drain_into`](Self::drain_into) once the DB is ready.
pub struct LogDrainHandle {
    rx: UnboundedReceiver<ChannelEntry>,
    file_guard: tracing_appender::non_blocking::WorkerGuard,
}

impl LogDrainHandle {
    /// Spawn the DB drain (`None` discards events); hold the returned guard until exit.
    pub fn drain_into(self, repo: Option<Arc<dyn OperationalLogRepository>>) -> FileWriterGuard {
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

/// Install the global subscriber; call once, early in `main`, before any event fires.
pub fn init_tracing(debug: bool, data_dir: &Path) -> LogDrainHandle {
    // `chat` calls `init_tracing_with_console` itself for a quiet console.
    init_tracing_with_console(debug, data_dir, ConsoleSink::Stdout, false)
}

/// Console log destination; `chat --json-events` needs `Stderr`, as stdout is NDJSON only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleSink {
    Stdout,
    Stderr,
}

pub fn init_tracing_with_console(
    debug: bool,
    data_dir: &Path,
    console: ConsoleSink,
    console_quiet: bool,
) -> LogDrainHandle {
    // Mutes targets that narrate what the user can't act on; `RUST_LOG` overrides. Never add
    // `giap*=`: `giap::trace`/`giap::kv` escape these carves only via their explicit `target:`.
    const NOISY: &str = "llama-cpp-2=error,llama_cpp_2=error,ggml=error,\
                         whisper=error,whisper_rs=error,ort=warn,\
                         goose=error,goose_providers=error,\
                         goose_local_inference=error,rmcp=error";

    // Per-window voice debug detail kept in the file, so a fault is on record without a repro.
    const GIAP_VERBOSE: &str = "pond_adapters_whisper=debug,pond_adapters_piper=debug,\
                                pond_core::shared::services::chat=debug,\
                                pond_server=debug";

    let filter_str = if debug {
        format!(
            "debug,sqlx=warn,hyper=warn,tower=warn,reqwest=warn,hyper_util=warn,rustls=warn,\
             llama-cpp-2=warn,llama_cpp_2=warn,ggml=warn,whisper=warn,whisper_rs=warn,ort=warn,\
             goose=error,goose_providers=error,goose_local_inference=error,rmcp=error"
        )
    } else {
        format!("info,{GIAP_VERBOSE},{NOISY}")
    };
    let rust_log_set = std::env::var("RUST_LOG").is_ok();

    // The quiet console must repeat `NOISY`: a bare `warn` directive replaces the carves
    // rather than adding to them. `RUST_LOG`, when set, wins for both layers.
    let file_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| filter_str.as_str().into());
    let console_filter = if console_quiet && !rust_log_set {
        tracing_subscriber::EnvFilter::new(format!("warn,{NOISY}"))
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| filter_str.as_str().into())
    };

    // ── Rolling file appender ────────────────────────────────────────────
    // Produces daily files: <data_dir>/logs/pond.log.YYYY-MM-DD
    let log_dir = data_dir.join("logs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "pond.log");
    let (non_blocking_file, file_guard) = tracing_appender::non_blocking(file_appender);

    // ── Event-log channel (with selective filter) ────────────────────────
    let (tx, rx) = mpsc::unbounded_channel();
    let db_layer = EventLogLayer { tx }.with_filter(TraceFilter);

    let console_layer = match console {
        ConsoleSink::Stdout => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout as fn() -> std::io::Stdout)
            .with_filter(console_filter)
            .boxed(),
        ConsoleSink::Stderr => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr as fn() -> std::io::Stderr)
            .with_filter(console_filter)
            .boxed(),
    };

    tracing_subscriber::registry()
        .with(console_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_file)
                .with_ansi(false)
                .with_filter(file_filter),
        )
        .with(db_layer)
        .init();

    LogDrainHandle { rx, file_guard }
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_giap_trace_event_is_emitted_at_a_level_the_filter_drops() {
        const GRANDFATHERED: &[&str] = &[
            // Frequent and user-initiated; promote if a Matter fault needs a captured log.
            "matter_device_command",
            "matter_sources_refreshed",
            // Per-turn; its INFO sibling `turn_device_*` covers what's been needed.
            "turn_device_identified",
            // Per-turn trim decision; promote with the next compaction work.
            "history_trim_skipped",
        ];

        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ is the parent of this crate");

        let mut offenders = Vec::new();
        let mut stack = vec![crates_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // `target/` holds generated copies of the same sources.
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let Ok(src) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let lines: Vec<&str> = src.lines().collect();
                    for (i, line) in lines.iter().enumerate() {
                        if !line.contains("target: \"giap::trace\"") {
                            continue;
                        }
                        // The macro name sits on the line above the target.
                        let emitted_at_debug = i
                            .checked_sub(1)
                            .is_some_and(|p| lines[p].contains("tracing::debug!"));
                        if !emitted_at_debug {
                            continue;
                        }
                        // Which event is it? The `kind` follows the target.
                        let kind = lines[i + 1..]
                            .iter()
                            .take(3)
                            .find_map(|l| l.split("kind = \"").nth(1))
                            .and_then(|k| k.split('"').next())
                            .unwrap_or("<unknown>")
                            .to_string();
                        if !GRANDFATHERED.contains(&kind.as_str()) {
                            offenders.push(format!("{}:{} kind={kind:?}", path.display(), i + 1));
                        }
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these `giap::trace` events are emitted at `debug!`, which the \
             production filter drops — they will never appear in a log. Use \
             `info!`, or add the kind to GRANDFATHERED with a reason:\n  {}",
            offenders.join("\n  ")
        );
    }
}
