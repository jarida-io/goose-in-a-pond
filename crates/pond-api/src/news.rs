//! A member's news: headline stories and the daily digest the inference lane wrote from their
//! topic map, the settings that steer it, reading a digest aloud, and asking for fresh news. No
//! route here calls a model; the contract is section 2 of `docs/architecture/my-goose-hub.md`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use pond_core::mcp::domain::tool_group::BRAVE_SEARCH_API_KEY;
use pond_core::mcp::domain::web_search::WebSearchStatus;
use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::news::{
    is_zone, parse_time_of_day, Digest, InterestRow, NewsBlocked, NewsSettings, Story, StoryKind,
    Vote,
};
use pond_core::shared::ports::news::NewsStore;
use pond_core::shared::services::news::coverage::covered;
use pond_core::shared::services::news::{schedule, speaker, speech};
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_infra::sqlite_news::SqliteNewsStore;

use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

/// Whether this Pond serves this part of My Goose; `/me/hub` reports it.
pub(crate) const SERVED: bool = true;

/// How far back the tiles reach.
const TILES_WINDOW: chrono::Duration = chrono::Duration::days(3);
/// Tiles one list shows at most.
const TILES_SHOWN: usize = 24;
/// One interest's page shows at most this many, newest first.
const INTEREST_PAGE: usize = 30;
/// How far back the reading history reaches.
const HISTORY_WINDOW: chrono::Duration = chrono::Duration::days(30);
/// A feedback note is kept to this many characters.
const NOTE_CHARS: usize = 500;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/news", get(news))
        .route("/me/news/settings", get(settings).put(put_settings))
        .route("/me/news/history", get(history))
        .route("/me/news/refresh", post(refresh))
        .route("/me/news/stories/{id}", get(story))
        .route("/me/news/stories/{id}/read", post(mark_read))
        .route("/me/news/stories/{id}/feedback", post(feedback))
        .route("/me/news/digests", get(digests))
        .route("/me/news/digests/{id}", get(digest))
        .route("/me/news/digests/{id}/listen", get(listen))
        .route("/me/news/digests/{id}/speak", post(speak))
}

fn store(state: &AppState) -> SqliteNewsStore {
    SqliteNewsStore::new(state.db.system.clone())
}

async fn household_zone(state: &AppState) -> String {
    match state.settings_repo.get().await {
        Ok(settings) => settings.timezone,
        Err(e) => {
            tracing::warn!(error = %e, "news: settings unreadable; reading times in UTC");
            "UTC".to_string()
        }
    }
}

fn picture_path(key: &Option<String>) -> Value {
    match key {
        Some(key) => json!(format!("/api/v1/report-images/{key}")),
        None => Value::Null,
    }
}

/// `{id, topic, category}` for a story's interest; an area of the topic map is `other`.
fn interest_view(story: &Story, interests: &HashMap<String, InterestRow>) -> Value {
    let category = interests
        .get(&story.interest.id)
        .map_or("other", |i| i.category.as_str());
    json!({"id": story.interest.id, "topic": story.interest.topic, "category": category})
}

fn story_view(story: &Story, interests: &HashMap<String, InterestRow>) -> Value {
    json!({
        "id": story.id,
        "kind": story.kind.as_str(),
        "headline": story.headline,
        "dek": story.dek,
        "picture": picture_path(&story.picture_key),
        "source": {"name": story.site, "url": story.url},
        "interest": interest_view(story, interests),
        "published_at": story.published_at,
        "written_at": story.written_at,
        "read": story.read_at.is_some(),
    })
}

fn story_detail(story: &Story, interests: &HashMap<String, InterestRow>) -> Value {
    let mut view = story_view(story, interests);
    let mut sources =
        vec![json!({"title": story.source_title, "url": story.url, "site": story.site})];
    sources.extend(
        story
            .further_sources
            .iter()
            .map(|r| json!({"title": r.title, "url": r.url, "site": r.site})),
    );
    if let Value::Object(map) = &mut view {
        map.insert("summary".into(), json!(story.summary));
        map.insert("why_you".into(), json!(story.why_you));
        map.insert("sources".into(), Value::Array(sources));
    }
    view
}

