//! A tool result the model reads during its turn and history never keeps: the tool marks it in
//! `CallToolResult._meta`, and every store of tool results keeps the marker's stub instead.
//! Web search and its pictures are such tools: Brave's terms (3(b)(i)) allow Search Results only
//! transient storage.

use serde_json::{Map, Value};

/// The `_meta` key a transient result carries; its value is `{"keep": "<stub>"}`.
pub const TRANSIENT_META_KEY: &str = "giap/transient";

/// The field under [`TRANSIENT_META_KEY`] holding what history keeps.
pub const KEEP_FIELD: &str = "keep";

/// Domains a web search stub names, at most.
pub const MAX_STUB_DOMAINS: usize = 5;

/// A domain as a stub names it, at most; past it a name is not a plain host.
const MAX_STUB_DOMAIN_BYTES: usize = 63;

const WEB_SEARCH_KEPT: &str = "Web search results are not kept after the turn.";

const WEB_PICTURES_KEPT: &str = "Pictures from the web are not kept after the turn.";

/// What history keeps of a web search: the sources' domains and nothing they said.
pub fn web_search_stub<S: AsRef<str>>(domains: &[S]) -> String {
    stub_naming(WEB_SEARCH_KEPT, domains)
}

/// What history keeps of pictures shown from the web: the sites they came from.
pub fn web_pictures_stub<S: AsRef<str>>(domains: &[S]) -> String {
    stub_naming(WEB_PICTURES_KEPT, domains)
}

fn stub_naming<S: AsRef<str>>(kept: &str, domains: &[S]) -> String {
    let mut named: Vec<&str> = Vec::new();
    for domain in domains.iter().map(|d| d.as_ref().trim()) {
        if named.len() == MAX_STUB_DOMAINS {
            break;
        }
        if is_plain_domain(domain) && !named.contains(&domain) {
            named.push(domain);
        }
    }
    if named.is_empty() {
        kept.to_string()
    } else {
        format!("{kept} Sources: {}.", named.join(", "))
    }
}

/// A host name and nothing else, so no page text can ride the stub in place of a domain.
fn is_plain_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= MAX_STUB_DOMAIN_BYTES
        && domain.contains('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// The `_meta` object marking a result transient, keeping `keep`.
pub fn transient_meta(keep: &str) -> Map<String, Value> {
    let mut marker = Map::new();
    marker.insert(KEEP_FIELD.to_string(), Value::String(keep.to_string()));
    let mut meta = Map::new();
    meta.insert(TRANSIENT_META_KEY.to_string(), Value::Object(marker));
    meta
}

/// The stub a result's `_meta` asks history to keep; `None` for a result history keeps whole.
pub fn keep_from_meta(meta: &Map<String, Value>) -> Option<String> {
    meta.get(TRANSIENT_META_KEY)?
        .get(KEEP_FIELD)?
        .as_str()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

/// One stored tool result, as an engine's store holds it, seen by the rule below.
#[derive(Debug, Clone, Default)]
pub struct StoredResult<'a> {
    pub meta: Option<&'a Map<String, Value>>,
    /// Each content block's text, `None` for a block that is not text.
    pub blocks: Vec<Option<&'a str>>,
    pub has_structured_content: bool,
}

/// The stub `result` must be reduced to before it may stay in a store: one text block holding
/// the stub, no structured content, its marker kept so the next look sees it done. `None` when
/// it stays as it is: it carries no marker, or it is its stub already.
pub fn stub_owed(result: &StoredResult<'_>) -> Option<String> {
    let keep = keep_from_meta(result.meta?)?;
    let reduced = !result.has_structured_content
        && result.blocks.len() == 1
        && result.blocks[0] == Some(keep.as_str());
    (!reduced).then_some(keep)
}

/// Where a turn stands when its engine's store could be swept of transient results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepPoint {
    /// Before the turn's first reply; catches what a crash or a missed sweep left.
    TurnStart,
    /// The turn ended on its own.
    TurnEnd { saw_transient: bool },
    /// The turn was dropped unfinished: cancelled, or it returned early.
    TurnDropped { newer_turn_began: bool },
}

