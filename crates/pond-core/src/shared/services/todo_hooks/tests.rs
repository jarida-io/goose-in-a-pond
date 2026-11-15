use super::*;
use crate::mcp::mocks::mock_member_notifier::MockMemberNotifier;
use crate::shared::domain::todo::{
    resolve_hook, Answer, HookCatalog, HookEvent, HookSource, Place, TodoKind, TodoSource,
    TodoState,
};
use crate::shared::mocks::mock_todo::MockTodoRepository;
use serde_json::Value;

const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

fn utc(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap()
        .with_timezone(&Utc)
}

struct Rig {
    repo: Arc<MockTodoRepository>,
    notifier: Arc<MockMemberNotifier>,
    engine: TodoHookEngine,
}

fn rig() -> Rig {
    let repo = Arc::new(MockTodoRepository::new());
    let notifier = Arc::new(MockMemberNotifier::new().with_devices("jerry", &["jerry-phone"]));
    let engine = TodoHookEngine::new(repo.clone(), repo.clone(), notifier.clone());
    Rig {
        repo,
        notifier,
        engine,
    }
}

fn catalog() -> HookCatalog {
    HookCatalog {
        places: vec![Place {
            id: "p-home".into(),
            owner: "jerry".into(),
            label: "Home".into(),
            lat: -1.29,
            lng: 36.82,
            radius_m: 150.0,
            created_at: utc("2026-11-09T09:00:00Z"),
        }],
        sources: vec![HookSource::presence(), HookSource::camera("camera-1")],
    }
}

fn todo(kind: TodoKind, text: &str, hooks: &[(&str, Value)]) -> Todo {
    let now = utc("2026-11-09T09:00:00Z");
    Todo {
        id: format!("t-{}", text.len()),
        owner: "jerry".into(),
        kind,
        text: text.into(),
        notes: None,
        state: TodoState::Open,
        due_at: None,
        ask: kind == TodoKind::Todo,
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
            .map(|(kind, spec)| resolve_hook(kind, spec, &catalog(), now, NAIROBI).unwrap())
            .collect(),
    }
}

async fn stored(rig: &Rig, id: &str) -> Todo {
    rig.repo.get("jerry", id).await.unwrap().unwrap()
}

#[tokio::test]
async fn a_due_time_hook_asks_once_in_gooses_voice_with_the_checkin_action() {
    let rig = rig();
    let t = todo(
        TodoKind::Todo,
        "call the garage about the Fielder",
        &[("time", json!({"at": "2026-11-09T18:00:00+03:00"}))],
    );
    rig.repo.create(&t).await.unwrap();

    let early = rig
        .engine
        .pass(utc("2026-11-09T14:59:00Z"), NAIROBI)
        .await
        .unwrap();
    assert_eq!(early.pushed, 0);

    let at = utc("2026-11-09T15:00:10Z");
    assert_eq!(rig.engine.pass(at, NAIROBI).await.unwrap().pushed, 1);
    let sent = rig.notifier.sent();
    assert_eq!(sent.len(), 1);
    let (member, notice) = &sent[0];
    assert_eq!(member, "jerry");
    assert_eq!(notice.category, "action_required");
    assert_eq!(notice.title, "Did you call the garage about the Fielder?");
    assert_eq!(
        notice.data,
        Some(json!({
            "action": "todo_checkin",
            "todo_id": t.id,
            "text": "call the garage about the Fielder",
            "label": "Done",
            "open": "todos",
        }))
    );

    let after = stored(&rig, &t.id).await;
    assert_eq!((after.asked, after.last_asked_at), (1, Some(at)));
    assert_eq!(after.hooks[0].next_fire_at, None, "a time fires once");
    assert_eq!(after.hooks[0].last_fired_at, Some(at));

    rig.engine
        .pass(utc("2026-11-09T16:00:00Z"), NAIROBI)
        .await
        .unwrap();
    assert_eq!(
        rig.notifier.sent().len(),
        1,
        "a spent time hook stays quiet"
    );
}

