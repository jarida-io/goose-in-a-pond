//! Network-egress tracker: records and privacy-classifies outbound HTTP calls, and gates them.
//! Lives in `pond-core` so any adapter can report here; adapters report, core owns the policy.

use std::sync::{Arc, OnceLock, RwLock};

use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::security::ports::event_log::EventLog;

// ── Request context ──────────────────────────────────────────────────────────

static CURRENT_SESSION_ID: RwLock<String> = RwLock::new(String::new());
static CURRENT_TOOL: RwLock<String> = RwLock::new(String::new());

/// Record the session ID for the turn currently in flight.
pub fn set_current_session_id(sid: &str) {
    if let Ok(mut guard) = CURRENT_SESSION_ID.write() {
        *guard = sid.to_string();
    }
}

/// The session ID for the in-flight turn, or an empty string if none is set.
pub fn current_session_id() -> String {
    CURRENT_SESSION_ID
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Record the built-in tool about to run, so its egress can be attributed.
pub fn set_current_tool(tool: &str) {
    if let Ok(mut guard) = CURRENT_TOOL.write() {
        *guard = tool.to_string();
    }
}

/// The tool currently in flight, or an empty string if none is set.
pub fn current_tool() -> String {
    CURRENT_TOOL.read().map(|g| g.clone()).unwrap_or_default()
}

// ── Sink ─────────────────────────────────────────────────────────────────────

/// Where egress is recorded; while unset (tests, standalone adapters) recording is a no-op.
static EGRESS_SINK: OnceLock<Arc<dyn EventLog>> = OnceLock::new();

/// Install the egress sink; the first call wins, later ones are ignored.
pub fn set_egress_sink(sink: Arc<dyn EventLog>) {
    let _ = EGRESS_SINK.set(sink);
}

fn egress_sink() -> Option<Arc<dyn EventLog>> {
    EGRESS_SINK.get().cloned()
}

// ── Network mode ─────────────────────────────────────────────────────────────

/// How hard egress is gated, from `settings.network_mode`; tiers reuse [`classify_host`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// Record every outbound call, refuse none.
    Open,
    /// Refuse hosts that classify as `Sensitive`.
    Allowlist,
    /// Refuse everything that is not loopback.
    Offline,
}

impl NetworkMode {
    /// Parse, defaulting to [`NetworkMode::Open`]: a typo must not silently cut the pond off.
    /// Unknown values are refused with 422 at `PUT /api/v1/settings` instead.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allowlist" => Self::Allowlist,
            "offline" => Self::Offline,
            "open" => Self::Open,
            other => {
                tracing::warn!(
                    target: "giap::trace",
                    value = %other,
                    "unrecognised network_mode; falling back to \"open\""
                );
                Self::Open
            }
        }
    }

    /// The stored spelling, for log lines and event attributes.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Allowlist => "allowlist",
            Self::Offline => "offline",
        }
    }
}

static NETWORK_MODE: RwLock<NetworkMode> = RwLock::new(NetworkMode::Open);

/// Install the network mode; also called on every settings change, so no restart is needed.
pub fn set_network_mode(mode: NetworkMode) {
    if let Ok(mut guard) = NETWORK_MODE.write() {
        *guard = mode;
    }
}

/// The network mode currently in force.
pub fn network_mode() -> NetworkMode {
    NETWORK_MODE.read().map(|g| *g).unwrap_or(NetworkMode::Open)
}

/// An outbound call the network mode refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressDenied {
    /// The destination host, as [`extract_host`] saw it.
    pub host: String,
    pub mode: NetworkMode,
    /// Why, in words the user can act on.
    pub reason: &'static str,
}

impl std::fmt::Display for EgressDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "network_mode = \"{}\" refused an outbound request to {}: {}",
            self.mode.as_str(),
            self.host,
            self.reason
        )
    }
}

impl std::error::Error for EgressDenied {}

