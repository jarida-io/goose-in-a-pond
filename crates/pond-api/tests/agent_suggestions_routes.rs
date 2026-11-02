//! Suggested agents: which cards a member sees, turning one on, declining one, and who owns the
//! copy when the desktop saves it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
#[allow(unused_imports)]
use pond_core::user_data::ports::profile::ProfileRepository as _;
#[allow(unused_imports)]
use pond_core::user_data::ports::settings::SettingsRepository as _;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use tower::ServiceExt;

#[path = "support/device_handshake.rs"]
mod device_handshake;
use device_handshake::DeviceHandshake;

struct Pond {
    router: axum::Router,
    member: String,
    profiles: Arc<dyn pond_core::user_data::ports::profile::ProfileRepository>,
    tasks: Arc<dyn pond_core::shared::ports::long_task::LongTaskRepository>,
    settings: Arc<SqliteSettingsRepository>,
    agent: Arc<pond_core::shared::mocks::mock_agent::MockAgent>,
    _dir: tempfile::TempDir,
}

/// One member and one phone; the phone is theirs when `paired`, the primary member is set when
/// `primary`.
async fn pond(paired: bool, primary: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    // The real store: the mock keeps no primary member.
    let settings = Arc::new(SqliteSettingsRepository::new(state.db.system.clone()));
    state.settings_repo = settings.clone();
    let agent = Arc::new(pond_core::shared::mocks::mock_agent::MockAgent::new());
    state.agent = agent.clone();
    let member = state
        .profile_repo
        .create(CreateProfileRequest {
            display_name: "Jerry".into(),
            avatar_emoji: "duck".into(),
        })
        .await
        .unwrap()
        .id;
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
    if paired {
        SqliteDeviceAttribution::new(state.db.system.clone())
            .set_device_profile(&device_id, Some(&member))
            .await
            .unwrap();
    }
    if primary {
        let mut settings = state.settings_repo.get().await.unwrap();
        settings.primary_profile_id = Some(member.clone());
        state.settings_repo.update(&settings).await.unwrap();
    }
    state.handshake = Arc::new(DeviceHandshake(device_id));
    state.recipe_repo = Some(Arc::new(SqliteRecipeRepository::new(
        state.db.system.clone(),
    )));
    let tasks: Arc<dyn pond_core::shared::ports::long_task::LongTaskRepository> = Arc::new(
        pond_infra::sqlite_long_task::SqliteLongTaskRepository::new(state.db.system.clone()),
    );
    Pond {
        profiles: state.profile_repo.clone(),
        tasks,
        settings,
        agent,
        router: build_router(
            Arc::new(state),
            std::path::PathBuf::from("pond-desktop/dist"),
        ),
        member,
        _dir: built.dir,
    }
}

/// `from_host` makes the request arrive from loopback, as the desktop's do.
fn call(
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
    from_host: bool,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer test-token");
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let mut request = builder.body(body).unwrap();
    if from_host {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                51_000,
            ))));
    }
    request
}

