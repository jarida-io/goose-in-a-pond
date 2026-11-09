//! SQLite-backed `WebWatchRepository`. Filters, items and changes are JSON.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::shared::ports::web_watch::{
    CheckOutcome, StoredChange, WatchEdit, WebWatch, WebWatchRepository,
};

pub struct SqliteWebWatchRepository {
    pool: Pool<Sqlite>,
}

impl SqliteWebWatchRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const WATCH_COLUMNS: &str =
    "id, profile_id, address, title, looking_for, filters, interval_hours, \
     paused, items, failures, last_checked_at, next_check_at, created_at, session_id";

fn time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("bad web watch timestamp {raw:?}"))
}

fn watch_of(row: &SqliteRow) -> Result<WebWatch> {
    let items: Option<String> = row.try_get("items")?;
    let last: Option<String> = row.try_get("last_checked_at")?;
    Ok(WebWatch {
        id: row.try_get("id")?,
        owner: row.try_get("profile_id")?,
        address: row.try_get("address")?,
        title: row.try_get("title")?,
        looking_for: row.try_get("looking_for")?,
        filters: serde_json::from_str(&row.try_get::<String, _>("filters")?).unwrap_or_default(),
        interval_hours: row.try_get::<i64, _>("interval_hours")?.max(1) as u32,
        paused: row.try_get::<i64, _>("paused")? != 0,
        items: items.and_then(|i| serde_json::from_str(&i).ok()),
        failures: row.try_get::<i64, _>("failures")?.max(0) as u32,
        last_checked_at: last.as_deref().map(time).transpose()?,
        next_check_at: time(&row.try_get::<String, _>("next_check_at")?)?,
        created_at: time(&row.try_get::<String, _>("created_at")?)?,
        session_id: row.try_get("session_id")?,
    })
}

fn change_of(row: &SqliteRow) -> Result<StoredChange> {
    Ok(StoredChange {
        id: row.try_get("id")?,
        watch_id: row.try_get("watch_id")?,
        found_at: time(&row.try_get::<String, _>("found_at")?)?,
        change: serde_json::from_str(&row.try_get::<String, _>("change")?)?,
        digested: row.try_get::<i64, _>("digested")? != 0,
    })
}

