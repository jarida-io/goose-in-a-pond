//! `/me/agents` and `/me/ledger` over HTTP: a member switches their own specialists, a no to
//! Budget deletes the money pictures held for it, and the ledger is read, corrected and
//! deleted by its member only, and only while Budget is on.

use axum::http::{Method, StatusCode};
use chrono::{Duration, NaiveDate, Utc};
use pond_core::user_data::domain::anchor::{Anchor, AnchorKind, AnchorOrigin};
use pond_core::user_data::domain::capture::{Capture, CaptureDetails, CaptureState};
use pond_core::user_data::domain::ledger::{month_bounds, LedgerEntry};
use pond_core::user_data::domain::specialist::{AgentSwitch, SpecialistId};
use serde_json::{json, Value};

#[path = "support/two_members.rs"]
mod two_members;
use two_members::{base64_of, call, get, png, pond, Pond, EMMANUEL, JERRY};

async fn switch(pond: &Pond, token: &str, agent: &str, on: bool) -> (StatusCode, Value) {
    let uri = format!("/me/agents/{agent}");
    call(pond, token, Method::PUT, &uri, Some(json!({"on": on}))).await
}

fn agent<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == id)
        .unwrap_or_else(|| panic!("no agent `{id}` in {view}"))
}

/// Sets a member's Budget switch as the engine or an earlier choice would have left it.
async fn set_budget(pond: &Pond, member: &str, state: AgentSwitch) {
    pond.stores
        .agents
        .set(member, SpecialistId::Budget, state, Utc::now())
        .await
        .unwrap();
}

