//! `show_pictures` for `giap-knowledge`: one Brave image search with the household's key, the
//! pictures to the user's card, and one plain line to the model, which never sees them. Marked
//! transient like `search_web` (small-model-tools.md section 8.11).

use std::collections::HashMap;

use pond_core::mcp::domain::transient_result::{transient_meta, web_pictures_stub};
use pond_core::mcp::domain::web_search::{
    is_brave_thumbnail, WebSearchStatus, BRAVE_API_BASE, BRAVE_IMAGE_SEARCH_PATH, BRAVE_TIMEOUT,
    BRAVE_TOKEN_HEADER,
};
use pond_core::mcp::services::web_search_status::{self, WebSearchStatusBoard};
use pond_core::shared::services::passages;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{Annotations, CallToolResult, ContentBlock, ErrorData, Meta, Role, TextContent},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{
    brave_client, brave_key, brave_query, defang, describe_failure, domain_of, echo, plain_text,
    sanitize, text_result, without_urls, BRAVE_MAX_QUERY_CHARS, BRAVE_MAX_QUERY_WORDS,
    MAX_TITLE_BYTES,
};
use crate::format::{format_api_error, format_dead_end};
use crate::knowledge::KnowledgeMcpServer;

const TOOL_NAME: &str = "show_pictures";

/// Pictures asked of Brave: more than are shown, so the ones without a proxied thumbnail or a web
/// page can be dropped.
const PICTURES_ASKED: usize = 20;

/// Pictures a card shows, at most.
const MAX_PICTURES: usize = 12;

/// Sites the model's line names before "and N other sites".
const SITES_NAMED: usize = 3;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ShowPicturesParams {
    /// What the user wants to see, in their own words. For a picture they attached, say what is
    /// in it.
    // Advertised as a required string; deserialised leniently so `extra` can rescue an old name.
    #[schemars(with = "String")]
    pub question: Option<String>,
    /// Catch-all for the names a model may remember instead ("query", "q", "topic").
    /// Not part of the advertised schema.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, Value>,
}

