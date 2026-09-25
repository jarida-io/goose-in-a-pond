//! Wolfram|Alpha computation for `giap-knowledge`. The AppID comes from the secret store
//! (the settings API serves `Settings` whole) and rides the URL, so log via [`redact_appid`].

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ErrorData},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::knowledge::{clean_query_for_search, KnowledgeMcpServer};

// ── Constants ──────────────────────────────────────────────────────────────

/// Secret-store key holding the Wolfram|Alpha AppID.
pub const WOLFRAM_APP_ID_KEY: &str = "WOLFRAM_APP_ID";

const WOLFRAM_QUERY_BASE: &str = "https://api.wolframalpha.com/v2/query";
const WOLFRAM_SIGNUP_URL: &str = "https://developer.wolframalpha.com/access";

/// Result budget in characters.
const WOLFRAM_BUDGET: usize = 1800;

/// Pods carried in the answer before the rest become explorable suggestions.
const MAX_PODS: usize = 5;

/// Suggestions offered at once; beyond this a small model picks by position, not meaning.
const MAX_EXPLORE: usize = 6;

/// One call to the Full Results API; `base` is injectable so tests can hit a stub.
struct WolframQuery<'a> {
    base: &'a str,
    app_id: &'a str,
    input: &'a str,
    assumption: Option<&'a str>,
    pod_id: Option<&'a str>,
}

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ComputeParams {
    /// The question to compute or look up, in plain words.
    pub query: Option<String>,
    /// Catch-all for any extra fields the model sends (e.g. "topic", "input").
    /// Not part of the advertised schema — exists purely to absorb unexpected
    /// keys, the same way every other tool in this crate does.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExploreParams {
    /// The id of the suggestion to open, exactly as printed in the previous
    /// result (for example "w3").
    pub id: Option<String>,
    /// The original question. Only needed when there is no id to hand.
    pub query: Option<String>,
    /// A Wolfram assumption code, used together with `query` when there is no id.
    pub assumption: Option<String>,
    /// Catch-all for any extra fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

// ── Explorable suggestions ─────────────────────────────────────────────────

/// What kind of refinement a suggestion represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExploreKind {
    /// Wolfram had to guess what a word meant; this is one of the other guesses.
    Assumption,
    /// A section of the result that was not included in the answer.
    Pod,
    /// A different reading of the question Wolfram thinks was intended.
    DidYouMean,
}

impl ExploreKind {
    /// Verb shown to the model and user; kept plain because the model picks by reading it.
    fn verb(self) -> &'static str {
        match self {
            ExploreKind::Assumption => "interpret as",
            ExploreKind::Pod => "show section",
            ExploreKind::DidYouMean => "ask instead",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ExploreKind::Assumption => "assumption",
            ExploreKind::Pod => "pod",
            ExploreKind::DidYouMean => "didyoumean",
        }
    }
}

/// One refinement Wolfram offered alongside an answer.
#[derive(Clone, Debug, PartialEq)]
pub struct ExploreOption {
    pub id: String,
    pub label: String,
    pub kind: ExploreKind,
    /// Input to send: the replacement question for `DidYouMean`, else the original.
    pub query: String,
    pub assumption: Option<String>,
    pub pod_id: Option<String>,
}

static NEXT_EXPLORE_ID: AtomicU64 = AtomicU64::new(1);

/// Number suggestions with process-unique ids (UI list keys); without a session, offer none.
fn with_ids(session: Option<&str>, mut options: Vec<ExploreOption>) -> Vec<ExploreOption> {
    if session.is_none() {
        return Vec::new();
    }
    for opt in &mut options {
        let n = NEXT_EXPLORE_ID.fetch_add(1, Ordering::Relaxed);
        opt.id = format!("w{n}");
    }
    options
}

// ── Tools ──────────────────────────────────────────────────────────────────

