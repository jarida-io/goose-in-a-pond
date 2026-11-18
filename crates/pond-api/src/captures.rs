//! A member's captures: the screenshots and photos their phone sends. The member chooses what
//! is sent, reads what each one showed and deletes any of them. A route only keeps a picture
//! and its pending row; the engine's runner finds the row and reads it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, put},
    Extension, Json, Router,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use pond_core::security::ports::policy::Principal;
use pond_core::user_data::domain::capture::{
    Capture, CaptureKind, CaptureState, NotTaken, Sensitivity, MAX_IMAGE_CHARS, MIME_TYPES,
};
use pond_core::user_data::ports::capture_pictures::CapturePictures;
use pond_core::user_data::services::screenshot_notes::PREFERENCE as OLD_SCREENSHOTS_PREFERENCE;

use crate::engine_log::call_off;
use crate::image_normalize::sniff;
use crate::me::{caller_member, failure, server_error, Failure};
use crate::AppState;

pub(crate) const SERVED: bool = true;

const SCREENSHOTS_PREFERENCE: &str = "captures_screenshots";
const ALBUMS_PREFERENCE: &str = "captures_albums";
const MAX_ALBUMS: usize = 20;
const MAX_ALBUM_CHARS: usize = 60;
/// Captures one member may have waiting to be read.
pub(crate) const MAX_PENDING: usize = 30;
/// Captures one read of the list shows, newest first.
const MAX_LISTED: usize = 200;

/// Serialises this module's read-modify-write of a member's preferences, so two changes made
/// at once both land.
static SETTINGS_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me/captures", get(list).post(send).delete(forget_all))
        .route("/me/captures/settings", put(choose))
        .route("/me/captures/{id}", delete(forget_one))
        .route("/me/captures/{id}/picture", get(picture))
}

fn no_such_capture() -> Failure {
    failure(StatusCode::NOT_FOUND, "no such capture of yours")
}

/// A body past the router's limit is a picture too large, whatever else it holds.
pub(crate) fn bad_picture_body(rejection: JsonRejection) -> Failure {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, NotTaken::TooLarge.as_str());
    }
    failure(StatusCode::UNPROCESSABLE_ENTITY, rejection.body_text())
}

/// Whether this Pond's model can look at a picture at all.
pub(crate) async fn can_read_pictures(state: &AppState) -> bool {
    state
        .llm_provider
        .read()
        .await
        .as_ref()
        .is_some_and(|provider| provider.capabilities().vision)
}

fn pictures(state: &AppState) -> Result<&Arc<dyn CapturePictures>, Failure> {
    state.engine.pictures.as_ref().ok_or_else(|| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "this pond has nowhere to keep pictures: start it with a data directory",
        )
    })
}

/// What a member lets their phone send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CaptureSettings {
    pub(crate) screenshots: bool,
    /// The phone albums whose photos are sent.
    pub(crate) albums: Vec<String>,
}

impl CaptureSettings {
    /// Screenshots are on unless the member said otherwise, here or under the older preference.
    fn read_from(preferences: &HashMap<String, String>) -> Self {
        let screenshots = match preferences
            .get(SCREENSHOTS_PREFERENCE)
            .or_else(|| preferences.get(OLD_SCREENSHOTS_PREFERENCE))
        {
            Some(choice) => choice == "true",
            None => true,
        };
        let albums = preferences
            .get(ALBUMS_PREFERENCE)
            .and_then(|stored| serde_json::from_str(stored).ok())
            .unwrap_or_default();
        Self {
            screenshots,
            albums,
        }
    }

    fn takes_album(&self, album: &str) -> bool {
        let album = album.to_lowercase();
        self.albums
            .iter()
            .any(|chosen| chosen.to_lowercase() == album)
    }
}

/// An album's name on one line; `None` when it has no words.
fn album_name(raw: &str) -> Option<String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()).then_some(name)
}

