//! Sinks: console, daily files in `<data_dir>/logs/` (never pruned), and the SQLite event log.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::field::Visit;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use pond_core::security::domain::redaction::RedactionLevel;
use pond_core::security::ports::event_log::OperationalLogRepository;
use pond_core::security::ports::redactor::Redactor;

// ── Internal channel entry ────────────────────────────────────────────────────

struct ChannelEntry {
    level: String,
    /// tracing target (e.g. `"giap::trace"`, `"pond_server"`)
    source: String,
    message: String,
    /// All structured key-value fields except `message`.
    fields: serde_json::Map<String, serde_json::Value>,
}

// ── What the Logs viewer may keep ─────────────────────────────────────────────

/// Fields whose text is what a household said or was told, or what the model wrote. Call sites
/// log a length instead; this catches the ones that don't, and the Matter controller's relay.
const HOUSEHOLD_CONTENT_FIELDS: &[&str] = &[
    "text",
    "reply",
    "response",
    "transcript",
    "utterance",
    "spoken",
    "prompt",
    "content",
    "title",
    "leaked",
    "line",
    "args",
    "raw",
    "normalized",
    "body",
];

/// Fields a call site may log as text because they explain a fault: hidden like the above on a
/// household's Pond, shown where a developer may see content.
const DEVELOPER_ONLY_FIELDS: &[&str] = &["tool_error"];

/// Ids the Pond and its engine generate (`session_id`, `tool_id`, `goose_sid`): never anything a
/// household said, and what a developer follows a turn by. A random id can look like a key or a
/// phone number to the rule redactor, which would leave a turn's lines with nothing in common.
fn is_generated_id(field: &str) -> bool {
    field == "id" || field.ends_with("_id") || field.ends_with("_sid")
}

const NAME_PLACEHOLDER: &str = "[redacted:name]";

/// The household's members' names, kept current by the server. Matched as whole words in the
/// case they are written, so "Hope" goes and "hope" stays.
#[derive(Clone, Default)]
pub struct HouseholdNames(Arc<std::sync::RwLock<Vec<String>>>);

impl HouseholdNames {
    /// Each display name whole, then each of its words of three letters or more: longest first,
    /// so "Grace Wanjiku" is replaced as one before "Grace" alone is looked for.
    pub fn set<'a>(&self, display_names: impl IntoIterator<Item = &'a str>) {
        let mut names: Vec<String> = Vec::new();
        for full in display_names {
            let full = full.trim();
            for name in std::iter::once(full).chain(full.split_whitespace()) {
                if name.chars().count() >= 3 && !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
        }
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        if let Ok(mut held) = self.0.write() {
            *held = names;
        }
    }

    fn redact(&self, text: &str) -> String {
        let Ok(names) = self.0.read() else {
            return text.to_string();
        };
        names
            .iter()
            .fold(text.to_string(), |acc, name| replace_word(&acc, name))
    }
}

/// `text` with every whole-word `word` replaced by [`NAME_PLACEHOLDER`].
fn replace_word(text: &str, word: &str) -> String {
    let is_boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(word) {
        let before = rest[..at].chars().next_back().or(out.chars().next_back());
        let after = rest[at + word.len()..].chars().next();
        out.push_str(&rest[..at]);
        out.push_str(if is_boundary(before) && is_boundary(after) {
            NAME_PLACEHOLDER
        } else {
            word
        });
        rest = &rest[at + word.len()..];
    }
    out.push_str(rest);
    out
}

/// Scrubs an entry before it reaches the Logs viewer: household content is replaced by its
/// length, members' names by a placeholder, and the rule redactor removes personal data from
/// every remaining string.
#[derive(Clone)]
pub struct LogScrubber {
    redactor: Option<Arc<dyn Redactor>>,
    names: HouseholdNames,
    show_household_content: bool,
}

impl LogScrubber {
    /// Every Pond's setting: only a developer on a test Pond has a reason to see content.
    pub fn household(redactor: Arc<dyn Redactor>, names: HouseholdNames) -> Self {
        Self {
            redactor: Some(redactor),
            names,
            show_household_content: false,
        }
    }