#[tool_router(router = wolfram_tool_router, vis = "pub")]
impl KnowledgeMcpServer {
    #[tool(description = "\
Wolfram|Alpha, for anything computed rather than read: arithmetic, unit and \
currency conversion, dates, statistics, science and geography data.")]
    async fn compute_answer(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ComputeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("compute_answer");
        let query = resolve_compute_query(&params.0);
        eprintln!("[wolfram] compute_answer called: query={:?}", query);

        if query.is_empty() {
            return Ok(text_result(
                "I need a question to compute. Retry with a 'query' parameter.",
            ));
        }

        let Some(app_id) = app_id().await else {
            eprintln!("[wolfram] no WOLFRAM_APP_ID configured");
            return Ok(text_result(&crate::format::format_not_configured(
                "Wolfram|Alpha",
                WOLFRAM_SIGNUP_URL,
            )));
        };

        let session = crate::session_from_meta(&ctx.meta);
        self.run_wolfram(
            WolframQuery {
                base: WOLFRAM_QUERY_BASE,
                app_id: &app_id,
                input: &query,
                assumption: None,
                pod_id: None,
            },
            session.as_deref(),
        )
        .await
    }
}

// ── Query execution ────────────────────────────────────────────────────────

impl KnowledgeMcpServer {
    /// Run one Wolfram query and format it for the model and the card.
    async fn run_wolfram(
        &self,
        q: WolframQuery<'_>,
        session: Option<&str>,
    ) -> Result<CallToolResult, ErrorData> {
        let url = build_query_url(&q);
        eprintln!("[wolfram] GET {}", redact_appid(&url));

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(20))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[wolfram] request failed: {}", redact_appid(&e.to_string()));
                return Ok(text_result(&crate::format::format_api_error(
                    "Wolfram|Alpha",
                    &redact_appid(&e.to_string()),
                )));
            }
        };

        // 403 means a bad AppID or exhausted quota; a bare "HTTP 403" reads as a network fault.
        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            eprintln!("[wolfram] HTTP 403 — AppID rejected or over quota");
            return Ok(text_result(
                "Wolfram|Alpha refused the request: the AppID is not valid, or this \
                 month's free query allowance is used up. Tell the user to check the \
                 Wolfram|Alpha key in Settings.",
            ));
        }

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[wolfram] HTTP {status}");
            return Ok(text_result(&crate::format::format_api_error(
                "Wolfram|Alpha",
                &format!("HTTP {status}"),
            )));
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[wolfram] parse failed: {e}");
                return Ok(text_result(&crate::format::format_api_error(
                    "Wolfram|Alpha",
                    &e.to_string(),
                )));
            }
        };

        Ok(text_result(&render_query_result(&body, q.input, session)))
    }
}

/// The Full Results API URL, plaintext only: pods feed a prompt and images double the bytes.
fn build_query_url(q: &WolframQuery<'_>) -> String {
    let mut url = format!(
        "{}?input={}&appid={}&output=json&format=plaintext&podtimeout=8",
        q.base,
        urlencoding::encode(q.input),
        urlencoding::encode(q.app_id),
    );
    if let Some(a) = q.assumption {
        url.push_str(&format!("&assumption={}", urlencoding::encode(a)));
    }
    if let Some(p) = q.pod_id {
        url.push_str(&format!("&includepodid={}", urlencoding::encode(p)));
    }
    url
}

/// Replace the AppID in any string with `appid=REDACTED`. Apply to every log line and
/// tool result: `reqwest`'s error Display includes the URL, key and all.
pub fn redact_appid(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find("appid=") {
        out.push_str(&rest[..idx]);
        out.push_str("appid=REDACTED");
        let after = &rest[idx + "appid=".len()..];
        let end = after.find(['&', ' ', '"']).unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

async fn app_id() -> Option<String> {
    crate::secrets::secret(WOLFRAM_APP_ID_KEY)
        .await
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

fn text_result(s: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s.to_string())])
}

// ── Response rendering ─────────────────────────────────────────────────────

/// One section of a Wolfram answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Pod {
    pub id: String,
    pub title: String,
    pub text: String,
    pub primary: bool,
}

