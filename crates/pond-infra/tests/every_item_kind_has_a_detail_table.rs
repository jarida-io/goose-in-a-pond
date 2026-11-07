//! Every item kind names a detail table that exists, is keyed by `item_id` and drops its header
//! with it, and the `items` CHECKs accept exactly the kinds and states pond-core declares.
//! A kind with no detail table or no lifecycle does not compile: both are exhaustive matches.

use pond_core::items::lifecycle::Lifecycle;
use pond_core::items::{timestamp, Audience, ItemId, Kind, Modality, ProducedBy, Relation, State};
use pond_infra::db::Database;
use sqlx::{Pool, Sqlite};
use std::collections::BTreeSet;
use tempfile::TempDir;

async fn migrated() -> (TempDir, Pool<Sqlite>) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::init(dir.path()).await.unwrap();
    (dir, db.system)
}

async fn insert_header(
    pool: &Pool<Sqlite>,
    kind: &str,
    state: &str,
    owner: Option<&str>,
    audience: &str,
) -> Result<String, sqlx::Error> {
    let id = ItemId::generate();
    let now = timestamp::now();
    sqlx::query(
        "INSERT INTO items (id, kind, source_id, external_id, owner_profile_id, audience, \
         observed_at, state, produced_by, created_at, updated_at) \
         VALUES (?, ?, 'test', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.as_str())
    .bind(kind)
    .bind(id.as_str())
    .bind(owner)
    .bind(audience)
    .bind(&now)
    .bind(state)
    .bind(ProducedBy::pond("test").to_stored())
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id.to_string())
}

#[tokio::test]
async fn every_kind_names_a_detail_table_keyed_by_item_id() {
    let (_dir, pool) = migrated().await;
    for kind in Kind::ALL {
        let _: Lifecycle = kind.lifecycle();
        let table = kind.detail_table();
        let fks: Vec<(i64, i64, String, String, String, String, String, String)> =
            sqlx::query_as(&format!("PRAGMA foreign_key_list({table})"))
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            fks.iter()
                .any(|fk| fk.2 == "items" && fk.3 == "item_id" && fk.4 == "id"),
            "{kind} names `{table}`, which has no item_id referencing items(id)"
        );
    }
}

#[tokio::test]
async fn every_detail_table_drops_its_header_when_a_row_goes() {
    let (_dir, pool) = migrated().await;
    let tables: BTreeSet<&str> = Kind::ALL.iter().map(|k| k.detail_table()).collect();
    for table in tables {
        let triggers: Vec<(String,)> =
            sqlx::query_as("SELECT sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            triggers.iter().any(|(sql,)| {
                let sql = sql.to_lowercase();
                sql.contains("after delete") && sql.contains("delete from items")
            }),
            "`{table}` has no AFTER DELETE trigger removing its header; \
             a deleted row would leave its text searchable"
        );
    }
}

#[tokio::test]
async fn the_kind_and_state_checks_accept_exactly_what_pond_core_declares() {
    let (_dir, pool) = migrated().await;
    for kind in Kind::ALL {
        insert_header(&pool, kind.as_str(), "active", None, "household")
            .await
            .unwrap_or_else(|e| panic!("items refused kind {kind}: {e}"));
    }
    for state in State::ALL {
        insert_header(&pool, "fact", state.as_str(), None, "household")
            .await
            .unwrap_or_else(|e| panic!("items refused state {}: {e}", state.as_str()));
    }
    assert!(insert_header(&pool, "memory", "active", None, "household")
        .await
        .is_err());
    assert!(insert_header(&pool, "fact", "merged", None, "household")
        .await
        .is_err());
    for audience in Audience::ALL {
        assert_eq!(Audience::parse(audience.as_str()), Some(audience));
    }
    for modality in Modality::ALL {
        let id = insert_header(&pool, "media", "active", None, "household")
            .await
            .unwrap();
        sqlx::query("UPDATE items SET modality = ? WHERE id = ?")
            .bind(modality.as_str())
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("items refused modality {}: {e}", modality.as_str()));
    }
}

#[tokio::test]
async fn a_private_item_needs_an_owner() {
    let (_dir, pool) = migrated().await;
    assert!(insert_header(&pool, "fact", "active", None, "private")
        .await
        .is_err());
    assert!(insert_header(&pool, "fact", "active", None, "household")
        .await
        .is_ok());
}

#[tokio::test]
async fn the_relation_check_accepts_exactly_what_pond_core_declares() {
    let (_dir, pool) = migrated().await;
    let a = insert_header(&pool, "conversation", "active", None, "household")
        .await
        .unwrap();
    let b = insert_header(&pool, "fact", "active", None, "household")
        .await
        .unwrap();
    for relation in Relation::ALL {
        sqlx::query(
            "INSERT INTO item_links (from_id, relation, to_id, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&a)
        .bind(relation.as_str())
        .bind(&b)
        .bind(timestamp::now())
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("item_links refused {}: {e}", relation.as_str()));
    }
    let self_link = sqlx::query(
        "INSERT INTO item_links (from_id, relation, to_id, created_at) VALUES (?, 'mentions', ?, ?)",
    )
    .bind(&a)
    .bind(&a)
    .bind(timestamp::now())
    .execute(&pool)
    .await;
    assert!(self_link.is_err(), "an item linked to itself");
}