/// Whether the store is swept at `point`. A finished turn that saw no transient result has
/// nothing to sweep, as its start cleared any earlier one. A dropped turn sweeps even so, as the
/// engine may have stored a result the stream never yielded, unless a newer turn has the
/// conversation: that turn sweeps at its start, and a rewrite under it would race its writes.
pub fn sweep_due(point: SweepPoint) -> bool {
    match point {
        SweepPoint::TurnStart => true,
        SweepPoint::TurnEnd { saw_transient } => saw_transient,
        SweepPoint::TurnDropped { newer_turn_began } => !newer_turn_began,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(stub: &str) -> Map<String, Value> {
        transient_meta(stub)
    }

    #[test]
    fn a_marked_result_owes_its_stub_until_it_is_one() {
        let stub = "Web search results are not kept after the turn. Sources: kws.go.ke.";
        let meta = marked(stub);
        let whole = StoredResult {
            meta: Some(&meta),
            blocks: vec![
                Some("Web results for: q\nThe park closes at 6 pm."),
                Some("[[[mcp-ui:search:{}]]]"),
            ],
            has_structured_content: false,
        };
        assert_eq!(stub_owed(&whole).as_deref(), Some(stub));

        let reduced = StoredResult {
            meta: Some(&meta),
            blocks: vec![Some(stub)],
            has_structured_content: false,
        };
        assert_eq!(stub_owed(&reduced), None, "a reduced result is left alone");

        for not_yet in [
            StoredResult {
                meta: Some(&meta),
                blocks: vec![Some(stub)],
                has_structured_content: true,
            },
            StoredResult {
                meta: Some(&meta),
                blocks: vec![Some(stub), None],
                has_structured_content: false,
            },
            StoredResult {
                meta: Some(&meta),
                blocks: vec![None],
                has_structured_content: false,
            },
            StoredResult {
                meta: Some(&meta),
                blocks: Vec::new(),
                has_structured_content: false,
            },
        ] {
            assert_eq!(stub_owed(&not_yet).as_deref(), Some(stub), "{not_yet:?}");
        }
    }

    #[test]
    fn an_unmarked_result_owes_nothing() {
        let other = serde_json::json!({"goose": {"mcpApp": {}}});
        for result in [
            StoredResult {
                meta: None,
                blocks: vec![Some("24C and clear")],
                has_structured_content: false,
            },
            StoredResult {
                meta: other.as_object(),
                blocks: vec![Some("24C and clear")],
                has_structured_content: true,
            },
        ] {
            assert_eq!(stub_owed(&result), None, "{result:?}");
        }
    }

    #[test]
    fn sweeps_are_due_as_the_turn_ends() {
        assert!(sweep_due(SweepPoint::TurnStart));
        assert!(sweep_due(SweepPoint::TurnEnd {
            saw_transient: true
        }));
        assert!(!sweep_due(SweepPoint::TurnEnd {
            saw_transient: false
        }));
        assert!(sweep_due(SweepPoint::TurnDropped {
            newer_turn_began: false
        }));
        assert!(!sweep_due(SweepPoint::TurnDropped {
            newer_turn_began: true
        }));
    }

    #[test]
    fn the_stub_names_domains_only() {
        assert_eq!(
            web_search_stub(&["kenyapower.co.ke", "nation.africa"]),
            "Web search results are not kept after the turn. Sources: kenyapower.co.ke, \
             nation.africa."
        );
        assert_eq!(
            web_search_stub::<&str>(&[]),
            "Web search results are not kept after the turn."
        );
    }

    #[test]
    fn the_stub_drops_repeats_and_anything_that_is_not_a_domain() {
        let stub = web_search_stub(&[
            "nation.africa",
            "nation.africa",
            "Junction Mall closes at 9pm",
            "https://evil.example/path",
            "",
            "localhost",
            "kws.go.ke",
        ]);
        assert_eq!(
            stub,
            "Web search results are not kept after the turn. Sources: nation.africa, kws.go.ke."
        );
    }

    #[test]
    fn a_pictures_stub_names_the_sites_by_the_same_rule() {
        assert_eq!(
            web_pictures_stub(&["commons.wikimedia.org", "not a domain", "kws.go.ke"]),
            "Pictures from the web are not kept after the turn. Sources: commons.wikimedia.org, \
             kws.go.ke."
        );
        assert_eq!(
            web_pictures_stub::<&str>(&[]),
            "Pictures from the web are not kept after the turn."
        );
    }

    #[test]
    fn the_stub_names_at_most_five() {
        let domains: Vec<String> = (0..9).map(|i| format!("s{i}.example")).collect();
        let stub = web_search_stub(&domains);
        assert!(stub.contains("s4.example"), "{stub}");
        assert!(!stub.contains("s5.example"), "{stub}");
    }

    #[test]
    fn the_marker_round_trips() {
        let meta = transient_meta("Web search results are not kept after the turn.");
        assert_eq!(
            Value::Object(meta.clone()),
            serde_json::json!({"giap/transient": {"keep": "Web search results are not kept after the turn."}})
        );
        assert_eq!(
            keep_from_meta(&meta).as_deref(),
            Some("Web search results are not kept after the turn.")
        );
    }

    #[test]
    fn other_meta_is_not_transient() {
        let other = serde_json::json!({"goose": {"x": 1}, "giap/transient": {"keep": "  "}});
        assert_eq!(keep_from_meta(other.as_object().unwrap()), None);
        let wrong = serde_json::json!({"giap/transient": "keep"});
        assert_eq!(keep_from_meta(wrong.as_object().unwrap()), None);
    }
}