/// Full Results payload as model text, prefixed with the `[[[mcp-ui:wolfram:...]]]` card hint.
pub fn render_query_result(body: &Value, query: &str, session: Option<&str>) -> String {
    let qr = &body["queryresult"];
    let pods = parse_pods(qr);

    if pods.is_empty() {
        // Nothing understood: offer Wolfram's "did you mean", else defer to Wikipedia.
        let suggestions = with_ids(session, parse_didyoumeans(qr, query));
        if !suggestions.is_empty() {
            let body_text = format!(
                "Wolfram|Alpha could not interpret '{}'. This is NOT the answer. It \
                 suggests these readings instead — call \
                 giap-knowledge__compute_answer again with one of them, worded \
                 exactly as written, or giap-knowledge__get_wikipedia_article if \
                 none of them is what the user meant.\n\n{}",
                query,
                render_explore_lines(&suggestions),
            );
            return with_ui_hint(query, None, &[], &suggestions, &body_text);
        }
        return crate::format::format_no_results(
            &format!("a computed answer for '{}'", query),
            &["giap-knowledge__get_wikipedia_article"],
        );
    }

    let (shown, overflow) = pods.split_at(pods.len().min(MAX_PODS));

    let mut lines = Vec::new();
    for pod in shown {
        lines.push(format!("**{}**: {}", pod.title, pod.text));
    }

    let mut options = parse_assumptions(qr, query);
    options.extend(overflow.iter().map(|pod| ExploreOption {
        id: String::new(),
        label: pod.title.clone(),
        kind: ExploreKind::Pod,
        query: query.to_string(),
        assumption: None,
        pod_id: Some(pod.id.clone()),
    }));
    options.truncate(MAX_EXPLORE);
    let options = with_ids(session, options);

    let mut body_text = crate::format::truncate_to_budget(&lines.join("\n"), WOLFRAM_BUDGET);
    if !options.is_empty() {
        body_text.push_str(&format!(
            "\n\nMore is available. To open one, call \
             giap-knowledge__compute_answer again with it, worded exactly as \
             written:\n{}",
            render_explore_lines(&options),
        ));
    }
    body_text.push_str(&format!("\n\nSource: {}", website_url(query)));

    with_ui_hint(query, primary_of(shown), shown, &options, &body_text)
}

fn render_explore_lines(options: &[ExploreOption]) -> String {
    options
        .iter()
        .map(|o| format!("- {} {}: \"{}\"", o.kind.verb(), o.label, o.query))
        .collect::<Vec<_>>()
        .join("\n")
}

fn primary_of(pods: &[Pod]) -> Option<&Pod> {
    pods.iter().find(|p| p.primary).or_else(|| {
        // Often no pod is primary; "Result" is Wolfram's name for the answer pod.
        pods.iter()
            .find(|p| p.id == "Result" || p.title.eq_ignore_ascii_case("result"))
            .or_else(|| pods.iter().find(|p| p.id != "Input"))
    })
}

/// Prepend the MCP-UI hint, sanitised: `extract_ui_hint` ends the marker at the first `]]]`.
fn with_ui_hint(
    query: &str,
    primary: Option<&Pod>,
    pods: &[Pod],
    options: &[ExploreOption],
    body_text: &str,
) -> String {
    let ui = serde_json::json!({
        "query": sanitize(query),
        "primary": primary.map(|p| sanitize(&p.text)),
        "primary_title": primary.map(|p| sanitize(&p.title)),
        "pods": pods.iter()
            .filter(|p| Some(p.id.as_str()) != primary.map(|x| x.id.as_str()))
            .map(|p| serde_json::json!({
                "title": sanitize(&p.title),
                "text": sanitize(&p.text),
            }))
            .collect::<Vec<_>>(),
        "explore": options.iter().map(|o| serde_json::json!({
            "id": o.id,
            "label": sanitize(&o.label),
            "kind": o.kind.as_str(),
            "verb": o.kind.verb(),
        })).collect::<Vec<_>>(),
        "source_url": website_url(query),
    });
    format!("[[[mcp-ui:wolfram:{}]]]\n{}", ui, body_text)
}

/// Neutralise `]]]` so a result cannot cut its own hint short.
fn sanitize(s: &str) -> String {
    s.replace("]]]", "] ] ]")
}

/// The human-facing Wolfram|Alpha page for a query; carries no AppID, so safe to store.
fn website_url(query: &str) -> String {
    format!(
        "https://www.wolframalpha.com/input?i={}",
        urlencoding::encode(query)
    )
}