#[tokio::test]
async fn only_the_one_timestamp_format_is_stored() {
    let (_dir, pool) = migrated().await;
    let id = ItemId::generate();
    let refused = sqlx::query(
        "INSERT INTO items (id, kind, source_id, external_id, audience, observed_at, \
         produced_by, created_at, updated_at) \
         VALUES (?, 'fact', 'test', 'x', 'household', datetime('now'), 'pond:test', ?, ?)",
    )
    .bind(id.as_str())
    .bind(timestamp::now())
    .bind(timestamp::now())
    .execute(&pool)
    .await;
    assert!(
        refused.is_err(),
        "items stored SQLite's datetime('now') format"
    );
}

async fn run(pool: &Pool<Sqlite>, sql: &str) {
    sqlx::query(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn headed(pool: &Pool<Sqlite>, table: &str, key: &str, owner: Option<&str>) -> String {
    let audience = if owner.is_some() {
        "private"
    } else {
        "household"
    };
    let id = insert_header(pool, "fact", "active", owner, audience)
        .await
        .unwrap();
    sqlx::query(&format!("UPDATE {table} SET item_id = ? WHERE id = ?"))
        .bind(&id)
        .bind(key)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn header_exists(pool: &Pool<Sqlite>, id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM items WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
        == 1
}

#[tokio::test]
async fn removing_a_member_removes_their_headers_and_keeps_the_rows_it_kept_before() {
    let (_dir, pool) = migrated().await;
    run(
        &pool,
        "INSERT INTO profiles (id, display_name) VALUES ('amina', 'Amina')",
    )
    .await;
    run(
        &pool,
        "INSERT INTO sessions (id, created_at, updated_at, profile_id) \
         VALUES ('s1', datetime('now'), datetime('now'), 'amina')",
    )
    .await;
    run(
        &pool,
        "INSERT INTO memory_fragments (id, profile_id, session_id, content, source, tags, \
         created_at) VALUES ('m1', 'amina', 's1', 'likes tea', 'chat', '[]', datetime('now'))",
    )
    .await;
    run(
        &pool,
        "INSERT INTO context_sources (id, kind, provider, profile_id, created_at) \
         VALUES ('src', 'mail', 'imap', 'amina', datetime('now'))",
    )
    .await;
    run(
        &pool,
        "INSERT INTO context_items (id, source_id, external_id, profile_id, source_kind, \
         item_kind, occurred_at, ingested_at, title, body, sensitivity) \
         VALUES ('src:1', 'src', '1', 'amina', 'mail', 'message', datetime('now'), \
         datetime('now'), 'Hi', 'Body', 'sensitive')",
    )
    .await;
    let session_item = headed(&pool, "sessions", "s1", Some("amina")).await;
    let memory_item = headed(&pool, "memory_fragments", "m1", Some("amina")).await;
    let context_item = headed(&pool, "context_items", "src:1", Some("amina")).await;

    run(&pool, "DELETE FROM profiles WHERE id = 'amina'").await;

    for id in [&session_item, &memory_item, &context_item] {
        assert!(
            !header_exists(&pool, id).await,
            "a removed member's header survived"
        );
    }
    let (owner, item_id): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT profile_id, item_id FROM sessions WHERE id = 's1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (owner, item_id),
        (None, None),
        "the conversation survives its member, unattributed and without a header"
    );
}

#[tokio::test]
async fn deleting_a_detail_row_deletes_its_header_and_deleting_a_header_only_unlinks_the_row() {
    let (_dir, pool) = migrated().await;
    run(
        &pool,
        "INSERT INTO memory_fragments (id, content, source, tags, created_at) \
         VALUES ('m1', 'a', 'chat', '[]', datetime('now')), \
                ('m2', 'b', 'chat', '[]', datetime('now'))",
    )
    .await;
    let first = headed(&pool, "memory_fragments", "m1", None).await;
    let second = headed(&pool, "memory_fragments", "m2", None).await;

    run(&pool, "DELETE FROM memory_fragments WHERE id = 'm1'").await;
    assert!(!header_exists(&pool, &first).await);

    sqlx::query("DELETE FROM items WHERE id = ?")
        .bind(&second)
        .execute(&pool)
        .await
        .unwrap();
    let left: Option<String> =
        sqlx::query_scalar("SELECT item_id FROM memory_fragments WHERE id = 'm2'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        left, None,
        "the memory row must outlive a deleted header in P1"
    );
}

#[tokio::test]
async fn a_new_kinds_details_go_with_its_header_and_its_header_with_them() {
    let (_dir, pool) = migrated().await;
    for delete_header_first in [true, false] {
        let id = insert_header(&pool, "media", "active", None, "household")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO media_details (item_id, blob_id, mime_type, size_bytes, content_hash) \
             VALUES (?, 'b', 'image/png', 10, 'h')",
        )
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
        let sql = if delete_header_first {
            "DELETE FROM items WHERE id = ?"
        } else {
            "DELETE FROM media_details WHERE item_id = ?"
        };
        sqlx::query(sql).bind(&id).execute(&pool).await.unwrap();
        let details: i64 =
            sqlx::query_scalar("SELECT count(*) FROM media_details WHERE item_id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(details, 0);
        assert!(!header_exists(&pool, &id).await);
    }
}
