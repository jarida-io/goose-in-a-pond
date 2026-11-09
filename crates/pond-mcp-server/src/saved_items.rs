//! `saved_items` for `giap-knowledge`: keep what the assistant made as a card (a recipe, a
//! listing, a place, a note) in a collection the member names, and find it again. Saving takes
//! the conversation's last answer unless the model gives the text, so a recipe is never typed out
//! twice.

use std::sync::{Arc, OnceLock};

use chrono::Utc;
use pond_core::models::domain::message::Role;
use pond_core::shared::domain::saved_item::{
    recipe_parts, SavedFields, SavedItem, SavedSource, MAX_SAVED_PER_MEMBER,
};
use pond_core::shared::ports::saved_item::SavedItemRepository;
use pond_core::shared::services::research_report::sources;
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::ports::session_storage::SessionStorage;
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

const TOOL_NAME: &str = "saved_items";

/// Cards a search names at most.
const FOUND: usize = 5;
/// Messages looked back through for the last answer.
const LOOK_BACK: usize = 8;

#[derive(Clone)]
pub struct SavedItemDeps {
    pub repo: Arc<dyn SavedItemRepository>,
    pub sessions: Arc<dyn SessionStorage>,
    pub authorities: Arc<TurnAuthorityRegistry>,
}

static SAVED_ITEM_DEPS: OnceLock<SavedItemDeps> = OnceLock::new();

/// Install the deps once, at startup; later calls are ignored.
pub fn init_saved_item_deps(deps: SavedItemDeps) {
    let _ = SAVED_ITEM_DEPS.set(deps);
}