// ── Payload parsing ────────────────────────────────────────────────────────

/// Wolfram gives a one-element collection as a bare object and several as an array.
fn as_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(_) => vec![v.clone()],
        _ => Vec::new(),
    }
}

pub fn parse_pods(qr: &Value) -> Vec<Pod> {
    as_list(&qr["pods"])
        .into_iter()
        .filter_map(|pod| {
            let title = pod["title"].as_str().unwrap_or("").trim().to_string();
            let id = pod["id"].as_str().unwrap_or("").trim().to_string();
            let text = as_list(&pod["subpods"])
                .iter()
                .filter_map(|sp| {
                    let t = sp["plaintext"].as_str().unwrap_or("").trim();
                    (!t.is_empty()).then(|| t.to_string())
                })
                .collect::<Vec<_>>()
                .join("; ");
            if text.is_empty() || title.is_empty() {
                return None;
            }
            Some(Pod {
                primary: pod["primary"].as_bool().unwrap_or(false),
                id,
                title,
                text,
            })
        })
        // The question's echo is not an answer and would cost a pod slot.
        .filter(|p| p.id != "Input" && p.id != "InputInformation")
        .collect()
}

pub fn parse_assumptions(qr: &Value, query: &str) -> Vec<ExploreOption> {
    let raw = if qr["assumptions"].get("values").is_some() {
        vec![qr["assumptions"].clone()]
    } else {
        as_list(&qr["assumptions"])
    };

    let mut out = Vec::new();
    for group in raw {
        let values = as_list(&group["values"]);
        // The first value is the reading Wolfram already used.
        for v in values.into_iter().skip(1) {
            let Some(input) = v["input"].as_str() else {
                continue;
            };
            let label = v["desc"]
                .as_str()
                .or_else(|| v["name"].as_str())
                .unwrap_or(input)
                .trim()
                .to_string();
            if label.is_empty() {
                continue;
            }
            out.push(ExploreOption {
                id: String::new(),
                label,
                kind: ExploreKind::Assumption,
                query: query.to_string(),
                assumption: Some(input.to_string()),
                pod_id: None,
            });
        }
    }
    out
}

pub fn parse_didyoumeans(qr: &Value, _query: &str) -> Vec<ExploreOption> {
    as_list(&qr["didyoumeans"])
        .into_iter()
        .filter_map(|d| {
            let val = d["val"].as_str()?.trim();
            (!val.is_empty()).then(|| ExploreOption {
                id: String::new(),
                label: val.to_string(),
                kind: ExploreKind::DidYouMean,
                // A "did you mean" replaces the question, so it is the stored query.
                query: val.to_string(),
                assumption: None,
                pod_id: None,
            })
        })
        .collect()
}

// ── Parameter resolution ───────────────────────────────────────────────────