#[tokio::test]
async fn a_reminder_on_a_repeat_says_reminder_and_moves_to_the_next_day() {
    let rig = rig();
    let t = todo(
        TodoKind::Reminder,
        "take the bins out",
        &[("repeat", json!({"every": "weekday", "at": "18:00"}))],
    );
    rig.repo.create(&t).await.unwrap();
    // Friday 13 November, 18:00 Nairobi.
    let at = utc("2026-11-13T15:00:00Z");
    rig.engine.pass(at, NAIROBI).await.unwrap();
    let sent = rig.notifier.sent();
    let (_, notice) = sent.last().unwrap();
    assert_eq!(notice.category, "info");
    assert_eq!(notice.title, "Reminder: take the bins out");
    assert_eq!(notice.body, "Every weekday at 18:00");
    assert_eq!(
        notice.data,
        Some(json!({
            "open": "todos",
            "todo_id": t.id,
            "quiet_pass": true,
            "speak_if_unfetched": true,
        })),
        "a reminder passes quiet hours and is said aloud if no phone fetches it"
    );
    let after = stored(&rig, &t.id).await;
    assert_eq!(after.asked, 0, "a reminder asks nothing");
    assert_eq!(
        after.hooks[0].next_fire_at,
        Some(utc("2026-11-16T15:00:00Z")),
        "Monday, past the weekend"
    );
}

#[tokio::test]
async fn a_due_date_without_hooks_fires_once() {
    let rig = rig();
    let mut t = todo(TodoKind::Todo, "renew the logbook", &[]);
    t.due_at = Some(utc("2026-11-09T10:00:00Z"));
    rig.repo.create(&t).await.unwrap();
    rig.engine
        .pass(utc("2026-11-09T10:00:30Z"), NAIROBI)
        .await
        .unwrap();
    rig.engine
        .pass(utc("2026-11-09T10:01:00Z"), NAIROBI)
        .await
        .unwrap();
    assert_eq!(rig.notifier.sent().len(), 1);
    assert_eq!(rig.notifier.sent()[0].1.title, "Did you renew the logbook?");
}

#[tokio::test]
async fn not_yet_asks_again_at_the_snooze_and_stops_after_the_third_ask() {
    let rig = rig();
    let mut t = todo(TodoKind::Todo, "pay the KPLC bill", &[]);
    t.due_at = Some(utc("2026-11-09T10:00:00Z"));
    rig.repo.create(&t).await.unwrap();

    let mut now = utc("2026-11-09T10:00:00Z");
    for ask in 1..=MAX_ASKS {
        rig.engine.pass(now, NAIROBI).await.unwrap();
        assert_eq!(rig.notifier.sent().len(), ask as usize, "ask {ask}");
        let mut answered = stored(&rig, &t.id).await;
        assert_eq!(answered.asked, ask);
        answered
            .answer(Answer::NotYet, Some(30), now, NAIROBI)
            .unwrap();
        rig.repo.update(&answered, false).await.unwrap();
        now += Duration::minutes(31);
    }
    let last = &rig.notifier.sent()[MAX_ASKS as usize - 1].1;
    assert!(last.body.contains("last time"), "{}", last.body);
    assert_eq!(stored(&rig, &t.id).await.snoozed_until, None);

    for _ in 0..3 {
        rig.engine.pass(now, NAIROBI).await.unwrap();
        now += Duration::hours(12);
    }
    assert_eq!(rig.notifier.sent().len(), MAX_ASKS as usize);
    assert_eq!(stored(&rig, &t.id).await.state, TodoState::Open);
}

#[tokio::test]
async fn a_done_todo_is_never_asked_about_again() {
    let rig = rig();
    let mut t = todo(
        TodoKind::Todo,
        "call Emmanuel",
        &[("repeat", json!({"every": "day", "at": "18:00"}))],
    );
    t.state = TodoState::Done;
    rig.repo.create(&t).await.unwrap();
    rig.engine
        .pass(utc("2026-11-10T15:00:00Z"), NAIROBI)
        .await
        .unwrap();
    assert!(rig.notifier.sent().is_empty());
}

