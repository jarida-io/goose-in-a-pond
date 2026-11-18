//! `/me/captures` over HTTP: a member chooses which pictures their phone sends, each picture
//! is kept once in a file only the pond's user can open, a member reads what their own showed
//! and never one about money, and deleting a capture deletes its picture.

use axum::http::{header, Method, StatusCode};
use chrono::Utc;
use pond_core::user_data::domain::capture::{Capture, CaptureDetails, CaptureState, Sensitivity};
use pond_core::user_data::domain::engine_job::{EngineJob, JobKind, JobState};
use serde_json::{json, Value};

#[path = "support/two_members.rs"]
mod two_members;
use two_members::{base64_of, call, get, jpeg, png, pond, send, Pond, EMMANUEL, JERRY};

fn picture(kind: &str, bytes: &[u8], mime_type: &str) -> Value {
    json!({"image": base64_of(bytes), "mime_type": mime_type, "kind": kind})
}

/// Sends a picture and returns the answer; `album` is left out when `None`.
async fn sent(
    pond: &Pond,
    token: &str,
    mut body: Value,
    album: Option<&str>,
) -> (StatusCode, Value) {
    if let Some(album) = album {
        body["album"] = json!(album);
    }
    call(pond, token, Method::POST, "/me/captures", Some(body)).await
}

/// The member's one capture still waiting to be read.
async fn waiting(pond: &Pond, member: &str) -> Capture {
    let mut pending: Vec<Capture> = pond
        .stores
        .captures
        .list(member)
        .await
        .unwrap()
        .into_iter()
        .filter(|capture| capture.state == CaptureState::Pending)
        .collect();
    assert_eq!(pending.len(), 1, "{pending:?}");
    pending.remove(0)
}

/// Reads a capture as the engine would: nothing sensitive, described.
async fn read_as(pond: &Pond, mut capture: Capture, description: &str) -> Capture {
    capture.read(
        description,
        Some("planning a trip"),
        CaptureDetails {
            places: vec!["Kisumu".into()],
            dates: vec!["3 December".into()],
            prices: vec!["KSh 8,400".into()],
            ..Default::default()
        },
        Utc::now(),
    );
    assert!(pond.stores.captures.save(&capture).await.unwrap());
    capture
}

/// Holds a capture for the Budget agent as the engine would on finding money in it.
async fn held_for_budget(pond: &Pond, mut capture: Capture) -> Capture {
    capture.hold_for_budget(Utc::now());
    assert!(pond.stores.captures.save(&capture).await.unwrap());
    capture
}

async fn picture_of(pond: &Pond, token: &str, id: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let uri = format!("/me/captures/{id}/picture");
    let response = send(pond, token, Method::GET, &uri, None).await;
    let status = response.status();
    let kind = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, kind, bytes.to_vec())
}

#[tokio::test]
async fn a_member_sends_a_picture_reads_what_it_showed_and_deletes_it() {
    let pond = pond(true).await;
    let at_first = get(&pond, JERRY, "/me/captures").await;
    assert_eq!(
        at_first,
        json!({
            "settings": {"screenshots": true, "albums": []},
            "available": true,
            "pending": 0,
            "captures": [],
        })
    );

    let mut body = picture("screenshot", &png(1), "image/png");
    body["taken_at"] = json!("2026-11-16T09:00:00Z");
    let (status, taken) = sent(&pond, JERRY, body, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{taken}");
    assert_eq!(taken, json!({"pending": 1}));

    let capture = waiting(&pond, &pond.jerry).await;
    assert_eq!(pond.picture_files(), [format!("{}.png", capture.id)]);
    let unread = get(&pond, JERRY, "/me/captures").await;
    assert_eq!(unread["pending"], 1);
    assert_eq!(
        unread["captures"],
        json!([]),
        "a picture not yet read is not listed"
    );

    let capture = read_as(&pond, capture, "A flight to Kisumu on 3 December").await;
    let listed = get(&pond, JERRY, "/me/captures").await;
    assert_eq!(listed["pending"], 0);
    assert_eq!(
        listed["captures"],
        json!([{
            "id": capture.id,
            "kind": "screenshot",
            "album": null,
            "taken_at": "2026-11-16T09:00:00Z",
            "description": "A flight to Kisumu on 3 December",
            "intent": "planning a trip",
            "details": {
                "names": [],
                "dates": ["3 December"],
                "places": ["Kisumu"],
                "prices": ["KSh 8,400"],
                "links": [],
            },
            "has_picture": true,
            "created_at": capture.created_at,
        }])
    );

    let (status, kind, bytes) = picture_of(&pond, JERRY, &capture.id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind.as_deref(), Some("image/png"));
    assert_eq!(bytes, png(1));

    let uri = format!("/me/captures/{}", capture.id);
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(pond.picture_files().is_empty(), "the picture goes with it");
    assert_eq!(
        picture_of(&pond, JERRY, &capture.id).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&pond, JERRY, "/me/captures").await["captures"],
        json!([])
    );
}

