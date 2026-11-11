use super::*;
use crate::db::Database;
use crate::sqlite_proposal::SqliteProposalRepository;
use crate::sqlite_reminder::SqliteReminderRepository;
use chrono::Duration;
use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
use pond_core::shared::domain::todo::{resolve_hook, Answer, HookCatalog, Tz};
use pond_core::shared::services::todo_hooks::TodoHookEngine;
use pond_core::shared::services::todo_offers::offer_pending_reminders;
use pond_core::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use pond_core::user_data::ports::proposal::ProposalRepository;
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use pond_core::user_data::services::proactive_review::MAX_PROPOSALS_PER_DAY;
use serde_json::json;
use std::sync::Arc;

const NAIROBI: Tz = Tz::Africa__Nairobi;

fn utc(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap()
        .with_timezone(&Utc)
}

async fn pond() -> (SqliteTodoRepository, Pool<Sqlite>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    for (id, name) in [("jerry", "Jerry"), ("emmanuel", "Emmanuel")] {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(&db.system)
            .await
            .unwrap();
    }
    (
        SqliteTodoRepository::new(db.system.clone()),
        db.system.clone(),
        tmp,
    )
}

fn home(owner: &str) -> Place {
    Place::new(
        owner,
        "Home",
        -1.29,
        36.82,
        None,
        utc("2026-11-09T06:00:00Z"),
    )
    .unwrap()
}

fn todo(owner: &str, text: &str, catalog: &HookCatalog, hooks: &[(&str, Value)]) -> Todo {
    let now = utc("2026-11-09T07:00:00Z");
    Todo {
        id: uuid::Uuid::new_v4().to_string(),
        owner: owner.into(),
        kind: TodoKind::Todo,
        text: text.into(),
        notes: None,
        state: TodoState::Open,
        due_at: None,
        ask: true,
        source: TodoSource::You,
        source_ref: None,
        last_asked_at: None,
        asked: 0,
        snoozed_until: None,
        created_at: now,
        updated_at: now,
        done_at: None,
        hooks: hooks
            .iter()
            .map(|(k, spec)| resolve_hook(k, spec, catalog, now, NAIROBI).unwrap())
            .collect(),
    }
}

#[tokio::test]
async fn a_todo_and_its_hooks_round_trip_and_only_their_owner_reads_them() {
    let (repo, _, _tmp) = pond().await;
    let place = home("jerry");
    repo.add_place(&place).await.unwrap();
    let catalog = HookCatalog {
        places: vec![place.clone()],
        sources: vec![],
    };
    let mut t = todo(
        "jerry",
        "call Emmanuel",
        &catalog,
        &[
            ("place", json!({"place_id": place.id})),
            ("repeat", json!({"every": "weekday", "at": "18:00"})),
        ],
    );
    t.notes = Some("about the Fielder".into());
    t.due_at = Some(utc("2026-11-10T15:00:00Z"));
    repo.create(&t).await.unwrap();

    let back = repo.get("jerry", &t.id).await.unwrap().unwrap();
    assert_eq!(back, t);
    assert_eq!(repo.get("emmanuel", &t.id).await.unwrap(), None);
    assert!(repo
        .list("emmanuel", StateFilter::All)
        .await
        .unwrap()
        .is_empty());
    assert!(!repo.delete("emmanuel", &t.id).await.unwrap());
    assert_eq!(repo.places("emmanuel").await.unwrap(), vec![]);
    assert!(!repo.delete_place("emmanuel", &place.id).await.unwrap());
}

#[tokio::test]
async fn open_todos_list_by_due_time_and_closed_ones_by_when_they_closed() {
    let (repo, _, _tmp) = pond().await;
    let none = HookCatalog::default();
    let mut later = todo("jerry", "later", &none, &[]);
    later.due_at = Some(utc("2026-11-12T06:00:00Z"));
    let mut sooner = todo("jerry", "sooner", &none, &[]);
    sooner.due_at = Some(utc("2026-11-10T06:00:00Z"));
    let undated = todo("jerry", "undated", &none, &[]);
    let mut done = todo("jerry", "done", &none, &[]);
    done.state = TodoState::Done;
    done.done_at = Some(utc("2026-11-09T08:00:00Z"));
    for t in [&later, &undated, &done, &sooner] {
        repo.create(t).await.unwrap();
    }
    let texts = |todos: Vec<Todo>| todos.into_iter().map(|t| t.text).collect::<Vec<_>>();
    assert_eq!(
        texts(repo.list("jerry", StateFilter::Open).await.unwrap()),
        ["sooner", "later", "undated"]
    );
    assert_eq!(
        texts(repo.list("jerry", StateFilter::All).await.unwrap()),
        ["sooner", "later", "undated", "done"]
    );
    assert_eq!(
        texts(repo.list("jerry", StateFilter::Done).await.unwrap()),
        ["done"]
    );
}