#[tokio::test]
async fn arriving_home_fires_a_place_hook_once_and_marks_the_event_handled() {
    let rig = rig();
    let t = todo(
        TodoKind::Reminder,
        "call Emmanuel",
        &[(
            "place",
            json!({"place_id": "p-home", "transition": "arrive"}),
        )],
    );
    rig.repo.create(&t).await.unwrap();
    let now = utc("2026-11-09T16:00:00Z");
    let arrive = |profile: &str, at: DateTime<Utc>| {
        HookEvent::new(Some(profile), "place", "p-home", "arrive", json!({}), at)
    };
    rig.repo.raise(&arrive("emmanuel", now)).await.unwrap();
    rig.repo.raise(&arrive("jerry", now)).await.unwrap();
    // The geofence flapping a minute later is the same arrival.
    rig.repo
        .raise(&arrive("jerry", now + Duration::minutes(1)))
        .await
        .unwrap();

    let report = rig
        .engine
        .pass(now + Duration::minutes(1), NAIROBI)
        .await
        .unwrap();
    assert_eq!(report.events, 3);
    assert_eq!(report.pushed, 1);
    let sent = rig.notifier.sent();
    assert_eq!(sent[0].1.title, "Reminder: call Emmanuel");
    assert_eq!(sent[0].1.body, "When you get home");
    assert!(rig
        .repo
        .events()
        .iter()
        .all(|(_, handled)| handled.is_some()));

    rig.repo
        .raise(&arrive("jerry", now + Duration::minutes(5)))
        .await
        .unwrap();
    rig.engine
        .pass(now + Duration::minutes(5), NAIROBI)
        .await
        .unwrap();
    assert_eq!(rig.notifier.sent().len(), 1, "inside the cooldown");

    rig.repo
        .raise(&arrive("jerry", now + Duration::hours(3)))
        .await
        .unwrap();
    rig.engine
        .pass(now + Duration::hours(3), NAIROBI)
        .await
        .unwrap();
    assert_eq!(rig.notifier.sent().len(), 2, "a later arrival fires again");
}

#[tokio::test]
async fn a_stale_event_is_handled_without_firing() {
    let rig = rig();
    let t = todo(
        TodoKind::Reminder,
        "lock the gate",
        &[(
            "event",
            json!({"source_id": "camera:camera-1", "event": "person"}),
        )],
    );
    rig.repo.create(&t).await.unwrap();
    let then = utc("2026-11-09T06:00:00Z");
    rig.repo
        .raise(&HookEvent::new(
            None,
            "camera",
            "camera-1",
            "person",
            json!({}),
            then,
        ))
        .await
        .unwrap();
    let report = rig
        .engine
        .pass(then + Duration::hours(2), NAIROBI)
        .await
        .unwrap();
    assert_eq!((report.events, report.pushed), (1, 0));
}

#[tokio::test]
async fn the_engine_knows_what_its_hooks_listen_for_and_prunes_old_events() {
    let rig = rig();
    rig.repo
        .create(&todo(
            TodoKind::Reminder,
            "feed the cat",
            &[(
                "event",
                json!({"source_id": "presence:home", "event": "arrive"}),
            )],
        ))
        .await
        .unwrap();
    let keys = rig.engine.listening_keys().await.unwrap();
    assert!(keys.contains(&EventKey::new("presence", "home", "arrive")));
    assert_eq!(keys.len(), 1);

    let now = utc("2026-11-09T09:00:00Z");
    let old = HookEvent::new(None, "camera", "camera-1", "motion", json!({}), now);
    rig.repo.raise(&old).await.unwrap();
    rig.repo
        .mark_handled(&[old.id.clone()], now - Duration::days(8))
        .await
        .unwrap();
    assert_eq!(rig.engine.prune(now).await.unwrap(), 1);
}