/// The pure gate. An unparseable host becomes `"unknown"`, which is `Sensitive`: failure narrows.
pub fn egress_verdict(host: &str, mode: NetworkMode) -> Result<(), &'static str> {
    let sensitivity = classify_host(host);
    match mode {
        NetworkMode::Open => Ok(()),
        NetworkMode::Allowlist => {
            if sensitivity == PrivacySensitivity::Sensitive {
                Err("host is not a loopback or curated public API; \
                     set network_mode to \"open\" to permit it")
            } else {
                Ok(())
            }
        }
        NetworkMode::Offline => {
            if sensitivity == PrivacySensitivity::Internal {
                Ok(())
            } else {
                Err("network_mode is \"offline\"; only loopback destinations are permitted")
            }
        }
    }
}

/// Check one outbound URL against the network mode BEFORE the request is made.
/// Refusals are recorded as `egress.denied`, fully attributed, keeping the host's sensitivity.
pub fn check_egress(url: &str) -> Result<(), EgressDenied> {
    let mode = network_mode();
    let host = extract_host(url);
    match egress_verdict(host, mode) {
        Ok(()) => Ok(()),
        Err(reason) => {
            let denied = EgressDenied {
                host: host.to_string(),
                mode,
                reason,
            };
            tracing::warn!(
                target: "giap::trace",
                kind = "egress_denied",
                host = %denied.host,
                mode = %mode.as_str(),
                "{denied}"
            );
            append_event(denied_event(
                &denied,
                &current_tool(),
                &current_session_id(),
            ));
            Err(denied)
        }
    }
}

/// Build the `Network` event for one refusal; takes tool and session so it stays pure.
pub fn denied_event(denied: &EgressDenied, tool: &str, session_id: &str) -> Event {
    let mut event = Event::new(EventCategory::Network, "egress.denied")
        .attr("host", denied.host.as_str())
        .attr("network_mode", denied.mode.as_str())
        .attr("reason", denied.reason)
        .sensitivity(classify_host(&denied.host));
    if !tool.is_empty() {
        event = event.attr("tool", tool);
    }
    if !session_id.is_empty() {
        event = event.session(session_id);
    }
    event
}

/// Append fire-and-forget, only if a runtime is running: the gate may be called from sync code.
fn append_event(event: Event) {
    let Some(sink) = egress_sink() else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            target: "giap::trace",
            "no tokio runtime on this thread; egress event not persisted"
        );
        return;
    };
    handle.spawn(async move {
        if let Err(e) = sink.append(event).await {
            tracing::warn!(target: "giap::trace", error = %e, "failed to record egress event");
        }
    });
}

/// One gated outbound call for new adapters: the gate, then the timer, then the record.
/// An `Err` from `begin` means the request must not be made; dropping it records nothing.
#[must_use = "an EgressCall that is never finished records no egress"]
pub struct EgressCall {
    url: String,
    method: &'static str,
    started: std::time::Instant,
}

/// Open a gated outbound call to `url`. See [`EgressCall`].
pub fn begin(url: &str, method: &'static str) -> Result<EgressCall, EgressDenied> {
    check_egress(url)?;
    Ok(EgressCall {
        url: url.to_string(),
        method,
        started: std::time::Instant::now(),
    })
}

impl EgressCall {
    /// Record the completed call. `None` means the request never got a status.
    pub fn finish(self, status: Option<u16>) {
        let latency_ms = self.started.elapsed().as_millis() as u64;
        record_egress(&self.url, self.method, status, latency_ms);
    }
}

// ── Recording ──────────────────────────────────────────────────────────────--

/// Record one outbound call against the in-flight tool/session; appends fire-and-forget.
pub fn record_egress(url: &str, method: &str, status: Option<u16>, latency_ms: u64) {
    let Some(sink) = egress_sink() else {
        return;
    };
    let host = extract_host(url).to_string();
    let tool = current_tool();
    let session_id = current_session_id();
    let event = egress_event(&host, &tool, &session_id, method, status, latency_ms);
    tokio::spawn(async move {
        if let Err(e) = sink.append(event).await {
            tracing::warn!(target: "giap::trace", error = %e, "failed to record egress event");
        }
    });
}