async fn send(pond: &Pond, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn offered(pond: &Pond, from_host: bool) -> Vec<String> {
    let (status, body) = send(
        pond,
        call(Method::GET, "/api/v1/agents/suggestions", None, from_host),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array()
        .unwrap()
        .iter()
        .map(|card| card["template"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_paired_member_is_offered_only_what_this_pond_can_run() {
    let pond = pond(true, false).await;
    let cards = offered(&pond, false).await;
    // A paired phone is presence; nothing here has weather, a camera, a tank or web search.
    for runs in ["welcome-back", "document-reader", "study-planner"] {
        assert!(
            cards.contains(&runs.to_string()),
            "{runs} is missing from {cards:?}"
        );
    }
    for cannot in [
        "morning-brief",
        "late-night-gate-alert",
        "low-tank-alert",
        "trip-planner",
    ] {
        assert!(
            !cards.contains(&cannot.to_string()),
            "{cannot} was offered: {cards:?}"
        );
    }
    let (_, body) = send(
        &pond,
        call(Method::GET, "/api/v1/agents/suggestions", None, false),
    )
    .await;
    let card = body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["template"] == "welcome-back")
        .unwrap();
    assert_eq!(card["title"], "Welcome back");
    assert_eq!(card["activations_summary"], "When you come home");
    assert_eq!(card["needs"][0], "presence");
    assert!(card["why"].as_str().is_some_and(|w| !w.is_empty()));
    assert!(card["description"].as_str().is_some_and(|d| !d.is_empty()));
}

#[tokio::test]
async fn nobody_identified_is_offered_nothing_and_may_change_nothing() {
    let pond = pond(false, false).await;
    assert!(offered(&pond, false).await.is_empty());
    let enable = "/api/v1/agents/suggestions/document-reader/enable";
    let (status, _) = send(&pond, call(Method::POST, enable, None, false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let dismiss = "/api/v1/agents/suggestions/document-reader/dismiss";
    let (status, _) = send(&pond, call(Method::POST, dismiss, None, false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Vacuity control: from the host with no primary member there is still nobody.
    assert!(offered(&pond, true).await.is_empty());
}

#[tokio::test]
async fn turning_one_on_copies_it_once_for_that_member() {
    let pond = pond(true, false).await;
    let enable = "/api/v1/agents/suggestions/document-reader/enable";
    let (status, recipe) = send(&pond, call(Method::POST, enable, None, false)).await;
    assert_eq!(status, StatusCode::CREATED, "{recipe}");
    assert_eq!(recipe["name"], "document-reader");
    assert_eq!(recipe["owner_profile_id"], pond.member.as_str());
    assert_eq!(recipe["template"], "document-reader");
    assert_eq!(recipe["active"], true);
    assert!(recipe["yaml"].as_str().unwrap().contains("giap_role:"));

    assert!(!offered(&pond, false)
        .await
        .contains(&"document-reader".to_string()));
    let (status, _) = send(&pond, call(Method::POST, enable, None, false)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the second tap must not make a second copy"
    );

    let (_, recipes) = send(&pond, call(Method::GET, "/api/v1/recipes", None, false)).await;
    assert_eq!(recipes.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_agent_this_pond_cannot_run_is_not_turned_on() {
    let pond = pond(true, false).await;
    let (status, body) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/agents/suggestions/morning-brief/enable",
            None,
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["missing"][0], "weather");
    let (status, _) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/agents/suggestions/no-such-agent/enable",
            None,
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn declining_a_card_hides_it_for_that_member() {
    let pond = pond(true, false).await;
    assert!(offered(&pond, false)
        .await
        .contains(&"study-planner".to_string()));
    let dismiss = "/api/v1/agents/suggestions/study-planner/dismiss";
    let (status, _) = send(&pond, call(Method::POST, dismiss, None, false)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cards = offered(&pond, false).await;
    assert!(!cards.contains(&"study-planner".to_string()));
    assert!(
        cards.contains(&"document-reader".to_string()),
        "only the declined card is hidden"
    );
    let (status, _) = send(&pond, call(Method::POST, dismiss, None, false)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "declining twice is fine");
    let (status, _) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/agents/suggestions/no-such-agent/dismiss",
            None,
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn from_the_desktop_the_primary_member_owns_what_is_saved() {
    let pond = pond(false, true).await;
    let enable = "/api/v1/agents/suggestions/study-planner/enable";
    let (status, recipe) = send(&pond, call(Method::POST, enable, None, true)).await;
    assert_eq!(status, StatusCode::CREATED, "{recipe}");
    assert_eq!(recipe["owner_profile_id"], pond.member.as_str());

    let hand_written = serde_json::json!({
        "name": "my-agent",
        "yaml": "instructions: Help.\ngiap_role:\n  tool_groups: []\n",
    });
    let (status, saved) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/recipes",
            Some(hand_written.clone()),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(saved["owner_profile_id"], pond.member.as_str());
    assert!(saved["template"].is_null());

    // Vacuity control: the same unpaired phone off the host owns nothing.
    let elsewhere = serde_json::json!({
        "name": "their-agent",
        "yaml": "instructions: Help.\ngiap_role:\n  tool_groups: []\n",
    });
    let (status, saved) = send(
        &pond,
        call(Method::POST, "/api/v1/recipes", Some(elsewhere), false),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(saved["owner_profile_id"].is_null());
    let (status, _) = send(&pond, call(Method::POST, enable, None, false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_edited_copy_may_narrow_its_tools_but_not_widen_them() {
    let pond = pond(true, false).await;
    let (_, recipe) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/agents/suggestions/document-reader/enable",
            None,
            false,
        ),
    )
    .await;
    let id = recipe["id"].as_str().unwrap().to_string();
    let yaml = recipe["yaml"].as_str().unwrap().to_string();
    let groups = "tool_groups: [giap-task-work, giap-knowledge]";
    assert!(
        yaml.contains(groups),
        "the template's groups moved; update this test"
    );

    let wider = yaml.replace(
        groups,
        "tool_groups: [giap-task-work, giap-knowledge, giap-weather]",
    );
    let (status, body) = send(
        &pond,
        call(
            Method::PUT,
            &format!("/api/v1/recipes/{id}"),
            Some(serde_json::json!({"yaml": wider})),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["widened"][0], "giap-weather");

    let narrower = yaml.replace(groups, "tool_groups: [giap-task-work]");
    let (status, body) = send(
        &pond,
        call(
            Method::PUT,
            &format!("/api/v1/recipes/{id}"),
            Some(serde_json::json!({"yaml": narrower})),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["template"], "document-reader",
        "an edit keeps where the copy came from"
    );
}

/// The status route lists every agent the pond keeps a prefill for, and a rebuild shows as
/// queued until the compile pass takes it.
#[tokio::test]
async fn prefill_status_lists_every_agent_and_a_rebuild_queues_one() {
    let pond = pond(true, false).await;
    let (status, body) = send(
        &pond,
        call(Method::GET, "/api/v1/agents/prefills", None, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let agents: Vec<String> = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["agent"].as_str().unwrap().to_string())
        .collect();
    for shipped in [
        "long-task-planner",
        "long-task-verifier",
        "long-task-worker",
    ] {
        assert!(agents.iter().any(|a| a == shipped), "{agents:?}");
    }
    assert_eq!(
        body["building"]["active"], false,
        "nothing compiles in a test"
    );

    let (status, _) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/agents/no-such-agent/prefill/rebuild",
            None,
            false,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown agent has no cache to rebuild"
    );
    let (status, _) = send(
        &pond,
        call(
            Method::POST,
            "/api/v1/agents/long-task-worker/prefill/rebuild",
            None,
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, body) = send(
        &pond,
        call(Method::GET, "/api/v1/agents/prefills", None, false),
    )
    .await;
    let worker = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "long-task-worker")
        .unwrap()
        .clone();
    // The record is process-wide; take the request back so no other test sees it.
    pond_core::shared::services::prefill_progress::process_prefill_progress().take_rebuilds();
    assert_eq!(worker["status"], "queued");
    assert_eq!(worker["kind"], "default");
}

/// The chat card's routes: a member sees their own task, its steps and its result, and can stop
/// it once; a caller nobody identified sees nothing.
#[tokio::test]
async fn a_member_sees_and_stops_their_long_task_and_nobody_else_does() {
    use pond_core::shared::domain::long_task::{LongTask, LongTaskBudget};
    use pond_core::user_data::domain::profile::ProfileScope;

    let pond = pond(true, false).await;
    let task = LongTask::start(
        ProfileScope::Owner(pond.member.clone()),
        None,
        "plan the school trip",
        None,
        LongTaskBudget::new(12, None).unwrap(),
        chrono::Utc::now(),
    )
    .unwrap();
    pond.tasks.create_task(&task).await.unwrap();

    let (status, body) = send(&pond, call(Method::GET, "/api/v1/long-tasks", None, false)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");
    let uri = format!("/api/v1/long-tasks/{}", task.id());
    let (status, body) = send(&pond, call(Method::GET, &uri, None, false)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["goal"], "plan the school trip");
    assert_eq!(body["state"], "planning");
    assert!(body["steps"].as_array().unwrap().is_empty());

    let (status, _) = send(
        &pond,
        call(Method::GET, "/api/v1/long-tasks/no-such-task", None, false),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let cancel = format!("{uri}/cancel");
    let (status, body) = send(&pond, call(Method::POST, &cancel, None, false)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send(&pond, call(Method::POST, &cancel, None, false)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a stopped task cannot be stopped again"
    );

    let stranger = pond_with_no_member().await;
    let (status, body) = send(
        &stranger,
        call(Method::GET, "/api/v1/long-tasks", None, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!([]),
        "nobody identified sees no task"
    );
}

/// Deep research asked partway through a conversation carries what was said before it, so the
/// helpers, who start in empty windows, still know it.
#[tokio::test]
async fn deep_research_takes_what_was_said_before_the_ask_into_its_brief() {
    use pond_core::shared::ports::long_task::LongTaskOwnerFilter;
    let pond = pond(true, false).await;
    turn_long_tasks_on(&pond).await;
    let (status, frames) = stream(
        &pond,
        call(
            Method::POST,
            "/api/v1/chat/stream",
            Some(serde_json::json!({"message": "We're four, and I'm lactose intolerant."})),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{frames:?}");
    let session = frames
        .iter()
        .find_map(|f| f["session_id"].as_str())
        .expect("the turn names its session")
        .to_string();
    let (status, frames) = stream(
        &pond,
        call(
            Method::POST,
            "/api/v1/chat/stream",
            Some(serde_json::json!({
                "message": "Plan our Mombasa trip.",
                "session_id": session,
                "deep_research": true
            })),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{frames:?}");
    let tasks = pond
        .tasks
        .list_tasks(LongTaskOwnerFilter::Member(&pond.member), true)
        .await
        .unwrap();
    let task = &tasks[0];
    let artifacts = pond.tasks.list_artifacts(task.id()).await.unwrap();
    let brief = pond_core::shared::services::long_task_runner::brief_artifact(&artifacts)
        .expect("the conversation before the ask is the task's brief");
    let body = pond
        .tasks
        .get_artifact(task.id(), &brief.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        body.body().contains("lactose intolerant"),
        "{}",
        body.body()
    );
    assert!(
        !body.body().contains("Plan our Mombasa trip"),
        "the ask is the goal, not the brief"
    );
}

/// Open tasks wait in the order they were asked for. A member sees where theirs stands and the
/// goals of their own tasks ahead of it, and can stop one to move up; another member's task is only
/// a place in the line. Deep research asked for behind them tells the turn where it waits.
#[tokio::test]
async fn a_member_sees_where_their_task_waits_and_nothing_of_another_members() {
    use pond_core::shared::domain::long_task::{LongTask, LongTaskBudget};
    use pond_core::user_data::domain::profile::ProfileScope;

    let pond = pond(true, false).await;
    let sam = pond
        .profiles
        .create(CreateProfileRequest {
            display_name: "Sam".into(),
            avatar_emoji: "fish".into(),
        })
        .await
        .unwrap()
        .id;
    let start = chrono::Utc::now() - chrono::Duration::minutes(10);
    let asked = |scope: ProfileScope, goal: &str, minutes: i64| {
        LongTask::start(
            scope,
            None,
            goal,
            None,
            LongTaskBudget::new(12, None).unwrap(),
            start + chrono::Duration::minutes(minutes),
        )
        .unwrap()
    };
    let first = asked(ProfileScope::Owner(pond.member.clone()), "plan the trip", 0);
    let sams = asked(ProfileScope::Owner(sam), "find a plumber", 1);
    let mine = asked(
        ProfileScope::Owner(pond.member.clone()),
        "a packing list",
        2,
    );
    for task in [&first, &sams, &mine] {
        pond.tasks.create_task(task).await.unwrap();
    }

    let view_of = |id: &str| {
        call(
            Method::GET,
            &format!("/api/v1/long-tasks/{id}"),
            None,
            false,
        )
    };
    let (status, view) = send(&pond, view_of(mine.id())).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["queue"]["position"], 3, "{view}");
    assert_eq!(view["queue"]["of"], 3, "{view}");
    assert_eq!(
        view["queue"]["ahead"],
        serde_json::json!([
            {"id": first.id(), "goal": "plan the trip", "state": "planning"},
            {"another_member": true},
        ])
    );
    let (_, front) = send(&pond, view_of(first.id())).await;
    assert_eq!(front["queue"]["position"], 1);
    assert_eq!(front["queue"]["ahead"], serde_json::json!([]));

    let (status, line) = send(
        &pond,
        call(Method::GET, "/api/v1/long-tasks/queue", None, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{line}");
    let line = line["line"].as_array().unwrap().clone();
    assert_eq!(line.len(), 3);
    assert_eq!(line[0]["id"], first.id());
    assert_eq!(line[0]["steps_total"], 0);
    assert_eq!(
        line[1],
        serde_json::json!({"position": 2, "another_member": true})
    );
    assert_eq!(line[2]["goal"], "a packing list");
    let listed = send(&pond, call(Method::GET, "/api/v1/long-tasks", None, false))
        .await
        .1
        .to_string();
    for seen in [
        view.to_string(),
        serde_json::json!(line).to_string(),
        listed,
    ] {
        assert!(
            !seen.contains("plumber"),
            "another member's goal is theirs: {seen}"
        );
    }

    turn_long_tasks_on(&pond).await;
    let ask = serde_json::json!({"message": "Which tents suit the rains?", "deep_research": true});
    let (status, frames) = stream(
        &pond,
        call(Method::POST, "/api/v1/chat/stream", Some(ask), false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{frames:?}");
    let waiting = pond
        .agent
        .last_request()
        .unwrap()
        .deep_research_task
        .and_then(|r| r.waiting)
        .expect("a task asked for behind others is told where it waits");
    assert!(waiting.starts_with("It is number 4 in line"), "{waiting}");
    assert!(waiting.contains("\"plan the trip\""), "{waiting}");
    assert!(!waiting.contains("plumber"), "{waiting}");

    let stop = format!("/api/v1/long-tasks/{}/cancel", first.id());
    let (status, _) = send(&pond, call(Method::POST, &stop, None, false)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, view) = send(&pond, view_of(mine.id())).await;
    assert_eq!(
        view["queue"]["position"], 2,
        "stopping the task ahead moves it up: {view}"
    );
    assert_eq!(
        view["queue"]["ahead"],
        serde_json::json!([{"another_member": true}])
    );
    let (_, stopped) = send(&pond, view_of(first.id())).await;
    assert_eq!(
        stopped["queue"],
        serde_json::Value::Null,
        "a stopped task is out of the line"
    );

    let stranger = pond_with_no_member().await;
    let (status, _) = send(
        &stranger,
        call(Method::GET, "/api/v1/long-tasks/queue", None, false),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "nobody identified sees no line"
    );
}

async fn pond_with_no_member() -> Pond {
    pond(false, false).await
}

/// The SSE frames of one streamed turn, in order.
async fn stream(pond: &Pond, request: Request<Body>) -> (StatusCode, Vec<serde_json::Value>) {
    let response = pond.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    if !status.is_success() {
        return (status, vec![serde_json::from_str(&text).unwrap()]);
    }
    let frames = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect();
    (status, frames)
}

async fn turn_long_tasks_on(pond: &Pond) {
    let mut settings = pond.settings.get().await.unwrap();
    settings.ext_orchestrator_enabled = true;
    settings.long_tasks_enabled = true;
    pond.settings.update(&settings).await.unwrap();
}

/// Deep research is offered and accepted only where it can run, and for a member it starts a
/// research task they own, opened in the stream and stored exactly as the prime's own
/// `task_start` call, with the turn told about it so the reply does not answer in its place.
#[tokio::test]
async fn deep_research_starts_a_research_task_the_chat_card_can_follow() {
    use pond_core::shared::domain::long_task::LongTaskKind;
    use pond_core::shared::ports::long_task::LongTaskOwnerFilter;

    let ask = serde_json::json!({
        "message": "Which heat pumps suit a cold one-bedroom flat?",
        "deep_research": true,
    });
    let availability = |pond: &Pond| {
        let request = call(Method::GET, "/api/v1/long-tasks/deep-research", None, false);
        let router = pond.router.clone();
        async move {
            let response = router.oneshot(request).await.unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        }
    };

    let pond = pond(true, false).await;
    assert_eq!(
        availability(&pond).await,
        serde_json::json!({"available": false, "reason": "off"})
    );
    let (status, frames) = stream(
        &pond,
        call(
            Method::POST,
            "/api/v1/chat/stream",
            Some(ask.clone()),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{frames:?}");
    assert_eq!(frames[0]["reason"], "deep_research_off");

    turn_long_tasks_on(&pond).await;
    assert_eq!(
        availability(&pond).await,
        serde_json::json!({"available": true})
    );
    let (status, frames) = stream(
        &pond,
        call(
            Method::POST,
            "/api/v1/chat/stream",
            Some(ask.clone()),
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{frames:?}");

    let tasks = pond
        .tasks
        .list_tasks(LongTaskOwnerFilter::Member(&pond.member), true)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1, "one research task for the member");
    let task = &tasks[0];
    assert_eq!(task.kind(), LongTaskKind::Research);
    assert_eq!(
        task.goal(),
        "Which heat pumps suit a cold one-bedroom flat?"
    );
    let session = task
        .origin_session_id()
        .expect("it reports to this conversation")
        .to_string();

    let call_frame = frames
        .iter()
        .position(|f| f["type"] == "tool_call" && f["tool"] == "giap-tasks__task_start")
        .expect("the stream opens with the task_start call");
    let result = frames
        .iter()
        .find(|f| f["type"] == "tool_result" && f["tool"] == "giap-tasks__task_start")
        .expect("and its result");
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Started task {}.", task.id())),
        "{result}"
    );
    let first_text = frames.iter().position(|f| f["type"] == "text");
    assert!(
        first_text.is_none_or(|text| call_frame < text),
        "the call comes before the reply"
    );
    let research = pond.agent.last_request().unwrap().deep_research_task;
    assert_eq!(
        research.as_ref().map(|r| r.task_id.as_str()),
        Some(task.id()),
        "the turn is told which task it started"
    );
    assert_eq!(
        research.and_then(|r| r.waiting),
        None,
        "nothing waits ahead of it"
    );

    let (status, history) = send(
        &pond,
        call(
            Method::GET,
            &format!("/api/v1/sessions/{session}/messages"),
            None,
            false,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    let stored = history.to_string();
    assert!(
        stored.contains(&format!("Started task {}", task.id())) && stored.contains("task_start"),
        "history keeps the call, so a reopened chat anchors the card: {stored}"
    );

    let uri = format!("/api/v1/long-tasks/{}", task.id());
    let (_, view) = send(&pond, call(Method::GET, &uri, None, false)).await;
    assert_eq!(view["kind"], "research");

    let stranger = pond_with_no_member().await;
    turn_long_tasks_on(&stranger).await;
    assert_eq!(
        availability(&stranger).await,
        serde_json::json!({"available": false, "reason": "unidentified"})
    );
    let (status, frames) = stream(
        &stranger,
        call(Method::POST, "/api/v1/chat/stream", Some(ask), false),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{frames:?}");
    assert_eq!(frames[0]["reason"], "deep_research_unidentified");
    assert!(
        stranger.agent.last_request().is_none(),
        "a refused turn never reaches the model"
    );
}

/// The response's status, content type, file name and body as text.
async fn fetch(pond: &Pond, uri: &str) -> (StatusCode, String, String, String) {
    let response = pond
        .router
        .clone()
        .oneshot(call(Method::GET, uri, None, false))
        .await
        .unwrap();
    let status = response.status();
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let (kind, disposition) = (
        header(axum::http::header::CONTENT_TYPE),
        header(axum::http::header::CONTENT_DISPOSITION),
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        kind,
        disposition,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

/// A report is joined by the pond from the summary and the drafted sections, so it is as long as
/// its plan; it exports as Markdown, a standalone page and, when it has dated events, a calendar.
#[tokio::test]
async fn a_long_report_is_joined_from_its_sections_and_exports_three_ways() {
    use pond_core::shared::domain::long_task::{
        Artifact, ArtifactKind, LongTask, LongTaskBudget, LongTaskKind,
    };
    use pond_core::user_data::domain::profile::ProfileScope;

    let pond = pond(true, false).await;
    let task = LongTask::start(
        ProfileScope::Owner(pond.member.clone()),
        None,
        "Plan our Mombasa trip for four friends",
        None,
        LongTaskBudget::new(24, None).unwrap(),
        chrono::Utc::now(),
    )
    .unwrap()
    .with_kind(LongTaskKind::Research);
    pond.tasks.create_task(&task).await.unwrap();
    let now = chrono::Utc::now();
    for (n, (kind, title, body)) in [
        (ArtifactKind::Findings, "Raw notes", "never shown in the report".to_string()),
        (ArtifactKind::Draft, "Day one", "Arrive and see [Fort Jesus](https://example.org/fort).".to_string()),
        (ArtifactKind::Draft, "Meals", "| Day | Dinner |\n|---|---|\n| Fri | Biryani |".to_string()),
        (ArtifactKind::Draft, "Result", "Four days by the sea.".to_string()),
        (
            ArtifactKind::Data,
            "Calendar",
            "{\"title\": \"Fort Jesus\", \"start\": \"2026-11-14T10:00:00+03:00\", \"location\": \"Mombasa\"}".to_string(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let at = now + chrono::Duration::seconds(n as i64);
        let artifact = Artifact::new(task.id(), None, kind, title, "one line", body, at).unwrap();
        pond.tasks.insert_artifact(&artifact).await.unwrap();
    }

    let uri = format!("/api/v1/long-tasks/{}", task.id());
    let (status, report) = send(
        &pond,
        call(Method::GET, &format!("{uri}/report"), None, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let markdown = report["markdown"].as_str().unwrap();
    assert!(
        markdown.starts_with("# Plan our Mombasa trip for four friends\n\nFour days by the sea.")
    );
    let (day, meals) = (
        markdown.find("## Day one").unwrap(),
        markdown.find("## Meals").unwrap(),
    );
    assert!(day < meals, "sections keep their order: {markdown}");
    assert!(
        !markdown.contains("never shown"),
        "research notes stay out of the report"
    );
    assert_eq!(report["sections"], 2);
    assert_eq!(report["kind"], "research");
    assert_eq!(report["sources"][0]["url"], "https://example.org/fort");
    assert_eq!(report["calendar_events"], 1);

    let (status, kind, disposition, body) =
        fetch(&pond, &format!("{uri}/export?format=markdown")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("text/markdown"));
    assert_eq!(
        disposition,
        "attachment; filename=\"plan-our-mombasa-trip-for-four-friends.md\""
    );
    assert_eq!(body, markdown);

    let (status, kind, _, body) = fetch(&pond, &format!("{uri}/export?format=html")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("text/html"));
    assert!(body.starts_with("<!doctype html>") && body.contains("<td>Biryani</td>"));

    let (status, kind, disposition, body) = fetch(&pond, &format!("{uri}/export?format=ics")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("text/calendar"));
    assert!(disposition.ends_with(".ics\""));
    assert!(
        body.contains("SUMMARY:Fort Jesus\r\n") && body.contains("DTSTART:20261114T070000Z\r\n")
    );

    let (status, ..) = fetch(&pond, &format!("{uri}/export?format=pdf")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let plain = LongTask::start(
        ProfileScope::Owner(pond.member.clone()),
        None,
        "nothing dated",
        None,
        LongTaskBudget::new(3, None).unwrap(),
        chrono::Utc::now(),
    )
    .unwrap();
    pond.tasks.create_task(&plain).await.unwrap();
    let (status, ..) = fetch(
        &pond,
        &format!("/api/v1/long-tasks/{}/export?format=ics", plain.id()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no dated events, no calendar file"
    );

    let stranger = pond_with_no_member().await;
    let (status, ..) = fetch(&stranger, &format!("{uri}/export?format=markdown")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