/// Sends a picture from Jerry's phone and returns its capture, still waiting to be read.
async fn sent(pond: &Pond, seed: u8) -> Capture {
    let body = json!({"image": base64_of(&png(seed)), "mime_type": "image/png", "kind": "photo"});
    let (status, taken) = call(pond, JERRY, Method::POST, "/me/captures", Some(body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{taken}");
    pond.stores
        .captures
        .list(&pond.jerry)
        .await
        .unwrap()
        .into_iter()
        .find(|capture| capture.state == CaptureState::Pending)
        .unwrap()
}

/// A picture of Jerry's the engine found money in, held for Budget.
async fn held(pond: &Pond, seed: u8) -> Capture {
    let mut capture = sent(pond, seed).await;
    capture.hold_for_budget(Utc::now());
    assert!(pond.stores.captures.save(&capture).await.unwrap());
    capture
}

/// A picture of Jerry's the engine read and found nothing sensitive in.
async fn read(pond: &Pond, seed: u8) -> Capture {
    let mut capture = sent(pond, seed).await;
    capture.read("A trail map", None, CaptureDetails::default(), Utc::now());
    assert!(pond.stores.captures.save(&capture).await.unwrap());
    capture
}

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// A ledger entry of `member`'s as the Budget agent would have read it.
async fn paid(
    pond: &Pond,
    member: &str,
    on: NaiveDate,
    payee: &str,
    amount_minor: i64,
    currency: &str,
    category: &str,
) -> LedgerEntry {
    let entry = LedgerEntry::new(
        member,
        on,
        payee,
        amount_minor,
        currency,
        category,
        Utc::now(),
    )
    .unwrap();
    pond.stores
        .ledger
        .add(std::slice::from_ref(&entry))
        .await
        .unwrap();
    entry
}

#[tokio::test]
async fn a_member_reads_their_agents_and_switches_their_own() {
    let pond = pond(true).await;
    let shipped = get(&pond, JERRY, "/me/agents").await;
    let ids: Vec<&str> = shipped["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["trip", "work", "hunt", "budget"]);
    assert_eq!(
        agent(&shipped, "budget"),
        &json!({
            "id": "budget",
            "title": "Budget",
            "about": "Keeps a ledger from the receipts and payment messages you capture, month by month.",
            "state": "off",
            "why": "",
        })
    );
    assert_eq!(agent(&shipped, "trip")["state"], "on");
    assert_eq!(agent(&shipped, "trip")["title"], "Trips and events");

    let (status, view) = switch(&pond, JERRY, "budget", true).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(agent(&view, "budget")["state"], "on");
    let (status, view) = switch(&pond, JERRY, "trip", false).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(agent(&view, "trip")["state"], "off");
    assert_eq!(agent(&view, "budget")["state"], "on");

    let theirs = get(&pond, EMMANUEL, "/me/agents").await;
    assert_eq!(agent(&theirs, "budget")["state"], "off");
    assert_eq!(agent(&theirs, "trip")["state"], "on");

    // An off that changes nothing stores nothing: Emmanuel stays on the shipped default.
    let (status, _) = switch(&pond, EMMANUEL, "budget", false).await;
    assert_eq!(status, StatusCode::OK);
    let stored = pond.stores.agents.switches(&pond.emmanuel).await.unwrap();
    assert!(stored.iter().all(|switch| switch.changed_at.is_none()));

    let (status, _) = switch(&pond, JERRY, "chef", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &pond,
        JERRY,
        Method::PUT,
        "/me/agents/budget",
        Some(json!({"on": "yes"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn an_offered_budget_says_why_and_a_no_deletes_the_pictures_held_for_it() {
    let pond = pond(true).await;
    for seed in 1..=3 {
        held(&pond, seed).await;
    }
    let kept = read(&pond, 9).await;
    set_budget(&pond, &pond.jerry, AgentSwitch::Offered).await;
    assert_eq!(pond.picture_files().len(), 4);

    let offered = get(&pond, JERRY, "/me/agents").await;
    assert_eq!(agent(&offered, "budget")["state"], "offered");
    assert_eq!(
        agent(&offered, "budget")["why"],
        "You have sent three pictures of receipts or payments"
    );
    assert_eq!(agent(&offered, "trip")["why"], "");

    let (status, view) = switch(&pond, JERRY, "budget", false).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(agent(&view, "budget")["state"], "declined");
    assert_eq!(agent(&view, "budget")["why"], "");
    assert!(pond
        .stores
        .captures
        .hot(&pond.jerry)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        pond.picture_files(),
        [format!("{}.png", kept.id)],
        "only the pictures held for Budget went"
    );

    let (_, view) = switch(&pond, JERRY, "budget", false).await;
    assert_eq!(
        agent(&view, "budget")["state"],
        "declined",
        "a no stays a no"
    );
    let (_, view) = switch(&pond, JERRY, "budget", true).await;
    assert_eq!(agent(&view, "budget")["state"], "on");
}

#[tokio::test]
async fn turning_budget_off_deletes_the_money_captures_held_and_their_pictures() {
    let pond = pond(true).await;
    set_budget(&pond, &pond.jerry, AgentSwitch::On).await;
    let receipt = held(&pond, 1).await;
    let kept = read(&pond, 2).await;
    assert_eq!(pond.picture_files().len(), 2);

    let (status, view) = switch(&pond, JERRY, "budget", false).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(agent(&view, "budget")["state"], "off");
    assert!(pond
        .stores
        .captures
        .hot(&pond.jerry)
        .await
        .unwrap()
        .is_empty());
    assert!(pond
        .stores
        .captures
        .find(&receipt.id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(pond.picture_files(), [format!("{}.png", kept.id)]);
    assert!(pond.stores.captures.find(&kept.id).await.unwrap().is_some());
}

#[tokio::test]
async fn the_ledger_is_read_by_month_with_a_total_for_each_currency() {
    let pond = pond(true).await;
    set_budget(&pond, &pond.jerry, AgentSwitch::On).await;
    let naivas = paid(
        &pond,
        &pond.jerry,
        day(2026, 11, 3),
        "Naivas",
        420_050,
        "KES",
        "groceries",
    )
    .await;
    paid(
        &pond,
        &pond.jerry,
        day(2026, 11, 9),
        "KPLC",
        350_000,
        "KES",
        "bills",
    )
    .await;
    paid(
        &pond,
        &pond.jerry,
        day(2026, 11, 9),
        "Carrefour",
        180_000,
        "KES",
        "groceries",
    )
    .await;
    paid(
        &pond,
        &pond.jerry,
        day(2026, 11, 12),
        "Hetzner",
        1_299,
        "EUR",
        "bills",
    )
    .await;
    paid(
        &pond,
        &pond.jerry,
        day(2026, 10, 31),
        "Java House",
        125_000,
        "KES",
        "eating_out",
    )
    .await;
    paid(
        &pond,
        &pond.jerry,
        day(2026, 12, 1),
        "Rent",
        4_500_000,
        "KES",
        "rent",
    )
    .await;

    let november = get(&pond, JERRY, "/me/ledger?month=2026-11").await;
    assert_eq!(november["month"], "2026-11");
    let entries = november["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 4, "{november}");
    assert_eq!(entries[0]["payee"], "Hetzner", "the newest day first");
    assert_eq!(entries[3]["payee"], "Naivas");
    assert_eq!(
        entries[3],
        json!({
            "id": naivas.id,
            "occurred_on": "2026-11-03",
            "payee": "Naivas",
            "amount": "4200.50",
            "currency": "KES",
            "category": "groceries",
            "anchor_id": null,
            "note": null,
        })
    );
    assert_eq!(
        november["totals"],
        json!([
            {
                "currency": "EUR",
                "total": "12.99",
                "by_category": [{"category": "bills", "total": "12.99"}],
            },
            {
                "currency": "KES",
                "total": "9500.50",
                "by_category": [
                    {"category": "groceries", "total": "6000.50"},
                    {"category": "bills", "total": "3500.00"},
                ],
            },
        ])
    );

    // With no month named, the ledger opens on the month it is now where the pond is.
    let now = get(&pond, JERRY, "/me/ledger").await;
    let (first, _) = month_bounds(now["month"].as_str().unwrap()).unwrap();
    assert!(
        (Utc::now().date_naive() - first).num_days().abs() <= 31,
        "{now}"
    );
    paid(&pond, &pond.jerry, first, "Kiosk", 5_000, "KES", "shopping").await;
    let this_month = get(&pond, JERRY, "/me/ledger").await;
    assert!(this_month["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["payee"] == "Kiosk"));

    let empty = get(&pond, JERRY, "/me/ledger?month=2025-01").await;
    assert_eq!(empty["entries"], json!([]));
    assert_eq!(empty["totals"], json!([]));
    for unreadable in ["November", "2026-13", "2026"] {
        let uri = format!("/me/ledger?month={unreadable}");
        let (status, why) = call(&pond, JERRY, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }
}

#[tokio::test]
async fn an_entry_is_corrected_and_deleted_by_its_member() {
    let pond = pond(true).await;
    set_budget(&pond, &pond.jerry, AgentSwitch::On).await;
    let entry = paid(
        &pond,
        &pond.jerry,
        day(2026, 11, 3),
        "Naivas",
        420_050,
        "KES",
        "groceries",
    )
    .await;
    let trip = Anchor::new(
        &pond.jerry,
        AnchorKind::Trip,
        "Three days in Mombasa",
        AnchorOrigin::You,
        Utc::now(),
    )
    .unwrap();
    assert!(pond.stores.anchors.add(&trip).await.unwrap());
    let uri = format!("/me/ledger/{}", entry.id);

    let (status, changed) = call(
        &pond,
        JERRY,
        Method::PATCH,
        &uri,
        Some(json!({
            "payee": "  Java   House ",
            "amount": "1250.5",
            "category": "Eating out",
            "occurred_on": "2026-11-04",
            "note": "Lunch with Amina",
            "anchor_id": trip.id,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(
        changed,
        json!({
            "id": entry.id,
            "occurred_on": "2026-11-04",
            "payee": "Java House",
            "amount": "1250.50",
            "currency": "KES",
            "category": "eating_out",
            "anchor_id": trip.id,
            "note": "Lunch with Amina",
        })
    );

    let (status, cleared) = call(
        &pond,
        JERRY,
        Method::PATCH,
        &uri,
        Some(json!({"amount": -300, "note": null, "anchor_id": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["amount"], "-300.00", "a refund");
    assert_eq!(cleared["note"], Value::Null);
    assert_eq!(cleared["anchor_id"], Value::Null);
    assert_eq!(cleared["payee"], "Java House", "what was left out stays");

    let theirs = Anchor::new(
        &pond.emmanuel,
        AnchorKind::Trip,
        "A week in Lamu",
        AnchorOrigin::You,
        Utc::now(),
    )
    .unwrap();
    assert!(pond.stores.anchors.add(&theirs).await.unwrap());
    let tomorrow_and_a_day = (Utc::now() + Duration::days(3)).date_naive();
    for unusable in [
        json!({"amount": "4,200.50"}),
        json!({"amount": "1250.505"}),
        json!({"amount": 0}),
        json!({"payee": "   "}),
        json!({"category": "gadgets"}),
        json!({"occurred_on": "3 November"}),
        json!({"occurred_on": tomorrow_and_a_day}),
        json!({"anchor_id": theirs.id}),
    ] {
        let (status, why) = call(&pond, JERRY, Method::PATCH, &uri, Some(unusable.clone())).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{unusable}: {why}"
        );
        assert!(why["error"].is_string(), "{why}");
    }
    let stored = pond
        .stores
        .ledger
        .get(&pond.jerry, &entry.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.amount_minor, -30_000,
        "a refused change changes nothing"
    );
    assert_eq!(stored.category, "eating_out");

    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&pond, JERRY, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &pond,
        JERRY,
        Method::PATCH,
        &uri,
        Some(json!({"note": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_member_never_reads_or_changes_anothers_ledger() {
    let pond = pond(true).await;
    set_budget(&pond, &pond.jerry, AgentSwitch::On).await;
    set_budget(&pond, &pond.emmanuel, AgentSwitch::On).await;
    let paid_on = day(2026, 11, 3);
    let mine = paid(
        &pond,
        &pond.jerry,
        paid_on,
        "Naivas",
        420_050,
        "KES",
        "groceries",
    )
    .await;
    paid(
        &pond,
        &pond.emmanuel,
        paid_on,
        "Shell",
        600_000,
        "KES",
        "transport",
    )
    .await;

    let theirs = get(&pond, EMMANUEL, "/me/ledger?month=2026-11").await;
    let payees: Vec<&str> = theirs["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["payee"].as_str().unwrap())
        .collect();
    assert_eq!(payees, ["Shell"]);
    assert_eq!(theirs["totals"][0]["total"], "6000.00");

    let uri = format!("/me/ledger/{}", mine.id);
    let (status, _) = call(
        &pond,
        EMMANUEL,
        Method::PATCH,
        &uri,
        Some(json!({"payee": "Mine now"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&pond, EMMANUEL, Method::DELETE, &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let still = pond
        .stores
        .ledger
        .get(&pond.jerry, &mine.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.payee, "Naivas");
}

#[tokio::test]
async fn the_ledger_is_closed_while_budget_is_not_on() {
    let pond = pond(true).await;
    let entry = paid(
        &pond,
        &pond.jerry,
        day(2026, 11, 3),
        "Naivas",
        420_050,
        "KES",
        "groceries",
    )
    .await;
    let uri = format!("/me/ledger/{}", entry.id);
    for state in [
        None,
        Some(AgentSwitch::Offered),
        Some(AgentSwitch::Declined),
        Some(AgentSwitch::Off),
    ] {
        if let Some(state) = state {
            set_budget(&pond, &pond.jerry, state).await;
        }
        for (method, uri, body) in [
            (Method::GET, "/me/ledger", None),
            (
                Method::PATCH,
                uri.as_str(),
                Some(json!({"payee": "Carrefour"})),
            ),
            (Method::DELETE, uri.as_str(), None),
        ] {
            let (status, why) = call(&pond, JERRY, method, uri, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{state:?}: {why}");
            assert!(why["error"].as_str().unwrap().contains("Budget"), "{why}");
        }
    }
    let kept = pond
        .stores
        .ledger
        .get(&pond.jerry, &entry.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.payee, "Naivas");

    let (_, view) = switch(&pond, JERRY, "budget", true).await;
    assert_eq!(agent(&view, "budget")["state"], "on");
    let open = get(&pond, JERRY, "/me/ledger?month=2026-11").await;
    assert_eq!(
        open["entries"][0]["payee"], "Naivas",
        "turning Budget off kept the ledger"
    );
}