/// Build the `Network` event for one outbound call; pure, so it is testable without a sink.
pub fn egress_event(
    host: &str,
    tool: &str,
    session_id: &str,
    method: &str,
    status: Option<u16>,
    latency_ms: u64,
) -> Event {
    let mut event = Event::new(EventCategory::Network, "egress.http")
        .attr("host", host)
        .attr("method", method)
        .attr("latency_ms", latency_ms as i64)
        .sensitivity(classify_host(host));
    if !tool.is_empty() {
        event = event.attr("tool", tool);
    }
    if !session_id.is_empty() {
        event = event.session(session_id);
    }
    if let Some(code) = status {
        event = event.attr("status", code as i64);
    }
    event
}

// ── Privacy classification ─────────────────────────────────────────────────--

/// Public, read-only APIs the built-in tools call; matched as exact host or subdomain.
const KNOWN_PUBLIC_SUFFIXES: &[&str] = &[
    "wikipedia.org",
    "wikimedia.org",
    "dictionaryapi.dev",
    "wolframalpha.com",
    "openlibrary.org",
    "restcountries.com",
    "openfoodfacts.org",
    "frankfurter.dev",
    "finnhub.io",
    "coingecko.com",
    "finance.yahoo.com",
    "hacker-news.firebaseio.com",
    "ycombinator.com",
    "guardianapis.com",
    "theguardian.com",
    "gnews.io",
    "open-meteo.com",
];

/// Privacy class of a host: loopback `Internal`, known public API `Public`, else `Sensitive`.
pub fn classify_host(host: &str) -> PrivacySensitivity {
    let h = host.trim().to_ascii_lowercase();
    if is_loopback(&h) {
        return PrivacySensitivity::Internal;
    }
    if KNOWN_PUBLIC_SUFFIXES
        .iter()
        .any(|s| h == *s || h.ends_with(&format!(".{s}")))
    {
        return PrivacySensitivity::Public;
    }
    PrivacySensitivity::Sensitive
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host == "127.0.0.1"
        || host.starts_with("127.")
        || host.ends_with(".localhost")
}