// Few, plain fields on purpose: this tool is in every turn's tool block.
// `default` keeps every field out of the schema's `required`; a small model asks for each one listed.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(transform = crate::without_null_defaults)]
pub struct SavedItemsParams {
    /// save, find or list
    #[schemars(with = "String", extend("default" = "save"))]
    pub action: Option<String>,
    /// recipe, listing, place or note
    #[schemars(with = "String", extend("default" = "note"))]
    pub kind: Option<String>,
    #[schemars(with = "String")]
    pub title: Option<String>,
    /// Such as Cars or Dinners
    #[schemars(with = "String")]
    pub collection: Option<String>,
    /// For find: words to look for
    #[schemars(with = "String")]
    pub query: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

fn text(value: Option<&String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl SavedItemsParams {
    fn action(&self) -> &str {
        match self.action.as_deref().map(str::trim) {
            Some(a)
                if ["find", "search", "look"]
                    .iter()
                    .any(|s| a.eq_ignore_ascii_case(s)) =>
            {
                "find"
            }
            Some(a) if a.eq_ignore_ascii_case("list") => "list",
            _ => "save",
        }
    }

    /// Text the model passed under any name, for a card it wrote itself.
    fn body(&self) -> Option<String> {
        ["body", "text", "content", "markdown"]
            .iter()
            .find_map(|k| self.extra.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn extra_text(&self, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|k| self.extra.get(*k))
            .and_then(|v| match v {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
    }
}

#[tool_router(router = saved_items_tool_router, vis = "pub")]
impl KnowledgeMcpServer {
    #[tool(description = "\
Keep what you made for the person as a card in their collection, such as a recipe or a car \
listing, or find one they kept. save takes your last answer unless you give text.")]
    async fn saved_items(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SavedItemsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool(TOOL_NAME);
        let text = run(&ctx.meta, params.0).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

async fn run(meta: &Meta, params: SavedItemsParams) -> String {
    let what = "keeping that";
    let Some(deps) = SAVED_ITEM_DEPS.get().cloned() else {
        return format_dead_end(what, "Saving cards is not set up on this pond yet.");
    };
    let authority = crate::session_from_meta(meta)
        .and_then(|s| deps.authorities.authority_for_engine_session(&s));
    let Some((owner, session)) = authority.and_then(|a| {
        a.profile_scope()
            .owner_id()
            .map(|o| (o.to_string(), a.session_id().to_string()))
    }) else {
        return format_dead_end(
            what,
            "Only a household member the pond knows can keep cards. Ask them to say who they are.",
        );
    };
    let result = match params.action() {
        "find" => find(&deps, &owner, &params).await,
        "list" => list(&deps, &owner).await,
        _ => save(&deps, &owner, &session, &params).await,
    };
    result.unwrap_or_else(|e| {
        tracing::warn!(target: "giap::trace", kind = "saved_items_failed", error = %e);
        format_dead_end(
            what,
            "The pond couldn't do that just now; try again shortly.",
        )
    })
}

/// The newest assistant answer in the conversation, the text a "save that" means.
async fn last_answer(deps: &SavedItemDeps, session: &str) -> Option<String> {
    let messages = deps
        .sessions
        .get_recent_messages(session, LOOK_BACK)
        .await
        .ok()?;
    messages
        .iter()
        .rev()
        .filter(|m| m.message.role == Role::Assistant)
        .map(|m| m.message.content.trim())
        .find(|c| !c.is_empty())
        .map(str::to_string)
}

/// The first heading or line of `body`, as a title when none was given.
fn title_in(body: &str) -> Option<String> {
    body.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches('#')
                .trim()
                .trim_matches('*')
                .trim()
        })
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
}

fn fields_for(kind: &str, body: &str, params: &SavedItemsParams) -> SavedFields {
    let first_link = sources(body).first().map(|s| s.url.clone());
    let link = params.extra_text(&["link", "url"]).or(first_link);
    match kind {
        "recipe" => {
            let (ingredients, steps) = recipe_parts(body);
            SavedFields::Recipe {
                servings: params.extra_text(&["servings"]),
                time: params.extra_text(&["time"]),
                ingredients,
                steps,
            }
        }
        "listing" => SavedFields::Listing {
            price: params.extra_text(&["price"]),
            currency: params.extra_text(&["currency"]),
            mileage: params.extra_text(&["mileage"]),
            year: params.extra_text(&["year"]),
            location: params.extra_text(&["location"]),
            link,
            photos: Vec::new(),
        },
        "place" => SavedFields::Place {
            address: params.extra_text(&["address"]),
            hours: params.extra_text(&["hours"]),
            link,
        },
        _ => SavedFields::Note,
    }
}

async fn save(
    deps: &SavedItemDeps,
    owner: &str,
    session: &str,
    params: &SavedItemsParams,
) -> anyhow::Result<String> {
    let what = "keeping that";
    if deps.repo.count_for(owner).await? >= MAX_SAVED_PER_MEMBER {
        return Ok(format_dead_end(
            what,
            &format!("They already keep {MAX_SAVED_PER_MEMBER} cards, the most one member may."),
        ));
    }
    let body = match params.body() {
        Some(body) => body,
        None => match last_answer(deps, session).await {
            Some(answer) => answer,
            None => {
                return Ok(format_dead_end(
                    what,
                    "There is no answer in this conversation to keep yet.",
                ))
            }
        },
    };
    let kind = params
        .kind
        .as_deref()
        .map(|k| k.trim().to_lowercase())
        .unwrap_or_else(|| "note".to_string());
    let title = text(params.title.as_ref())
        .or_else(|| title_in(&body))
        .unwrap_or_else(|| "Saved".to_string());
    let card_sources: Vec<SavedSource> = sources(&body)
        .into_iter()
        .map(|s| SavedSource {
            title: s.title,
            url: s.url,
        })
        .collect();
    let item = match SavedItem::new(
        uuid::Uuid::new_v4().to_string(),
        owner.to_string(),
        &title,
        fields_for(&kind, &body, params),
        &body,
        params.collection.as_deref(),
        card_sources,
        Some(session.to_string()),
        Utc::now(),
    ) {
        Ok(item) => item,
        Err(refused) => return Ok(format_dead_end(what, &refused.words())),
    };
    deps.repo.create(&item).await?;
    tracing::info!(target: "giap::trace", kind = "saved_item", card = item.fields.kind());
    Ok(match &item.collection {
        Some(c) => format!(
            "Kept \"{}\" as a {} in {c}.",
            item.title,
            item.fields.kind()
        ),
        None => format!("Kept \"{}\" as a {}.", item.title, item.fields.kind()),
    })
}

async fn find(
    deps: &SavedItemDeps,
    owner: &str,
    params: &SavedItemsParams,
) -> anyhow::Result<String> {
    let query = text(params.query.as_ref())
        .or_else(|| text(params.title.as_ref()))
        .unwrap_or_default();
    if query.is_empty() {
        return list(deps, owner).await;
    }
    let found = deps.repo.search(owner, &query, FOUND).await?;
    if found.is_empty() {
        return Ok(format!("Nothing they kept matches \"{query}\"."));
    }
    let mut out = format!("Cards that match \"{query}\":");
    for item in found {
        out.push_str(&format!("\n- {}", card_line(&item)));
        let first = item
            .body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'));
        if let Some(first) = first {
            out.push_str(&format!(
                ": {}",
                first.chars().take(160).collect::<String>()
            ));
        }
    }
    Ok(out)
}

async fn list(deps: &SavedItemDeps, owner: &str) -> anyhow::Result<String> {
    let collections = deps.repo.collections(owner).await?;
    let newest = deps.repo.list_visible(owner, None, None).await?;
    if newest.is_empty() {
        return Ok("They keep no cards yet.".to_string());
    }
    let mut out = String::new();
    if !collections.is_empty() {
        let named: Vec<String> = collections
            .iter()
            .map(|(c, n)| format!("{c} ({n})"))
            .collect();
        out.push_str(&format!("Collections: {}.\n", named.join(", ")));
    }
    out.push_str("Newest:");
    for item in newest.iter().take(FOUND) {
        out.push_str(&format!("\n- {}", card_line(item)));
    }
    Ok(out)
}

fn card_line(item: &SavedItem) -> String {
    let mut line = format!("{} ({}", item.title, item.fields.kind());
    if let Some(c) = &item.collection {
        line.push_str(&format!(", {c}"));
    }
    line.push(')');
    if let SavedFields::Listing { price: Some(p), .. } = &item.fields {
        line.push_str(&format!(", {p}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: serde_json::Value) -> SavedItemsParams {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn the_action_and_text_are_read_however_a_model_names_them() {
        assert_eq!(params(serde_json::json!({})).action(), "save");
        assert_eq!(
            params(serde_json::json!({"action": "Search"})).action(),
            "find"
        );
        assert_eq!(
            params(serde_json::json!({"action": "list"})).action(),
            "list"
        );
        let p = params(serde_json::json!({"content": " # Pilau\\n- rice ", "price": 1450000}));
        assert_eq!(p.body().as_deref(), Some("# Pilau\\n- rice"));
        assert_eq!(p.extra_text(&["price"]).as_deref(), Some("1450000"));
    }

    #[test]
    fn a_recipe_keeps_its_ingredients_and_steps_and_a_listing_its_link() {
        let body = "# Pilau\n## Ingredients\n- 2 cups rice\n- Pilau masala\n## Method\n1. Fry the onions.\n2. Add the rice.\n";
        let SavedFields::Recipe {
            ingredients, steps, ..
        } = fields_for("recipe", body, &params(serde_json::json!({})))
        else {
            panic!("a recipe");
        };
        assert_eq!(ingredients, vec!["2 cups rice", "Pilau masala"]);
        assert_eq!(steps, vec!["Fry the onions.", "Add the rice."]);
        let listing = fields_for(
            "listing",
            "A good one: [Forester](https://cars.example/used/2)",
            &params(serde_json::json!({"price": "KES 1,450,000"})),
        );
        assert_eq!(listing.link(), Some("https://cars.example/used/2"));
        assert_eq!(title_in("\n## **Pilau**\nrest").as_deref(), Some("Pilau"));
    }

    #[tokio::test]
    async fn without_deps_saving_is_refused() {
        let text = run(&Meta::new(), params(serde_json::json!({}))).await;
        assert!(text.contains("not set up"), "{text}");
    }
}
