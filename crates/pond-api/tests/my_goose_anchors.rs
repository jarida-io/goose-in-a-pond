//! `/me/anchors`, `/me/deliverables`, `/me/questions` and `/me/engine/jobs` over HTTP: each
//! member reaches only their own anchors and what hangs off them, a shared anchor is read and
//! its questions answered but never changed, and work is asked for by queueing an engine job.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use chrono::{Duration, SubsecRound, Utc};
use pond_api::{build_router, EngineStores};
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::shared::domain::long_task::LongTaskState;
use pond_core::shared::ports::long_task::LongTaskRepository;
use pond_core::user_data::domain::anchor::{
    Anchor, AnchorFact, AnchorKind, AnchorOrigin, Deliverable, DeliverableState, Objection,
    ObjectionKind,
};
use pond_core::user_data::domain::engine_job::{EngineJob, JobClass, JobKind, JobState};
use pond_core::user_data::domain::interest::{ObservedInterest, Signal};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::domain::question::{
    Question, QuestionPurpose, QuestionState, INTEREST_DROP, INTEREST_FOLLOW,
};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::services::engine::recipes::{deliverable_title, slot_named};
use pond_core::user_data::services::engine::task::task_for_job;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_interests::SqliteInterestRepository;
use pond_infra::sqlite_long_task::SqliteLongTaskRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

const JERRY: &str = "jerry-token";
const EMMANUEL: &str = "emmanuel-token";

/// `jerry-token` is Jerry's phone and `emmanuel-token` Emmanuel's.
struct TwoPhones {
    jerry: String,
    emmanuel: String,
}

