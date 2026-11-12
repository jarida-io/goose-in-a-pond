//! `web_watch` for `giap-knowledge`: start, list or stop a watch on a page that lists things, such
//! as a car site's results, or watch one product's price. The pond checks it in code on a schedule
//! and tells the member when something they asked for appears or drops in price.

use std::sync::{Arc, OnceLock};

use chrono::{Duration, Utc};
use pond_core::mcp::services::page_addresses;
use pond_core::shared::domain::cart::{money_words, Notify};
use pond_core::shared::domain::web_watch::{
    WatchFilters, WatchItem, DEFAULT_INTERVAL_HOURS, MAX_INTERVAL_HOURS, MAX_WATCHES_PER_MEMBER,
    MIN_INTERVAL_HOURS,
};
use pond_core::shared::ports::cart::ModelUse;
use pond_core::shared::ports::web_watch::{WebWatch, WebWatchRepository};
use pond_core::shared::services::cart_watch::{self, AddRefusal, NewCartItem};
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ErrorData, Meta},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::format::format_dead_end;
use crate::knowledge::KnowledgeMcpServer;
use crate::page_data::Item;
use crate::web_page::{read_watched, FetchFailure};

const TOOL_NAME: &str = "web_watch";

/// Items a reply names as matching now.
const SHOWN_MATCHES: usize = 3;

#[derive(Clone)]
pub struct WebWatchDeps {
    pub repo: Arc<dyn WebWatchRepository>,
    /// The registry the goose adapter publishes each live turn into: who is asking.
    pub authorities: Arc<TurnAuthorityRegistry>,
}

static WEB_WATCH_DEPS: OnceLock<WebWatchDeps> = OnceLock::new();

/// Install the deps once, at startup; later calls are ignored.
pub fn init_web_watch_deps(deps: WebWatchDeps) {
    let _ = WEB_WATCH_DEPS.set(deps);
}

// Undocumented fields and plain types on purpose: this tool is in every turn's tool block.
// `default` keeps every field out of the schema's `required`; a small model asks for each one listed.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(transform = crate::without_null_defaults)]
pub struct WebWatchParams {
    /// start, list, stop, or watch_price for one product's page
    #[schemars(with = "String", extend("default" = "start"))]
    pub action: Option<String>,
    #[schemars(with = "String")]
    pub url: Option<String>,
    /// Words each match's title has, space-separated
    #[schemars(with = "String")]
    pub words: Option<Value>,
    #[schemars(with = "f64")]
    pub max_price: Option<Value>,
    #[schemars(with = "u32")]
    pub min_year: Option<Value>,
    #[schemars(with = "f64")]
    pub max_mileage: Option<Value>,
    #[schemars(with = "u32", extend("default" = 6))]
    pub every_hours: Option<Value>,
    #[schemars(with = "String")]
    pub watch_id: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => pond_core::shared::domain::web_watch::number_in(s),
        _ => None,
    }
    .filter(|n| n.is_finite() && *n >= 0.0)
}

fn text(value: Option<&String>) -> String {
    value.map(|s| s.trim().to_string()).unwrap_or_default()
}

impl WebWatchParams {
    fn action(&self) -> &str {
        match self.action.as_deref().map(str::trim) {
            Some(a) if a.eq_ignore_ascii_case("list") => "list",
            Some(a)
                if ["watch_price", "price", "cart"]
                    .iter()
                    .any(|s| a.eq_ignore_ascii_case(s)) =>
            {
                "watch_price"
            }
            Some(a)
                if ["stop", "delete", "remove", "cancel"]
                    .iter()
                    .any(|s| a.eq_ignore_ascii_case(s)) =>
            {
                "stop"
            }
            _ => "start",
        }
    }