fn digest_view(digest: &Digest) -> Value {
    let sections: Vec<Value> = digest
        .sections
        .iter()
        .map(|s| {
            json!({
                "interest": {"id": s.interest.id, "topic": s.interest.topic},
                "heading": s.heading,
                "body": s.body,
                "pictures": s.pictures.iter().map(|p| json!({
                    "path": format!("/api/v1/report-images/{}", p.key),
                    "caption": p.caption,
                    "source_url": p.source_url,
                })).collect::<Vec<_>>(),
                "references": s.references.iter().map(|r| json!({
                    "title": r.title, "url": r.url, "site": r.site,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "id": digest.id,
        "title": digest.title,
        "written_at": digest.written_at,
        "intro": digest.intro,
        "sections": sections,
        "listen": format!("/api/v1/me/news/digests/{}/listen", digest.id),
    })
}

fn settings_view(settings: &NewsSettings, household: &str) -> Value {
    json!({
        "digest": settings.digest,
        "digest_time": settings.digest_time,
        "spoken_on_speaker": settings.spoken_on_speaker,
        "tiles": settings.tiles,
        "timezone": settings.zone(household).name(),
    })
}

async fn interests_by_id(
    news: &SqliteNewsStore,
    member: &str,
) -> Result<HashMap<String, InterestRow>, Failure> {
    Ok(news
        .interests(member)
        .await
        .map_err(server_error)?
        .into_iter()
        .map(|i| (i.id.clone(), i))
        .collect())
}

/// Why the member's news has nothing to write from, if it has not.
async fn blocked(state: &AppState, rows: &[InterestRow]) -> Option<NewsBlocked> {
    if covered(rows).is_empty() {
        return Some(NewsBlocked::NoInterests);
    }
    let key = match &state.secret_repo {
        Some(repo) => repo.get(BRAVE_SEARCH_API_KEY).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "news: the secret store could not be read");
            None
        }),
        None => None,
    };
    let status = pond_core::mcp::services::web_search_status::global().status_for(key.as_deref());
    match status {
        WebSearchStatus::NotConfigured | WebSearchStatus::Rejected | WebSearchStatus::Quota => {
            Some(NewsBlocked::WebSearch)
        }
        WebSearchStatus::Ok | WebSearchStatus::Unverified => None,
    }
}

/// Whether a story belongs among the tiles: a long read always, a story only under an interest
/// that feeds the tiles, or an area of the map.
fn on_the_tiles(story: &Story, interests: &HashMap<String, InterestRow>) -> bool {
    if story.kind == StoryKind::LongRead {
        return true;
    }
    match interests.get(&story.interest.id) {
        Some(i) => !i.avoid && i.news.tiles(),
        None => story.interest.id.starts_with("area:"),
    }
}

#[derive(Deserialize)]
struct NewsQuery {
    interest: Option<String>,
}

/// `GET /me/news`: the tiles (or one interest's page), the latest digest, whether news is being
/// written, and why there is none when nothing can be.
async fn news(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Query(query): Query<NewsQuery>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let news = store(&state);
    let now = Utc::now();
    let zone_name = household_zone(&state).await;
    let settings = news.settings(&member).await.map_err(server_error)?;
    let interests = interests_by_id(&news, &member).await?;
    let rows: Vec<InterestRow> = interests.values().cloned().collect();
    let stories: Vec<Story> = match query.interest.as_deref().filter(|i| !i.is_empty()) {
        Some(interest) => news
            .interest_stories(&member, interest, INTEREST_PAGE)
            .await
            .map_err(server_error)?,
        None if settings.tiles => news
            .stories_since(&member, now - TILES_WINDOW)
            .await
            .map_err(server_error)?
            .into_iter()
            .filter(|s| on_the_tiles(s, &interests))
            .take(TILES_SHOWN)
            .collect(),
        None => Vec::new(),
    };
    let latest = news.latest_digest(&member).await.map_err(server_error)?;
    let book = news.book(&member).await.map_err(server_error)?;
    let zone = settings.zone(&zone_name);
    let day = schedule::digest_day(now, &zone, &settings);
    let written_today = match day {
        Some(day) => news
            .digest_on(&member, day)
            .await
            .map_err(server_error)?
            .is_some(),
        None => false,
    };
    let next = schedule::next_digest_at(now, &zone, &settings, |d| Some(d) == day && written_today);
    let newest_story = news
        .stories_since(&member, now - HISTORY_WINDOW)
        .await
        .map_err(server_error)?
        .first()
        .map(|s| s.written_at);
    let last_written: Option<DateTime<Utc>> = [newest_story, latest.as_ref().map(|d| d.written_at)]
        .into_iter()
        .flatten()
        .max();
    Ok(Json(json!({
        "stories": stories.iter().map(|s| story_view(s, &interests)).collect::<Vec<_>>(),
        "digest": latest.as_ref().map(digest_view),
        "state": if schedule::writing(now, &book) { "writing" } else { "idle" },
        "next_digest_at": next,
        "last_written_at": last_written,
        "blocked": blocked(&state, &rows).await.map(NewsBlocked::as_str),
    })))
}

async fn settings(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let settings = store(&state)
        .settings(&member)
        .await
        .map_err(server_error)?;
    Ok(Json(settings_view(
        &settings,
        &household_zone(&state).await,
    )))
}

/// `PUT /me/news/settings`: any of the fields; `"timezone": null` follows the household again.
async fn put_settings(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Json(body): Json<Map<String, Value>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let news = store(&state);
    let mut settings = news.settings(&member).await.map_err(server_error)?;
    let flag = |name: &str| -> Result<Option<bool>, Failure> {
        match body.get(name) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(failure(
                StatusCode::BAD_REQUEST,
                format!("{name} must be true or false"),
            )),
        }
    };
    if let Some(on) = flag("tiles")? {
        settings.tiles = on;
    }
    if let Some(on) = flag("digest")? {
        settings.digest = on;
    }
    if let Some(on) = flag("spoken_on_speaker")? {
        settings.spoken_on_speaker = on;
    }
    if let Some(time) = body.get("digest_time") {
        let time = time
            .as_str()
            .and_then(parse_time_of_day)
            .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "digest_time must be \"HH:MM\""))?;
        settings.digest_time = time.format("%H:%M").to_string();
    }
    match body.get("timezone") {
        None => {}
        Some(Value::Null) => settings.timezone = None,
        Some(Value::String(zone)) if is_zone(zone) => {
            settings.timezone = Some(zone.trim().to_string())
        }
        Some(_) => {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "timezone must be an IANA zone such as Africa/Nairobi, or null",
            ))
        }
    }
    news.save_settings(&member, &settings)
        .await
        .map_err(server_error)?;
    Ok(Json(settings_view(
        &settings,
        &household_zone(&state).await,
    )))
}

