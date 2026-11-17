//! The engine purge on a pond that already holds what the retired generators wrote: what no
//! member asked for goes, what a member kept or acted on stays, and screenshot lines become
//! captures.

use pond_infra::db::Database;
use sqlx::{Pool, Sqlite};

const PURGE: i64 = 117;
const AT: &str = "2026-11-10T08:00:00Z";

async fn exec(pool: &Pool<Sqlite>, sql: &str, binds: &[&str]) {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.execute(pool).await.unwrap();
}

async fn ids(pool: &Pool<Sqlite>, sql: &str) -> Vec<String> {
    let mut found: Vec<String> = sqlx::query_scalar(sql).fetch_all(pool).await.unwrap();
    found.sort();
    found
}

/// A pond migrated to just before the purge, with a row of each kind, then migrated through it.
async fn purged() -> (Pool<Sqlite>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let pool = Database::connect(&tmp.path().join("pond_system.db"))
        .await
        .unwrap();
    let mut before = sqlx::migrate!("migrations/system");
    before.migrations = before
        .migrations
        .iter()
        .filter(|m| m.version < PURGE)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    before.run(&pool).await.unwrap();

    for member in ["jerry", "liz"] {
        exec(
            &pool,
            "INSERT INTO profiles (id, display_name) VALUES (?, ?)",
            &[member, member],
        )
        .await;
    }
    for (id, memory, state) in [("q-queued", "m1", "queued"), ("q-taken", "m2", "taken")] {
        exec(
            &pool,
            "INSERT INTO suggestion_queue (id, profile_id, prompt, reason, source_memory_id, \
             answered_by, state) VALUES (?, 'jerry', 'What about the fence?', 'From a note.', ?, \
             'giap-memory', ?)",
            &[id, memory, state],
        )
        .await;
    }
    for (id, source, state) in [
        ("t-goose-open", "goose", "open"),
        ("t-goose-done", "goose", "done"),
        ("t-you-open", "you", "open"),
        ("t-chat-open", "chat", "open"),
        ("t-idea-open", "idea", "open"),
    ] {
        exec(
            &pool,
            "INSERT INTO todos (id, profile_id, text, state, source, created_at, updated_at) \
             VALUES (?, 'jerry', ?, ?, ?, ?, ?)",
            &[id, id, state, source, AT, AT],
        )
        .await;
        exec(
            &pool,
            "INSERT INTO todo_hooks (id, todo_id, kind, label, created_at) \
             VALUES (?, ?, 'time', 'Friday at 9', ?)",
            &[format!("h-{id}").as_str(), id, AT],
        )
        .await;
    }
    for (id, kind) in [("n-note", "note"), ("n-lesson", "lesson")] {
        exec(
            &pool,
            "INSERT INTO agent_notes (id, profile_id, kind, text, created_at) \
             VALUES (?, 'jerry', ?, 'It may rain at 4.', ?)",
            &[id, kind, AT],
        )
        .await;
    }
    for state in ["new", "working", "done", "dismissed"] {
        exec(
            &pool,
            "INSERT INTO ideas (id, profile_id, topic, title, pitch, why, state, created_at, \
             updated_at) VALUES (?, 'jerry', 'duck', 'Roast a duck', 'pitch', 'why', ?, ?, ?)",
            &[format!("i-{state}").as_str(), state, AT, AT],
        )
        .await;
    }
    exec(
        &pool,
        "INSERT INTO member_topics (id, profile_id, area, subject, query, newsworthy, score, \
         built_at) VALUES ('topic-1', 'jerry', 'cars', 'Fielder', 'q', 0, 0.5, ?)",
        &[AT],
    )
    .await;
    exec(
        &pool,
        "INSERT INTO member_portraits (profile_id, summary, updated_at) \
         VALUES ('jerry', 'Asks about the weather.', ?)",
        &[AT],
    )
    .await;
    exec(
        &pool,
        "INSERT INTO interests (id, profile_id, topic, topic_key, created_at, updated_at) \
         VALUES ('int-1', 'jerry', 'Toyota Fielder', 'toyota fielder', ?, ?)",
        &[AT, AT],
    )
    .await;
    for (id, member, note, taken) in [
        (
            "shot-1",
            "jerry",
            "A ticket for the Diani SGR on 19 December.",
            "2026-11-09T18:00:00Z",
        ),
        ("shot-2", "liz", "A recipe for pilau.", ""),
        ("shot-held", "jerry", "A note the capture already has.", ""),
    ] {
        exec(
            &pool,
            "INSERT INTO screenshot_notes (id, profile_id, note, taken_at, created_at) \
             VALUES (?, ?, ?, NULLIF(?, ''), ?)",
            &[id, member, note, taken, AT],
        )
        .await;
    }
    exec(
        &pool,
        "INSERT INTO captures (id, profile_id, kind, state, description, created_at, updated_at) \
         VALUES ('shot-held', 'jerry', 'photo', 'pending', 'its own words', ?, ?)",
        &[AT, AT],
    )
    .await;

    sqlx::migrate!("migrations/system")
        .run(&pool)
        .await
        .unwrap();
    (pool, tmp)
}