#[tokio::test]
async fn replacing_hooks_replaces_them_all_and_deleting_a_place_takes_its_hooks() {
    let (repo, _, _tmp) = pond().await;
    let place = home("jerry");
    repo.add_place(&place).await.unwrap();
    let catalog = HookCatalog {
        places: vec![place.clone()],
        sources: vec![],
    };
    let mut t = todo(
        "jerry",
        "water the plants",
        &catalog,
        &[("repeat", json!({"every": "day", "at": "07:00"}))],
    );
    repo.create(&t).await.unwrap();
    t.hooks = vec![resolve_hook(
        "place",
        &json!({"place_id": place.id, "transition": "leave"}),
        &catalog,
        utc("2026-11-09T07:00:00Z"),
        NAIROBI,
    )
    .unwrap()];
    t.text = "water the plants before leaving".into();
    repo.update(&t, true).await.unwrap();
    let back = repo.get("jerry", &t.id).await.unwrap().unwrap();
    assert_eq!(back.hooks.len(), 1);
    assert_eq!(back.hooks[0].label, "When you leave home");
    assert_eq!(back.text, "water the plants before leaving");

    assert!(repo.delete_place("jerry", &place.id).await.unwrap());
    assert!(repo
        .get("jerry", &t.id)
        .await
        .unwrap()
        .unwrap()
        .hooks
        .is_empty());
    assert!(repo.delete("jerry", &t.id).await.unwrap());
    assert_eq!(repo.get("jerry", &t.id).await.unwrap(), None);
}

#[tokio::test]
async fn the_engine_runs_over_sqlite_from_hook_to_answer() {
    let (repo, _, _tmp) = pond().await;
    let repo = Arc::new(repo);
    let notifier = Arc::new(MockMemberNotifier::new().with_devices("jerry", &["phone"]));
    let engine = TodoHookEngine::new(repo.clone(), repo.clone(), notifier.clone());
    let place = home("jerry");
    repo.add_place(&place).await.unwrap();
    let catalog = HookCatalog {
        places: vec![place.clone()],
        sources: vec![],
    };
    let clocked = todo(
        "jerry",
        "call the garage about the Fielder",
        &catalog,
        &[("time", json!({"at": "2026-11-09T18:00:00+03:00"}))],
    );
    let placed = todo(
        "jerry",
        "call Emmanuel",
        &catalog,
        &[("place", json!({"place_id": place.id}))],
    );
    let mut dated = todo("jerry", "renew the logbook", &catalog, &[]);
    dated.due_at = Some(utc("2026-11-09T15:00:00Z"));
    for t in [&clocked, &placed, &dated] {
        repo.create(t).await.unwrap();
    }
    let now = utc("2026-11-09T15:00:30Z");
    repo.raise(&HookEvent::new(
        Some("jerry"),
        "place",
        &place.id,
        "arrive",
        json!({}),
        now,
    ))
    .await
    .unwrap();

    let report = engine.pass(now, NAIROBI).await.unwrap();
    assert_eq!((report.pushed, report.events), (3, 1));
    assert!(repo.unhandled(10).await.unwrap().is_empty());
    for id in [&clocked.id, &placed.id, &dated.id] {
        let t = repo.get("jerry", id).await.unwrap().unwrap();
        assert_eq!((t.asked, t.last_asked_at), (1, Some(now)), "{}", t.text);
    }
    assert!(
        engine
            .pass(now + Duration::minutes(1), NAIROBI)
            .await
            .unwrap()
            .pushed
            == 0
    );

    let mut answered = repo.get("jerry", &clocked.id).await.unwrap().unwrap();
    answered
        .answer(Answer::NotYet, Some(60), now, NAIROBI)
        .unwrap();
    repo.update(&answered, false).await.unwrap();
    assert_eq!(
        repo.follow_ups_due(now + Duration::minutes(61))
            .await
            .unwrap()
            .len(),
        1
    );
    let report = engine
        .pass(now + Duration::minutes(61), NAIROBI)
        .await
        .unwrap();
    assert_eq!(report.pushed, 1);
    let t = repo.get("jerry", &clocked.id).await.unwrap().unwrap();
    assert_eq!((t.asked, t.snoozed_until), (2, None));

    assert_eq!(engine.prune(now + Duration::days(8)).await.unwrap(), 1);
    assert_eq!(notifier.sent().len(), 4);
}