async fn story(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let news = store(&state);
    let story = news
        .story(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such story"))?;
    let interests = interests_by_id(&news, &member).await?;
    Ok(Json(story_detail(&story, &interests)))
}

async fn mark_read(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    if store(&state)
        .mark_read(&member, &id, Utc::now())
        .await
        .map_err(server_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(failure(StatusCode::NOT_FOUND, "no such story"))
    }
}

#[derive(Deserialize)]
struct Feedback {
    vote: String,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /me/news/stories/{id}/feedback`: up and down move the topic map at its next rebuild;
/// wrong hides the story, and its note goes to the log with the page it came from.
async fn feedback(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<Feedback>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let vote = Vote::parse(&body.vote)
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "vote must be up, down or wrong"))?;
    let note: Option<String> = body
        .note
        .map(|n| n.trim().chars().take(NOTE_CHARS).collect::<String>())
        .filter(|n| !n.is_empty());
    let news = store(&state);
    let story = news
        .story(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such story"))?;
    news.set_vote(&member, &id, vote, note.as_deref(), Utc::now())
        .await
        .map_err(server_error)?;
    if vote == Vote::Wrong {
        tracing::info!(
            target: "giap::trace",
            kind = "news_story_wrong",
            story = %story.id,
            url = %story.url,
            note = note.as_deref().unwrap_or(""),
            "a member said a news story is wrong"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn history(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let news = store(&state);
    let interests = interests_by_id(&news, &member).await?;
    let read = news
        .read_stories(&member, Utc::now() - HISTORY_WINDOW)
        .await
        .map_err(server_error)?;
    let stories: Vec<Value> = read
        .iter()
        .map(|s| {
            let mut view = story_view(s, &interests);
            if let Value::Object(map) = &mut view {
                map.insert("read_at".into(), json!(s.read_at));
            }
            view
        })
        .collect();
    Ok(Json(json!({ "stories": stories })))
}

async fn digests(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let list = store(&state).digests(&member).await.map_err(server_error)?;
    let digests: Vec<Value> = list
        .iter()
        .map(|d| json!({"id": d.id, "title": d.title, "written_at": d.written_at}))
        .collect();
    Ok(Json(json!({ "digests": digests })))
}

async fn find_digest(state: &AppState, member: &str, id: &str) -> Result<Digest, Failure> {
    store(state)
        .digest(member, id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such digest"))
}

async fn digest(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    Ok(Json(digest_view(&find_digest(&state, &member, &id).await?)))
}

/// `GET /me/news/digests/{id}/listen`: the digest in the pond's voice, made once and kept.
async fn listen(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let digest = find_digest(&state, &member, &id).await?;
    let cached = state
        .data_dir
        .as_deref()
        .map(|dir| pond_infra::news_audio::path_for(dir, &digest.id));
    if let Some(path) = &cached {
        if let Some(wav) = pond_infra::news_audio::read(path).await {
            return Ok(wav_response(wav));
        }
    }
    let Some(voice) = &state.tts else {
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "this Pond has no voice to read with",
        ));
    };
    let wav = speech::listen_wav(voice.as_ref(), &digest)
        .await
        .map_err(server_error)?
        .ok_or_else(|| {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "the voice could not read the digest",
            )
        })?;
    if let Some(path) = &cached {
        pond_infra::news_audio::write(path, &wav).await;
    }
    Ok(wav_response(wav))
}

fn wav_response(wav: Vec<u8>) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "audio/wav")],
        Body::from(wav),
    )
        .into_response()
}

/// `POST /me/news/digests/{id}/speak`: the intro and the headings on the house speaker, now.
async fn speak(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let digest = find_digest(&state, &member, &id).await?;
    let Some(speaker) = speaker::installed() else {
        return Err(failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "this Pond has no speaker to read on",
        ));
    };
    let name = state
        .profile_repo
        .get(&member)
        .await
        .ok()
        .flatten()
        .map(|p| {
            p.preferences
                .get("preferred_name")
                .filter(|n| !n.trim().is_empty())
                .cloned()
                .unwrap_or(p.display_name)
        });
    let text = speech::speaker_text(&digest, name.as_deref());
    tokio::spawn(async move {
        if let Err(e) = speaker.announce(&text).await {
            tracing::warn!(target: "giap::trace", kind = "news_speak_failed", error = %e);
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"state": "speaking"}))))
}

/// `POST /me/news/refresh`: fresh news at the lane's next quiet moment.
async fn refresh(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let news = store(&state);
    let rows = news.interests(&member).await.map_err(server_error)?;
    if let Some(why) = blocked(&state, &rows).await {
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"state": "idle", "blocked": why.as_str()})),
        ));
    }
    news.request_refresh(&member, Utc::now())
        .await
        .map_err(server_error)?;
    if let Some(lane) = &state.lane {
        lane.wake(LaneJob::NewsTiles).await;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"state": "writing"}))))
}