    fn scrub(
        &self,
        message: &str,
        fields: &mut serde_json::Map<String, serde_json::Value>,
    ) -> String {
        if !self.show_household_content {
            for (name, value) in fields.iter_mut() {
                if !HOUSEHOLD_CONTENT_FIELDS.contains(&name.as_str())
                    && !DEVELOPER_ONLY_FIELDS.contains(&name.as_str())
                {
                    continue;
                }
                if let Some(text) = value.as_str() {
                    *value = format!("[redacted:household {} chars]", text.chars().count()).into();
                }
            }
        }
        let message = if self.show_household_content {
            message.to_string()
        } else {
            for (name, value) in fields.iter_mut() {
                if is_generated_id(name) {
                    continue;
                }
                if let Some(text) = value.as_str() {
                    let named = self.names.redact(text);
                    if named != text {
                        *value = named.into();
                    }
                }
            }
            self.names.redact(message)
        };
        let message = message.as_str();
        let Some(redactor) = &self.redactor else {
            return message.to_string();
        };
        for (name, value) in fields.iter_mut() {
            if is_generated_id(name) {
                continue;
            }
            if let Some(text) = value.as_str() {
                let redacted = redactor.redact(text, RedactionLevel::Full);
                if !redacted.findings.is_empty() {
                    *value = redacted.text.into();
                }
            }
        }
        redactor.redact(message, RedactionLevel::Full).text
    }
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
        let meta = event.metadata();
        let _ = self.tx.send(ChannelEntry {
            level: meta.level().to_string(),
            source: meta.target().to_string(),
            message: visitor.message,
            fields: visitor.fields,
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
    pub fn drain_into(
        self,
        repo: Option<Arc<dyn OperationalLogRepository>>,
        scrubber: LogScrubber,
    ) -> FileWriterGuard {
        let LogDrainHandle { rx, file_guard } = self;
        if let Some(repo) = repo {
            let mut rx = rx;
            tokio::spawn(async move {
                while let Some(mut entry) = rx.recv().await {
                    let message = scrubber.scrub(&entry.message, &mut entry.fields);
                    let metadata = if entry.fields.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&entry.fields).ok()
                    };
                    let _ = repo
                        .insert(&entry.level, &entry.source, &message, metadata.as_deref())
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

    // Decided at the top of `main`, before there was a subscriber to hear it.
    crate::node_path::log_decision();

    LogDrainHandle { rx, file_guard }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_infra::rule_redactor::RuleRedactor;
    use serde_json::{json, Map, Value};

    fn fields(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    #[test]
    fn household_content_is_kept_as_its_length_only() {
        let scrubber =
            LogScrubber::household(Arc::new(RuleRedactor::new()), HouseholdNames::default());
        let mut f = fields(json!({
            "text": "remind Grace to take her tablets",
            "session_id": "6f1c2e9a-3b4d-4c5e-9f0a-1b2c3d4e5f60",
            "line": 42,
        }));

        scrubber.scrub("tool call failed", &mut f);

        assert_eq!(f["text"], "[redacted:household 32 chars]");
        // Ids and numbers are what a developer debugs with; a numeric field is never text.
        assert_eq!(f["session_id"], "6f1c2e9a-3b4d-4c5e-9f0a-1b2c3d4e5f60");
        assert_eq!(f["line"], 42);
    }

    #[test]
    fn personal_data_is_redacted_from_the_message_and_every_field() {
        let scrubber =
            LogScrubber::household(Arc::new(RuleRedactor::new()), HouseholdNames::default());
        let mut f = fields(json!({ "error": "no account for amina@example.com" }));

        let message = scrubber.scrub("sign-in failed for amina@example.com", &mut f);

        assert_eq!(message, "sign-in failed for [redacted:email]");
        assert_eq!(f["error"], "no account for [redacted:email]");
    }

    #[test]
    fn a_generated_id_is_kept_even_where_it_looks_like_a_key_or_a_phone_number() {
        let scrubber =
            LogScrubber::household(Arc::new(RuleRedactor::new()), HouseholdNames::default());
        // A tool call's id and a session's, as a Pond logged them and the redactor took them.
        let ids = json!({
            "tool_id": "k4EaZ7ZbU8YZAGuHOaXhOhueinXyb0FQ",
            "session_id": "ede1e522-194e-4f72-a020-7946095858c563b0",
            "goose_sid": "20261030_1",
        });
        let mut f = fields(ids.clone());

        scrubber.scrub("", &mut f);

        assert_eq!(Value::Object(f), ids);
    }

    #[test]
    fn showing_household_content_keeps_it() {
        let scrubber = LogScrubber {
            redactor: None,
            names: HouseholdNames::default(),
            show_household_content: true,
        };
        let mut f = fields(json!({ "reply": "It is 21 degrees outside." }));
        scrubber.scrub("", &mut f);
        assert_eq!(f["reply"], "It is 21 degrees outside.");
    }

    fn names(display_names: &[&str]) -> HouseholdNames {
        let names = HouseholdNames::default();
        names.set(display_names.iter().copied());
        names
    }

    #[test]
    fn a_members_name_is_hidden_wherever_it_appears() {
        let scrubber = LogScrubber::household(
            Arc::new(RuleRedactor::new()),
            names(&["Grace Wanjiku", "Amina"]),
        );
        let mut f = fields(json!({ "error": "no contact called Amina", "device_id": "dev-7" }));

        let message = scrubber.scrub("pairing from Amina's iPhone for Grace Wanjiku", &mut f);

        assert_eq!(
            message,
            "pairing from [redacted:name]'s iPhone for [redacted:name]"
        );
        assert_eq!(f["error"], "no contact called [redacted:name]");
        assert_eq!(f["device_id"], "dev-7");
    }

    #[test]
    fn a_name_inside_a_longer_word_or_in_lower_case_is_left_alone() {
        let n = names(&["Grace", "Hope"]);
        // "Gracefully" is not Grace; "hope" written in lower case is the word, not the member.
        assert_eq!(
            n.redact("Gracefully retried; we hope Grace sees it"),
            "Gracefully retried; we hope [redacted:name] sees it"
        );
    }

    #[test]
    fn a_short_name_is_not_matched_on_its_own() {
        // Two letters match too much ordinary text to hide safely.
        let n = names(&["Jo Kamau"]);
        assert_eq!(
            n.redact("Jo asked; Kamau answered"),
            "Jo asked; [redacted:name] answered"
        );
    }

    #[test]
    fn a_developer_sees_names() {
        let scrubber = LogScrubber {
            redactor: None,
            names: names(&["Amina"]),
            show_household_content: true,
        };
        let mut f = fields(json!({}));
        assert_eq!(scrubber.scrub("Amina paired", &mut f), "Amina paired");
    }

    #[test]
    fn a_tool_error_is_hidden_from_a_household_and_shown_to_a_developer() {
        let error = json!({ "tool_error": "No reminder named 'Grace's tablets'" });

        let mut household = fields(error.clone());
        LogScrubber::household(Arc::new(RuleRedactor::new()), HouseholdNames::default())
            .scrub("", &mut household);
        assert_eq!(household["tool_error"], "[redacted:household 35 chars]");

        let mut developer = fields(error);
        LogScrubber {
            redactor: None,
            names: HouseholdNames::default(),
            show_household_content: true,
        }
        .scrub("", &mut developer);
        assert_eq!(
            developer["tool_error"],
            "No reminder named 'Grace's tablets'"
        );
    }

    /// The scrubber is the net; the rule is that no drained log line writes content at all.
    #[test]
    fn no_drained_log_line_writes_household_content() {
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
                    // `target/` holds generated copies; `tests/` never reaches a household's log.
                    if path
                        .file_name()
                        .is_some_and(|n| n == "target" || n == "tests")
                    {
                        continue;
                    }
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let Ok(src) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    let src = src.split("#[cfg(test)]").next().unwrap_or_default();
                    let shown = path.display().to_string();
                    offenders.extend(
                        content_written_by_drained_logs(src)
                            .into_iter()
                            .map(|(line, field)| format!("{shown}:{line} {field}")),
                    );
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these warn!/error!/giap::trace lines reach the Logs viewer with a household \
             content field as text. Log its length (`text_chars = text.chars().count()`) \
             instead:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// `(line, field)` for each content field a drained macro writes as text.
    fn content_written_by_drained_logs(src: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = src.lines().collect();
        let mut found = Vec::new();
        for (start, line) in lines.iter().enumerate() {
            let drained = line.contains("tracing::warn!(")
                || line.contains("tracing::error!(")
                || (line.contains("tracing::info!(")
                    && lines
                        .get(start + 1)
                        .is_some_and(|next| next.contains("target: \"giap::trace\"")));
            if !drained {
                continue;
            }
            // Follow the macro's own parentheses, so a one-line call ends where it closes.
            let mut depth = 0i32;
            let opening = line.find("!(").map_or(0, |i| i + 1);
            for (offset, body) in lines[start..].iter().enumerate() {
                let counted = if offset == 0 { &body[opening..] } else { body };
                let trimmed = body.trim_start();
                for field in HOUSEHOLD_CONTENT_FIELDS {
                    let as_value = trimmed
                        .strip_prefix(field)
                        .map(str::trim_start)
                        .and_then(|rest| rest.strip_prefix('='))
                        .is_some_and(|rest| {
                            let rest = rest.trim_start();
                            rest.starts_with('%') || rest.starts_with('?')
                        });
                    let shorthand =
                        trimmed == format!("{field},") || trimmed == format!("%{field},");
                    let interpolated = body.contains(&format!("{{{field}}}"))
                        || body.contains(&format!("{{{field}:"));
                    if as_value || shorthand || interpolated {
                        found.push((start + offset + 1, field.to_string()));
                    }
                }
                depth += counted.matches('(').count() as i32;
                depth -= counted.matches(')').count() as i32;
                if depth <= 0 {
                    break;
                }
            }
        }
        found
    }

    #[test]
    fn the_source_scan_sees_each_way_a_field_is_written() {
        let src = "tracing::warn!(\n    text = %text,\n    \"x\"\n);\n\
                   tracing::error!(reply, \"y\");\n\
                   tracing::warn!(\"envelope: {body}\");\n\
                   tracing::info!(\n    target: \"giap::trace\",\n    title = ?title,\n);\n\
                   tracing::warn!(\n    text_chars = text.chars().count(),\n);\n\
                   tracing::debug!(\n    text = %text,\n);\n\
                   Err(e) => tracing::warn!(error = %e, \"push failed\"),\n\
                   format!(\"sent {}\",\n    title,\n);\n";
        let fields: Vec<String> = content_written_by_drained_logs(src)
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        // The one-line `reply,` form is not caught: a shorthand must sit on its own line. The
        // `title,` after a one-line warn belongs to a `format!`, not to the log line.
        assert_eq!(fields, vec!["text", "body", "title"]);
    }

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
            // Fires on every proposals poll on a one-member pond; INFO would flood the log.
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