#[tokio::test]
async fn a_todo_asked_three_times_is_never_due_again() {
    let (repo, _, _tmp) = pond().await;
    let mut t = todo("jerry", "pay the KPLC bill", &HookCatalog::default(), &[]);
    t.due_at = Some(utc("2026-11-09T06:00:00Z"));
    t.asked = MAX_ASKS;
    t.snoozed_until = Some(utc("2026-11-09T06:30:00Z"));
    repo.create(&t).await.unwrap();
    let now = utc("2026-11-10T06:00:00Z");
    assert!(repo.due_dates(now).await.unwrap().is_empty());
    assert!(repo.follow_ups_due(now).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_members_cart_items_accounts_and_webhooks_are_sources() {
    let (repo, pool, _tmp) = pond().await;
    let now = "2026-11-09T07:00:00Z";
    sqlx::query(
        "INSERT INTO cart_items (id, profile_id, url, title, site, created_at, updated_at) \
         VALUES ('c1', 'jerry', 'https://www.kilimall.co.ke/x', 'Fielder', '', ?, ?), \
                ('c2', 'emmanuel', 'https://jiji.co.ke/y', 'Vitz', '', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO context_sources (id, kind, provider, profile_id, created_at) VALUES \
         ('mail:gmail:jerry', 'mail', 'gmail', 'jerry', ?), \
         ('calendar:google:jerry', 'calendar', 'google', 'jerry', ?), \
         ('camera:camera-1', 'camera', 'camera-1', 'jerry', ?)",
    )
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO connector_webhooks (id, profile_id, name, token_hash, created_at) \
         VALUES ('wh1', 'jerry', 'Doorbell', 'h1', ?)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    let cart = repo.cart_sources("jerry").await.unwrap();
    assert_eq!(cart.len(), 1);
    assert_eq!(
        (cart[0].id.as_str(), cart[0].label.as_str()),
        ("cart:c1", "Fielder on Kilimall")
    );
    let ids: Vec<String> = repo
        .connector_sources("jerry")
        .await
        .unwrap()
        .into_iter()
        .map(|s| format!("{} {}", s.id, s.label))
        .collect();
    assert_eq!(
        ids,
        [
            "mail:mail:gmail:jerry Mail (Gmail)",
            "calendar:calendar:google:jerry Calendar (Google)",
            "webhook:wh1 Doorbell",
        ]
    );
    assert!(repo.connector_sources("emmanuel").await.unwrap().is_empty());
}

fn mention(id: &str, owner: &str) -> CapturedReminder {
    CapturedReminder {
        id: id.into(),
        about: format!("thing {id}"),
        when_said: "on Friday".into(),
        session_id: "s1".into(),
        window_id: format!("w-{id}"),
        subject: "Jerry".into(),
        profile_id: Some(owner.into()),
        said_at: utc("2026-11-09T07:00:00Z"),
        captured_at: utc("2026-11-09T07:05:00Z"),
        disposition: ReminderDisposition::Pending,
    }
}

#[tokio::test]
async fn offers_share_the_daily_cap_with_proposals_and_are_settled_once() {
    let (_, pool, _tmp) = pond().await;
    let reminders = SqliteReminderRepository::new(pool.clone());
    let proposals = SqliteProposalRepository::new(pool.clone());
    for i in 0..(MAX_PROPOSALS_PER_DAY + 2) {
        reminders
            .capture(&mention(&format!("r{i}"), "jerry"))
            .await
            .unwrap();
    }
    let now = utc("2026-11-09T08:00:00Z");
    let report = offer_pending_reminders(&reminders, &proposals, 50, now)
        .await
        .unwrap();
    assert_eq!(report.offered.len(), MAX_PROPOSALS_PER_DAY);
    assert_eq!(report.capped, 2);
    assert_eq!(
        proposals
            .count_made_since("jerry", now - Duration::days(1))
            .await
            .unwrap(),
        MAX_PROPOSALS_PER_DAY,
        "the reviewer sees the offers in its count"
    );
    let again = offer_pending_reminders(&reminders, &proposals, 50, now + Duration::hours(1))
        .await
        .unwrap();
    assert!(again.offered.is_empty(), "the cap holds across runs");

    let open = reminders.open_to("jerry").await.unwrap();
    assert_eq!(
        open.len(),
        MAX_PROPOSALS_PER_DAY + 2,
        "the cap limits the pushes, not what Jerry can find and answer"
    );
    assert!(reminders.open_to("emmanuel").await.unwrap().is_empty());
    let id = report.offered[0].id.clone();
    assert!(!reminders
        .settle_offer("emmanuel", &id, true, now)
        .await
        .unwrap());
    assert!(reminders
        .settle_offer("jerry", &id, true, now)
        .await
        .unwrap());
    assert!(!reminders
        .settle_offer("jerry", &id, false, now)
        .await
        .unwrap());
    // One the cap held back is answered the same way, and the offer job then leaves it alone.
    let held: Vec<String> = open
        .iter()
        .map(|r| r.id.clone())
        .filter(|id| report.offered.iter().all(|o| &o.id != id))
        .collect();
    assert_eq!(held.len(), 2);
    assert!(reminders
        .settle_offer("jerry", &held[0], true, now)
        .await
        .unwrap());
    assert_eq!(
        reminders.open_to("jerry").await.unwrap().len(),
        MAX_PROPOSALS_PER_DAY
    );
    let next_day = offer_pending_reminders(&reminders, &proposals, 50, now + Duration::days(2))
        .await
        .unwrap();
    assert!(
        next_day.offered.iter().all(|r| r.id != held[0]),
        "an answered reminder is never pushed"
    );
}