#[tokio::test]
async fn what_no_member_asked_for_is_gone_and_what_they_kept_stays() {
    let (pool, _tmp) = purged().await;
    assert_eq!(
        ids(&pool, "SELECT id FROM suggestion_queue").await,
        ["q-taken"]
    );
    assert_eq!(
        ids(&pool, "SELECT id FROM todos").await,
        ["t-chat-open", "t-goose-done", "t-idea-open", "t-you-open"]
    );
    assert_eq!(
        ids(&pool, "SELECT todo_id FROM todo_hooks").await,
        ["t-chat-open", "t-goose-done", "t-idea-open", "t-you-open"],
        "the heartbeat's open to-do takes its hooks with it"
    );
    assert_eq!(ids(&pool, "SELECT id FROM agent_notes").await, ["n-lesson"]);
    assert_eq!(
        ids(&pool, "SELECT id FROM ideas").await,
        ["i-dismissed", "i-done", "i-working"]
    );
    assert!(ids(&pool, "SELECT id FROM member_topics").await.is_empty());
    assert!(ids(&pool, "SELECT profile_id FROM member_portraits")
        .await
        .is_empty());
    assert_eq!(ids(&pool, "SELECT id FROM interests").await, ["int-1"]);
    assert_eq!(
        ids(&pool, "SELECT id FROM profiles").await,
        ["jerry", "liz"]
    );
}

#[tokio::test]
async fn each_screenshot_line_becomes_a_read_capture_once() {
    let (pool, _tmp) = purged().await;
    type Row = (
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, profile_id, kind, taken_at, state, sensitive, description, created_at, \
         updated_at, picture_file, content_hash FROM captures ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let shot = |id: &str| rows.iter().find(|row| row.0 == id).cloned().unwrap();

    let (_, member, kind, taken, state, sensitive, description, created, updated, picture, hash) =
        shot("shot-1");
    assert_eq!(
        (
            member.as_str(),
            kind.as_str(),
            state.as_str(),
            sensitive.as_str()
        ),
        ("jerry", "screenshot", "read", "none")
    );
    assert_eq!(taken.as_deref(), Some("2026-11-09T18:00:00Z"));
    assert_eq!(
        description.as_deref(),
        Some("A ticket for the Diani SGR on 19 December.")
    );
    assert_eq!((created.as_str(), updated.as_str()), (AT, AT));
    assert_eq!((picture, hash), (None, None));

    assert_eq!(shot("shot-2").1, "liz");
    assert_eq!(shot("shot-2").3, None);
    let held = shot("shot-held");
    assert_eq!(
        (held.2.as_str(), held.4.as_str(), held.6.as_deref()),
        ("photo", "pending", Some("its own words")),
        "a capture already under that id is left as it was"
    );
    assert_eq!(rows.len(), 3);
}