/// What the user wants to see, from `question` or a remembered name for it.
fn resolve_request(params: &ShowPicturesParams) -> String {
    let named = params.question.as_deref().map(str::trim);
    if let Some(q) = named.filter(|q| !q.is_empty()) {
        return q.to_string();
    }
    ["query", "q", "topic"]
        .iter()
        .filter_map(|key| params.extra.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|q| !q.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
}

#[tool_router(router = pictures_tool_router, vis = "pub")]
impl KnowledgeMcpServer {
    #[tool(description = "\
Show the user pictures from the web of what they want to see: a place, an animal, a plant, a \
dish, a product. The pictures appear on their screen; you will not see them.")]
    async fn show_pictures(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ShowPicturesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool(TOOL_NAME);
        let request = resolve_request(&params.0);
        if request.is_empty() {
            return Ok(text_result(
                "I need what the user wants to see. Retry this tool with a 'question' parameter \
                 carrying it in their own words.",
            ));
        }
        let Some(key) = brave_key().await else {
            return Ok(text_result(&not_set_up()));
        };
        let Some(brave) = brave_client() else {
            return Ok(text_result(&failed("the search client could not be built")));
        };
        Ok(PictureSearch {
            brave_base: BRAVE_API_BASE,
            brave,
            board: web_search_status::global(),
        }
        .run(&key, &request)
        .await)
    }
}

fn not_set_up() -> String {
    format_dead_end(
        "pictures for this request",
        "Web search is not set up on this pond.",
    )
}

fn failed(why: &str) -> String {
    format_api_error("Picture search", why)
}

/// One picture as the card shows it.
#[derive(Debug, Clone, PartialEq)]
struct Picture {
    title: String,
    /// The page the picture is on: what tapping it opens.
    url: String,
    domain: String,
    /// Brave's proxied thumbnail, never the original image.
    thumbnail: String,
    width: Option<u64>,
    height: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct ImageResponse {
    #[serde(default)]
    results: Vec<Value>,
    #[serde(default)]
    extra: Option<ImageExtra>,
}

#[derive(Debug, Default, Deserialize)]
struct ImageExtra {
    #[serde(default)]
    might_be_offensive: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawImage {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    thumbnail: Option<RawThumbnail>,
}

#[derive(Debug, Default, Deserialize)]
struct RawThumbnail {
    #[serde(default)]
    src: Option<String>,
    #[serde(default)]
    width: Option<u64>,
    #[serde(default)]
    height: Option<u64>,
}

/// What one image search came to.
#[derive(Debug, PartialEq)]
enum Found {
    Pictures(Vec<Picture>),
    /// Brave flagged the set as possibly offensive, so none is shown.
    Withheld,
    None,
}

/// One picture search. `brave_base` and the client are injectable so tests hit stubs.
struct PictureSearch<'a> {
    brave_base: &'a str,
    brave: &'a reqwest::Client,
    board: &'a WebSearchStatusBoard,
}

impl PictureSearch<'_> {
    async fn run(&self, key: &str, request: &str) -> CallToolResult {
        let Some(url) = image_search_url(self.brave_base, request) else {
            return text_result(&failed("the search address could not be built"));
        };
        let response = crate::http::traced_get_with(self.brave, url.as_str(), |b| {
            b.header(BRAVE_TOKEN_HEADER, key)
                .header(reqwest::header::ACCEPT, "application/json")
                .timeout(BRAVE_TIMEOUT)
        })
        .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => return text_result(&failed(&describe_failure(&e))),
        };
        let status = response.status().as_u16();
        let verdict = WebSearchStatus::from_brave_http(status);
        if let Some(verdict) = verdict {
            self.board.record(key, verdict);
        }
        match verdict {
            Some(WebSearchStatus::Ok) => {}
            Some(WebSearchStatus::Rejected) => {
                return text_result(&format_dead_end(
                    "pictures for this request",
                    "The web search key was rejected.",
                ))
            }
            Some(WebSearchStatus::Quota) => {
                return text_result(&format_dead_end(
                    "pictures for this request",
                    "This month's web search allowance is used up.",
                ))
            }
            _ => return text_result(&failed(&format!("it answered HTTP {status}"))),
        }
        let Ok(body) = response.json::<ImageResponse>().await else {
            return text_result(&failed("its answer could not be read"));
        };
        let found = pictures_of(body);
        tracing::info!(
            target: "giap::trace",
            kind = "web_pictures",
            shown = match &found {
                Found::Pictures(p) => p.len(),
                _ => 0,
            },
            withheld = matches!(found, Found::Withheld),
            "pictures searched"
        );
        result_for(request, found)
    }
}

/// The image search request: strict filtering, Brave's default too, and worldwide, since `KE` is
/// not among `country`'s values and pictures are rarely about where the household is.
fn image_search_url(base: &str, request: &str) -> Option<reqwest::Url> {
    let q = brave_query(request, BRAVE_MAX_QUERY_WORDS, BRAVE_MAX_QUERY_CHARS);
    let count = PICTURES_ASKED.to_string();
    reqwest::Url::parse_with_params(
        &format!("{}{BRAVE_IMAGE_SEARCH_PATH}", base.trim_end_matches('/')),
        [
            ("q", q.as_str()),
            ("count", count.as_str()),
            ("safesearch", "strict"),
            ("country", "ALL"),
        ],
    )
    .ok()
}

/// The pictures a card may show: a proxied thumbnail and a web page each, at most
/// [`MAX_PICTURES`]; none when Brave flags the set.
fn pictures_of(body: ImageResponse) -> Found {
    if body
        .extra
        .and_then(|e| e.might_be_offensive)
        .unwrap_or(false)
    {
        return Found::Withheld;
    }
    let pictures: Vec<Picture> = body
        .results
        .into_iter()
        .filter_map(|value| serde_json::from_value::<RawImage>(value).ok())
        .filter_map(picture)
        .take(MAX_PICTURES)
        .collect();
    if pictures.is_empty() {
        Found::None
    } else {
        Found::Pictures(pictures)
    }
}