#[async_trait]
impl WebWatchRepository for SqliteWebWatchRepository {
    async fn create(&self, watch: &WebWatch) -> Result<()> {
        sqlx::query(&format!(
            "INSERT INTO web_watches ({WATCH_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(&watch.id)
        .bind(&watch.owner)
        .bind(&watch.address)
        .bind(&watch.title)
        .bind(&watch.looking_for)
        .bind(serde_json::to_string(&watch.filters)?)
        .bind(i64::from(watch.interval_hours))
        .bind(i64::from(watch.paused))
        .bind(watch.items.as_ref().map(serde_json::to_string).transpose()?)
        .bind(i64::from(watch.failures))
        .bind(watch.last_checked_at.map(|t| t.to_rfc3339()))
        .bind(watch.next_check_at.to_rfc3339())
        .bind(watch.created_at.to_rfc3339())
        .bind(&watch.session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<WebWatch>> {
        let row = sqlx::query(&format!(
            "SELECT {WATCH_COLUMNS} FROM web_watches WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(watch_of).transpose()
    }

    async fn list_for(&self, owner: &str) -> Result<Vec<WebWatch>> {
        let rows = sqlx::query(&format!(
            "SELECT {WATCH_COLUMNS} FROM web_watches WHERE profile_id = ? ORDER BY created_at DESC"
        ))
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(watch_of).collect()
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<WebWatch>> {
        let rows = sqlx::query(&format!(
            "SELECT {WATCH_COLUMNS} FROM web_watches WHERE paused = 0 AND next_check_at <= ? \
             ORDER BY next_check_at LIMIT ?"
        ))
        .bind(now.to_rfc3339())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(watch_of).collect()
    }

    async fn record_check(&self, id: &str, outcome: &CheckOutcome) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE web_watches SET items = ?, failures = ?, last_checked_at = ?, \
             next_check_at = ? WHERE id = ?",
        )
        .bind(
            outcome
                .items
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(i64::from(outcome.failures))
        .bind(outcome.checked_at.to_rfc3339())
        .bind(outcome.next_check_at.to_rfc3339())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        for change in &outcome.changes {
            sqlx::query(
                "INSERT INTO web_watch_changes (watch_id, found_at, change, matched, digested) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(outcome.checked_at.to_rfc3339())
            .bind(serde_json::to_string(change)?)
            .bind(i64::from(change.matched))
            // A match is pushed when found, so no digest repeats it.
            .bind(i64::from(change.matched))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn edit(&self, owner: &str, id: &str, edit: &WatchEdit) -> Result<bool> {
        let Some(mut watch) = self.get(id).await?.filter(|w| w.owner == owner) else {
            return Ok(false);
        };
        if let Some(looking_for) = &edit.looking_for {
            watch.looking_for = looking_for.clone();
        }
        if let Some(filters) = &edit.filters {
            watch.filters = filters.clone();
        }
        if let Some(hours) = edit.interval_hours {
            watch.interval_hours = hours;
        }
        if let Some(paused) = edit.paused {
            watch.paused = paused;
        }
        let done = sqlx::query(
            "UPDATE web_watches SET looking_for = ?, filters = ?, interval_hours = ?, paused = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(&watch.looking_for)
        .bind(serde_json::to_string(&watch.filters)?)
        .bind(i64::from(watch.interval_hours))
        .bind(i64::from(watch.paused))
        .bind(id)
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete(&self, owner: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM web_watches WHERE id = ? AND profile_id = ?")
            .bind(id)
            .bind(owner)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn changes(&self, watch_id: &str, limit: usize) -> Result<Vec<StoredChange>> {
        let rows = sqlx::query(
            "SELECT id, watch_id, found_at, change, digested FROM web_watch_changes \
             WHERE watch_id = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(watch_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(change_of).collect()
    }

    async fn undigested(&self, owner: &str) -> Result<Vec<StoredChange>> {
        let rows = sqlx::query(
            "SELECT c.id, c.watch_id, c.found_at, c.change, c.digested FROM web_watch_changes c \
             JOIN web_watches w ON w.id = c.watch_id \
             WHERE w.profile_id = ? AND c.digested = 0 ORDER BY c.id",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(change_of).collect()
    }

    async fn owners_with_undigested(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT w.profile_id FROM web_watch_changes c \
             JOIN web_watches w ON w.id = c.watch_id WHERE c.digested = 0",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| r.try_get("profile_id").map_err(Into::into))
            .collect()
    }

    async fn mark_digested(&self, ids: &[i64]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for id in ids {
            sqlx::query("UPDATE web_watch_changes SET digested = 1 WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn prune_changes(&self, before: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query("DELETE FROM web_watch_changes WHERE found_at < ?")
            .bind(before.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use chrono::Duration;
    use pond_core::shared::domain::web_watch::{ChangeKind, WatchChange, WatchFilters, WatchItem};

    async fn repo() -> (SqliteWebWatchRepository, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for (id, name) in [("amina", "Amina"), ("baraka", "Baraka")] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&db.system)
                .await
                .unwrap();
        }
        (SqliteWebWatchRepository::new(db.system.clone()), tmp)
    }

    fn item(title: &str) -> WatchItem {
        WatchItem {
            title: title.into(),
            price: Some("1400000".into()),
            link: Some(format!("https://c.example/{title}")),
            ..Default::default()
        }
    }

    fn watch(id: &str, owner: &str, next: DateTime<Utc>) -> WebWatch {
        WebWatch {
            id: id.into(),
            owner: owner.into(),
            address: "https://c.example/used".into(),
            title: "Used cars".into(),
            looking_for: "a Forester".into(),
            filters: WatchFilters {
                words: vec!["forester".into()],
                max_price: Some(1_500_000.0),
                ..Default::default()
            },
            interval_hours: 6,
            paused: false,
            items: Some(vec![item("a")]),
            failures: 0,
            last_checked_at: None,
            next_check_at: next,
            created_at: next,
            session_id: Some("s1".into()),
        }
    }

    fn change(title: &str, matched: bool) -> WatchChange {
        WatchChange {
            kind: ChangeKind::New,
            item: item(title),
            matched,
        }
    }

    #[tokio::test]
    async fn a_watch_round_trips_and_only_due_unpaused_ones_are_due() {
        let (repo, _tmp) = repo().await;
        let now = Utc::now();
        let due = watch("w1", "amina", now - Duration::minutes(1));
        repo.create(&due).await.unwrap();
        repo.create(&watch("w2", "amina", now + Duration::hours(1)))
            .await
            .unwrap();
        let mut paused = watch("w3", "baraka", now - Duration::hours(1));
        paused.paused = true;
        repo.create(&paused).await.unwrap();
        let got = repo.get("w1").await.unwrap().unwrap();
        assert_eq!(got.filters, due.filters);
        assert_eq!(got.items, due.items);
        assert_eq!(got.session_id.as_deref(), Some("s1"));
        let ids: Vec<String> = repo
            .due(now, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(ids, vec!["w1"]);
        assert_eq!(repo.list_for("amina").await.unwrap().len(), 2);
        assert!(repo.list_for("nobody").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_check_keeps_its_changes_and_only_the_unmatched_wait_for_a_digest() {
        let (repo, _tmp) = repo().await;
        let now = Utc::now();
        repo.create(&watch("w1", "amina", now)).await.unwrap();
        let outcome = CheckOutcome {
            items: Some(vec![item("b"), item("c")]),
            failures: 0,
            checked_at: now,
            next_check_at: now + Duration::hours(6),
            changes: vec![change("b", true), change("c", false)],
        };
        repo.record_check("w1", &outcome).await.unwrap();
        let got = repo.get("w1").await.unwrap().unwrap();
        assert_eq!(got.items.unwrap().len(), 2);
        assert_eq!(
            got.last_checked_at.map(|t| t.timestamp()),
            Some(now.timestamp())
        );
        assert_eq!(repo.changes("w1", 10).await.unwrap().len(), 2);
        assert_eq!(repo.owners_with_undigested().await.unwrap(), vec!["amina"]);
        let waiting = repo.undigested("amina").await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].change.item.title, "c");
        repo.mark_digested(&[waiting[0].id]).await.unwrap();
        assert!(repo.owners_with_undigested().await.unwrap().is_empty());
        assert_eq!(
            repo.prune_changes(now + Duration::seconds(1))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn only_the_owner_edits_or_deletes_and_a_member_leaving_takes_their_watches() {
        let (repo, _tmp) = repo().await;
        let now = Utc::now();
        repo.create(&watch("w1", "amina", now)).await.unwrap();
        let edit = WatchEdit {
            interval_hours: Some(12),
            paused: Some(true),
            ..Default::default()
        };
        assert!(!repo.edit("baraka", "w1", &edit).await.unwrap());
        assert!(repo.edit("amina", "w1", &edit).await.unwrap());
        let got = repo.get("w1").await.unwrap().unwrap();
        assert_eq!((got.interval_hours, got.paused), (12, true));
        assert!(!repo.delete("baraka", "w1").await.unwrap());
        repo.create(&watch("w2", "baraka", now)).await.unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = 'baraka'")
            .execute(&repo.pool)
            .await
            .unwrap();
        assert!(repo.get("w2").await.unwrap().is_none());
        assert!(repo.delete("amina", "w1").await.unwrap());
        assert!(repo.get("w1").await.unwrap().is_none());
    }
}
