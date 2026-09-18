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

/// Initialise the global tracing subscriber.
///
/// **Call this once**, early in `main`, before any tracing macros fire.
/// The returned [`LogDrainHandle`] must be kept alive; call
/// [`drain_into`](LogDrainHandle::drain_into) once the database pool is ready.
pub fn init_tracing(debug: bool, data_dir: &Path) -> LogDrainHandle {
    // Non-interactive commands (serve, setup, status, …) keep the normal INFO
    // console. The interactive `chat` path opts into a quiet console directly.
    init_tracing_with_console(debug, data_dir, ConsoleSink::Stdout, false)
}

/// Where the human-readable console log layer writes.
///
/// `pond-server chat --json-events` uses `Stderr` so stdout carries NOTHING
/// but the NDJSON contract lines; every other command keeps `Stdout`.
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
    // ggml / llama.cpp / whisper.cpp route their verbose per-load and per-token
    // logs into tracing at INFO (via llama-cpp-2's send_logs_to_tracing). On the
    // interactive voice-chat path that dumps the model-load banner and tensor
    // tables straight onto the console, drowning the voice UI. Carve those
    // targets down to WARN so only genuine errors survive; inference is surfaced
    // as a clean one-line summary instead (see ChatService turn completion).
    // `RUST_LOG` still overrides everything for a full-verbosity debug session.
    // The goose crates log the whole agent loop (extension init, session
    // writes, reply spans) at INFO/WARN on every turn. GIAP owns its own
    // telemetry (giap::trace events, the [turn] summary line, turn_metrics),
    // so goose's narration is pure per-turn formatting and I/O cost — carve
    // it down to ERROR. Real goose failures still surface, and `RUST_LOG`
    // restores full goose verbosity for a debug session.
    //
    // These targets are silenced by NAME because they warn about things the
    // user cannot act on and did not ask about: a Metal capability notice,
    // llama.cpp's opinion of a model's token types, ggml's tensor tables.
    // Anything genuinely wrong with GIAP is logged by GIAP.
    //
    // TARGETS THAT MUST NOT BE CARVED: `giap::trace` and `giap::kv`. Both are
    // raised with an explicit `target:` from inside crates silenced here —
    // `giap::kv` narrates the on-disk prompt-snapshot lifecycle from
    // `goose_local_inference`, which this list pins at ERROR. `EnvFilter`
    // matches the TARGET rather than the module, so naming one is what lets a
    // GIAP event escape a carve aimed at its host crate. An earlier version of
    // that logging used the default target and was invisible on every pond it
    // was written for. Adding a `giap*=...` directive below would silently
    // restore that.
    const NOISY: &str = "llama-cpp-2=error,llama_cpp_2=error,ggml=error,\
                         whisper=error,whisper_rs=error,ort=warn,\
                         goose=error,goose_providers=error,\
                         goose_local_inference=error,rmcp=error";

    // What the FILE keeps above `info`. A voice session that misbehaves is
    // diagnosed from these: which windows the wake word matched, what the
    // transcript was before and after the wake word came off, which voice
    // synthesized. They are `debug` because they are per-window and would
    // bury the console — but the file is exactly where they belong, and
    // needing to reproduce a fault with `RUST_LOG=debug` set means the
    // interesting run is always the one that was not recorded.
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

    // Per-layer filters so the console can be quieter than the file. The FILE
    // always gets the full detail; the CONSOLE, on the interactive voice path
    // (`console_quiet`), takes warnings only — the human-facing turn lines are
    // printed directly via `diag!`/`out!`, not through tracing, so they are
    // unaffected.
    //
    // The carve-outs have to be repeated here. A bare `warn` directive is not
    // "warn, keeping the rules above"; it REPLACES them, so every noisy target
    // silenced above came back at WARN on precisely the surface the setting
    // exists to keep quiet. That is where the wall of `llama-cpp-2:
    // control-looking token` and `ggml_metal_device_init` over the voice UI
    // came from.
    //
    // `RUST_LOG`, when set, wins for BOTH so a debug session sees everything.
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

    // Route the console fmt layer to stdout or stderr. In `--json-events` mode
    // stdout is reserved for NDJSON, so diagnostics go to stderr.
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
    /// A `giap::trace` event emitted at `debug!` is never recorded.
    ///
    /// The production file filter is `info,{GIAP_VERBOSE},{NOISY}`, and
    /// `giap::trace` is deliberately not one of the verbose targets — the note
    /// above `NOISY` explains why adding a `giap*=` directive would be a
    /// mistake. The consequence is easy to miss when writing a new event:
    /// `info!` is kept, `debug!` is silently dropped, and the author sees
    /// nothing wrong because the code compiles and the event exists.
    ///
    /// It cost real time. `kind = "prefix_cache_invalidated"` carries the
    /// reason a KV prefix went cold — the single most useful number for
    /// diagnosing voice-turn latency — and was emitted at `debug!`, so it had
    /// never been recorded on any pond. The cache-hit side emitted nothing at
    /// all. Between them the KV hit rate was unobservable, which is how a
    /// cache that misses most turns stays unnoticed.
    ///
    /// This scans for the mistake rather than describing it. The three
    /// grandfathered events below are still invisible; they are listed so the
    /// next person to touch them makes that a decision instead of a discovery.
    #[test]
    fn no_giap_trace_event_is_emitted_at_a_level_the_filter_drops() {
        const GRANDFATHERED: &[&str] = &[
            // Frequent and user-initiated; promote if a Matter fault ever needs
            // diagnosing from a log somebody else captured.
            "matter_device_command",
            "matter_sources_refreshed",
            // Per-turn, and its INFO sibling `turn_device_*` covers the case
            // anyone has needed so far.
            "turn_device_identified",
            // Per-turn context-trim decision. Worth promoting alongside the
            // next piece of compaction work rather than on its own.
            "history_trim_skipped",
            // Fires on every proposals poll on a one-member pond, because that
            // is exactly the shape that reaches the fallthrough: an
            // unidentified session resolves to `Household`, and `sole_member`
            // then names the only person there. At INFO it would be one line
            // per poll for the whole life of the process, which is how a log
            // stops being readable. Its value is in a debug session, where
            // `RUST_LOG` is raised anyway.
            "proposal_caller_sole_member",
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