fn picture(raw: RawImage) -> Option<Picture> {
    let thumb = raw.thumbnail?;
    let thumbnail = thumb.src?.trim().to_string();
    if !is_brave_thumbnail(&thumbnail) {
        return None;
    }
    let url = raw.url?.trim().to_string();
    let parsed = reqwest::Url::parse(&url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let domain = domain_of(&url);
    let source = raw
        .source
        .map(|s| plain_text(&s))
        .map(|s| s.trim_start_matches("www.").to_string())
        .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace));
    let title = passages::clip(
        &defang(&without_urls(&plain_text(raw.title.as_deref().unwrap_or("")))),
        MAX_TITLE_BYTES,
    );
    Some(Picture {
        title,
        url,
        domain: source.unwrap_or(domain),
        thumbnail,
        width: thumb.width,
        height: thumb.height,
    })
}

fn result_for(request: &str, found: Found) -> CallToolResult {
    let pictures = match found {
        Found::Pictures(pictures) => pictures,
        Found::Withheld => {
            return text_result(&format_dead_end(
                &format!("pictures for \"{}\"", echo(request)),
                "The pictures found may be offensive, so none are shown.",
            ))
        }
        Found::None => {
            return text_result(&format_dead_end(
                &format!("pictures for \"{}\"", echo(request)),
                "The search found none to show.",
            ))
        }
    };
    let sites = sites_of(&pictures);
    let mut result = CallToolResult::success(vec![
        ContentBlock::text(model_line(request, pictures.len(), &sites)),
        card_block(request, &pictures),
    ]);
    result.meta = Some(Meta(transient_meta(&web_pictures_stub(&sites))));
    result
}

/// The sites the pictures came from, in order, each once.
fn sites_of(pictures: &[Picture]) -> Vec<String> {
    let mut sites: Vec<String> = Vec::new();
    for picture in pictures {
        if !picture.domain.is_empty() && !sites.contains(&picture.domain) {
            sites.push(picture.domain.clone());
        }
    }
    sites
}

/// What the model reads: that the pictures are on the screen, and from where. No title, so
/// there is nothing to describe as if it had seen them.
fn model_line(request: &str, shown: usize, sites: &[String]) -> String {
    let (count, verb) = if shown == 1 {
        ("1 picture".to_string(), "is")
    } else {
        (format!("{shown} pictures"), "are")
    };
    let named: Vec<String> = sites
        .iter()
        .take(SITES_NAMED)
        .map(|s| defang(s))
        .collect();
    let others = sites.len().saturating_sub(SITES_NAMED);
    let from = match (named.as_slice(), others) {
        ([], _) => String::new(),
        ([one], 0) => format!(", from {one}"),
        (many, 0) => format!(
            ", from {} and {}",
            many[..many.len() - 1].join(", "),
            many[many.len() - 1]
        ),
        (many, 1) => format!(", from {} and 1 other site", many.join(", ")),
        (many, n) => format!(", from {} and {n} other sites", many.join(", ")),
    };
    format!(
        "{count} for \"{}\" {verb} on the user's screen{from}. You have not seen them.",
        echo(request)
    )
}