/// Extract the bare host from a URL (no scheme, port, or path).
pub fn extract_host(url: &str) -> &str {
    url.find("://")
        .map(|i| &url[i + 3..])
        .and_then(|s| s.split('/').next())
        .and_then(|h| h.split(':').next())
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Network mode ────────────────────────────────────────────────────────

    /// Keep recording loopback: the push-relay guard's calls go to a loopback mock.
    #[test]
    fn loopback_is_classified_internal_and_still_recorded() {
        assert_eq!(classify_host("127.0.0.1"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("localhost"), PrivacySensitivity::Internal);
        assert_ne!(
            classify_host("gpu-box.tailnet.example"),
            PrivacySensitivity::Internal
        );
        // The event carries the classification, so a reader can filter on it.
        let event = egress_event("127.0.0.1", "", "", "LLM", Some(200), 5);
        assert_eq!(event.privacy_sensitivity, PrivacySensitivity::Internal);
    }

    #[test]
    fn network_mode_matrix_covers_every_mode_and_classification() {
        // Real hosts, so a KNOWN_PUBLIC_SUFFIXES change fails this test rather than sliding past.
        let internal = "127.0.0.1";
        let public = "en.wikipedia.org";
        let sensitive = "tracker.example.com";
        assert_eq!(classify_host(internal), PrivacySensitivity::Internal);
        assert_eq!(classify_host(public), PrivacySensitivity::Public);
        assert_eq!(classify_host(sensitive), PrivacySensitivity::Sensitive);

        // Open refuses nothing.
        for h in [internal, public, sensitive] {
            assert!(
                egress_verdict(h, NetworkMode::Open).is_ok(),
                "open must permit {h}"
            );
        }

        // Allowlist refuses exactly what classifies Sensitive.
        assert!(
            egress_verdict(internal, NetworkMode::Allowlist).is_ok(),
            "allowlist must permit loopback"
        );
        assert!(
            egress_verdict(public, NetworkMode::Allowlist).is_ok(),
            "allowlist must permit a curated public API"
        );
        assert!(
            egress_verdict(sensitive, NetworkMode::Allowlist).is_err(),
            "allowlist must refuse a Sensitive host"
        );

        // Offline permits only loopback -- including the public APIs.
        assert!(
            egress_verdict(internal, NetworkMode::Offline).is_ok(),
            "offline must permit loopback"
        );
        assert!(
            egress_verdict(public, NetworkMode::Offline).is_err(),
            "offline must refuse even a curated public API"
        );
        assert!(
            egress_verdict(sensitive, NetworkMode::Offline).is_err(),
            "offline must refuse a Sensitive host"
        );
    }

    #[test]
    fn an_unparseable_url_is_refused_by_both_restrictive_modes() {
        // extract_host gives up and says "unknown", which classifies Sensitive.
        let host = extract_host("not a url");
        assert_eq!(host, "unknown");
        assert!(
            egress_verdict(host, NetworkMode::Allowlist).is_err(),
            "an unparseable host must not be permitted under allowlist"
        );
        assert!(
            egress_verdict(host, NetworkMode::Offline).is_err(),
            "an unparseable host must not be permitted under offline"
        );

        // Known limitation: bracketed IPv6 yields host "[", so it is refused (fails closed).
        let v6 = extract_host("http://[::1]:8080/health");
        assert!(
            egress_verdict(v6, NetworkMode::Offline).is_err(),
            "bracketed IPv6 loopback is not recognised today; it must fail CLOSED"
        );
    }

    #[test]
    fn unrecognised_network_mode_parses_as_open_not_as_a_restriction() {
        assert_eq!(NetworkMode::parse("open"), NetworkMode::Open);
        assert_eq!(NetworkMode::parse("allowlist"), NetworkMode::Allowlist);
        assert_eq!(NetworkMode::parse("offline"), NetworkMode::Offline);
        assert_eq!(NetworkMode::parse("  OFFLINE "), NetworkMode::Offline);

        assert_eq!(
            NetworkMode::parse("offlien"),
            NetworkMode::Open,
            "a typo must not silently restrict the network"
        );
        assert_eq!(NetworkMode::parse(""), NetworkMode::Open);

        // Round-trip, so as_str and parse cannot drift.
        for m in [
            NetworkMode::Open,
            NetworkMode::Allowlist,
            NetworkMode::Offline,
        ] {
            assert_eq!(NetworkMode::parse(m.as_str()), m);
        }
    }

    #[test]
    fn a_refused_call_is_recorded_with_its_reason() {
        let denied = EgressDenied {
            host: "tracker.example.com".to_string(),
            mode: NetworkMode::Offline,
            reason: "network_mode is \"offline\"; only loopback destinations are permitted",
        };
        let ev = denied_event(&denied, "search_web", "sess-denied");

        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.action, "egress.denied");
        assert_eq!(
            ev.attributes.get("host"),
            Some(&"tracker.example.com".into())
        );
        assert_eq!(ev.attributes.get("network_mode"), Some(&"offline".into()));
        assert_eq!(ev.attributes.get("reason"), Some(&denied.reason.into()));
        assert_eq!(ev.attributes.get("tool"), Some(&"search_web".into()));
        assert_eq!(ev.session_id.as_deref(), Some("sess-denied"));
        // Still `Sensitive`, so the retention rules for this class still apply to it.
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Sensitive);
        // The message a user actually sees has to name the setting and the host.
        let rendered = denied.to_string();
        assert!(
            rendered.contains("network_mode") && rendered.contains("tracker.example.com"),
            "a refusal must be actionable, got: {rendered}"
        );
    }

    #[test]
    fn extracts_host_from_url() {
        assert_eq!(
            extract_host("https://en.wikipedia.org/w/api.php?x=1"),
            "en.wikipedia.org"
        );
        assert_eq!(extract_host("http://127.0.0.1:8080/foo"), "127.0.0.1");
        assert_eq!(extract_host("not a url"), "unknown");
    }

    #[test]
    fn classifies_loopback_as_internal() {
        assert_eq!(classify_host("localhost"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("127.0.0.1"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("::1"), PrivacySensitivity::Internal);
        assert_eq!(
            classify_host("searxng.localhost"),
            PrivacySensitivity::Internal
        );
    }

    #[test]
    fn classifies_known_apis_as_public() {
        for host in [
            "en.wikipedia.org",
            "api.coingecko.com",
            "hacker-news.firebaseio.com",
            "api.open-meteo.com",
            "geocoding-api.open-meteo.com",
        ] {
            assert_eq!(
                classify_host(host),
                PrivacySensitivity::Public,
                "{host} should be Public"
            );
        }
        assert_eq!(
            classify_host("EN.WIKIPEDIA.ORG"),
            PrivacySensitivity::Public
        );
    }

    #[test]
    fn classifies_unknown_hosts_as_sensitive() {
        assert_eq!(
            classify_host("tracker.example.com"),
            PrivacySensitivity::Sensitive
        );
        // A look-alike suffix must not match (no substring/false-positive).
        assert_eq!(
            classify_host("notwikipedia.org.evil.com"),
            PrivacySensitivity::Sensitive
        );
    }

    #[test]
    fn egress_event_shape_and_classification() {
        let ev = egress_event(
            "en.wikipedia.org",
            "search_wikipedia",
            "sess-1",
            "GET",
            Some(200),
            42,
        );
        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.action, "egress.http");
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(ev.session_id.as_deref(), Some("sess-1"));
        assert_eq!(ev.attributes.get("host"), Some(&"en.wikipedia.org".into()));
        assert_eq!(ev.attributes.get("tool"), Some(&"search_wikipedia".into()));
        assert_eq!(ev.attributes.get("method"), Some(&"GET".into()));
        assert_eq!(ev.attributes.get("status"), Some(&200_i64.into()));
        assert_eq!(ev.attributes.get("latency_ms"), Some(&42_i64.into()));
    }

    #[test]
    fn egress_event_omits_empty_fields_and_missing_status() {
        let ev = egress_event("tracker.example.com", "", "", "GET", None, 5);
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Sensitive);
        assert!(ev.session_id.is_none());
        assert!(!ev.attributes.contains_key("tool"));
        assert!(!ev.attributes.contains_key("status"));
    }

    #[tokio::test]
    async fn record_egress_appends_to_sink_with_context() {
        use crate::security::domain::event::EventQuery;
        use crate::security::ports::event_log::EventLog;
        use async_trait::async_trait;
        use std::sync::Mutex;

        struct CapturingLog(Arc<Mutex<Vec<Event>>>);
        #[async_trait]
        impl EventLog for CapturingLog {
            async fn append(&self, event: Event) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
            async fn query(&self, _q: EventQuery) -> anyhow::Result<Vec<Event>> {
                Ok(self.0.lock().unwrap().clone())
            }
            async fn purge(&self, _q: EventQuery) -> anyhow::Result<u64> {
                Ok(0)
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        set_egress_sink(Arc::new(CapturingLog(captured.clone())));
        set_current_session_id("sess-xyz");
        set_current_tool("get_current_weather");

        record_egress(
            "https://api.open-meteo.com/v1/forecast?x=1",
            "GET",
            Some(200),
            12,
        );

        // record_egress spawns the append; give it a moment to land.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.lock().unwrap().clone();
        let ev = events
            .iter()
            .find(|e| e.attributes.get("host") == Some(&"api.open-meteo.com".into()))
            .expect("egress event recorded");
        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(ev.session_id.as_deref(), Some("sess-xyz"));
        assert_eq!(
            ev.attributes.get("tool"),
            Some(&"get_current_weather".into())
        );
        assert_eq!(ev.attributes.get("status"), Some(&200_i64.into()));
    }
}