/// The albums as stored: each named once, whatever its case.
fn chosen_albums(sent: &[String]) -> Result<Vec<String>, &'static str> {
    let mut albums: Vec<String> = Vec::new();
    for name in sent.iter().filter_map(|raw| album_name(raw)) {
        if name.chars().count() > MAX_ALBUM_CHARS {
            return Err("an album's name is at most 60 characters: shorten it");
        }
        let lower = name.to_lowercase();
        if !albums.iter().any(|kept| kept.to_lowercase() == lower) {
            albums.push(name);
        }
    }
    if albums.len() > MAX_ALBUMS {
        return Err("that is more than 20 albums: choose the ones you most want read");
    }
    Ok(albums)
}

async fn preferences_of(
    state: &AppState,
    member: &str,
) -> Result<HashMap<String, String>, Failure> {
    let profile = state
        .profile_repo
        .get(member)
        .await
        .map_err(server_error)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no such member"))?;
    Ok(profile.preferences)
}

pub(crate) async fn settings_of(
    state: &AppState,
    member: &str,
) -> Result<CaptureSettings, Failure> {
    Ok(CaptureSettings::read_from(
        &preferences_of(state, member).await?,
    ))
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SettingsChange {
    #[serde(default)]
    pub(crate) screenshots: Option<bool>,
    #[serde(default)]
    pub(crate) albums: Option<Vec<String>>,
}

/// Stores what the member chose, and forgets the screenshots still waiting when they turn
/// screenshots off.
pub(crate) async fn change_settings(
    state: &AppState,
    member: &str,
    change: SettingsChange,
) -> Result<CaptureSettings, Failure> {
    let albums = change
        .albums
        .as_deref()
        .map(chosen_albums)
        .transpose()
        .map_err(|why| failure(StatusCode::UNPROCESSABLE_ENTITY, why))?;
    let settings = {
        let _held = SETTINGS_LOCK.lock().await;
        let mut preferences = preferences_of(state, member).await?;
        if let Some(screenshots) = change.screenshots {
            preferences.insert(SCREENSHOTS_PREFERENCE.to_string(), screenshots.to_string());
        }
        if let Some(albums) = albums {
            preferences.insert(ALBUMS_PREFERENCE.to_string(), json!(albums).to_string());
        }
        let settings = CaptureSettings::read_from(&preferences);
        state
            .profile_repo
            .update_preferences(member, preferences)
            .await
            .map_err(server_error)?;
        settings
    };
    if change.screenshots == Some(false) {
        let waiting: Vec<Capture> = state
            .engine
            .captures
            .list(member)
            .await
            .map_err(server_error)?
            .into_iter()
            .filter(|capture| {
                capture.kind == CaptureKind::Screenshot && capture.state == CaptureState::Pending
            })
            .collect();
        forget(state, member, &waiting).await?;
    }
    tracing::info!(
        target: "giap::trace",
        kind = "captures_settings",
        screenshots = settings.screenshots,
        albums = settings.albums.len(),
        "a member chose which pictures their phone sends"
    );
    Ok(settings)
}

/// Every capture the member has, the ones held for the Budget agent among them.
pub(crate) async fn every_capture(state: &AppState, member: &str) -> Result<Vec<Capture>, Failure> {
    let captures = &state.engine.captures;
    let mut all = captures.list(member).await.map_err(server_error)?;
    all.extend(captures.hot(member).await.map_err(server_error)?);
    Ok(all)
}

/// Deletes captures of the member's, each with its picture and the job reading it.
pub(crate) async fn forget(
    state: &AppState,
    member: &str,
    captures: &[Capture],
) -> Result<(), Failure> {
    let now = Utc::now();
    for capture in captures {
        let job = state
            .engine
            .jobs
            .open_for_capture(&capture.id)
            .await
            .map_err(server_error)?;
        if let Some(job) = job {
            call_off(state, job, "Deleted by the member.", now)
                .await
                .map_err(server_error)?;
        }
        // Before the row: a failure part-way leaves a row with no picture, never a picture
        // with no row.
        if let Some(file) = capture.picture_file.as_deref() {
            pictures(state)?.delete(file).await.map_err(server_error)?;
        }
        state
            .engine
            .captures
            .delete(member, &capture.id)
            .await
            .map_err(server_error)?;
    }
    Ok(())
}

/// What a member is shown of their captures: the ones read, and never one about money.
pub(crate) fn is_shown(capture: &Capture) -> bool {
    capture.state == CaptureState::Read && capture.sensitive == Sensitivity::None
}

fn view(capture: &Capture) -> Value {
    json!({
        "id": capture.id,
        "kind": capture.kind,
        "album": capture.album,
        "taken_at": capture.taken_at,
        "description": capture.description,
        "intent": capture.intent,
        "details": capture.details,
        "has_picture": capture.picture_file.is_some(),
        "created_at": capture.created_at,
    })
}

async fn list(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let settings = settings_of(&state, &member).await?;
    let captures = &state.engine.captures;
    let shown: Vec<Value> = captures
        .list(&member)
        .await
        .map_err(server_error)?
        .iter()
        .filter(|capture| is_shown(capture))
        .take(MAX_LISTED)
        .map(view)
        .collect();
    let pending = captures
        .count_pending(&member)
        .await
        .map_err(server_error)?;
    Ok(Json(json!({
        "settings": settings,
        "available": can_read_pictures(&state).await,
        "pending": pending,
        "captures": shown,
    })))
}

async fn choose(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<SettingsChange>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(change) =
        body.map_err(|e| failure(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()))?;
    let settings = change_settings(&state, &member, change).await?;
    Ok(Json(json!(settings)))
}

/// One picture as a phone sends it.
#[derive(Debug, Deserialize)]
pub(crate) struct Sent {
    /// Base64 of a JPEG, PNG or WebP.
    pub(crate) image: String,
    pub(crate) mime_type: String,
    pub(crate) kind: CaptureKind,
    /// The phone album a photo came from; none for a picture the member shared by hand.
    #[serde(default)]
    pub(crate) album: Option<String>,
    #[serde(default)]
    pub(crate) taken_at: Option<DateTime<Utc>>,
}

/// What became of a picture a phone sent; `pending` is how many the member has waiting.
pub(crate) enum Taken {
    Queued {
        pending: usize,
    },
    /// The member already sent this picture, and this is what was kept of it.
    Held {
        capture: Box<Capture>,
        pending: usize,
    },
}

/// The picture's bytes, with padding optional and line breaks skipped, as phones send both.
fn decoded(image: &str) -> Option<Vec<u8>> {
    let lenient = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let payload = image.rsplit_once("base64,").map_or(image, |(_, data)| data);
    let bytes = if payload.bytes().any(|b| b.is_ascii_whitespace()) {
        let compact: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        lenient.decode(compact)
    } else {
        lenient.decode(payload)
    };
    bytes.ok().filter(|bytes| !bytes.is_empty())
}

/// The picture's type by its own bytes, when it is one the pond keeps.
fn kept_type(bytes: &[u8]) -> Option<&'static str> {
    let mime = sniff(bytes)?.mime();
    MIME_TYPES.contains(&mime).then_some(mime)
}