fn resolve_compute_query(params: &ComputeParams) -> String {
    if let Some(q) = params
        .query
        .as_ref()
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
    {
        return q.to_string();
    }
    for key in &[
        "topic",
        "input",
        "question",
        "expression",
        "q",
        "text",
        "search",
    ] {
        if let Some(s) = params.extra.get(*key).and_then(|v| v.as_str()) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    // Last resort: the user's own words, stripped like the Wikipedia tools do.
    let msg = crate::last_user_message();
    if msg.is_empty() {
        return String::new();
    }
    clean_query_for_search(&msg)
}

/// What `explore_computation` was actually asked for.
#[derive(Debug, PartialEq)]
pub enum ExploreTarget {
    Known(Box<ExploreOption>),
    Explicit {
        query: String,
        assumption: Option<String>,
    },
    UnknownId(String),
    Nothing,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const S: Option<&str> = Some("test-session");

    /// A two-pod success, shaped the way the Full Results API returns one.
    fn distance_payload() -> Value {
        json!({"queryresult": {
            "success": true,
            "pods": [
                {"id": "Input", "title": "Input interpretation",
                 "subpods": [{"plaintext": "convert 3 miles to km"}]},
                {"id": "Result", "title": "Result", "primary": true,
                 "subpods": [{"plaintext": "4.828 km"}]},
                {"id": "Comparison", "title": "Comparison",
                 "subpods": [{"plaintext": "about 1.2 x the length of Central Park"}]}
            ]
        }})
    }

    /// A result whose word is ambiguous, so Wolfram offers other readings.
    fn mercury_payload() -> Value {
        json!({"queryresult": {
            "pods": [{"id": "Result", "title": "Result", "primary": true,
                      "subpods": [{"plaintext": "the planet"}]}],
            "assumptions": {
                "type": "Clash", "word": "mercury", "count": 3,
                "values": [
                    {"name": "Planet",    "desc": "a planet",           "input": "*C.mercury-_*Planet-"},
                    {"name": "Element",   "desc": "a chemical element", "input": "*C.mercury-_*Element-"},
                    {"name": "Mythology", "desc": "a Roman god",        "input": "*C.mercury-_*Mythology-"}
                ]
            }
        }})
    }

    // ── Reading a result ──────────────────────────────────────────────────

    #[test]
    fn a_result_leads_with_the_answer_and_drops_the_echoed_question() {
        let pods = parse_pods(&distance_payload()["queryresult"]);
        assert!(!pods.iter().any(|p| p.id == "Input"), "got: {pods:?}");
        assert_eq!(primary_of(&pods).map(|p| p.text.as_str()), Some("4.828 km"));
    }

    #[test]
    fn subpods_are_joined_rather_than_dropped() {
        let body = json!({"queryresult": {"pods": [
            {"id": "Result", "title": "Result", "subpods": [
                {"plaintext": "first"}, {"plaintext": "second"}, {"plaintext": "   "}
            ]}
        ]}});
        let pods = parse_pods(&body["queryresult"]);
        assert_eq!(pods[0].text, "first; second");
    }

    #[test]
    fn the_rendered_answer_carries_a_ui_hint_and_a_keyless_source() {
        let out = render_query_result(&distance_payload(), "3 miles in km", S);
        assert!(out.starts_with("[[[mcp-ui:wolfram:"), "got: {out}");
        assert!(out.contains("4.828 km"));
        assert!(out.contains("wolframalpha.com/input?i="));
        // The website link is persisted, so it must never be the keyed API endpoint.
        assert!(!out.contains("appid"), "got: {out}");
    }

    #[test]
    fn a_single_assumption_group_object_parses_like_a_list() {
        let object_form = json!({"assumptions": {
            "values": [{"input": "a", "desc": "first"}, {"input": "b", "desc": "second"}]
        }});
        let array_form = json!({"assumptions": [{
            "values": [{"input": "a", "desc": "first"}, {"input": "b", "desc": "second"}]
        }]});
        assert_eq!(
            parse_assumptions(&object_form, "q"),
            parse_assumptions(&array_form, "q")
        );
        assert_eq!(parse_assumptions(&object_form, "q").len(), 1);
    }

    #[test]
    fn an_uninterpretable_query_offers_wolframs_own_rewording() {
        let session = Some("s-dym");
        let body = json!({"queryresult": {
            "success": false, "pods": [],
            "didyoumeans": {"score": "0.4", "level": "medium", "val": "integrate x^2"}
        }});
        let out = render_query_result(&body, "integrat x2", session);
        assert!(out.contains("ask instead"), "got: {out}");
        // With no id to resolve, the replacement wording is all the model can act on.
        assert!(out.contains("integrate x^2"), "got: {out}");
        assert!(
            out.contains("giap-knowledge__compute_answer"),
            "the reader must be told which tool to re-ask with; got: {out}"
        );
    }

    #[test]
    fn a_total_miss_hands_off_to_wikipedia_rather_than_apologising() {
        let body = json!({"queryresult": {"success": false, "pods": []}});
        let out = render_query_result(&body, "how is Wangari Maathai remembered", S);
        assert!(out.contains("NOT the answer"), "got: {out}");
        assert!(
            out.contains("giap-knowledge__get_wikipedia_article"),
            "got: {out}"
        );
    }

    #[test]
    fn an_unattributed_call_is_offered_no_ids_at_all() {
        let out = render_query_result(&mercury_payload(), "mercury", None);
        assert!(
            out.contains("the planet"),
            "the answer still comes back: {out}"
        );
        assert!(!out.contains("\n- ["), "no ids may be offered: {out}");
        assert!(!out.contains("explore_computation"), "got: {out}");
    }

    // ── Secret hygiene ────────────────────────────────────────────────────

    #[test]
    fn a_logged_url_never_carries_the_app_id() {
        let url = build_query_url(&WolframQuery {
            base: WOLFRAM_QUERY_BASE,
            app_id: "SECRET-KEY-123",
            input: "2+2",
            assumption: None,
            pod_id: None,
        });
        assert!(
            url.contains("SECRET-KEY-123"),
            "the real call needs the key"
        );
        let safe = redact_appid(&url);
        assert!(!safe.contains("SECRET-KEY-123"), "got: {safe}");
        assert!(safe.contains("appid=REDACTED"), "got: {safe}");
        // Redaction must not eat the rest of the query string.
        assert!(safe.contains("output=json"), "got: {safe}");
        assert!(safe.contains("input=2%2B2"), "got: {safe}");
    }

    #[test]
    fn redaction_survives_the_shapes_an_error_string_arrives_in() {
        // reqwest renders the URL inside a sentence, sometimes quoted.
        let msg = "error sending request for url (https://api.wolframalpha.com/v2/query?input=x&appid=KEY123)";
        assert!(!redact_appid(msg).contains("KEY123"));
        assert!(!redact_appid("\"appid=KEY123\"").contains("KEY123"));
        assert!(!redact_appid("appid=KEY123 failed").contains("KEY123"));
        // A string with no key must come back byte-identical.
        assert_eq!(redact_appid("nothing to hide"), "nothing to hide");
    }

    #[test]
    fn an_assumption_is_carried_into_the_url_it_refines() {
        let url = build_query_url(&WolframQuery {
            base: WOLFRAM_QUERY_BASE,
            app_id: "k",
            input: "mercury",
            assumption: Some("*C.mercury-_*Element-"),
            pod_id: None,
        });
        assert!(
            url.contains("assumption=%2AC.mercury-_%2AElement-"),
            "got: {url}"
        );
        let pod = build_query_url(&WolframQuery {
            base: WOLFRAM_QUERY_BASE,
            app_id: "k",
            input: "mercury",
            assumption: None,
            pod_id: Some("Result"),
        });
        assert!(pod.contains("includepodid=Result"), "got: {pod}");
    }

    // ── Marker safety ─────────────────────────────────────────────────────

    #[test]
    fn a_result_containing_the_marker_terminator_cannot_cut_its_own_hint_short() {
        let body = json!({"queryresult": {"pods": [
            {"id": "Result", "title": "Result", "primary": true,
             "subpods": [{"plaintext": "matrix [[[1,2]]] rows"}]}
        ]}});
        let out = render_query_result(&body, "matrix", S);
        let hint_end = out.find("]]]").expect("hint must terminate");
        let hint = &out["[[[mcp-ui:wolfram:".len()..hint_end];
        serde_json::from_str::<Value>(hint).expect("hint payload must be valid JSON");
    }

    // ── Parameter resolution ──────────────────────────────────────────────

    #[test]
    fn a_query_is_taken_from_whatever_key_the_model_invented() {
        let mut extra = HashMap::new();
        extra.insert("expression".to_string(), json!("17% of 340"));
        let params = ComputeParams { query: None, extra };
        assert_eq!(resolve_compute_query(&params), "17% of 340");
    }

    #[test]
    fn an_explicit_query_beats_the_catch_all() {
        let mut extra = HashMap::new();
        extra.insert("topic".to_string(), json!("wrong"));
        let params = ComputeParams {
            query: Some("  right  ".into()),
            extra,
        };
        assert_eq!(resolve_compute_query(&params), "right");
    }

    /// Regenerates the fixture the pond-api parser test keeps a copy of:
    /// `cargo test -p pond-mcp-server print_a_real_rendered_result -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn print_a_real_rendered_result() {
        let body = json!({"queryresult": {
            "pods": [
                {"id": "Result", "title": "Result", "primary": true,
                 "subpods": [{"plaintext": "4.828 km"}]},
                {"id": "Comparison", "title": "Comparison",
                 "subpods": [{"plaintext": "about 1.2 x the length of Central Park"}]}
            ],
            "assumptions": {"values": [
                {"input": "*C.mile-_*Unit-", "desc": "the unit"},
                {"input": "*C.mile-_*Word-", "desc": "a word"}
            ]}
        }});
        println!(
            "{}",
            render_query_result(&body, "3 miles in km", Some("cap"))
        );
    }

    // ── The tool, end to end ──────────────────────────────────────────────
    // Real socket and MCP dispatch against a stub; no live check against Wolfram yet.

    /// One-shot loopback HTTP server; `std::net` because this crate's tokio lacks `net`.
    fn stub_wolfram(
        status: u16,
        body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let reason = if status == 200 { "OK" } else { "Error" };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    /// The shape the Full Results API documents for a conversion query.
    const REAL_SHAPE: &str = r#"{"queryresult":{"success":true,"error":false,"numpods":3,
        "pods":[
          {"title":"Input interpretation","scanner":"Identity","id":"Input","position":100,
           "subpods":[{"title":"","plaintext":"convert 3 miles to kilometers"}]},
          {"title":"Result","scanner":"Unit","id":"Result","position":200,"primary":true,
           "subpods":[{"title":"","plaintext":"4.828 km (kilometers)"}]},
          {"title":"Unit conversions","scanner":"Unit","id":"UnitConversion","position":300,
           "subpods":[{"title":"","plaintext":"4828 meters"}]}
        ]}}"#;

    #[tokio::test]
    async fn a_real_call_reaches_the_api_and_comes_back_rendered() {
        let (base, requests) = stub_wolfram(200, REAL_SHAPE);
        let server = KnowledgeMcpServer::new(crate::build_http_client());

        let result = server
            .run_wolfram(
                WolframQuery {
                    base: &base,
                    app_id: "TEST-APPID",
                    input: "3 miles in km",
                    assumption: None,
                    pod_id: None,
                },
                Some("s-live"),
            )
            .await
            .expect("the tool must not error on a good response");

        // 1. What actually went on the wire is a request Wolfram would accept.
        let req = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("no request arrived");
        assert!(req.starts_with("GET /"), "got: {req}");
        assert!(req.contains("input=3%20miles%20in%20km"), "got: {req}");
        assert!(req.contains("appid=TEST-APPID"), "got: {req}");
        assert!(req.contains("output=json"), "got: {req}");

        // 2. What came back is the answer, rendered, with a card hint on it.
        let text = only_text(&result);
        assert!(text.starts_with("[[[mcp-ui:wolfram:"), "got: {text}");
        assert!(text.contains("4.828 km"), "got: {text}");
        // Wolfram's echo of the question is not the answer and must not lead.
        assert!(
            !text.contains("convert 3 miles to kilometers"),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn a_rejected_app_id_says_so_instead_of_reporting_a_network_fault() {
        let (base, _rx) = stub_wolfram(403, "Invalid appid");
        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let result = server
            .run_wolfram(
                WolframQuery {
                    base: &base,
                    app_id: "BAD",
                    input: "2+2",
                    assumption: None,
                    pod_id: None,
                },
                Some("s-403"),
            )
            .await
            .unwrap();

        let text = only_text(&result);
        assert!(text.contains("AppID is not valid"), "got: {text}");
        assert!(text.contains("allowance"), "got: {text}");
        assert!(
            !text.contains("BAD"),
            "the key must not be echoed back: {text}"
        );
    }

    #[tokio::test]
    async fn a_broken_response_body_is_reported_rather_than_panicking() {
        let (base, _rx) = stub_wolfram(200, "this is not json");
        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let result = server
            .run_wolfram(
                WolframQuery {
                    base: &base,
                    app_id: "k",
                    input: "2+2",
                    assumption: None,
                    pod_id: None,
                },
                Some("s-junk"),
            )
            .await
            .unwrap();
        assert!(
            only_text(&result).contains("Wolfram|Alpha"),
            "got: {}",
            only_text(&result)
        );
    }

    #[tokio::test]
    async fn a_dead_endpoint_degrades_without_leaking_the_key() {
        // Nothing listens on port 1; reqwest's error text carries the URL, AppID included.
        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let result = server
            .run_wolfram(
                WolframQuery {
                    base: "http://127.0.0.1:1",
                    app_id: "SECRET-KEY-123",
                    input: "2+2",
                    assumption: None,
                    pod_id: None,
                },
                Some("s-dead"),
            )
            .await
            .unwrap();
        let text = only_text(&result);
        assert!(
            !text.contains("SECRET-KEY-123"),
            "the key reached the model: {text}"
        );
    }

    // ── The MCP surface ───────────────────────────────────────────────────

    /// A `RequestContext` carrying an engine session, the way goose stamps one.
    async fn ctx_for(session: Option<&str>) -> RequestContext<RoleServer> {
        use rmcp::model::RequestId;
        let (_client, server_stream) = tokio::io::duplex(64);
        let running = rmcp::service::serve_directly(
            KnowledgeMcpServer::new(crate::build_http_client()),
            server_stream,
            None,
        );
        let mut ctx = RequestContext::new(RequestId::Number(0), running.peer().clone());
        if let Some(s) = session {
            ctx.meta.0.insert(
                crate::SESSION_ID_META_KEY.to_string(),
                Value::String(s.to_string()),
            );
        }
        ctx
    }

    #[tokio::test]
    async fn both_tools_are_actually_exposed_by_the_server() {
        use rmcp::handler::server::ServerHandler;
        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let listed = server
            .list_tools(None, ctx_for(None).await)
            .await
            .expect("list_tools must succeed");
        let names: Vec<&str> = listed.tools.iter().map(|t| t.name.as_ref()).collect();

        // Wolfram's #[tool_router] is composed in `new()`; losing that would still compile.
        assert!(names.contains(&"compute_answer"), "got: {names:?}");
        // And the ones this file's sibling owns are still there.
        assert!(names.contains(&"get_wikipedia_article"), "got: {names:?}");
        assert_eq!(names.len(), 2, "giap-knowledge serves two tools: {names:?}");
    }

    #[tokio::test]
    async fn the_advertised_schema_is_one_a_model_can_fill_in() {
        use rmcp::handler::server::ServerHandler;
        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let listed = server.list_tools(None, ctx_for(None).await).await.unwrap();
        let tool = listed
            .tools
            .iter()
            .find(|t| t.name == "compute_answer")
            .expect("compute_answer must be listed");

        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert_eq!(schema["type"], "object", "got: {schema}");
        assert!(schema["properties"]["query"].is_object(), "got: {schema}");
        // `extra` is #[schemars(skip)] so it never reaches the prompt.
        assert!(schema["properties"]["extra"].is_null(), "got: {schema}");
        assert!(
            tool.description
                .as_ref()
                .is_some_and(|d| d.contains("Wolfram")),
            "the description is what the model routes on"
        );
    }

    #[tokio::test]
    async fn calling_the_tool_without_a_key_names_the_signup_page() {
        use rmcp::handler::server::ServerHandler;
        use rmcp::model::CallToolRequestParams;

        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "compute_answer",
            "arguments": {"query": "3 miles in km"},
        }))
        .unwrap();

        // No secret store in a test binary: the real out-of-the-box, no-key path.
        let result = server
            .call_tool(params, ctx_for(Some("s-nokey")).await)
            .await
            .expect("a missing key is not a tool error");
        let text = only_text(&result);
        assert!(text.contains("Wolfram|Alpha"), "got: {text}");
        assert!(text.contains("developer.wolframalpha.com"), "got: {text}");
    }

    #[tokio::test]
    async fn an_unknown_tool_name_is_refused_by_the_router() {
        use rmcp::handler::server::ServerHandler;
        use rmcp::model::CallToolRequestParams;
        let server = KnowledgeMcpServer::new(crate::build_http_client());
        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "compute_answers",
            "arguments": {},
        }))
        .unwrap();
        // Vacuity guard: a router that accepted anything would prove nothing.
        assert!(server.call_tool(params, ctx_for(None).await).await.is_err());
    }

    /// The single text block out of a tool result.
    fn only_text(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .expect("tool results in this module are a single text block")
    }
}