#[tokio::test]
async fn the_same_picture_sent_twice_is_stored_once() {
    let pond = pond(true).await;
    let (status, _) = sent(
        &pond,
        JERRY,
        picture("screenshot", &jpeg(7), "image/jpeg"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let first = waiting(&pond, &pond.jerry).await;

    let (status, again) = sent(&pond, JERRY, picture("photo", &jpeg(7), "image/jpeg"), None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["pending"], 1);
    assert_eq!(again["capture"]["id"], json!(first.id));
    assert_eq!(again["capture"]["kind"], "screenshot");
    assert_eq!(again["capture"]["description"], Value::Null);
    assert_eq!(pond.picture_files(), [format!("{}.jpg", first.id)]);
    assert_eq!(
        pond.stores.captures.list(&pond.jerry).await.unwrap().len(),
        1
    );

    let (status, _) = sent(
        &pond,
        EMMANUEL,
        picture("screenshot", &jpeg(7), "image/jpeg"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "another member's is their own"
    );
    assert_eq!(pond.picture_files().len(), 2);

    // Once what it showed is forgotten, the same picture is a new one.
    let uri = format!("/me/captures/{}", first.id);
    call(&pond, JERRY, Method::DELETE, &uri, None).await;
    let (status, _) = sent(
        &pond,
        JERRY,
        picture("screenshot", &jpeg(7), "image/jpeg"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn turning_screenshots_off_refuses_them_and_clears_the_ones_waiting() {
    let pond = pond(true).await;
    sent(
        &pond,
        JERRY,
        picture("screenshot", &png(1), "image/png"),
        None,
    )
    .await;
    let kept = read_as(&pond, waiting(&pond, &pond.jerry).await, "A trail map").await;
    sent(
        &pond,
        JERRY,
        picture("screenshot", &png(2), "image/png"),
        None,
    )
    .await;
    sent(&pond, JERRY, picture("photo", &png(3), "image/png"), None).await;
    sent(
        &pond,
        EMMANUEL,
        picture("screenshot", &png(4), "image/png"),
        None,
    )
    .await;
    assert_eq!(pond.picture_files().len(), 4);

    let (status, settings) = call(
        &pond,
        JERRY,
        Method::PUT,
        "/me/captures/settings",
        Some(json!({"screenshots": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(settings, json!({"screenshots": false, "albums": []}));

    let left = pond.stores.captures.list(&pond.jerry).await.unwrap();
    let photo = waiting(&pond, &pond.jerry).await;
    assert_eq!(
        left.len(),
        2,
        "the read screenshot and the waiting photo stay"
    );
    assert!(left.iter().any(|capture| capture.id == kept.id));
    assert_eq!(photo.kind.as_str(), "photo");
    assert_eq!(
        pond.picture_files().len(),
        3,
        "the waiting screenshot's picture went"
    );
    assert_eq!(
        pond.stores
            .captures
            .count_pending(&pond.emmanuel)
            .await
            .unwrap(),
        1
    );

    let (status, refused) = sent(
        &pond,
        JERRY,
        picture("screenshot", &png(5), "image/png"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("turn them on"));
    let (status, _) = sent(&pond, JERRY, picture("photo", &png(6), "image/png"), None).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a photo shared by hand is still taken"
    );

    let profile = pond.profiles.get(&pond.jerry).await.unwrap().unwrap();
    assert_eq!(
        profile
            .preferences
            .get("captures_screenshots")
            .map(String::as_str),
        Some("false")
    );
    let (_, on) = call(
        &pond,
        JERRY,
        Method::PUT,
        "/me/captures/settings",
        Some(json!({"screenshots": true})),
    )
    .await;
    assert_eq!(on["screenshots"], true);
    let (status, _) = sent(
        &pond,
        JERRY,
        picture("screenshot", &png(5), "image/png"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn the_older_screenshots_preference_still_counts_until_the_member_chooses_again() {
    let pond = pond(true).await;
    let mut profile = pond.profiles.get(&pond.jerry).await.unwrap().unwrap();
    profile
        .preferences
        .insert("ideas_from_screenshots".into(), "false".into());
    profile.preferences.insert("theme".into(), "dark".into());
    pond.profiles
        .update_preferences(&pond.jerry, profile.preferences)
        .await
        .unwrap();
    assert_eq!(
        get(&pond, JERRY, "/me/captures").await["settings"]["screenshots"],
        false
    );
    assert_eq!(
        get(&pond, EMMANUEL, "/me/captures").await["settings"]["screenshots"],
        true,
        "a member who never chose has them on"
    );

    let (_, settings) = call(
        &pond,
        JERRY,
        Method::PUT,
        "/me/captures/settings",
        Some(json!({"screenshots": true, "albums": ["Receipts"]})),
    )
    .await;
    assert_eq!(
        settings,
        json!({"screenshots": true, "albums": ["Receipts"]})
    );
    let profile = pond.profiles.get(&pond.jerry).await.unwrap().unwrap();
    assert_eq!(
        profile.preferences.get("theme").map(String::as_str),
        Some("dark"),
        "the member's other preferences are left as they were"
    );
    assert_eq!(
        profile
            .preferences
            .get("captures_albums")
            .map(String::as_str),
        Some(r#"["Receipts"]"#)
    );
}

#[tokio::test]
async fn a_photo_is_taken_from_a_chosen_album_or_when_shared_by_hand() {
    let pond = pond(true).await;
    let (status, settings) = call(
        &pond,
        JERRY,
        Method::PUT,
        "/me/captures/settings",
        Some(json!({"albums": [" Receipts ", "receipts", "Trip to  Lamu", ""]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(
        settings,
        json!({"screenshots": true, "albums": ["Receipts", "Trip to Lamu"]})
    );

    let photo = |seed| picture("photo", &png(seed), "image/png");
    let (status, refused) = sent(&pond, JERRY, photo(1), Some("Camera")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("album"));
    assert!(pond.picture_files().is_empty());

    let (status, _) = sent(&pond, JERRY, photo(1), Some("RECEIPTS")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        waiting(&pond, &pond.jerry).await.album.as_deref(),
        Some("RECEIPTS")
    );
    let (status, _) = sent(&pond, JERRY, photo(2), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "shared by hand");
    let (status, _) = sent(&pond, EMMANUEL, photo(3), Some("Receipts")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Jerry's albums are not Emmanuel's"
    );

    let too_many: Vec<String> = (0..21).map(|n| format!("Album {n}")).collect();
    for unusable in [
        json!({"albums": too_many}),
        json!({"albums": ["a".repeat(61)]}),
    ] {
        let (status, why) = call(
            &pond,
            JERRY,
            Method::PUT,
            "/me/captures/settings",
            Some(unusable),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }
    assert_eq!(
        get(&pond, JERRY, "/me/captures").await["settings"]["albums"],
        json!(["Receipts", "Trip to Lamu"]),
        "a refused change changes nothing"
    );
}

#[tokio::test]
async fn a_money_capture_is_never_listed_shown_or_served() {
    let pond = pond(true).await;
    sent(
        &pond,
        JERRY,
        picture("screenshot", &png(1), "image/png"),
        None,
    )
    .await;
    let receipt = held_for_budget(&pond, waiting(&pond, &pond.jerry).await).await;
    assert_eq!(receipt.sensitive, Sensitivity::Money);

    let listed = get(&pond, JERRY, "/me/captures").await;
    assert_eq!(listed["captures"], json!([]));
    assert_eq!(listed["pending"], 0);
    assert_eq!(
        picture_of(&pond, JERRY, &receipt.id).await.0,
        StatusCode::NOT_FOUND
    );

    let (status, again) = sent(
        &pond,
        JERRY,
        picture("screenshot", &png(1), "image/png"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["capture"],
        Value::Null,
        "the same picture, and nothing about it"
    );
    assert_eq!(pond.picture_files().len(), 1);

    let (status, _) = call(&pond, JERRY, Method::DELETE, "/me/captures", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(pond
        .stores
        .captures
        .hot(&pond.jerry)
        .await
        .unwrap()
        .is_empty());
    assert!(
        pond.picture_files().is_empty(),
        "deleting everything takes the held ones too"
    );
}

#[tokio::test]
async fn one_member_never_reads_or_deletes_anothers_capture_or_its_picture() {
    let pond = pond(true).await;
    sent(
        &pond,
        JERRY,
        picture("screenshot", &png(1), "image/png"),
        None,
    )
    .await;
    let mine = read_as(
        &pond,
        waiting(&pond, &pond.jerry).await,
        "A flight to Kisumu",
    )
    .await;

    assert_eq!(
        get(&pond, EMMANUEL, "/me/captures").await["captures"],
        json!([])
    );
    assert_eq!(
        picture_of(&pond, EMMANUEL, &mine.id).await.0,
        StatusCode::NOT_FOUND
    );
    let uri = format!("/me/captures/{}", mine.id);
    let (status, _) = call(&pond, EMMANUEL, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&pond, EMMANUEL, Method::DELETE, "/me/captures", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, off) = call(
        &pond,
        EMMANUEL,
        Method::PUT,
        "/me/captures/settings",
        Some(json!({"screenshots": false})),
    )
    .await;
    assert_eq!(off["screenshots"], false);

    let still = get(&pond, JERRY, "/me/captures").await;
    assert_eq!(still["captures"][0]["id"], json!(mine.id));
    assert_eq!(still["settings"]["screenshots"], true);
    assert_eq!(picture_of(&pond, JERRY, &mine.id).await.0, StatusCode::OK);
    assert_eq!(pond.picture_files().len(), 1);
}

#[tokio::test]
async fn a_picture_is_refused_for_what_it_is_or_when_enough_are_waiting() {
    let pond = pond(true).await;
    let too_large = "A".repeat(2_000_001);
    for (body, expected) in [
        (
            picture("screenshot", &png(1), "image/gif"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            picture("screenshot", b"GIF89a not one the pond keeps", "image/png"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            picture("screenshot", b"<svg onload=alert(1)>", "image/png"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            json!({"image": "not base64!", "mime_type": "image/png", "kind": "photo"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"image": too_large, "mime_type": "image/png", "kind": "photo"}),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            picture("video", &png(1), "image/png"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"mime_type": "image/png", "kind": "photo"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, why) = sent(&pond, JERRY, body, None).await;
        assert_eq!(status, expected, "{why}");
        assert!(why["error"].is_string(), "{why}");
    }
    assert!(
        pond.picture_files().is_empty(),
        "a refused picture is never written"
    );

    // The file is named by what the bytes are, whatever the phone called them.
    let (status, _) = sent(&pond, JERRY, picture("photo", &jpeg(0), "image/png"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(pond.picture_files()[0].ends_with(".jpg"));

    for seed in 1..30 {
        let (status, taken) = sent(
            &pond,
            JERRY,
            picture("photo", &png(seed), "image/png"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{taken}");
        assert_eq!(taken["pending"], u64::from(seed) + 1);
    }
    let (status, full) = sent(&pond, JERRY, picture("photo", &png(99), "image/png"), None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{full}");
    assert_eq!(pond.picture_files().len(), 30);
    let (status, _) = sent(
        &pond,
        EMMANUEL,
        picture("photo", &png(99), "image/png"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "the queue is each member's own"
    );
}

#[tokio::test]
async fn a_pond_whose_model_reads_no_pictures_takes_none() {
    let pond = pond(false).await;
    assert_eq!(get(&pond, JERRY, "/me/captures").await["available"], false);
    let (status, refused) = sent(&pond, JERRY, picture("photo", &png(1), "image/png"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("cannot read pictures"));
    assert!(pond.picture_files().is_empty());
}

#[tokio::test]
async fn deleting_a_capture_calls_off_the_job_reading_it() {
    let pond = pond(true).await;
    sent(
        &pond,
        JERRY,
        picture("screenshot", &png(1), "image/png"),
        None,
    )
    .await;
    let capture = waiting(&pond, &pond.jerry).await;
    let job = EngineJob::for_capture(&pond.jerry, &capture.id, JobKind::Capture, Utc::now());
    pond.stores.jobs.add(&job).await.unwrap();

    let uri = format!("/me/captures/{}", capture.id);
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let job = pond.stores.jobs.get(&job.id).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Cancelled);
    assert!(pond
        .stores
        .jobs
        .open_for_capture(&capture.id)
        .await
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn a_picture_is_a_file_only_the_ponds_user_can_open() {
    use std::os::unix::fs::PermissionsExt;
    let pond = pond(true).await;
    let (status, _) = sent(&pond, JERRY, picture("photo", &png(1), "image/png"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let dir = pond.pictures_dir();
    assert_eq!(mode(&dir), 0o700);
    let files = pond.picture_files();
    assert_eq!(files.len(), 1);
    assert_eq!(mode(&dir.join(&files[0])), 0o600);
    let capture = waiting(&pond, &pond.jerry).await;
    assert_eq!(
        files[0],
        format!("{}.png", capture.id),
        "named by the capture, not the phone"
    );
}

#[tokio::test]
async fn the_hub_names_the_parts_the_idea_engine_added() {
    let pond = pond(true).await;
    let hub = get(&pond, JERRY, "/me/hub").await;
    for part in [
        "anchors",
        "facts",
        "questions",
        "captures",
        "agents",
        "ledger",
        "intentions",
        "engine_log",
        "screenshots",
    ] {
        assert_eq!(hub[part], true, "{part}: {hub}");
    }
    assert_eq!(hub["version"], 1);
}