fn hash_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Keeps a picture a phone sent as a pending capture, unless the member's choices, the model
/// or the picture itself rule it out, or the member already sent it.
pub(crate) async fn take(state: &AppState, member: &str, sent: Sent) -> Result<Taken, Failure> {
    let settings = settings_of(state, member).await?;
    let album = sent.album.as_deref().and_then(album_name);
    match (sent.kind, &album) {
        (CaptureKind::Screenshot, _) if !settings.screenshots => {
            return Err(failure(
                StatusCode::CONFLICT,
                "screenshots are off: turn them on in My Goose first",
            ));
        }
        (CaptureKind::Photo, Some(album)) if !settings.takes_album(album) => {
            return Err(failure(
                StatusCode::CONFLICT,
                "that album is not one you chose: add it to your albums in My Goose first",
            ));
        }
        _ => {}
    }
    if !can_read_pictures(state).await {
        return Err(failure(
            StatusCode::CONFLICT,
            "this Pond's model cannot read pictures: choose one that can in Models",
        ));
    }
    let wrong_type = || {
        failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            NotTaken::WrongType.as_str(),
        )
    };
    if !MIME_TYPES.contains(&sent.mime_type.trim().to_lowercase().as_str()) {
        return Err(wrong_type());
    }
    if sent.image.len() > MAX_IMAGE_CHARS {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            NotTaken::TooLarge.as_str(),
        ));
    }
    let bytes = decoded(&sent.image).ok_or_else(|| {
        failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the picture did not arrive whole: send it again as base64",
        )
    })?;
    // The file is named by what the bytes are, never by the label the phone gave them.
    let mime_type = kept_type(&bytes).ok_or_else(wrong_type)?;
    let pictures = pictures(state)?;

    let captures = &state.engine.captures;
    let hash = hash_of(&bytes);
    let pending = captures.count_pending(member).await.map_err(server_error)?;
    let held = |capture: Capture| Taken::Held {
        capture: Box::new(capture),
        pending,
    };
    if let Some(capture) = captures
        .by_hash(member, &hash)
        .await
        .map_err(server_error)?
    {
        return Ok(held(capture));
    }
    if pending >= MAX_PENDING {
        return Err(failure(
            StatusCode::TOO_MANY_REQUESTS,
            "enough pictures are waiting to be read: send more later",
        ));
    }

    let mut capture = Capture::new(member, sent.kind, Utc::now());
    capture.album = album;
    capture.taken_at = sent.taken_at;
    capture.content_hash = Some(hash.clone());
    let file = pictures
        .save(&capture.id, mime_type, &bytes)
        .await
        .map_err(server_error)?;
    capture.picture_file = Some(file.clone());
    let added = captures.add(&capture).await;
    if !matches!(added, Ok(true)) {
        if let Err(e) = pictures.delete(&file).await {
            tracing::warn!(error = %e, "the picture of a capture that was not kept could not be deleted");
        }
    }
    if added.map_err(server_error)? {
        return Ok(Taken::Queued {
            pending: pending + 1,
        });
    }
    // `add` refuses only a picture sent twice at once, so the read now finds the one kept.
    captures
        .by_hash(member, &hash)
        .await
        .map_err(server_error)?
        .map(held)
        .ok_or_else(|| server_error("the picture could not be kept: send it again"))
}

