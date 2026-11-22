//! `/me/intentions` over HTTP: a member reads what Goose means to do for them and what came of
//! what it meant to do, withdraws one that is still waiting, and never reaches another's.

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, SubsecRound, Utc};
use pond_core::shared::domain::todo::Transition;
use pond_core::user_data::domain::intention::{Condition, IntentAct, Intention, IntentionState};
use serde_json::{json, Value};

#[path = "support/two_members.rs"]
mod two_members;
use two_members::{call, get, pond, Pond, EMMANUEL, JERRY};

/// An intention of `member`'s made `minutes_ago`, waiting as the heartbeat would have left it.
async fn meant(
    pond: &Pond,
    member: &str,
    act: IntentAct,
    text: &str,
    condition: Condition,
    label: &str,
    minutes_ago: i64,
) -> Intention {
    let made = Utc::now().trunc_subsecs(0) - Duration::minutes(minutes_ago);
    let mut intention = Intention::new(
        member,
        act,
        text,
        condition,
        label,
        made + Duration::days(2),
        made,
    )
    .unwrap();
    intention.why = "The summit starts at 8.".into();
    assert!(pond.stores.intentions.add(&intention).await.unwrap());
    intention
}

async fn settled(pond: &Pond, intention: &Intention) {
    assert!(pond.stores.intentions.save(intention).await.unwrap());
}

fn tomorrow() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(0) + Duration::hours(20)
}

fn ids(view: &Value) -> Vec<&str> {
    view["intentions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|intention| intention["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_member_reads_what_goose_means_to_do_and_what_came_of_the_rest() {
    let pond = pond(true).await;
    let leave = meant(
        &pond,
        &pond.jerry,
        IntentAct::Push,
        "Leave by 7:15, the summit doors open at 8",
        Condition::Time { at: tomorrow() },
        "tomorrow at 7am",
        30,
    )
    .await;
    let mut told = meant(
        &pond,
        &pond.jerry,
        IntentAct::LookInto,
        "Check the train times to Mombasa",
        Condition::Chat,
        "when you next talk to Goose",
        20,
    )
    .await;
    told.done("Told Jerry the 8am train is full.", Utc::now())
        .unwrap();
    settled(&pond, &told).await;
    let mut late = meant(
        &pond,
        &pond.jerry,
        IntentAct::ProposeTodo,
        "Offer to book the car service",
        Condition::Place {
            place_id: "office".into(),
            transition: Transition::Arrive,
        },
        "when you arrive at the office",
        10,
    )
    .await;
    late.expire(Utc::now()).unwrap();
    settled(&pond, &late).await;

    let waiting = get(&pond, JERRY, "/me/intentions").await;
    assert_eq!(
        waiting,
        json!({"intentions": [{
            "id": leave.id,
            "act": "push",
            "text": "Leave by 7:15, the summit doors open at 8",
            "why": "The summit starts at 8.",
            "when": "tomorrow at 7am",
            "until": leave.until,
            "state": "waiting",
            "outcome": null,
            "anchor_id": null,
            "created_at": leave.created_at,
            "settled_at": null,
        }]})
    );
    assert_eq!(
        get(&pond, JERRY, "/me/intentions?state=waiting").await,
        waiting
    );

    let over = get(&pond, JERRY, "/me/intentions?state=settled").await;
    assert_eq!(
        ids(&over),
        [late.id.as_str(), told.id.as_str()],
        "newest first"
    );
    assert_eq!(over["intentions"][0]["state"], "expired");
    assert_eq!(
        over["intentions"][0]["when"],
        "when you arrive at the office"
    );
    assert_eq!(over["intentions"][1]["state"], "done");
    assert_eq!(
        over["intentions"][1]["outcome"],
        "Told Jerry the 8am train is full."
    );
    assert!(over["intentions"][1]["settled_at"].is_string());

    let (status, why) = call(
        &pond,
        JERRY,
        Method::GET,
        "/me/intentions?state=later",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
}

#[tokio::test]
async fn a_member_withdraws_a_waiting_intention_and_nothing_that_is_over() {
    let pond = pond(true).await;
    let leave = meant(
        &pond,
        &pond.jerry,
        IntentAct::Push,
        "Leave by 7:15, the summit doors open at 8",
        Condition::Time { at: tomorrow() },
        "tomorrow at 7am",
        30,
    )
    .await;
    let mut told = meant(
        &pond,
        &pond.jerry,
        IntentAct::Ask,
        "Ask which day suits the trip",
        Condition::Chat,
        "when you next talk to Goose",
        20,
    )
    .await;
    told.done("Asked.", Utc::now()).unwrap();
    settled(&pond, &told).await;

    let uri = format!("/me/intentions/{}", leave.id);
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let stored = pond
        .stores
        .intentions
        .get(&pond.jerry, &leave.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, IntentionState::Dropped);
    assert!(stored.settled_at.is_some());
    assert_eq!(
        get(&pond, JERRY, "/me/intentions").await["intentions"],
        json!([])
    );
    let over = get(&pond, JERRY, "/me/intentions?state=settled").await;
    assert_eq!(ids(&over), [told.id.as_str(), leave.id.as_str()]);
    assert_eq!(over["intentions"][1]["state"], "dropped");

    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "withdrawn twice is withdrawn"
    );
    let uri = format!("/me/intentions/{}", told.id);
    let (status, why) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}");
    let (status, _) = call(&pond, JERRY, Method::DELETE, "/me/intentions/nothing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_member_never_reads_or_withdraws_anothers_intention() {
    let pond = pond(true).await;
    let mine = meant(
        &pond,
        &pond.jerry,
        IntentAct::Push,
        "Leave by 7:15, the summit doors open at 8",
        Condition::Time { at: tomorrow() },
        "tomorrow at 7am",
        30,
    )
    .await;
    let theirs = meant(
        &pond,
        &pond.emmanuel,
        IntentAct::Refresh,
        "Refresh the Lamu plan",
        Condition::Chat,
        "when you next talk to Goose",
        5,
    )
    .await;

    let seen = get(&pond, EMMANUEL, "/me/intentions").await;
    assert_eq!(ids(&seen), [theirs.id.as_str()]);
    assert_eq!(
        get(&pond, EMMANUEL, "/me/intentions?state=settled").await["intentions"],
        json!([])
    );
    let uri = format!("/me/intentions/{}", mine.id);
    let (status, _) = call(&pond, EMMANUEL, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let still = get(&pond, JERRY, "/me/intentions").await;
    assert_eq!(ids(&still), [mine.id.as_str()]);
    assert_eq!(still["intentions"][0]["state"], "waiting");
}
