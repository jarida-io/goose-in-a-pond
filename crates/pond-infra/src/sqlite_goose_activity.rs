//! SQLite store for what Goose did by itself (`goose_activity`).

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::shared::domain::goose_activity::{ActivityKind, GooseActivity};
use pond_core::shared::ports::goose_activity::GooseActivityRepository;
use sqlx::{Pool, Sqlite};

pub struct SqliteGooseActivity {
    pool: Pool<Sqlite>,
}

impl SqliteGooseActivity {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

type Row = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

const COLUMNS: &str = "id, profile_id, kind, summary, detail, ref_id, at, undone_at";

fn time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn item(row: Row) -> Option<GooseActivity> {
    let (id, profile_id, kind, summary, detail, ref_id, at, undone_at) = row;
    Some(GooseActivity {
        id,
        profile_id,
        kind: ActivityKind::parse(&kind)?,
        summary,
        detail,
        ref_id,
        at: time(&at)?,
        undone_at: undone_at.as_deref().and_then(time),
    })
}

#[async_trait]
impl GooseActivityRepository for SqliteGooseActivity {
    async fn add(&self, item: &GooseActivity) -> Result<()> {
        sqlx::query(
            "INSERT INTO goose_activity (id, profile_id, kind, summary, detail, ref_id, at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&item.id)
        .bind(&item.profile_id)
        .bind(item.kind.as_str())
        .bind(&item.summary)
        .bind(&item.detail)
        .bind(&item.ref_id)
        .bind(item.at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_for(&self, member: &str, limit: usize) -> Result<Vec<GooseActivity>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM goose_activity WHERE profile_id = ? \
             ORDER BY at DESC, rowid DESC LIMIT ?"
        ))
        .bind(member)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(item).collect())
    }

    async fn get(&self, member: &str, id: &str) -> Result<Option<GooseActivity>> {
        let row: Option<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM goose_activity WHERE profile_id = ? AND id = ?"
        ))
        .bind(member)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(item))
    }

    async fn mark_undone(&self, member: &str, id: &str, at: DateTime<Utc>) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE goose_activity SET undone_at = ? \
             WHERE profile_id = ? AND id = ? AND undone_at IS NULL",
        )
        .bind(at.to_rfc3339())
        .bind(member)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn count_since(
        &self,
        member: &str,
        kind: ActivityKind,
        since: DateTime<Utc>,
    ) -> Result<usize> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM goose_activity WHERE profile_id = ? AND kind = ? AND at >= ?",
        )
        .bind(member)
        .bind(kind.as_str())
        .bind(since.to_rfc3339())
        .fetch_one(&self.pool)
        .await?;
        Ok(count.max(0) as usize)
    }

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64> {
        let gone = sqlx::query("DELETE FROM goose_activity WHERE at < ?")
            .bind(before.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(gone.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    async fn store() -> (SqliteGooseActivity, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        for id in ["m1", "m2"] {
            sqlx::query("INSERT INTO profiles (id, display_name, avatar_emoji) VALUES (?, ?, ?)")
                .bind(id)
                .bind(id)
                .bind("duck")
                .execute(&db.system)
                .await
                .unwrap();
        }
        (SqliteGooseActivity::new(db.system), tmp)
    }

    #[tokio::test]
    async fn entries_are_their_members_newest_first_and_taken_back_once() {
        let (store, _tmp) = store().await;
        let now = Utc::now();
        let old = GooseActivity::note("m1", "an older note", "n1", now - Duration::hours(3));
        let new = GooseActivity::errand("m1", "when the pool opens", "At 6am.", now);
        let other = GooseActivity::note("m2", "not theirs", "n2", now);
        for entry in [&old, &new, &other] {
            store.add(entry).await.unwrap();
        }

        let listed = store.list_for("m1", 10).await.unwrap();
        assert_eq!(listed, vec![new.clone(), old.clone()]);
        assert_eq!(store.get("m1", &other.id).await.unwrap(), None);

        assert!(!store.mark_undone("m2", &old.id, now).await.unwrap());
        assert!(store.mark_undone("m1", &old.id, now).await.unwrap());
        assert!(!store.mark_undone("m1", &old.id, now).await.unwrap());
        assert!(store
            .get("m1", &old.id)
            .await
            .unwrap()
            .unwrap()
            .undone_at
            .is_some());

        let since = now - Duration::hours(1);
        assert_eq!(
            store
                .count_since("m1", ActivityKind::Errand, since)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .count_since("m1", ActivityKind::Note, since)
                .await
                .unwrap(),
            0
        );
        assert_eq!(store.prune(now - Duration::hours(1)).await.unwrap(), 1);
        assert_eq!(store.list_for("m1", 10).await.unwrap().len(), 1);
    }
}