async fn send(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    body: Result<Json<Sent>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let Json(sent) = body.map_err(bad_picture_body)?;
    Ok(match take(&state, &member, sent).await? {
        Taken::Queued { pending } => (StatusCode::ACCEPTED, Json(json!({ "pending": pending }))),
        Taken::Held { capture, pending } => {
            // One about money is told apart from a new picture and shown no further.
            let shown = (capture.sensitive == Sensitivity::None).then(|| view(&capture));
            (
                StatusCode::OK,
                Json(json!({ "pending": pending, "capture": shown })),
            )
        }
    })
}

async fn picture(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let capture = state
        .engine
        .captures
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .filter(|capture| capture.sensitive == Sensitivity::None)
        .ok_or_else(no_such_capture)?;
    let gone = || {
        failure(
            StatusCode::NOT_FOUND,
            "this picture is no longer kept: what it showed still is",
        )
    };
    let file = capture.picture_file.as_deref().ok_or_else(gone)?;
    let (bytes, mime_type) = pictures(&state)?
        .read(file)
        .await
        .map_err(server_error)?
        .ok_or_else(gone)?;
    Ok((
        [
            (header::CONTENT_TYPE, mime_type),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (
                HeaderName::from_static("x-content-type-options"),
                "nosniff".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn forget_one(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let capture = state
        .engine
        .captures
        .get(&member, &id)
        .await
        .map_err(server_error)?
        .ok_or_else(no_such_capture)?;
    forget(&state, &member, std::slice::from_ref(&capture)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn forget_all(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
) -> Result<StatusCode, Failure> {
    let member = caller_member(&state, principal.as_ref()).await?;
    let all = every_capture(&state, &member).await?;
    forget(&state, &member, &all).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preferences(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn screenshots_are_on_unless_the_member_said_otherwise_under_either_preference() {
        let on =
            |pairs: &[(&str, &str)]| CaptureSettings::read_from(&preferences(pairs)).screenshots;
        assert!(on(&[]));
        assert!(on(&[(OLD_SCREENSHOTS_PREFERENCE, "true")]));
        assert!(!on(&[(OLD_SCREENSHOTS_PREFERENCE, "false")]));
        assert!(!on(&[(SCREENSHOTS_PREFERENCE, "false")]));
        assert!(on(&[
            (SCREENSHOTS_PREFERENCE, "true"),
            (OLD_SCREENSHOTS_PREFERENCE, "false"),
        ]));
        assert!(!on(&[
            (SCREENSHOTS_PREFERENCE, "false"),
            (OLD_SCREENSHOTS_PREFERENCE, "true"),
        ]));
    }

    #[test]
    fn albums_are_stored_once_each_on_one_line_and_matched_whatever_the_case() {
        let sent = |names: &[&str]| {
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            chosen_albums(&sent(&[" Receipts ", "receipts", "", "Trip  to\nLamu"])),
            Ok(sent(&["Receipts", "Trip to Lamu"]))
        );
        assert!(chosen_albums(&["a".repeat(MAX_ALBUM_CHARS)]).is_ok());
        assert!(chosen_albums(&["a".repeat(MAX_ALBUM_CHARS + 1)]).is_err());
        let many: Vec<String> = (0..=MAX_ALBUMS).map(|n| format!("Album {n}")).collect();
        assert!(chosen_albums(&many[..MAX_ALBUMS]).is_ok());
        assert!(chosen_albums(&many).is_err());

        let settings = CaptureSettings::read_from(&preferences(&[(
            ALBUMS_PREFERENCE,
            r#"["Receipts","Trip to Lamu"]"#,
        )]));
        assert!(settings.takes_album("RECEIPTS"));
        assert!(!settings.takes_album("Camera"));
        let unreadable = CaptureSettings::read_from(&preferences(&[(ALBUMS_PREFERENCE, "oops")]));
        assert!(unreadable.albums.is_empty());
    }

    #[test]
    fn a_picture_is_decoded_however_a_phone_wraps_it_and_typed_by_its_own_bytes() {
        let png = b"\x89PNG\r\n\x1a\n rest of a png";
        let padded = base64::engine::general_purpose::STANDARD.encode(png);
        let bare = padded.trim_end_matches('=').to_string();
        let wrapped = format!("data:image/png;base64,{}\n{}", &padded[..8], &padded[8..]);
        for sent in [&padded, &bare, &wrapped] {
            assert_eq!(decoded(sent).as_deref(), Some(&png[..]), "{sent}");
        }
        assert_eq!(decoded(""), None);
        assert_eq!(decoded("not base64!"), None);

        assert_eq!(kept_type(png), Some("image/png"));
        assert_eq!(kept_type(b"\xff\xd8\xff\xe0 jpeg"), Some("image/jpeg"));
        assert_eq!(kept_type(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(kept_type(b"GIF89a a gif"), None);
        assert_eq!(kept_type(b"<svg onload=alert(1)>"), None);
        assert_eq!(hash_of(b"abc").len(), 64);
        assert_eq!(hash_of(b"abc"), hash_of(b"abc"));
        assert_ne!(hash_of(b"abc"), hash_of(b"abd"));
    }
}
