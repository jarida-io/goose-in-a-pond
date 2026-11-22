//! Idea cards as a paired member's phone calls them: the set, each button, the morning set's
//! refresh, and the chat chips the `ask` cards lead.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use pond_api::build_router;
use pond_core::shared::ports::web_watch::WebWatchRepository;
use pond_core::user_data::domain::card::{
    ActionKind, Card, CardAction, CardKind, CardNotes, CardSource, CardSourceKind, CardState, Why,
    WhySource,
};
use pond_core::user_data::domain::dont_list::Strength;
use pond_core::user_data::domain::engine_job::{JobClass, JobKind};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_web_watch::SqliteWebWatchRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    engine: pond_api::EngineStores,
    watches: SqliteWebWatchRepository,
    member: String,
    other: String,
    _dir: tempfile::TempDir,
}

/// Two members; the phone is the first one's.
async fn a_pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let mut ids = Vec::new();
    for name in ["Jerry", "Amina"] {
        ids.push(
            state
                .profile_repo
                .create(CreateProfileRequest {
                    display_name: name.into(),
                    avatar_emoji: "x".into(),
                })
                .await
                .unwrap()
                .id,
        );
    }
    let device_id = state
        .device_registry
        .register(RegisterDeviceRequest {
            id: None,
            name: "Phone".into(),
            device_type: "gotg".into(),
            hostname: None,
            capabilities: vec![],
            room_id: None,
        })
        .await
        .unwrap()
        .id;
    SqliteDeviceAttribution::new(state.db.system.clone())
        .set_device_profile(&device_id, Some(&ids[0]))
        .await
        .unwrap();
    state.handshake = Arc::new(DeviceHandshake(device_id));
    let engine = state.engine.clone();
    let watches = SqliteWebWatchRepository::new(state.db.system.clone());
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        engine,
        watches,
        member: ids[0].clone(),
        other: ids[1].clone(),
        _dir: built.dir,
    }
}

