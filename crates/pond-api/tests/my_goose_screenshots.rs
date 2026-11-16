//! `/me/screenshots` over HTTP, as a phone build without captures calls it: the routes answer
//! in the shapes that build reads, over the member's captures of kind `screenshot`.

use axum::http::{Method, StatusCode};
use chrono::Utc;
use pond_core::user_data::domain::capture::{Capture, CaptureDetails, CaptureKind, CaptureState};
use serde_json::{json, Value};

#[path = "support/two_members.rs"]
mod two_members;
use two_members::{base64_of, call, get, png, pond, Pond, EMMANUEL, JERRY};

fn screenshot(seed: u8) -> Value {
    json!({"image": base64_of(&png(seed)), "mime_type": "image/png"})
}

fn keys(view: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = view
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// A capture of Jerry's as the engine leaves one it has read.
async fn read_capture(pond: &Pond, kind: CaptureKind, description: &str) -> Capture {
    let now = Utc::now();
    let mut capture = Capture::new(&pond.jerry, kind, now);
    capture.read(description, None, CaptureDetails::default(), now);
    assert!(pond.stores.captures.add(&capture).await.unwrap());
    capture
}

async fn enable(pond: &Pond, token: &str, enabled: bool) -> Value {
    let body = json!({"enabled": enabled});
    let (status, view) = call(pond, token, Method::PUT, "/me/screenshots", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

#[tokio::test]
async fn the_screenshot_routes_answer_in_the_shapes_the_older_phone_build_reads() {
    let pond = pond(true).await;
    let at_first = get(&pond, JERRY, "/me/screenshots").await;
    assert_eq!(
        keys(&at_first),
        ["available", "enabled", "max_pending", "notes", "pending"]
    );
    assert_eq!(at_first["enabled"], true);
    assert_eq!(at_first["available"], true);
    assert_eq!(at_first["pending"], 0);
    assert_eq!(at_first["max_pending"], 30);
    assert_eq!(at_first["notes"], json!([]));

    let off = enable(&pond, JERRY, false).await;
    assert_eq!(off["enabled"], false);
    let (status, refused) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(screenshot(1)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(pond.picture_files().is_empty(), "nothing is held while off");

    let on = enable(&pond, JERRY, true).await;
    assert_eq!(on["enabled"], true);
    assert_eq!(
        get(&pond, JERRY, "/me/captures").await["settings"]["screenshots"],
        true
    );

    let (status, taken) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(screenshot(1)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{taken}");
    assert_eq!(taken, json!({"pending": 1}));
    let (status, again) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(screenshot(1)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{again}");
    assert_eq!(
        again,
        json!({"pending": 1}),
        "the same screenshot is kept once"
    );
    let kept = pond.stores.captures.list(&pond.jerry).await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].kind, CaptureKind::Screenshot);
    assert_eq!(kept[0].state, CaptureState::Pending);
    assert_eq!(pond.picture_files().len(), 1);

    let (status, wrong) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(json!({"image": base64_of(&png(2)), "mime_type": "image/gif"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{wrong}");
    let (status, huge) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(json!({"image": "A".repeat(2_000_001), "mime_type": "image/png"})),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{huge}");

    for seed in 2..=30 {
        let (status, _) = call(
            &pond,
            JERRY,
            Method::POST,
            "/me/screenshots",
            Some(screenshot(seed)),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, full) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(screenshot(31)),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{full}");
    assert_eq!(get(&pond, JERRY, "/me/screenshots").await["pending"], 30);

    // Turning it off forgets what was still waiting to be read.
    let off = enable(&pond, JERRY, false).await;
    assert_eq!(off["enabled"], false);
    assert_eq!(off["pending"], 0);
    assert!(pond
        .stores
        .captures
        .list(&pond.jerry)
        .await
        .unwrap()
        .is_empty());
    assert!(pond.picture_files().is_empty());
}

#[tokio::test]
async fn a_pond_whose_model_cannot_read_pictures_takes_none() {
    let pond = pond(false).await;
    let view = enable(&pond, JERRY, true).await;
    assert_eq!(view["enabled"], true);
    assert_eq!(view["available"], false);
    let (status, refused) = call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(screenshot(1)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(pond.picture_files().is_empty());
}

#[tokio::test]
async fn a_note_is_what_a_read_screenshot_showed_and_the_member_deletes_one_or_all() {
    let pond = pond(true).await;
    let trail = read_capture(
        &pond,
        CaptureKind::Screenshot,
        "The Ngong Hills hiking trail",
    )
    .await;
    let recipe = read_capture(
        &pond,
        CaptureKind::Screenshot,
        "A recipe for chicken biryani",
    )
    .await;
    let photo = read_capture(&pond, CaptureKind::Photo, "A poster for the AI Summit").await;
    let mut receipt = Capture::new(&pond.jerry, CaptureKind::Screenshot, Utc::now());
    receipt.hold_for_budget(Utc::now());
    assert!(pond.stores.captures.add(&receipt).await.unwrap());
    call(
        &pond,
        JERRY,
        Method::POST,
        "/me/screenshots",
        Some(screenshot(1)),
    )
    .await;

    let view = get(&pond, JERRY, "/me/screenshots").await;
    assert_eq!(view["pending"], 1);
    let notes = view["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2, "{view}");
    assert_eq!(
        keys(&notes[0]),
        ["created_at", "id", "note", "taken_at", "used_at"]
    );
    let said: Vec<&str> = notes
        .iter()
        .map(|note| note["note"].as_str().unwrap())
        .collect();
    assert!(said.contains(&"The Ngong Hills hiking trail"), "{said:?}");
    assert!(said.contains(&"A recipe for chicken biryani"), "{said:?}");
    assert_eq!(notes[0]["used_at"], Value::Null);
    assert_eq!(
        get(&pond, EMMANUEL, "/me/screenshots").await["notes"],
        json!([])
    );

    let uri = format!("/me/screenshots/{}", trail.id);
    let (status, _) = call(&pond, EMMANUEL, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another member's note");
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let uri = format!("/me/screenshots/{}", photo.id);
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a photo is not a screenshot's note"
    );
    let left = get(&pond, JERRY, "/me/screenshots").await;
    assert_eq!(left["notes"].as_array().unwrap().len(), 1);
    assert_eq!(left["notes"][0]["id"], json!(recipe.id));

    let (status, gone) = call(&pond, JERRY, Method::DELETE, "/me/screenshots", None).await;
    assert_eq!(status, StatusCode::OK, "{gone}");
    assert_eq!(gone, json!({"deleted": 2, "dropped": 1}));
    let view = get(&pond, JERRY, "/me/screenshots").await;
    assert_eq!(view["notes"], json!([]));
    assert_eq!(view["pending"], 0);
    assert!(pond.picture_files().is_empty());
    let left = pond.stores.captures.list(&pond.jerry).await.unwrap();
    assert_eq!(
        left.len(),
        1,
        "the photo is not the older routes' to delete"
    );
    assert_eq!(left[0].id, photo.id);
    assert!(pond
        .stores
        .captures
        .hot(&pond.jerry)
        .await
        .unwrap()
        .is_empty());
}