    fn filters(&self) -> WatchFilters {
        let words = match &self.words {
            Some(Value::Array(words)) => words
                .iter()
                .filter_map(Value::as_str)
                .map(|w| w.trim().to_lowercase())
                .filter(|w| !w.is_empty())
                .collect(),
            Some(Value::String(words)) => words
                .split([',', ' '])
                .map(|w| w.trim().to_lowercase())
                .filter(|w| !w.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        WatchFilters {
            words,
            max_price: number(self.max_price.as_ref()),
            min_year: number(self.min_year.as_ref()).map(|y| y as u32),
            max_mileage: number(self.max_mileage.as_ref()),
        }
    }

    fn interval(&self) -> u32 {
        number(self.every_hours.as_ref())
            .map(|h| (h.round() as u32).clamp(MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS))
            .unwrap_or(DEFAULT_INTERVAL_HOURS)
    }
}

/// A page item as a watch keeps it.
pub fn watch_item(item: Item) -> WatchItem {
    WatchItem {
        title: item.title,
        price: item.price,
        currency: item.currency,
        mileage: item.mileage,
        year: item.year,
        link: item.link,
        image: item.image,
        snippet: item.snippet,
    }
}

#[tool_router(router = web_watch_tool_router, vis = "pub")]
impl KnowledgeMcpServer {
    #[tool(description = "\
Keep checking any web page (listings, blogs, news feeds, release notes, announcement boards, or \
content pages) and tell the person when new articles appear, content updates, or prices drop. \
Only url is needed; the other fields narrow what matches.")]
    async fn web_watch(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<WebWatchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool(TOOL_NAME);
        let text = run(&ctx.meta, params.0).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

async fn run(meta: &Meta, params: WebWatchParams) -> String {
    let what = "watching that page";
    let Some(deps) = WEB_WATCH_DEPS.get().cloned() else {
        return format_dead_end(what, "Watching pages is not set up on this pond yet.");
    };
    let owner = crate::session_from_meta(meta)
        .and_then(|s| deps.authorities.authority_for_engine_session(&s))
        .and_then(|a| a.profile_scope().owner_id().map(str::to_string));
    let Some(owner) = owner else {
        return format_dead_end(
            what,
            "Only a household member the pond knows can keep watches. Ask them to say who they are.",
        );
    };
    let session = crate::session_from_meta(meta);
    let result = match params.action() {
        "list" => list(&deps, &owner).await,
        "stop" => stop(&deps, &owner, &params).await,
        "watch_price" => watch_price(&owner, &params).await,
        _ => start(&deps, &owner, &params, session).await,
    };
    result.unwrap_or_else(|e| {
        tracing::warn!(target: "giap::trace", kind = "web_watch_failed", error = %e);
        format_dead_end(
            what,
            "The pond couldn't save that just now; try again shortly.",
        )
    })
}

async fn start(
    deps: &WebWatchDeps,
    owner: &str,
    params: &WebWatchParams,
    session: Option<String>,
) -> anyhow::Result<String> {
    let what = "watching that page";
    let asked = text(params.url.as_ref());
    if asked.is_empty() {
        return Ok(format_dead_end(
            what,
            "I need the page's address: the link the person gave, or a page read_web_page read.",
        ));
    }
    let Some(address) = page_addresses::global().resolve(&asked) else {
        return Ok(format_dead_end(
            what,
            "I watch only a page whose address the person gave, or one read_web_page read.",
        ));
    };
    let mine = deps.repo.list_for(owner).await?;
    if mine.len() >= MAX_WATCHES_PER_MEMBER {
        return Ok(format_dead_end(
            what,
            &format!(
                "They already keep {MAX_WATCHES_PER_MEMBER} watches, the most one member may. \
                 Offer to stop one first."
            ),
        ));
    }
    if mine.iter().any(|w| w.address == address) {
        return Ok(format_dead_end(what, "They already watch that page."));
    }
    let page = match read_watched(&address).await {
        Ok(page) => page,
        Err(failure) => return Ok(format_dead_end(what, &unreadable(&failure))),
    };
    // One product's page with no items or sections can be price-watched.
    if page.items.is_empty() && cart_watch::installed().is_some() {
        return watch_price(owner, params).await;
    }
    if page.items.is_empty() {
        return Ok(format_dead_end(
            what,
            "That page has no readable content to compare from one check to the next.",
        ));
    }
    let filters = params.filters();
    let interval = params.interval();
    let items: Vec<WatchItem> = page.items.into_iter().map(watch_item).collect();
    let now = Utc::now();
    let title = if page.title.is_empty() {
        crate::web_search::domain_of(&page.address)
    } else {
        page.title.clone()
    };
    let watch = WebWatch {
        id: uuid::Uuid::new_v4().to_string(),
        owner: owner.to_string(),
        address: page.address.clone(),
        title: title.clone(),
        // What the person said when they asked, for their list; the filters are what match.
        looking_for: crate::web_search::defang(&crate::last_user_message())
            .chars()
            .take(300)
            .collect(),
        filters: filters.clone(),
        interval_hours: interval,
        paused: false,
        items: Some(items.clone()),
        failures: 0,
        last_checked_at: Some(now),
        next_check_at: now + Duration::hours(interval.into()),
        created_at: now,
        session_id: session,
    };
    deps.repo.create(&watch).await?;
    tracing::info!(target: "giap::trace", kind = "web_watch_started", items = items.len(), interval);
    let matching: Vec<&WatchItem> = items.iter().filter(|i| filters.matches(i)).collect();
    let has_prices = items.iter().any(|i| i.price.is_some());
    let mut out = if has_prices {
        format!(
            "Watching {title} every {interval} hours for {}. It lists {} items now; {} match. The \
             person will be told on their phone when a new match appears or one drops in price, and \
             gets a daily summary of other changes.",
            filters.words_for_people(),
            items.len(),
            matching.len()
        )
    } else {
        format!(
            "Watching {title} every {interval} hours for {}. It tracks {} sections/articles now; {} match. The \
             person will be told on their phone when new content or an update appears, and \
             gets a daily summary of other changes.",
            filters.words_for_people(),
            items.len(),
            matching.len()
        )
    };
    for item in matching.iter().take(SHOWN_MATCHES) {
        let detail = item
            .price_words()
            .map(|p| format!(", {p}"))
            .or_else(|| item.snippet.as_ref().map(|s| format!(": {s}")))
            .unwrap_or_default();
        out.push_str(&format!("\n- {}{}", item.title, detail));
    }
    Ok(out)
}

async fn list(deps: &WebWatchDeps, owner: &str) -> anyhow::Result<String> {
    let watches = deps.repo.list_for(owner).await?;
    if watches.is_empty() {
        return Ok("They keep no watches.".to_string());
    }
    let mut out = format!("Their watches ({}):", watches.len());
    for w in watches {
        out.push_str(&format!(
            "\n- {} (id {}): {}, every {} hours{}",
            w.title,
            w.id,
            w.filters.words_for_people(),
            w.interval_hours,
            if w.paused { ", paused" } else { "" }
        ));
    }
    Ok(out)
}

async fn stop(deps: &WebWatchDeps, owner: &str, params: &WebWatchParams) -> anyhow::Result<String> {
    let id = text(params.watch_id.as_ref());
    let asked = text(params.url.as_ref()).to_lowercase();
    let watches = deps.repo.list_for(owner).await?;
    let found = watches.iter().find(|w| {
        (!id.is_empty() && w.id == id)
            || (!asked.is_empty()
                && (w.address.to_lowercase().contains(&asked)
                    || w.title.to_lowercase().contains(&asked)))
    });
    let Some(watch) = found else {
        return Ok(format_dead_end(
            "stopping that watch",
            "They have no watch by that id. List them first to find its id.",
        ));
    };
    deps.repo.delete(owner, &watch.id).await?;
    Ok(format!("Stopped watching {}.", watch.title))
}

/// Watches the price of the one product a page sells, in the member's cart.
async fn watch_price(owner: &str, params: &WebWatchParams) -> anyhow::Result<String> {
    let what = "watching that price";
    let Some(cart) = cart_watch::installed() else {
        return Ok(format_dead_end(
            what,
            "Watching prices is not set up on this pond yet.",
        ));
    };
    let asked = text(params.url.as_ref());
    let Some(address) = page_addresses::global().resolve(&asked) else {
        return Ok(format_dead_end(
            what,
            "I watch only a page whose address the person gave, or one read_web_page read.",
        ));
    };
    let target = number(params.max_price.as_ref().or(params.extra.get("target")));
    let new = NewCartItem {
        url: address,
        target,
        collection: None,
        every_hours: number(params.every_hours.as_ref()),
        notify: Some(if target.is_some() {
            Notify::Target
        } else {
            Notify::Drop
        }),
    };
    let now = Utc::now();
    let item = match cart.add(owner, new, now).await? {
        Ok(item) => item,
        Err(AddRefusal::AlreadyWatched(item)) => {
            return Ok(format!(
                "They already watch this price: {}.",
                cart_watch::item_words(&item)
            ))
        }
        Err(AddRefusal::TooMany) => {
            return Ok(format_dead_end(
                what,
                "They already watch as many prices as one member may. Offer to remove one in \
                 My Goose, under Cart.",
            ))
        }
        Err(AddRefusal::NotAnAddress) => {
            return Ok(format_dead_end(what, "That is not a web address."))
        }
    };
    // A turn holds the model, so this first read uses the page's own data; a page without any is
    // read again by the cart's own checks.
    let checked = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        cart.check(item.clone(), ModelUse::Never, now),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(item);
    tracing::info!(target: "giap::trace", kind = "cart_added_by_chat", priced = checked.price.is_some());
    let when = match checked.target {
        Some(t) => format!(
            "when it is at or under {}",
            money_words(t, checked.currency.as_deref())
        ),
        None => "when it gets cheaper".to_string(),
    };
    Ok(if checked.price.is_some() {
        format!(
            "Watching the price: {}. The person will be told on their phone {when}. It is in \
             My Goose, under Cart.",
            cart_watch::item_words(&checked)
        )
    } else {
        format!(
            "Watching {}. Its price could not be read yet; the pond tries again shortly and tells \
             the person on their phone {when}. It is in My Goose, under Cart.",
            checked.url
        )
    })
}

fn unreadable(failure: &FetchFailure) -> String {
    match failure {
        FetchFailure::Status(401 | 403) => "The site would not let the pond read the page.".into(),
        FetchFailure::Status(429) => "The site asked the pond to wait; try again later.".into(),
        FetchFailure::Status(404 | 410) => "There is no page at that address.".into(),
        FetchFailure::NotPublic => "That address is not on the public web.".into(),
        _ => "The page couldn't be read just now.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: serde_json::Value) -> WebWatchParams {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn the_model_sees_every_field_as_optional_with_its_real_default() {
        let tools = crate::prefix_oracle::all_tools();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|(_, t)| t.name == name)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| panic!("{name} is not offered"))
        };
        for name in ["web_watch", "saved_items"] {
            let required = tool(name).input_schema.get("required").cloned();
            assert!(
                required.is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)),
                "{name} lists required fields, so a small model asks the person for each one"
            );
        }
        let watch = tool("web_watch").input_schema;
        assert_eq!(
            watch["properties"]["every_hours"]["default"],
            serde_json::json!(DEFAULT_INTERVAL_HOURS),
            "the interval the model is shown is the one a watch gets"
        );
        assert_eq!(watch["properties"]["action"]["default"], "start");
        assert!(watch["properties"]["url"].get("default").is_none());
    }

    #[test]
    fn filters_are_read_from_words_and_numbers_however_written() {
        let p = params(serde_json::json!({
            "url": "c.example/used",
            "words": "Subaru, Forester",
            "max_price": "1.5M",
            "min_year": 2015,
            "max_mileage": "100,000 km",
            "every_hours": 200
        }));
        assert_eq!(p.action(), "start");
        let filters = p.filters();
        assert_eq!(filters.words, vec!["subaru", "forester"]);
        assert_eq!(filters.max_price, Some(1_500_000.0));
        assert_eq!(filters.min_year, Some(2015));
        assert_eq!(filters.max_mileage, Some(100_000.0));
        assert_eq!(p.interval(), MAX_INTERVAL_HOURS);
        let listed = params(serde_json::json!({"words": ["Forester"], "every_hours": 0}));
        assert_eq!(listed.filters().words, vec!["forester"]);
        assert_eq!(listed.interval(), MIN_INTERVAL_HOURS);
        assert_eq!(
            params(serde_json::json!({})).interval(),
            DEFAULT_INTERVAL_HOURS
        );
    }

    #[test]
    fn the_action_is_read_from_the_words_a_model_uses() {
        assert_eq!(
            params(serde_json::json!({"action": "List"})).action(),
            "list"
        );
        assert_eq!(
            params(serde_json::json!({"action": "remove"})).action(),
            "stop"
        );
        assert_eq!(
            params(serde_json::json!({"action": "watch"})).action(),
            "start"
        );
        assert_eq!(
            params(serde_json::json!({"action": "watch_price"})).action(),
            "watch_price"
        );
    }

    #[tokio::test]
    async fn without_deps_or_a_member_a_watch_is_refused() {
        let text = run(
            &Meta::new(),
            params(serde_json::json!({"url": "c.example"})),
        )
        .await;
        assert!(text.contains("not set up"), "{text}");
    }

    #[test]
    fn the_tool_is_listed() {
        let tools = KnowledgeMcpServer::web_watch_tool_router().list_all();
        assert!(tools.iter().any(|t| t.name == TOOL_NAME));
    }
}