#[async_trait]
impl Handshake for TwoPhones {
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used by this fixture")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(matches!(token, JERRY | EMMANUEL))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let device = match token {
            JERRY => &self.jerry,
            EMMANUEL => &self.emmanuel,
            _ => return Ok(None),
        };
        Ok(Some(TokenCaller {
            client_id: device.clone(),
            device_id: device.clone(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used by this fixture")
    }
}

struct Pond {
    router: axum::Router,
    jerry: String,
    emmanuel: String,
    stores: EngineStores,
    tasks: SqliteLongTaskRepository,
    interests: SqliteInterestRepository,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    state.skip_onboarding = true;
    let mut members = Vec::new();
    for name in ["Jerry", "Emmanuel"] {
        let member = state
            .profile_repo
            .create(CreateProfileRequest {
                display_name: name.into(),
                avatar_emoji: "duck".into(),
            })
            .await
            .unwrap()
            .id;
        let device = state
            .device_registry
            .register(RegisterDeviceRequest {
                id: None,
                name: format!("{name}'s phone"),
                device_type: "gotg".into(),
                hostname: None,
                capabilities: vec![],
                room_id: None,
            })
            .await
            .unwrap()
            .id;
        SqliteDeviceAttribution::new(pool.clone())
            .set_device_profile(&device, Some(&member))
            .await
            .unwrap();
        members.push((member, device));
    }
    state.handshake = Arc::new(TwoPhones {
        jerry: members[0].1.clone(),
        emmanuel: members[1].1.clone(),
    });
    let stores = state.engine.clone();
    Pond {
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        jerry: members[0].0.clone(),
        emmanuel: members[1].0.clone(),
        stores,
        tasks: SqliteLongTaskRepository::new(pool.clone()),
        interests: SqliteInterestRepository::new(pool),
        _dir: built.dir,
    }
}

async fn send(
    pond: &Pond,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/api/v1{uri}"))
        .header("Authorization", format!("Bearer {token}"));
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    pond.router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

async fn call(
    pond: &Pond,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = send(pond, token, method, uri, body).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(pond: &Pond, token: &str, uri: &str) -> Value {
    let (status, body) = call(pond, token, Method::GET, uri, None).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

async fn post(pond: &Pond, token: &str, uri: &str, body: Option<Value>) -> StatusCode {
    call(pond, token, Method::POST, uri, body).await.0
}

/// Adds an anchor of Jerry's by hand and returns its id.
async fn anchor(pond: &Pond, kind: &str, title: &str) -> String {
    let (status, made) = call(
        pond,
        JERRY,
        Method::POST,
        "/me/anchors",
        Some(json!({"kind": kind, "title": title})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    made["id"].as_str().unwrap().to_string()
}

/// A plan Goose noticed in one of Jerry's conversations, waiting for his yes.
async fn candidate(pond: &Pond, title: &str) -> String {
    let now = Utc::now();
    let mut noticed = Anchor::new(
        &pond.jerry,
        AnchorKind::Event,
        title,
        AnchorOrigin::Chat,
        now,
    )
    .unwrap();
    noticed.set_fact(AnchorFact::new("When", "in December", "you said"), now);
    assert!(pond.stores.anchors.add(&noticed).await.unwrap());
    noticed.id
}

/// A deliverable of `slot` as the engine would have left it in `state`.
async fn made(pond: &Pond, anchor_id: &str, slot: &str, state: DeliverableState) -> Deliverable {
    let anchor = pond.stores.anchors.find(anchor_id).await.unwrap().unwrap();
    let recipe = slot_named(&anchor, slot).unwrap();
    let mut deliverable = Deliverable::new(
        anchor_id,
        recipe.kind,
        recipe.slot,
        &deliverable_title(&anchor, recipe),
        Utc::now(),
    )
    .unwrap();
    deliverable.state = state;
    if state != DeliverableState::Queued {
        deliverable.version = 1;
        deliverable.hook = "Three days by the sea for under KSh 60,000.".into();
        deliverable.body = "# Three days in Mombasa\n\nDay 1: the morning train.".into();
    }
    assert!(pond
        .stores
        .anchors
        .add_deliverable(&deliverable)
        .await
        .unwrap());
    deliverable
}

async fn stored(pond: &Pond, deliverable: &Deliverable) -> Deliverable {
    pond.stores
        .anchors
        .find_deliverable(&deliverable.id)
        .await
        .unwrap()
        .unwrap()
}

/// A queued job of Jerry's that makes or refreshes `deliverable`.
async fn job_for(pond: &Pond, deliverable: &Deliverable, class: JobClass) -> EngineJob {
    let job = EngineJob::for_deliverable(
        &pond.jerry,
        &deliverable.anchor_id,
        &deliverable.id,
        &deliverable.title,
        class,
        None,
        Utc::now(),
    );
    pond.stores.jobs.add(&job).await.unwrap();
    job
}

/// Starts `job` as the runner does: a long task of its own, and the job running under it.
async fn started(pond: &Pond, mut job: EngineJob) -> EngineJob {
    let now = Utc::now();
    let (task, plan) = task_for_job(&job, "Write it.", &pond.jerry, now).unwrap();
    pond.tasks.create_task(&task).await.unwrap();
    if let Some(plan) = plan {
        pond.tasks.replace_open_steps(&plan).await.unwrap();
        pond.tasks
            .set_task_state(task.id(), LongTaskState::Running, now)
            .await
            .unwrap();
    }
    job.start(Some(task.id()), now).unwrap();
    assert!(pond.stores.jobs.save(&job).await.unwrap());
    job
}

async fn job(pond: &Pond, id: &str) -> EngineJob {
    pond.stores.jobs.get(id).await.unwrap().unwrap()
}

/// A question of Jerry's in `batch`; `about` is its anchor, or what else it is about.
async fn asked(
    pond: &Pond,
    batch: &str,
    purpose: QuestionPurpose,
    text: &str,
    options: &[&str],
    about: Option<&str>,
) -> Question {
    let options: Vec<String> = options.iter().map(|option| option.to_string()).collect();
    let mut question = Question::new(
        &pond.jerry,
        batch,
        purpose,
        text,
        &options,
        false,
        Utc::now(),
    )
    .unwrap();
    match purpose {
        QuestionPurpose::Anchor => question.anchor_id = about.map(str::to_string),
        _ => question.subject_ref = about.map(str::to_string),
    }
    let kept = pond
        .stores
        .questions
        .add_batch(std::slice::from_ref(&question))
        .await
        .unwrap();
    assert_eq!(kept.len(), 1);
    question
}

/// Options as a surface reads them: a label and a description each.
fn options(labels: &[&str]) -> Value {
    Value::Array(
        labels
            .iter()
            .map(|label| json!({"label": label, "description": ""}))
            .collect(),
    )
}

async fn question(pond: &Pond, id: &str) -> Question {
    pond.stores
        .questions
        .get(&pond.jerry, id)
        .await
        .unwrap()
        .unwrap()
}

/// An interest Goose guessed from one of Jerry's conversations.
async fn guessed(pond: &Pond, topic: &str) -> String {
    let seen = ObservedInterest {
        topic: topic.into(),
        category: "sport".into(),
        facets: vec![],
        signal: Signal::Asked,
    };
    pond.interests
        .observe(&pond.jerry, "a-conversation", &seen)
        .await
        .unwrap();
    let all = pond.interests.list(&pond.jerry).await.unwrap();
    all.into_iter().find(|i| i.topic == topic).unwrap().id
}

async fn rules(pond: &Pond, token: &str) -> Vec<Value> {
    get(pond, token, "/me/dont").await["rules"]
        .as_array()
        .unwrap()
        .clone()
}

fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_member_adds_reads_and_corrects_an_anchor() {
    let p = pond().await;
    assert_eq!(get(&p, JERRY, "/me/anchors").await, json!({"anchors": []}));

    let (status, trip) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/anchors",
        Some(json!({
            "kind": "trip",
            "title": " Trip to  Mombasa ",
            "starts_at": "2026-12-03T00:00:00Z",
            "ends_at": "2026-12-06T00:00:00Z",
            "place": " Mombasa ",
            "notes": "Two of us,  by train",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{trip}");
    let id = trip["id"].as_str().unwrap().to_string();
    assert_eq!(trip["title"], "Trip to Mombasa");
    assert_eq!(trip["kind"], "trip");
    assert_eq!(trip["state"], "active");
    assert_eq!(trip["origin"], "you");
    assert_eq!(trip["agent"], "trip");
    assert_eq!(trip["starts_at"], "2026-12-03T00:00:00Z");
    assert_eq!(trip["ends_at"], "2026-12-06T00:00:00Z");
    assert_eq!(trip["place"], "Mombasa");
    assert_eq!(
        trip["facts"],
        json!([{"label": "Notes", "value": "Two of us, by train", "source": "you"}])
    );
    assert_eq!(trip["mine"], true);
    assert_eq!(trip["shared_by"], Value::Null);
    assert_eq!(trip["shares"], json!([]));
    assert_eq!(trip["deliverables"], json!([]));
    assert_eq!(trip["questions"], json!([]));
    assert_eq!(trip["open_questions"], 0);
    assert_eq!(trip["job"], Value::Null);
    assert!(trip["confirmed_at"].is_string());
    for internal in ["profile_id", "shared_with", "origin_ref", "title_key"] {
        assert!(trip.get(internal).is_none(), "{internal} in {trip}");
    }

    let (status, again) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/anchors",
        Some(json!({"kind": "event", "title": "trip to mombasa!", "place": "Diani"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "one title, one anchor: {again}");
    assert_eq!(again["id"], id.as_str());
    assert_eq!(again["kind"], "trip");
    assert_eq!(again["place"], "Mombasa");

    for refused in [
        json!({"kind": "standing", "title": "Every day"}),
        json!({"kind": "holiday", "title": "Lamu"}),
        json!({"kind": "trip", "title": "  ... "}),
        json!({"kind": "trip"}),
        json!({"kind": "trip", "title": "Lamu",
               "starts_at": "2026-12-06T00:00:00Z", "ends_at": "2026-12-03T00:00:00Z"}),
        json!({"kind": "trip", "title": "Lamu", "starts_at": "next week"}),
    ] {
        let (status, body) = call(&p, JERRY, Method::POST, "/me/anchors", Some(refused)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body["error"].is_string(), "{body}");
    }

    let car = anchor(&p, "purchase", "A used Subaru Forester").await;
    let list = get(&p, JERRY, "/me/anchors").await;
    assert_eq!(
        ids(&list["anchors"]),
        [id.as_str(), car.as_str()],
        "dated first"
    );
    assert!(list["anchors"][0].get("questions").is_none(), "{list}");

    let (status, changed) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/anchors/{id}"),
        Some(json!({
            "title": "Mombasa with Achieng",
            "ends_at": "2026-12-07T00:00:00Z",
            "place": null,
            "facts": [
                {"label": "Budget", "value": " KSh 60,000 "},
                {"label": "Stay", "value": "Nyali", "source": "booking.com"},
                {"label": "budget", "value": "KSh 80,000"},
                {"label": "  ", "value": "dropped"},
            ],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["id"], id.as_str());
    assert_eq!(changed["title"], "Mombasa with Achieng");
    assert_eq!(
        changed["starts_at"], "2026-12-03T00:00:00Z",
        "left as it was"
    );
    assert_eq!(changed["ends_at"], "2026-12-07T00:00:00Z");
    assert_eq!(changed["place"], Value::Null);
    assert_eq!(
        changed["facts"],
        json!([
            {"label": "budget", "value": "KSh 80,000", "source": "you"},
            {"label": "Stay", "value": "Nyali", "source": "booking.com"},
        ]),
        "the list as the member left it"
    );
    assert_eq!(get(&p, JERRY, &format!("/me/anchors/{id}")).await, changed);

    let (status, taken) = call(
        &p,
        JERRY,
        Method::PATCH,
        &format!("/me/anchors/{id}"),
        Some(json!({"title": "a used subaru forester"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{taken}");
    for refused in [
        json!({"title": "  "}),
        json!({"starts_at": "2026-12-09T00:00:00Z"}),
        json!({"starts_at": null, "ends_at": "soon"}),
    ] {
        let uri = format!("/me/anchors/{id}");
        let (status, body) = call(&p, JERRY, Method::PATCH, &uri, Some(refused)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let (status, _) = call(&p, JERRY, Method::GET, "/me/anchors/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &p,
        JERRY,
        Method::PATCH,
        "/me/anchors/nope",
        Some(json!({"title": "Lamu"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_candidate_waits_for_a_yes_and_fades_on_a_no() {
    let p = pond().await;
    let summit = candidate(&p, "AI Summit Nairobi").await;
    let list = get(&p, JERRY, "/me/anchors").await;
    assert_eq!(list["anchors"][0]["state"], "candidate");
    assert_eq!(list["anchors"][0]["origin"], "chat");
    assert_eq!(list["anchors"][0]["confirmed_at"], Value::Null);

    for _ in 0..2 {
        let (status, confirmed) = call(
            &p,
            JERRY,
            Method::POST,
            &format!("/me/anchors/{summit}/confirm"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{confirmed}");
        assert_eq!(confirmed["state"], "active");
        assert!(confirmed["confirmed_at"].is_string());
    }
    let dismiss = format!("/me/anchors/{summit}/dismiss");
    assert_eq!(
        post(&p, JERRY, &dismiss, None).await,
        StatusCode::CONFLICT,
        "a yes is not taken back by a dismiss"
    );

    let wedding = candidate(&p, "Achieng's wedding").await;
    let dismiss = format!("/me/anchors/{wedding}/dismiss");
    for _ in 0..2 {
        assert_eq!(
            post(&p, JERRY, &dismiss, None).await,
            StatusCode::NO_CONTENT
        );
    }
    let list = get(&p, JERRY, "/me/anchors").await;
    assert_eq!(ids(&list["anchors"]), [summit.as_str()]);
    let faded = get(&p, JERRY, &format!("/me/anchors/{wedding}")).await;
    assert_eq!(faded["state"], "faded");
    for (method, uri, body) in [
        (Method::POST, format!("/me/anchors/{wedding}/confirm"), None),
        (Method::POST, format!("/me/anchors/{wedding}/stop"), None),
        (
            Method::PATCH,
            format!("/me/anchors/{wedding}"),
            Some(json!({"place": "Kisumu"})),
        ),
    ] {
        let (status, body) = call(&p, JERRY, method, &uri, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}: {body}");
    }

    // Adding it by hand brings a faded one back, and is the yes to one still waiting.
    let (status, back) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/anchors",
        Some(json!({"kind": "event", "title": "Achieng's Wedding", "place": "Kisumu"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{back}");
    assert_eq!(back["id"], wedding.as_str());
    assert_eq!(back["state"], "active");
    assert_eq!(back["origin"], "you");
    assert_eq!(back["ended_at"], Value::Null);
    assert_eq!(back["place"], "Kisumu");
    assert_eq!(
        back["facts"],
        json!([{"label": "When", "value": "in December", "source": "you said"}]),
        "what it held is kept"
    );

    let launch = candidate(&p, "Jarida launch").await;
    let (status, yes) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/anchors",
        Some(json!({"kind": "project", "title": "jarida launch",
                    "starts_at": "2027-01-15T09:00:00Z", "notes": "With the Nairobi team"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{yes}");
    assert_eq!(yes["id"], launch.as_str());
    assert_eq!(yes["state"], "active");
    assert_eq!(yes["origin"], "chat", "still the one Goose noticed");
    assert_eq!(yes["kind"], "project", "what the member says it is");
    assert_eq!(yes["agent"], "work");
    assert_eq!(yes["starts_at"], "2027-01-15T09:00:00Z");
    assert_eq!(
        yes["facts"],
        json!([
            {"label": "When", "value": "in December", "source": "you said"},
            {"label": "Notes", "value": "With the Nairobi team", "source": "you"},
        ])
    );

    let stop = format!(
        "/me/anchors/{}/stop",
        candidate(&p, "Nairobi Marathon").await
    );
    assert_eq!(
        post(&p, JERRY, &stop, Some(json!({"reasons": []}))).await,
        StatusCode::CONFLICT,
        "a candidate is dismissed, not stopped"
    );
}

#[tokio::test]
async fn an_anchor_shows_its_deliverables_questions_and_the_steps_of_its_job() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    made(&p, &trip, "stays", DeliverableState::Queued).await;
    let budget = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "What is your budget?",
        &["Under KSh 50,000", "More"],
        Some(&trip),
    )
    .await;
    let mut settled = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "Who is going?",
        &["Just me", "Two of us"],
        Some(&trip),
    )
    .await;
    assert!(settled.skip(Utc::now()));
    assert!(p.stores.questions.save(&settled).await.unwrap());

    let mut asking = EngineJob::new(
        JobKind::Questions,
        JobClass::Open,
        Some(&p.jerry),
        "Trip to Mombasa",
        Utc::now(),
    );
    asking.anchor_id = Some(trip.clone());
    p.stores.jobs.add(&asking).await.unwrap();
    let asking = started(&p, asking).await;
    let waiting = job_for(&p, &plan, JobClass::Open).await;

    let list = get(&p, JERRY, "/me/anchors").await;
    let shown = &list["anchors"][0];
    assert_eq!(
        shown["deliverables"],
        json!([
            {"id": plan.id, "kind": "plan", "slot": "plan", "title": "Plan for Trip to Mombasa",
             "state": "ready", "hook": "Three days by the sea for under KSh 60,000.",
             "version": 1, "changed": "", "unseen": true},
            {"id": shown["deliverables"][1]["id"], "kind": "shortlist", "slot": "stays",
             "title": "Places to stay for Trip to Mombasa", "state": "queued", "hook": "",
             "version": 0, "changed": "", "unseen": false},
        ])
    );
    assert_eq!(shown["open_questions"], 1);
    assert_eq!(
        shown["job"],
        json!({"kind": "questions", "subject": "Trip to Mombasa", "state": "running", "rounds": 0}),
        "the running job, not the queued one"
    );

    let whole = get(&p, JERRY, &format!("/me/anchors/{trip}")).await;
    assert_eq!(
        whole["questions"],
        json!([{"id": budget.id, "header": "", "text": "What is your budget?",
                "options": options(&["Under KSh 50,000", "More"]), "multi": false}])
    );
    assert_eq!(whole["deliverables"], shown["deliverables"]);
    assert_eq!(
        whole["job"]["steps"],
        json!([{"key": "make", "role": "engine-make", "label": "Writing a draft",
                "state": "pending", "summary": null}])
    );

    let mut asking = job(&p, &asking.id).await;
    asking
        .settle(JobState::Passed, "Two questions asked.", Utc::now())
        .unwrap();
    p.stores.jobs.save(&asking).await.unwrap();
    let whole = get(&p, JERRY, &format!("/me/anchors/{trip}")).await;
    assert_eq!(
        whole["job"],
        json!({"kind": "deliverable", "subject": "Plan for Trip to Mombasa", "state": "queued",
               "rounds": 0, "steps": []}),
        "a queued job has no task yet"
    );
    assert_eq!(job(&p, &waiting.id).await.task_id, None);
}

#[tokio::test]
async fn stopping_an_anchor_calls_off_its_jobs_and_writes_the_rule() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Making).await;
    let refreshing = started(&p, job_for(&p, &plan, JobClass::Deadline).await).await;
    let stays = made(&p, &trip, "stays", DeliverableState::Ready).await;
    let waiting = job_for(&p, &stays, JobClass::Deadline).await;
    let elsewhere = anchor(&p, "project", "Jarida launch").await;
    let digest = made(&p, &elsewhere, "digest", DeliverableState::Ready).await;
    let untouched = job_for(&p, &digest, JobClass::Open).await;

    let stop = format!("/me/anchors/{trip}/stop");
    let status = post(
        &p,
        JERRY,
        &stop,
        Some(json!({"reasons": ["bad_timing", "bad_timing"], "text": " We  postponed it "})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let list = get(&p, JERRY, "/me/anchors").await;
    assert_eq!(ids(&list["anchors"]), [elsewhere.as_str()]);
    let stopped = get(&p, JERRY, &format!("/me/anchors/{trip}")).await;
    assert_eq!(stopped["state"], "stopped");
    assert!(stopped["ended_at"].is_string());
    assert_eq!(stopped["job"], Value::Null);

    for called_off in [&refreshing, &waiting] {
        let job = job(&p, &called_off.id).await;
        assert_eq!(job.state, JobState::Cancelled);
        assert_eq!(job.outcome.as_deref(), Some("Stopped by the member."));
        assert!(job.finished_at.is_some());
    }
    let task = refreshing.task_id.as_deref().unwrap();
    let task = p.tasks.get_task(task).await.unwrap().unwrap();
    assert_eq!(task.state(), LongTaskState::Cancelled);
    assert_eq!(
        stored(&p, &plan).await.state,
        DeliverableState::Ready,
        "back to the version the member had"
    );
    assert_eq!(job(&p, &untouched.id).await.state, JobState::Queued);

    let kept = rules(&p, JERRY).await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["rule"], "Trip to Mombasa");
    assert_eq!(kept[0]["strength"], "avoid");
    assert_eq!(kept[0]["reasons"], json!(["bad_timing"]));
    assert_eq!(kept[0]["said"], "We postponed it");
    assert_eq!(kept[0]["origin"], "stop");
    assert_eq!(kept[0]["origin_ref"], trip.as_str());

    let status = post(&p, JERRY, &stop, Some(json!({"never": true}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "already stopped");
    assert_eq!(
        rules(&p, JERRY).await,
        kept,
        "a second stop says nothing new"
    );

    let stop = format!("/me/anchors/{elsewhere}/stop");
    assert_eq!(post(&p, JERRY, &stop, None).await, StatusCode::NO_CONTENT);
    assert_eq!(rules(&p, JERRY).await.len(), 1, "no reason, no rule");
    assert_eq!(job(&p, &untouched.id).await.state, JobState::Cancelled);

    let gossip = anchor(&p, "interest", "Celebrity gossip").await;
    let stop = format!("/me/anchors/{gossip}/stop");
    let (status, body) = call(
        &p,
        JERRY,
        Method::POST,
        &stop,
        Some(json!({"reasons": ["boring"]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let status = post(
        &p,
        JERRY,
        &stop,
        Some(json!({"reasons": [], "never": true})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let kept = rules(&p, JERRY).await;
    assert_eq!(kept[0]["rule"], "Celebrity gossip");
    assert_eq!(kept[0]["strength"], "never");
    assert_eq!(kept[0]["said"], Value::Null);

    // Adding a stopped one again takes back the rule its stop left.
    let (status, back) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/anchors",
        Some(json!({"kind": "trip", "title": "Trip to Mombasa"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{back}");
    assert_eq!(back["id"], trip.as_str());
    assert_eq!(back["state"], "active");
    let kept = rules(&p, JERRY).await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["rule"], "Celebrity gossip");
}

#[tokio::test]
async fn one_member_never_reads_or_changes_anothers_anchor_deliverable_or_question() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    let budget = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "What is your budget?",
        &["Under KSh 50,000", "More"],
        Some(&trip),
    )
    .await;
    job_for(&p, &plan, JobClass::Open).await;

    assert_eq!(
        get(&p, EMMANUEL, "/me/anchors").await,
        json!({"anchors": []})
    );
    assert_eq!(
        get(&p, EMMANUEL, "/me/questions").await,
        json!({"batches": []})
    );
    assert_eq!(
        get(&p, EMMANUEL, "/me/engine/jobs").await,
        json!({"jobs": []})
    );
    let (anchor, deliverable, question) = (
        format!("/me/anchors/{trip}"),
        format!("/me/deliverables/{}", plan.id),
        format!("/me/questions/{}", budget.id),
    );
    for (method, uri, body) in [
        (Method::GET, anchor.clone(), None),
        (Method::PATCH, anchor.clone(), Some(json!({"title": "His"}))),
        (Method::POST, format!("{anchor}/confirm"), None),
        (Method::POST, format!("{anchor}/dismiss"), None),
        (
            Method::POST,
            format!("{anchor}/stop"),
            Some(json!({"never": true})),
        ),
        (
            Method::PUT,
            format!("{anchor}/shares"),
            Some(json!({"profile_ids": [p.emmanuel]})),
        ),
        (Method::GET, deliverable.clone(), None),
        (Method::GET, format!("{deliverable}/file"), None),
        (Method::POST, format!("{deliverable}/refresh"), None),
        (Method::POST, format!("{deliverable}/seen"), None),
        (
            Method::POST,
            format!("{deliverable}/reject"),
            Some(json!({"verb": "never", "reasons": []})),
        ),
        (
            Method::POST,
            format!("{question}/answer"),
            Some(json!({"choices": ["More"]})),
        ),
        (Method::POST, format!("{question}/skip"), None),
    ] {
        let (status, body) = call(&p, EMMANUEL, method, &uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
    }

    let mine = get(&p, JERRY, &anchor).await;
    assert_eq!(mine["state"], "active");
    assert_eq!(mine["title"], "Trip to Mombasa");
    assert_eq!(mine["shares"], json!([]));
    let mine = stored(&p, &plan).await;
    assert_eq!(mine.state, DeliverableState::Ready);
    assert!(mine.is_unseen());
    assert_eq!(question_state(&p, &budget.id).await, QuestionState::Open);
    assert!(rules(&p, EMMANUEL).await.is_empty());
    assert!(rules(&p, JERRY).await.is_empty());
    assert_eq!(p.stores.jobs.open().await.unwrap().len(), 1);

    let unauthorised = Request::builder()
        .uri("/api/v1/me/anchors")
        .body(Body::empty())
        .unwrap();
    let response = p.router.clone().oneshot(unauthorised).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

async fn question_state(pond: &Pond, id: &str) -> QuestionState {
    question(pond, id).await.state
}

#[tokio::test]
async fn a_shared_anchor_is_read_and_its_questions_answered_but_not_changed() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    let budget = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "What is your budget?",
        &["Under KSh 50,000", "More"],
        Some(&trip),
    )
    .await;
    let who = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "Who is going?",
        &["Just me", "Two of us"],
        Some(&trip),
    )
    .await;
    asked(
        &p,
        "b2",
        QuestionPurpose::Profile,
        "Do you drive?",
        &["Yes", "No"],
        None,
    )
    .await;

    let shares = format!("/me/anchors/{trip}/shares");
    let (status, body) = call(
        &p,
        JERRY,
        Method::PUT,
        &shares,
        Some(json!({"profile_ids": [p.emmanuel, "a-stranger"]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, shared) = call(
        &p,
        JERRY,
        Method::PUT,
        &shares,
        Some(json!({"profile_ids": [p.emmanuel, p.jerry]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shared}");
    assert_eq!(
        shared["shares"],
        json!([p.emmanuel]),
        "never with its owner"
    );
    assert_eq!(shared["mine"], true);

    let list = get(&p, EMMANUEL, "/me/anchors").await;
    let his = &list["anchors"][0];
    assert_eq!(his["id"], trip.as_str());
    assert_eq!(his["mine"], false);
    assert_eq!(his["shared_by"], "Jerry");
    assert_eq!(his["shares"], json!([p.emmanuel]));
    assert_eq!(his["open_questions"], 2);
    assert_eq!(his["deliverables"][0]["id"], plan.id.as_str());
    assert_eq!(
        his["deliverables"][0]["unseen"], false,
        "the mark is its owner's"
    );
    assert!(!list.to_string().contains(&p.jerry), "{list}");
    let whole = get(&p, EMMANUEL, &format!("/me/anchors/{trip}")).await;
    assert_eq!(
        ids(&whole["questions"]),
        [budget.id.as_str(), who.id.as_str()]
    );

    let deliverable = format!("/me/deliverables/{}", plan.id);
    let read = get(&p, EMMANUEL, &deliverable).await;
    assert_eq!(read["body"], plan.body.as_str());
    assert_eq!(read["mine"], false);
    assert_eq!(read["unseen"], false);
    let file = send(
        &p,
        EMMANUEL,
        Method::GET,
        &format!("{deliverable}/file"),
        None,
    )
    .await;
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(
        post(&p, EMMANUEL, &format!("{deliverable}/seen"), None).await,
        StatusCode::NO_CONTENT
    );
    assert!(
        stored(&p, &plan).await.is_unseen(),
        "still new to its owner"
    );

    let anchor = format!("/me/anchors/{trip}");
    for (method, uri, body) in [
        (Method::PATCH, anchor.clone(), Some(json!({"title": "His"}))),
        (Method::POST, format!("{anchor}/confirm"), None),
        (Method::POST, format!("{anchor}/dismiss"), None),
        (
            Method::POST,
            format!("{anchor}/stop"),
            Some(json!({"never": true})),
        ),
        (
            Method::PUT,
            shares.clone(),
            Some(json!({"profile_ids": []})),
        ),
        (Method::POST, format!("{deliverable}/refresh"), None),
        (
            Method::POST,
            format!("{deliverable}/reject"),
            Some(json!({"verb": "not_this", "reasons": ["too_generic"]})),
        ),
        (Method::POST, format!("/me/questions/{}/skip", who.id), None),
    ] {
        let (status, body) = call(&p, EMMANUEL, method, &uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert!(body["error"].as_str().unwrap().contains("shared with you"));
    }
    assert_eq!(get(&p, JERRY, &anchor).await["state"], "active");
    assert_eq!(stored(&p, &plan).await.state, DeliverableState::Ready);
    assert!(rules(&p, EMMANUEL).await.is_empty());
    assert!(p.stores.jobs.open().await.unwrap().is_empty());

    let batches = get(&p, EMMANUEL, "/me/questions").await;
    assert_eq!(
        batches,
        json!({"batches": [{
            "id": "b1", "purpose": "anchor", "anchor_id": trip, "session_id": null, "round": 1,
            "title": "Trip to Mombasa", "state": "open",
            "questions": [
                {"id": budget.id, "header": "", "text": "What is your budget?",
                 "options": options(&["Under KSh 50,000", "More"]), "multi": false},
                {"id": who.id, "header": "", "text": "Who is going?",
                 "options": options(&["Just me", "Two of us"]), "multi": false},
            ],
        }]}),
        "the shared anchor's questions, and none of Jerry's own"
    );
    let answer = format!("/me/questions/{}/answer", budget.id);
    let status = post(&p, EMMANUEL, &answer, Some(json!({"choices": ["more"]}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let answered = question(&p, &budget.id).await;
    assert_eq!(answered.state, QuestionState::Answered);
    assert_eq!(answered.answer.unwrap().choices, ["More"]);
    let queued = p
        .stores
        .jobs
        .open_for_deliverable(&plan.id)
        .await
        .unwrap()
        .expect("the answer asks for a refresh");
    assert_eq!(queued.profile_id.as_deref(), Some(p.jerry.as_str()));
    assert_eq!(queued.class, JobClass::Asked);
    assert_eq!(
        get(&p, EMMANUEL, "/me/engine/jobs").await,
        json!({"jobs": []})
    );

    let (status, _) = call(
        &p,
        JERRY,
        Method::PUT,
        &shares,
        Some(json!({"profile_ids": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for uri in [anchor, deliverable] {
        let (status, _) = call(&p, EMMANUEL, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let answer = format!("/me/questions/{}/answer", who.id);
    let status = post(&p, EMMANUEL, &answer, Some(json!({"choices": ["Just me"]}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no longer shared");
    assert_eq!(
        get(&p, EMMANUEL, "/me/questions").await,
        json!({"batches": []})
    );
}

#[tokio::test]
async fn a_refresh_is_asked_for_once_and_only_while_it_can_be_made() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    let refresh = format!("/me/deliverables/{}/refresh", plan.id);
    for _ in 0..2 {
        let (status, asked) = call(&p, JERRY, Method::POST, &refresh, None).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(asked, json!({"state": "queued"}));
    }
    let open = p.stores.jobs.open().await.unwrap();
    assert_eq!(open.len(), 1, "two taps, one job");
    let mut job = open.into_iter().next().unwrap();
    assert_eq!(job.kind, JobKind::Deliverable);
    assert_eq!(job.class, JobClass::Asked);
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.task_id, None, "the runner opens the task");
    assert_eq!(job.profile_id.as_deref(), Some(p.jerry.as_str()));
    assert_eq!(job.anchor_id.as_deref(), Some(trip.as_str()));
    assert_eq!(job.deliverable_id.as_deref(), Some(plan.id.as_str()));
    assert_eq!(job.subject, "Plan for Trip to Mombasa");
    let read = get(&p, JERRY, &format!("/me/deliverables/{}", plan.id)).await;
    assert_eq!(
        read["job"],
        json!({"kind": "deliverable", "subject": "Plan for Trip to Mombasa",
               "state": "queued", "rounds": 0})
    );

    job.start(None, Utc::now()).unwrap();
    p.stores.jobs.save(&job).await.unwrap();
    let (_, asked) = call(&p, JERRY, Method::POST, &refresh, None).await;
    assert_eq!(asked, json!({"state": "running"}), "the open job answers");
    job.settle(JobState::Passed, "Passed its first check.", Utc::now())
        .unwrap();
    p.stores.jobs.save(&job).await.unwrap();
    let (_, asked) = call(&p, JERRY, Method::POST, &refresh, None).await;
    assert_eq!(asked, json!({"state": "queued"}));
    assert_eq!(p.stores.jobs.recent(&p.jerry, 10).await.unwrap().len(), 2);

    // A refresh the engine queued for itself moves ahead when the member asks for it.
    let stays = made(&p, &trip, "stays", DeliverableState::Unsure).await;
    let due = job_for(&p, &stays, JobClass::Deadline).await;
    let (status, asked) = call(
        &p,
        JERRY,
        Method::POST,
        &format!("/me/deliverables/{}/refresh", stays.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(asked, json!({"state": "queued"}));
    let only = p.stores.jobs.open_for_deliverable(&stays.id).await.unwrap();
    let only = only.unwrap();
    assert_eq!(only.id, due.id);
    assert_eq!(only.class, JobClass::Asked);
    assert_eq!(p.stores.jobs.open().await.unwrap().len(), 2);

    let packing = made(&p, &trip, "packing", DeliverableState::Rejected).await;
    let (status, body) = call(
        &p,
        JERRY,
        Method::POST,
        &format!("/me/deliverables/{}/refresh", packing.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let stop = format!("/me/anchors/{trip}/stop");
    assert_eq!(post(&p, JERRY, &stop, None).await, StatusCode::NO_CONTENT);
    let (status, body) = call(&p, JERRY, Method::POST, &refresh, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(p.stores.jobs.open().await.unwrap().is_empty());
    let (status, _) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/deliverables/nope/refresh",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn turning_a_deliverable_down_writes_the_rule_its_verb_asks_for() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    let stays = made(&p, &trip, "stays", DeliverableState::Unsure).await;
    let packing = made(&p, &trip, "packing", DeliverableState::Ready).await;
    let reject = |deliverable: &Deliverable| format!("/me/deliverables/{}/reject", deliverable.id);

    for refused in [
        json!({"verb": "maybe", "reasons": []}),
        json!({"reasons": ["too_generic"]}),
        json!({"verb": "never", "reasons": ["dull"]}),
    ] {
        let (status, body) = call(&p, JERRY, Method::POST, &reject(&plan), Some(refused)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert!(rules(&p, JERRY).await.is_empty());

    let status = post(
        &p,
        JERRY,
        &reject(&plan),
        Some(
            json!({"verb": "not_this", "reasons": ["too_generic", "too_generic"],
                    "text": "Too  vague"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(stored(&p, &plan).await.state, DeliverableState::Rejected);
    let kept = rules(&p, JERRY).await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["rule"], "Plan for Trip to Mombasa");
    assert_eq!(kept[0]["strength"], "avoid");
    assert_eq!(kept[0]["reasons"], json!(["too_generic"]));
    assert_eq!(kept[0]["said"], "Too vague");
    assert_eq!(kept[0]["origin"], "reject");
    assert_eq!(kept[0]["origin_ref"], plan.id.as_str());
    assert!(
        p.stores.jobs.open().await.unwrap().is_empty(),
        "\"not this one\" needs no model"
    );

    let status = post(
        &p,
        JERRY,
        &reject(&stays),
        Some(json!({"verb": "never", "reasons": ["not_relevant"],
                    "text": "No hostels, ever"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(stored(&p, &stays).await.state, DeliverableState::Rejected);
    let kept = rules(&p, JERRY).await;
    assert_eq!(kept.len(), 2, "{kept:?}");
    let never = &kept[0];
    assert_eq!(never["rule"], "No hostels, ever", "their words, for now");
    assert_eq!(never["strength"], "never");
    assert_eq!(never["reasons"], json!(["not_relevant"]));
    assert_eq!(never["said"], "No hostels, ever");
    assert_eq!(never["origin"], "reject");
    assert_eq!(never["origin_ref"], stays.id.as_str());
    let open = p.stores.jobs.open().await.unwrap();
    assert_eq!(open.len(), 1, "one job to write the general rule");
    let rewrite = &open[0];
    assert_eq!(rewrite.kind, JobKind::Rule);
    assert_eq!(rewrite.class, JobClass::Upkeep);
    assert_eq!(rewrite.state, JobState::Queued);
    assert_eq!(rewrite.subject, never["id"].as_str().unwrap());
    assert_eq!(rewrite.task_id, None);
    assert_eq!(rewrite.deliverable_id, None);
    assert_eq!(rewrite.profile_id.as_deref(), Some(p.jerry.as_str()));

    let status = post(&p, JERRY, &reject(&packing), Some(json!({"verb": "never"}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let kept = rules(&p, JERRY).await;
    assert_eq!(kept.len(), 3, "{kept:?}");
    let titled = kept
        .iter()
        .find(|rule| rule["rule"] == "Packing list for Trip to Mombasa")
        .expect("the title stands in for words they did not give");
    assert_eq!(titled["strength"], "never");
    assert_eq!(titled["said"], Value::Null);
    assert_eq!(titled["origin_ref"], packing.id.as_str());
    assert_eq!(
        p.stores.jobs.open().await.unwrap().len(),
        1,
        "no words, nothing to rewrite"
    );

    let status = post(
        &p,
        JERRY,
        &reject(&packing),
        Some(json!({"verb": "not_this", "reasons": ["bad_timing"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "already turned down");
    assert_eq!(rules(&p, JERRY).await, kept);

    let car = anchor(&p, "purchase", "A used Subaru Forester").await;
    let unmade = made(&p, &car, "shortlist", DeliverableState::Queued).await;
    let (status, body) = call(
        &p,
        JERRY,
        Method::POST,
        &reject(&unmade),
        Some(json!({"verb": "never"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(rules(&p, JERRY).await.len(), 3);

    // Turning down the version on screen calls off the refresh that was replacing it.
    let costs = made(&p, &car, "costs", DeliverableState::Making).await;
    let refreshing = started(&p, job_for(&p, &costs, JobClass::Open).await).await;
    let status = post(
        &p,
        JERRY,
        &reject(&costs),
        Some(json!({"verb": "not_this", "reasons": ["wrong_facts"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(stored(&p, &costs).await.state, DeliverableState::Rejected);
    let refreshing = job(&p, &refreshing.id).await;
    assert_eq!(refreshing.state, JobState::Cancelled);
    assert_eq!(
        refreshing.outcome.as_deref(),
        Some("Turned down by the member.")
    );
    let task = refreshing.task_id.as_deref().unwrap();
    let task = p.tasks.get_task(task).await.unwrap().unwrap();
    assert_eq!(task.state(), LongTaskState::Cancelled);
}

#[tokio::test]
async fn a_deliverable_is_read_marked_seen_and_kept_as_a_file() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let mut plan = made(&p, &trip, "plan", DeliverableState::Unsure).await;
    plan.objections = vec![Objection::new(
        ObjectionKind::Grounding,
        "The train fare",
        "Quote the fare from the operator",
    )];
    plan.changed = "The Saturday train now leaves at 08:00.".into();
    plan.version = 2;
    plan.task_id = Some("engine-1".into());
    plan.refresh_at = Some(Utc::now() + Duration::days(3));
    assert!(p.stores.anchors.save_deliverable(&plan).await.unwrap());
    let plan = stored(&p, &plan).await;

    let uri = format!("/me/deliverables/{}", plan.id);
    let read = get(&p, JERRY, &uri).await;
    assert_eq!(read["id"], plan.id.as_str());
    assert_eq!(read["anchor_id"], trip.as_str());
    assert_eq!(read["anchor_title"], "Trip to Mombasa");
    assert_eq!(read["kind"], "plan");
    assert_eq!(read["slot"], "plan");
    assert_eq!(read["title"], "Plan for Trip to Mombasa");
    assert_eq!(read["state"], "unsure");
    assert_eq!(read["body"], plan.body.as_str());
    assert_eq!(read["hook"], plan.hook.as_str());
    assert_eq!(read["facts"], json!([]));
    assert_eq!(read["sources"], json!([]));
    assert_eq!(
        read["objections"],
        json!([{"kind": "grounding", "about": "The train fare",
                "fix": "Quote the fare from the operator"}])
    );
    assert_eq!(read["changed"], "The Saturday train now leaves at 08:00.");
    assert_eq!(read["version"], 2);
    assert_eq!(read["task_id"], "engine-1");
    assert!(read["refresh_at"].is_string());
    assert_eq!(read["unseen"], true);
    assert_eq!(read["mine"], true);
    assert_eq!(read["job"], Value::Null);

    for _ in 0..2 {
        let status = post(&p, JERRY, &format!("{uri}/seen"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(get(&p, JERRY, &uri).await["unseen"], false);
    let list = get(&p, JERRY, "/me/anchors").await;
    assert_eq!(list["anchors"][0]["deliverables"][0]["unseen"], false);
    let kept = stored(&p, &plan).await;
    assert_eq!(kept.body, plan.body, "seen changes nothing else");
    assert_eq!(kept.updated_at, plan.updated_at);

    let file = send(&p, JERRY, Method::GET, &format!("{uri}/file"), None).await;
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(
        file.headers()[header::CONTENT_TYPE],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(
        file.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"plan-for-trip-to-mombasa.md\""
    );
    let bytes = axum::body::to_bytes(file.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(bytes.to_vec()).unwrap(), plan.body);

    let unmade = made(&p, &trip, "stays", DeliverableState::Queued).await;
    let file = format!("/me/deliverables/{}/file", unmade.id);
    let (status, body) = call(&p, JERRY, Method::GET, &file, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) = call(&p, JERRY, Method::GET, "/me/deliverables/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn questions_come_in_batches_under_what_they_are_about() {
    let p = pond().await;
    assert_eq!(
        get(&p, JERRY, "/me/questions").await,
        json!({"batches": []})
    );
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let budget = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "What is your budget?",
        &["Under KSh 50,000", "More"],
        Some(&trip),
    )
    .await;
    let who = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "Who is going?",
        &["Just me", "Two of us"],
        Some(&trip),
    )
    .await;
    let f1 = guessed(&p, "Formula 1").await;
    asked(
        &p,
        "b2",
        QuestionPurpose::Interest,
        "Do you want to keep up with Formula 1?",
        &[INTEREST_FOLLOW, INTEREST_DROP],
        Some(&f1),
    )
    .await;
    asked(
        &p,
        "b3",
        QuestionPurpose::Profile,
        "Do you drive?",
        &["Yes", "No"],
        None,
    )
    .await;
    asked(
        &p,
        "b4",
        QuestionPurpose::Ledger,
        "What was the KSh 4,200 at Carrefour for?",
        &["Groceries", "Household"],
        Some("a-capture"),
    )
    .await;
    let mut late = asked(
        &p,
        "b5",
        QuestionPurpose::Profile,
        "Do you cook?",
        &["Yes", "No"],
        None,
    )
    .await;
    late.expires_at = Some(Utc::now() - Duration::hours(1));
    assert!(p.stores.questions.save(&late).await.unwrap());

    let shown = get(&p, JERRY, "/me/questions").await;
    let batches = shown["batches"].as_array().unwrap();
    let heads: Vec<Value> = batches
        .iter()
        .map(|b| json!([b["id"], b["purpose"], b["anchor_id"], b["title"]]))
        .collect();
    assert_eq!(
        heads,
        [
            json!(["b4", "ledger", null, "Your spending"]),
            json!(["b3", "profile", null, "A few things about you"]),
            json!(["b2", "interest", null, "Things you keep coming back to"]),
            json!(["b1", "anchor", trip, "Trip to Mombasa"]),
        ],
        "newest first, and a question past its time is not asked"
    );
    assert_eq!(
        batches[3]["questions"],
        json!([
            {"id": budget.id, "header": "", "text": "What is your budget?",
             "options": options(&["Under KSh 50,000", "More"]), "multi": false},
            {"id": who.id, "header": "", "text": "Who is going?",
             "options": options(&["Just me", "Two of us"]), "multi": false},
        ])
    );
    assert_eq!(
        batches[2]["questions"][0]["options"],
        options(&["Follow it", "Not interested"])
    );

    let answer = format!("/me/questions/{}/answer", budget.id);
    for refused in [
        json!({"choices": ["A million"]}),
        json!({"choices": [], "text": "  "}),
        json!({"choices": ["More", "Under KSh 50,000"]}),
        json!({"choices": "More"}),
    ] {
        let (status, body) = call(&p, JERRY, Method::POST, &answer, Some(refused)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body["error"].is_string(), "{body}");
    }
    assert_eq!(question_state(&p, &budget.id).await, QuestionState::Open);

    let skip = format!("/me/questions/{}/skip", who.id);
    for _ in 0..2 {
        assert_eq!(post(&p, JERRY, &skip, None).await, StatusCode::NO_CONTENT);
    }
    assert_eq!(question_state(&p, &who.id).await, QuestionState::Skipped);
    let answer_skipped = format!("/me/questions/{}/answer", who.id);
    let (status, body) = call(
        &p,
        JERRY,
        Method::POST,
        &answer_skipped,
        Some(json!({"choices": ["Just me"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let status = post(&p, JERRY, &answer, Some(json!({"choices": ["More"]}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(post(&p, JERRY, &skip, None).await, StatusCode::NO_CONTENT);
    assert_eq!(
        post(&p, JERRY, &answer, None).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, _) = call(
        &p,
        JERRY,
        Method::POST,
        &answer,
        Some(json!({"choices": ["More"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "answered once");
    let skip = format!("/me/questions/{}/skip", budget.id);
    assert_eq!(post(&p, JERRY, &skip, None).await, StatusCode::CONFLICT);
    let shown = get(&p, JERRY, "/me/questions").await;
    assert_eq!(ids(&shown["batches"]), ["b4", "b3", "b2"]);

    // A stopped anchor's questions have nothing left to feed.
    let car = anchor(&p, "purchase", "A used Subaru Forester").await;
    asked(
        &p,
        "b6",
        QuestionPurpose::Anchor,
        "Petrol or diesel?",
        &["Petrol", "Diesel"],
        Some(&car),
    )
    .await;
    let shown = get(&p, JERRY, "/me/questions").await;
    assert_eq!(ids(&shown["batches"]), ["b6", "b4", "b3", "b2"]);
    let stop = format!("/me/anchors/{car}/stop");
    assert_eq!(post(&p, JERRY, &stop, None).await, StatusCode::NO_CONTENT);
    let shown = get(&p, JERRY, "/me/questions").await;
    assert_eq!(ids(&shown["batches"]), ["b4", "b3", "b2"]);
    let (status, _) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/questions/nope/answer",
        Some(json!({"choices": ["Yes"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_anchors_answer_asks_for_a_refresh_of_what_the_member_can_read() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    let stays = made(&p, &trip, "stays", DeliverableState::Unsure).await;
    let packing = made(&p, &trip, "packing", DeliverableState::Queued).await;
    let running = job_for(&p, &stays, JobClass::Deadline).await;
    let mut running = running;
    running.start(None, Utc::now()).unwrap();
    p.stores.jobs.save(&running).await.unwrap();
    let budget = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "What is your budget?",
        &["Under KSh 50,000", "More"],
        Some(&trip),
    )
    .await;
    let who = asked(
        &p,
        "b1",
        QuestionPurpose::Anchor,
        "Who is going?",
        &["Just me", "Two of us"],
        Some(&trip),
    )
    .await;

    let answer = format!("/me/questions/{}/answer", budget.id);
    let status = post(
        &p,
        JERRY,
        &answer,
        Some(json!({"choices": ["more"], "text": " about  150k "})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let answered = question(&p, &budget.id).await;
    assert_eq!(answered.state, QuestionState::Answered);
    assert!(answered.answered_at.is_some());
    let given = answered.answer.unwrap();
    assert_eq!(given.choices, ["More"], "stored as the option is spelled");
    assert_eq!(given.text.as_deref(), Some("about 150k"));

    let open = p.stores.jobs.open().await.unwrap();
    assert_eq!(
        open.len(),
        2,
        "one for the plan, and the one already running"
    );
    let asked_for = p.stores.jobs.open_for_deliverable(&plan.id).await.unwrap();
    let asked_for = asked_for.unwrap();
    assert_eq!(asked_for.class, JobClass::Asked);
    assert_eq!(asked_for.state, JobState::Queued);
    assert_eq!(asked_for.task_id, None);
    assert_eq!(asked_for.anchor_id.as_deref(), Some(trip.as_str()));
    let untouched = p.stores.jobs.open_for_deliverable(&stays.id).await.unwrap();
    let untouched = untouched.unwrap();
    assert_eq!(untouched.id, running.id);
    assert_eq!(untouched.class, JobClass::Deadline);
    let unmade = p.stores.jobs.open_for_deliverable(&packing.id).await;
    assert!(unmade.unwrap().is_none(), "nothing to refresh yet");

    // Their own words alone are kept and ask for the same refresh, never a second one.
    let answer = format!("/me/questions/{}/answer", who.id);
    let status = post(
        &p,
        JERRY,
        &answer,
        Some(json!({"choices": [], "text": "Achieng and her sister"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let answered = question(&p, &who.id).await;
    assert!(answered.has_free_text());
    assert!(answered.answer.unwrap().choices.is_empty());
    let open = p.stores.jobs.open().await.unwrap();
    assert_eq!(
        open.len(),
        3,
        "no second refresh; the settled batch queues the next round"
    );
    let next: Vec<&EngineJob> = open
        .iter()
        .filter(|job| job.kind == JobKind::Questions)
        .collect();
    assert_eq!(next.len(), 1, "{open:?}");
    assert_eq!(next[0].class, JobClass::Asked);
    assert_eq!(next[0].anchor_id.as_deref(), Some(trip.as_str()));
}

#[tokio::test]
async fn an_answer_about_the_member_feeds_their_interests_and_fact_sheet() {
    let p = pond().await;
    let f1 = guessed(&p, "Formula 1").await;
    let gossip = guessed(&p, "Celebrity gossip").await;
    let follow = asked(
        &p,
        "b1",
        QuestionPurpose::Interest,
        "Do you want to keep up with Formula 1?",
        &[INTEREST_FOLLOW, INTEREST_DROP],
        Some(&f1),
    )
    .await;
    let drop = asked(
        &p,
        "b1",
        QuestionPurpose::Interest,
        "Do you want to keep up with celebrity gossip?",
        &[INTEREST_FOLLOW, INTEREST_DROP],
        Some(&gossip),
    )
    .await;
    let drive = asked(
        &p,
        "b2",
        QuestionPurpose::Profile,
        "Do you drive?",
        &["Yes", "No"],
        None,
    )
    .await;
    let spend = asked(
        &p,
        "b3",
        QuestionPurpose::Ledger,
        "What was the KSh 4,200 at Carrefour for?",
        &["Groceries", "Household"],
        Some("a-capture"),
    )
    .await;
    let answer = |question: &Question| format!("/me/questions/{}/answer", question.id);

    let status = post(
        &p,
        JERRY,
        &answer(&follow),
        Some(json!({"choices": ["follow it"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let interest = p.interests.get(&p.jerry, &f1).await.unwrap().unwrap();
    assert!(interest.confirmed && !interest.avoid);
    assert!(interest.feeds_research());
    assert!(rules(&p, JERRY).await.is_empty());

    let status = post(
        &p,
        JERRY,
        &answer(&drop),
        Some(json!({"choices": [INTEREST_DROP]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let interest = p.interests.get(&p.jerry, &gossip).await.unwrap().unwrap();
    assert!(interest.avoid && !interest.confirmed);
    let kept = rules(&p, JERRY).await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["rule"], "Celebrity gossip");
    assert_eq!(kept[0]["strength"], "avoid");
    assert_eq!(kept[0]["origin"], "you");
    assert_eq!(kept[0]["origin_ref"], gossip.as_str());
    assert!(
        p.stores.jobs.open().await.unwrap().is_empty(),
        "an interest needs no job"
    );

    let status = post(
        &p,
        JERRY,
        &answer(&spend),
        Some(json!({"choices": ["Groceries"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(question_state(&p, &spend.id).await, QuestionState::Answered);
    assert!(
        p.stores.jobs.open().await.unwrap().is_empty(),
        "a ledger answer is only kept"
    );
    assert_eq!(rules(&p, JERRY).await.len(), 1);

    let status = post(
        &p,
        JERRY,
        &answer(&drive),
        Some(json!({"choices": ["Yes"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let open = p.stores.jobs.open().await.unwrap();
    assert_eq!(
        open.len(),
        1,
        "the fact sheet is asked to take the answer in"
    );
    assert_eq!(open[0].kind, JobKind::FactSheet);
    assert_eq!(open[0].class, JobClass::Upkeep);
    assert_eq!(open[0].state, JobState::Queued);
    assert_eq!(open[0].task_id, None);
    assert_eq!(open[0].profile_id.as_deref(), Some(p.jerry.as_str()));
}

#[tokio::test]
async fn the_engine_log_lists_a_members_recent_jobs_in_plain_words() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Trip to Mombasa").await;
    let plan = made(&p, &trip, "plan", DeliverableState::Ready).await;
    let start = Utc::now().trunc_subsecs(0) - Duration::hours(2);
    let at = |minutes: i64| start + Duration::minutes(minutes);

    let mut refreshed = EngineJob::for_deliverable(
        &p.jerry,
        &trip,
        &plan.id,
        &plan.title,
        JobClass::Deadline,
        None,
        at(0),
    );
    refreshed.start(Some("engine-1"), at(1)).unwrap();
    refreshed.count_round(vec![Objection::new(
        ObjectionKind::Vague,
        "The budget table",
        "Give a figure for each day",
    )]);
    refreshed.count_round(Vec::new());
    refreshed
        .settle(JobState::Passed, "Passed after 2 checks.", at(9))
        .unwrap();
    let capture = uuid::Uuid::new_v4().to_string();
    let picture = EngineJob::for_capture(&p.jerry, &capture, JobKind::Capture, at(10));
    let receipt = EngineJob::for_capture(&p.jerry, &capture, JobKind::Ledger, at(11));
    let rule = EngineJob::new(
        JobKind::Rule,
        JobClass::Upkeep,
        Some(&p.jerry),
        &uuid::Uuid::new_v4().to_string(),
        at(12),
    );
    let his = EngineJob::new(
        JobKind::FactSheet,
        JobClass::Upkeep,
        Some(&p.emmanuel),
        "Fact sheet",
        at(13),
    );
    for job in [&refreshed, &picture, &receipt, &rule, &his] {
        p.stores.jobs.add(job).await.unwrap();
    }

    let log = get(&p, JERRY, "/me/engine/jobs").await;
    let jobs = log["jobs"].as_array().unwrap();
    let lines: Vec<Value> = jobs
        .iter()
        .map(|job| json!([job["kind"], job["subject"], job["state"]]))
        .collect();
    assert_eq!(
        lines,
        [
            json!(["rule", "A rule", "queued"]),
            json!(["ledger", "A payment record", "queued"]),
            json!(["capture", "A picture", "queued"]),
            json!(["deliverable", "Plan for Trip to Mombasa", "passed"]),
        ],
        "newest first"
    );
    assert!(!log.to_string().contains(&capture), "{log}");
    let oldest = &jobs[3];
    assert_eq!(oldest["id"], refreshed.id.as_str());
    assert_eq!(oldest["rounds"], 2);
    assert_eq!(oldest["objections"], json!([]));
    assert_eq!(oldest["outcome"], "Passed after 2 checks.");
    assert_eq!(oldest["anchor_id"], trip.as_str());
    assert_eq!(oldest["deliverable_id"], plan.id.as_str());
    assert_eq!(oldest["created_at"], json!(at(0)));
    assert_eq!(oldest["started_at"], json!(at(1)));
    assert_eq!(oldest["finished_at"], json!(at(9)));
    assert_eq!(jobs[0]["started_at"], Value::Null);
    assert_eq!(jobs[0]["outcome"], Value::Null);
    for internal in ["profile_id", "task_id", "class"] {
        assert!(oldest.get(internal).is_none(), "{internal} in {oldest}");
    }

    let his_log = get(&p, EMMANUEL, "/me/engine/jobs").await;
    assert_eq!(his_log["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(his_log["jobs"][0]["subject"], "Fact sheet");

    for more in 0..110 {
        let job = EngineJob::new(
            JobKind::Heartbeat,
            JobClass::Upkeep,
            Some(&p.jerry),
            "",
            at(20 + more),
        );
        p.stores.jobs.add(&job).await.unwrap();
    }
    let count = |log: Value| log["jobs"].as_array().unwrap().len();
    assert_eq!(count(get(&p, JERRY, "/me/engine/jobs").await), 30);
    assert_eq!(count(get(&p, JERRY, "/me/engine/jobs?limit=2").await), 2);
    assert_eq!(
        count(get(&p, JERRY, "/me/engine/jobs?limit=500").await),
        100
    );
    let newest = get(&p, JERRY, "/me/engine/jobs?limit=1").await;
    assert_eq!(newest["jobs"][0]["subject"], "A check-in");
    let (status, body) = call(&p, JERRY, Method::GET, "/me/engine/jobs?limit=lots", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

/// A structured question of Jerry's for `trip`, settling `gap`, in `batch`.
async fn about_gap(pond: &Pond, batch: &str, trip: &str, gap: &str, text: &str) -> Question {
    let mut question = Question::new(
        &pond.jerry,
        batch,
        QuestionPurpose::Anchor,
        text,
        &["Just me", "Two of us", "More"],
        false,
        Utc::now(),
    )
    .unwrap();
    question.anchor_id = Some(trip.to_string());
    question.gap = Some(gap.to_string());
    question.header = "Who".into();
    let kept = pond
        .stores
        .questions
        .add_batch(std::slice::from_ref(&question))
        .await
        .unwrap();
    assert_eq!(kept.len(), 1);
    question
}

#[tokio::test]
async fn a_batch_is_answered_at_once_and_its_plan_asks_the_next_round() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Diani in April").await;
    let who = about_gap(&p, "b1", &trip, "who", "Who is going?").await;
    let budget = about_gap(&p, "b1", &trip, "budget", "How much will you spend?").await;
    let uri = "/me/questions/batches/b1/answer";

    let (status, body) = call(&p, JERRY, Method::POST, uri, Some(json!({"answers": []}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let stranger = json!({"answers": [{"question_id": "nope", "choices": ["More"]}]});
    let (status, _) = call(&p, JERRY, Method::POST, uri, Some(stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &p,
        JERRY,
        Method::POST,
        "/me/questions/batches/nope/answer",
        Some(json!({"answers": [{"question_id": who.id, "choices": [0]}]})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let wrong = json!({"answers": [{"question_id": who.id, "choices": ["A crowd"]}]});
    let (status, _) = call(&p, JERRY, Method::POST, uri, Some(wrong)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let both = json!({"answers": [
        {"question_id": who.id, "choices": [1]},
        {"question_id": budget.id, "choices": [], "text": "about 30k"},
    ]});
    assert_eq!(
        post(&p, JERRY, uri, Some(both.clone())).await,
        StatusCode::NO_CONTENT
    );
    let given = question(&p, &who.id).await.answer.unwrap();
    assert_eq!(given.choices, ["Two of us"], "a place reads as its label");
    assert_eq!(
        question(&p, &budget.id)
            .await
            .answer
            .unwrap()
            .text
            .as_deref(),
        Some("about 30k")
    );
    let (status, _) = call(&p, JERRY, Method::POST, uri, Some(both)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(get(&p, JERRY, "/me/questions").await["batches"], json!([]));

    let open = p.stores.jobs.open().await.unwrap();
    let next: Vec<&EngineJob> = open
        .iter()
        .filter(|j| j.kind == JobKind::Questions)
        .collect();
    assert_eq!(next.len(), 1, "the trip still has open gaps: {open:?}");
    assert_eq!(next[0].anchor_id.as_deref(), Some(trip.as_str()));
}

#[tokio::test]
async fn enough_passes_on_a_batch_and_ends_its_plans_chain() {
    let p = pond().await;
    let trip = anchor(&p, "trip", "Diani in April").await;
    let who = about_gap(&p, "b1", &trip, "who", "Who is going?").await;
    let (status, _) = call(
        &p,
        EMMANUEL,
        Method::POST,
        "/me/questions/batches/b1/enough",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not his batch");
    assert_eq!(
        post(&p, JERRY, "/me/questions/batches/b1/enough", None).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(question(&p, &who.id).await.state, QuestionState::Skipped);
    let plan = get(&p, JERRY, &format!("/me/anchors/{trip}")).await;
    assert!(
        plan["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["label"] == "Questions"),
        "{plan}"
    );
    let open = p.stores.jobs.open().await.unwrap();
    assert!(
        !open.iter().any(|j| j.kind == JobKind::Questions),
        "no next round after enough: {open:?}"
    );
}

#[tokio::test]
async fn a_chats_batch_is_listed_for_that_chat_and_a_waiting_turn_takes_its_answers() {
    use pond_core::user_data::domain::question::{QuestionDraft, QuestionOption};
    let p = pond().await;
    let draft = QuestionDraft {
        header: "Dates".into(),
        text: "When do you want to go?".into(),
        options: vec![
            QuestionOption::new("12-15 Dec", "A long weekend"),
            QuestionOption::new("19-22 Dec", "Before Christmas"),
        ],
        recommended: Some(1),
        ..QuestionDraft::default()
    };
    let desk = p.stores.desk.clone();
    let jerry = p.jerry.clone();
    let asking = tokio::spawn(async move {
        desk.ask(&jerry, "chat-1", vec![draft], false, Utc::now())
            .await
            .unwrap()
    });
    let mut listed = Value::Null;
    for _ in 0..100 {
        listed = get(&p, JERRY, "/me/questions?session_id=chat-1").await;
        if !listed["batches"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let batch = &listed["batches"][0];
    assert_eq!(batch["purpose"], "chat");
    assert_eq!(batch["session_id"], "chat-1");
    assert_eq!(batch["round"], 1);
    assert_eq!(batch["state"], "open");
    assert_eq!(batch["title"], "A chat with Goose");
    let asked = &batch["questions"][0];
    assert_eq!(asked["header"], "Dates");
    assert_eq!(asked["recommended"], 1);
    assert_eq!(
        asked["options"][0],
        json!({"label": "12-15 Dec", "description": "A long weekend"})
    );
    assert_eq!(
        get(&p, JERRY, "/me/questions?session_id=chat-2").await["batches"],
        json!([])
    );
    assert_eq!(
        get(&p, EMMANUEL, "/me/questions?session_id=chat-1").await["batches"],
        json!([])
    );

    let uri = format!(
        "/me/questions/batches/{}/answer",
        batch["id"].as_str().unwrap()
    );
    let answer = json!({"answers": [{"question_id": asked["id"], "choices": ["19-22 Dec"]}]});
    assert_eq!(
        post(&p, JERRY, &uri, Some(answer)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(asking.await.unwrap(), "Dates: 19-22 Dec");
}