async fn call(pond: &Pond, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(format!("/api/v1{uri}"))
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn card(owner: &str, kind: CardKind, title: &str, action: Option<(ActionKind, &str)>) -> Card {
    let now = Utc::now();
    Card {
        id: uuid::Uuid::new_v4().to_string(),
        profile_id: owner.to_string(),
        kind,
        title: title.to_string(),
        why: Why {
            text: format!("You asked about {title}"),
            source: WhySource {
                kind: CardSourceKind::Web,
                label: "Karura Forest".to_string(),
                url: Some("https://karura.example/".to_string()),
            },
        },
        when: Some("Saturday 7 am".to_string()),
        where_: Some("Karura Forest".to_string()),
        cost: Some("KSh 600".to_string()),
        effort: None,
        action: action.map(|(kind, extra)| CardAction {
            kind,
            label: "Do it".to_string(),
            prompt: (kind == ActionKind::Chat).then(|| extra.to_string()),
            text: None,
            due: None,
            url: (kind == ActionKind::Watch).then(|| extra.to_string()),
            question: None,
        }),
        sources: vec![CardSource {
            kind: CardSourceKind::Web,
            title: "Karura Forest".to_string(),
            url: Some("https://karura.example/".to_string()),
        }],
        notes: CardNotes {
            found: "Entry is KSh 600".to_string(),
            next: String::new(),
            caveats: String::new(),
        },
        anchor: None,
        state: CardState::New,
        batch_id: "batch".to_string(),
        rank: 0,
        created_at: now,
        fades_at: Card::fade_time(now),
    }
}

async fn stored(pond: &Pond, owner: &str, cards: &[Card]) {
    pond.engine
        .cards
        .add_set(owner, cards, Utc::now())
        .await
        .unwrap();
}

#[tokio::test]
async fn the_set_is_the_members_own_and_says_where_the_next_one_stands() {
    let pond = a_pond().await;
    let (status, hub) = call(&pond, Method::GET, "/me/hub", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hub["idea_cards"], true);
    assert!(hub["cards"].is_array(), "the chat's context cards stay");

    let (_, empty) = call(&pond, Method::GET, "/me/idea-cards", json!({})).await;
    assert_eq!(
        empty,
        json!({"cards": [], "set": {"state": "none", "made_at": null}})
    );

    let mine = card(
        &pond.member,
        CardKind::Do,
        "Run Karura",
        Some((ActionKind::Todo, "")),
    );
    let theirs = card(
        &pond.other,
        CardKind::Do,
        "Swim",
        Some((ActionKind::Todo, "")),
    );
    stored(&pond, &pond.member, &[mine.clone()]).await;
    stored(&pond, &pond.other, &[theirs.clone()]).await;

    let (status, list) = call(&pond, Method::GET, "/me/idea-cards", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["set"]["state"], "ready");
    let cards = list["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1);
    let shown = &cards[0];
    assert_eq!(shown["id"], mine.id.as_str());
    assert_eq!(shown["kind"], "do");
    assert_eq!(shown["why"]["source"]["kind"], "web");
    assert_eq!(shown["where"], "Karura Forest");
    assert_eq!(shown["action"]["kind"], "todo");
    assert_eq!(shown["notes"]["found"], "Entry is KSh 600");
    assert_eq!(shown["state"], "new");
    assert!(shown.get("profile_id").is_none());

    for path in ["save", "unsave", "do", "deeper", "dismiss"] {
        let (status, _) = call(
            &pond,
            Method::POST,
            &format!("/me/idea-cards/{}/{path}", theirs.id),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} on another's card");
    }

    let (status, saved) = call(
        &pond,
        Method::POST,
        &format!("/me/idea-cards/{}/save", mine.id),
        json!({}),
    )
    .await;
    assert_eq!((status, &saved["state"]), (StatusCode::OK, &json!("saved")));
    let (_, unsaved) = call(
        &pond,
        Method::POST,
        &format!("/me/idea-cards/{}/unsave", mine.id),
        json!({}),
    )
    .await;
    assert_eq!(unsaved["state"], "new");

    let (status, refreshed) = call(&pond, Method::POST, "/me/idea-cards/refresh", json!({})).await;
    assert_eq!(
        (status, &refreshed),
        (StatusCode::ACCEPTED, &json!({"state": "queued"}))
    );
    call(&pond, Method::POST, "/me/idea-cards/refresh", json!({})).await;
    let open = pond
        .engine
        .jobs
        .open_of_kind(&pond.member, JobKind::Cards)
        .await
        .unwrap();
    assert_eq!(open.len(), 1, "never two at once");
    assert_eq!(open[0].class, JobClass::Asked);
    let (_, list) = call(&pond, Method::GET, "/me/idea-cards", json!({})).await;
    assert_eq!(list["set"]["state"], "queued");
}

#[tokio::test]
async fn each_action_does_its_thing_and_marks_the_card_done() {
    let pond = a_pond().await;
    let todo = card(
        &pond.member,
        CardKind::Do,
        "Run the Karura loop",
        Some((ActionKind::Todo, "")),
    );
    let chat = card(
        &pond.member,
        CardKind::Ask,
        "What to pack",
        Some((ActionKind::Chat, "What should I pack for Karura?")),
    );
    let watch = card(
        &pond.member,
        CardKind::Do,
        "Watch Karura events",
        Some((ActionKind::Watch, "https://karura.example/events")),
    );
    let deeper = card(
        &pond.member,
        CardKind::NextStep,
        "Plan a Karura picnic",
        None,
    );
    stored(
        &pond,
        &pond.member,
        &[todo.clone(), chat.clone(), watch.clone(), deeper.clone()],
    )
    .await;
    let post = |id: &str, path: &str| format!("/me/idea-cards/{id}/{path}");

    let (status, done) = call(&pond, Method::POST, &post(&todo.id, "do"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["card"]["state"], "done");
    assert_eq!(done["result"]["kind"], "todo");
    assert_eq!(done["result"]["todo"]["text"], "Run the Karura loop");
    let (status, _) = call(&pond, Method::POST, &post(&todo.id, "do"), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "once");

    let (_, asked) = call(&pond, Method::POST, &post(&chat.id, "do"), json!({})).await;
    assert_eq!(asked["result"]["prompt"], "What should I pack for Karura?");

    let (_, watched) = call(&pond, Method::POST, &post(&watch.id, "do"), json!({})).await;
    let watch_id = watched["result"]["watch_id"].as_str().unwrap();
    let kept = pond.watches.get(watch_id).await.unwrap().unwrap();
    assert_eq!(kept.address, "https://karura.example/events");
    assert_eq!(kept.owner, pond.member);

    let (status, _) = call(&pond, Method::POST, &post(&deeper.id, "do"), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "no action of its own");
    let (status, went) = call(&pond, Method::POST, &post(&deeper.id, "deeper"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{went}");
    assert_eq!(went["card"]["state"], "done");
    let anchor_id = went["anchor_id"].as_str().unwrap();
    let plan = pond
        .engine
        .anchors
        .get(&pond.member, anchor_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.title, "Plan a Karura picnic");
    let queued = pond
        .engine
        .jobs
        .open_of_kind(&pond.member, JobKind::Deliverable)
        .await
        .unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].class, JobClass::Asked);
    assert_eq!(queued[0].anchor_id.as_deref(), Some(anchor_id));

    let (_, list) = call(&pond, Method::GET, "/me/idea-cards", json!({})).await;
    assert!(list["cards"].as_array().unwrap().is_empty(), "{list}");
}

#[tokio::test]
async fn not_this_is_kept_as_an_avoid_rule_with_the_chips_and_the_words() {
    let pond = a_pond().await;
    let shown = card(
        &pond.member,
        CardKind::Do,
        "Run Karura",
        Some((ActionKind::Todo, "")),
    );
    stored(&pond, &pond.member, &[shown.clone()]).await;
    let (status, _) = call(
        &pond,
        Method::POST,
        &format!("/me/idea-cards/{}/dismiss", shown.id),
        json!({"reasons": ["bad_timing"], "text": "not in the rains"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rules = pond
        .engine
        .dont
        .list(&pond.member, Some(Strength::Avoid))
        .await
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].rule, "Run Karura");
    assert_eq!(rules[0].said.as_deref(), Some("not in the rains"));
    assert_eq!(rules[0].origin_ref.as_deref(), Some(shown.id.as_str()));
    let held = pond
        .engine
        .cards
        .get(&pond.member, &shown.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(held.state, CardState::Dismissed);
}

#[tokio::test]
async fn the_members_ask_cards_lead_the_chat_chips() {
    let pond = a_pond().await;
    let ask = card(
        &pond.member,
        CardKind::Ask,
        "What to pack",
        Some((ActionKind::Chat, "What should I pack for Karura?")),
    );
    let titled = card(
        &pond.member,
        CardKind::Ask,
        "Is Karura open on Monday?",
        None,
    );
    let not_ask = card(
        &pond.member,
        CardKind::Do,
        "Run Karura",
        Some((ActionKind::Todo, "")),
    );
    stored(&pond, &pond.member, &[ask.clone(), titled.clone(), not_ask]).await;
    stored(
        &pond,
        &pond.other,
        &[card(&pond.other, CardKind::Ask, "Theirs", None)],
    )
    .await;

    let (status, body) = call(&pond, Method::GET, "/suggestions", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let chips = body["suggestions"].as_array().unwrap();
    let cards: Vec<&Value> = chips
        .iter()
        .filter(|chip| chip["id"].as_str().unwrap().starts_with("idea_card:"))
        .collect();
    assert_eq!(cards.len(), 2, "{body}");
    assert_eq!(chips[0]["prompt"], "What should I pack for Karura?");
    assert_eq!(chips[0]["because"], "You asked about What to pack");
    assert_eq!(chips[0]["card"], json!({"kind": "idea_card", "id": ask.id}));
    assert_eq!(chips[1]["prompt"], "Is Karura open on Monday?");
}