/// The `[[[mcp-ui:pictures:{...}]]]` card, for the user only: goose drops such blocks before the
/// provider.
fn card_block(request: &str, pictures: &[Picture]) -> ContentBlock {
    let card = serde_json::json!({
        "query": sanitize(request),
        "pictures": pictures.iter().map(|p| serde_json::json!({
            "title": sanitize(&p.title),
            "url": sanitize(&p.url),
            "domain": sanitize(&p.domain),
            "thumbnail": sanitize(&p.thumbnail),
            "width": p.width,
            "height": p.height,
        })).collect::<Vec<_>>(),
    });
    ContentBlock::Text(
        TextContent::new(format!("[[[mcp-ui:pictures:{card}]]]"))
            .with_annotations(Annotations::default().with_audience(vec![Role::User])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    const THUMB: &str = "https://imgs.search.brave.com/p/rs:fit:500:0:0/g:ce/aHR0cHM6";

    fn image(title: &str, page: &str, source: &str, thumb: &str) -> Value {
        serde_json::json!({
            "type": "image_result",
            "title": title,
            "url": page,
            "source": source,
            "thumbnail": {"src": thumb, "width": 500, "height": 333},
            "properties": {"url": "https://upload.example/original.jpg", "width": 3000, "height": 2000},
        })
    }

    fn body(results: Vec<Value>, offensive: bool) -> String {
        serde_json::json!({
            "type": "images",
            "query": {"original": "x"},
            "results": results,
            "extra": {"might_be_offensive": offensive},
        })
        .to_string()
    }

    /// A one-route stub answering `requests` connections with `status` and `body`.
    fn stub(status: u16, body: String) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = vec![0u8; 16 * 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        });
        (port, rx)
    }

    fn fresh_board() -> &'static WebSearchStatusBoard {
        Box::leak(Box::new(WebSearchStatusBoard::new()))
    }

    async fn search_against(port: u16, board: &'static WebSearchStatusBoard) -> CallToolResult {
        let base = format!("http://127.0.0.1:{port}");
        PictureSearch {
            brave_base: &base,
            brave: brave_client().expect("the Brave client"),
            board,
        }
        .run("test-key-123", "Show me pictures of the Maasai Mara")
        .await
    }

    fn text_of(result: &CallToolResult, block: usize) -> String {
        result.content[block]
            .as_text()
            .map(|t| t.text.clone())
            .expect("a text block")
    }

    fn agent_visible_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text(t) => {
                    let audience = t.annotations.as_ref().and_then(|a| a.audience.as_ref());
                    audience
                        .is_none_or(|roles| roles.contains(&Role::Assistant))
                        .then(|| t.text.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_schema_asks_for_one_required_question() {
        let tool = KnowledgeMcpServer::pictures_tool_router()
            .list_all()
            .into_iter()
            .find(|t| t.name == TOOL_NAME)
            .expect("show_pictures is routed");
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert_eq!(schema["required"], serde_json::json!(["question"]), "{schema}");
        assert_eq!(schema["properties"]["question"]["type"], "string", "{schema}");
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 1, "only `question` is advertised: {schema}");
    }

    #[test]
    fn the_description_names_the_act_not_the_backend() {
        let tools = KnowledgeMcpServer::pictures_tool_router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == TOOL_NAME)
            .expect("show_pictures is listed");
        let description = tool.description.as_deref().unwrap_or_default().to_lowercase();
        assert!(description.starts_with("show the user pictures"), "{description}");
        for vendor in ["brave", "google", "bing", "duckduckgo"] {
            assert!(!description.contains(vendor), "{vendor} in {description}");
        }
    }

    #[test]
    fn remembered_names_for_the_request_are_absorbed() {
        let params: ShowPicturesParams =
            serde_json::from_value(serde_json::json!({"query": "  pangolins "})).unwrap();
        assert_eq!(resolve_request(&params), "pangolins");
        let empty: ShowPicturesParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(resolve_request(&empty), "");
    }

    #[tokio::test]
    async fn the_request_is_strict_worldwide_and_keyed() {
        let (port, requests) = stub(200, body(vec![], false));
        let _ = search_against(port, fresh_board()).await;
        let head = requests.recv().unwrap();
        let line = head.lines().next().unwrap();
        assert!(line.starts_with("GET /res/v1/images/search?"), "{line}");
        for param in [
            "q=Show+me+pictures+of+the+Maasai+Mara",
            "count=20",
            "safesearch=strict",
            "country=ALL",
        ] {
            assert!(line.contains(param), "{param} missing: {line}");
        }
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("x-subscription-token: test-key-123"), "{head}");
        assert!(!lower.contains("x-loc-"), "{head}");
    }

    #[tokio::test]
    async fn pictures_go_to_the_card_and_the_model_reads_one_plain_line() {
        let results = vec![
            image("Wildebeest crossing <b>the Mara</b>", "https://www.kws.go.ke/mara", "www.kws.go.ke", THUMB),
            image("Lions at dawn", "https://commons.wikimedia.org/lions", "commons.wikimedia.org", THUMB),
            image("Unproxied", "https://evil.example/page", "evil.example", "https://evil.example/img.jpg"),
            image("Not a web page", "javascript:alert(1)", "x.example", THUMB),
            image("Balloon safari", "https://magicalkenya.com/balloon", "magicalkenya.com", THUMB),
            image("Mara river", "https://nation.africa/mara", "nation.africa", THUMB),
        ];
        let (port, _requests) = stub(200, body(results, false));
        let board = fresh_board();
        let result = search_against(port, board).await;
        assert_eq!(board.status_for(Some("test-key-123")), WebSearchStatus::Ok);

        let line = agent_visible_text(&result);
        assert_eq!(
            line,
            "4 pictures for \"Show me pictures of the Maasai Mara\" are on the user's screen, \
             from kws.go.ke, commons.wikimedia.org, magicalkenya.com and 1 other site. You have \
             not seen them."
        );
        assert!(!line.contains("Lions"), "no title reaches the model: {line}");

        let card = text_of(&result, 1);
        let json: Value = serde_json::from_str(
            card.trim_start_matches("[[[mcp-ui:pictures:").trim_end_matches("]]]"),
        )
        .unwrap();
        let pictures = json["pictures"].as_array().unwrap();
        assert_eq!(pictures.len(), 4, "{json}");
        assert_eq!(pictures[0]["title"], "Wildebeest crossing the Mara");
        assert_eq!(pictures[0]["url"], "https://www.kws.go.ke/mara");
        assert_eq!(pictures[0]["thumbnail"], THUMB);
        assert_eq!(pictures[0]["width"], 500);
        assert!(!card.contains("evil.example"), "{card}");
        assert!(!card.contains("upload.example"), "no original image: {card}");

        let stub = result
            .meta
            .as_ref()
            .and_then(|m| pond_core::mcp::domain::transient_result::keep_from_meta(&m.0))
            .expect("marked transient");
        assert_eq!(
            stub,
            "Pictures from the web are not kept after the turn. Sources: kws.go.ke, \
             commons.wikimedia.org, magicalkenya.com, nation.africa."
        );
    }

    #[tokio::test]
    async fn a_set_brave_flags_as_offensive_shows_nothing() {
        let (port, _requests) = stub(
            200,
            body(vec![image("A", "https://a.example/", "a.example", THUMB)], true),
        );
        let result = search_against(port, fresh_board()).await;
        assert_eq!(result.content.len(), 1, "no card");
        assert!(result.meta.is_none(), "nothing of Brave's to keep");
        assert!(text_of(&result, 0).contains("may be offensive"));
    }

    #[tokio::test]
    async fn no_picture_to_show_is_a_dead_end_not_a_card() {
        let (port, _requests) = stub(200, body(vec![], false));
        let result = search_against(port, fresh_board()).await;
        assert_eq!(result.content.len(), 1);
        assert!(text_of(&result, 0).contains("found none to show"));
    }

    #[tokio::test]
    async fn each_brave_status_maps_as_web_search_does() {
        for (status, told, verdict) in [
            (401, "key was rejected", Some(WebSearchStatus::Rejected)),
            (403, "key was rejected", Some(WebSearchStatus::Rejected)),
            (402, "allowance is used up", Some(WebSearchStatus::Quota)),
            (429, "allowance is used up", Some(WebSearchStatus::Quota)),
            (500, "temporarily unavailable", None),
        ] {
            let (port, _requests) = stub(status, "{}".to_string());
            let board = fresh_board();
            let result = search_against(port, board).await;
            let text = text_of(&result, 0);
            assert!(text.contains(told), "{status}: {text}");
            assert!(!text.contains("127.0.0.1"), "{status}: {text}");
            if let Some(verdict) = verdict {
                assert_eq!(board.status_for(Some("test-key-123")), verdict, "{status}");
            }
        }
    }

    #[tokio::test]
    async fn an_unreachable_brave_names_no_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let started = std::time::Instant::now();
        let text = text_of(&search_against(port, fresh_board()).await, 0);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(text.contains("Picture search is temporarily unavailable"), "{text}");
        assert!(!text.contains("Maasai"), "the address carries the request: {text}");
    }

    #[test]
    fn the_line_counts_and_names_sites_in_plain_words() {
        let sites = |n: usize| (0..n).map(|i| format!("s{i}.example")).collect::<Vec<_>>();
        assert_eq!(
            model_line("lions", 1, &sites(1)),
            "1 picture for \"lions\" is on the user's screen, from s0.example. You have not \
             seen them."
        );
        assert!(model_line("lions", 2, &sites(2)).contains("from s0.example and s1.example."));
        assert!(model_line("lions", 9, &sites(6)).contains("and 3 other sites."));
        assert!(!model_line("[[[mcp-ui:x:{}]]]", 2, &sites(2)).contains("[[["));
    }
}
