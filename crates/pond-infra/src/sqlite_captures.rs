//! SQLite-backed [`CaptureRepository`] over the `captures` table. The picture is a file the
//! row names; deleting the file is the caller's.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::capture::{Capture, CaptureKind, CaptureState, Sensitivity};
use pond_core::user_data::ports::captures::CaptureRepository;

use crate::sqlite_rows::{
    count_in, flag_in, json_in, json_of, maybe_stamp, maybe_time_of, stamp, time_of, word_in,
};

pub struct SqliteCaptureRepository {
    pool: Pool<Sqlite>,
}

impl SqliteCaptureRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, kind, album, taken_at, state, sensitive, hot, \
     description, intent, details_json, picture_file, picture_until, content_hash, attempts, \
     created_at, updated_at";

fn capture_of(row: &SqliteRow) -> Result<Capture> {
    Ok(Capture {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        kind: word_in(row, "kind", CaptureKind::parse)?,
        album: row.try_get("album")?,
        taken_at: maybe_time_of(row, "taken_at")?,
        state: word_in(row, "state", CaptureState::parse)?,
        sensitive: word_in(row, "sensitive", Sensitivity::parse)?,
        hot: flag_in(row, "hot")?,
        description: row.try_get("description")?,
        intent: row.try_get("intent")?,
        details: json_in(row, "details_json")?,
        picture_file: row.try_get("picture_file")?,
        picture_until: maybe_time_of(row, "picture_until")?,
        content_hash: row.try_get("content_hash")?,
        attempts: count_in(row, "attempts")?,
        created_at: time_of(row, "created_at")?,
        updated_at: time_of(row, "updated_at")?,
    })
}

impl SqliteCaptureRepository {
    async fn all_of(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE profile_id = ? ORDER BY rowid"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("read every capture a member has")?;
        rows.iter().map(capture_of).collect()
    }
}

#[async_trait]
impl CaptureRepository for SqliteCaptureRepository {
    async fn add(&self, capture: &Capture) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO captures (id, profile_id, kind, album, taken_at, state, sensitive, hot, \
             description, intent, details_json, picture_file, picture_until, content_hash, \
             attempts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&capture.id)
        .bind(&capture.profile_id)
        .bind(capture.kind.as_str())
        .bind(&capture.album)
        .bind(maybe_stamp(capture.taken_at))
        .bind(capture.state.as_str())
        .bind(capture.sensitive.as_str())
        .bind(capture.hot as i64)
        .bind(&capture.description)
        .bind(&capture.intent)
        .bind(json_of(&capture.details)?)
        .bind(&capture.picture_file)
        .bind(maybe_stamp(capture.picture_until))
        .bind(&capture.content_hash)
        .bind(i64::from(capture.attempts))
        .bind(stamp(capture.created_at))
        .bind(stamp(capture.updated_at))
        .execute(&self.pool)
        .await
        .context("keep a capture")?;
        Ok(done.rows_affected() > 0)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Capture>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read a capture")?;
        row.as_ref().map(capture_of).transpose()
    }

    async fn by_hash(&self, profile_id: &str, content_hash: &str) -> Result<Option<Capture>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE profile_id = ? AND content_hash = ?"
        ))
        .bind(profile_id)
        .bind(content_hash)
        .fetch_optional(&self.pool)
        .await
        .context("find a capture by its picture")?;
        row.as_ref().map(capture_of).transpose()
    }

    async fn list(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE profile_id = ? AND hot = 0 \
             ORDER BY created_at DESC, rowid DESC"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's captures")?;
        rows.iter().map(capture_of).collect()
    }

    async fn hot(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE profile_id = ? AND hot = 1 \
             ORDER BY created_at, rowid"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's money captures")?;
        rows.iter().map(capture_of).collect()
    }

    async fn count_pending(&self, profile_id: &str) -> Result<usize> {
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM captures WHERE profile_id = ? AND state = 'pending'",
        )
        .bind(profile_id)
        .fetch_one(&self.pool)
        .await
        .context("count a member's unread captures")?;
        Ok(pending.max(0) as usize)
    }

    async fn save(&self, capture: &Capture) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE captures SET kind = ?, album = ?, taken_at = ?, state = ?, sensitive = ?, \
             hot = ?, description = ?, intent = ?, details_json = ?, picture_file = ?, \
             picture_until = ?, content_hash = ?, attempts = ?, updated_at = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(capture.kind.as_str())
        .bind(&capture.album)
        .bind(maybe_stamp(capture.taken_at))
        .bind(capture.state.as_str())
        .bind(capture.sensitive.as_str())
        .bind(capture.hot as i64)
        .bind(&capture.description)
        .bind(&capture.intent)
        .bind(json_of(&capture.details)?)
        .bind(&capture.picture_file)
        .bind(maybe_stamp(capture.picture_until))
        .bind(&capture.content_hash)
        .bind(i64::from(capture.attempts))
        .bind(stamp(capture.updated_at))
        .bind(&capture.id)
        .bind(&capture.profile_id)
        .execute(&self.pool)
        .await
        .context("save a capture")?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<Option<Capture>> {
        let Some(capture) = self.get(profile_id, id).await? else {
            return Ok(None);
        };
        sqlx::query("DELETE FROM captures WHERE profile_id = ? AND id = ?")
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete a capture")?;
        Ok(Some(capture))
    }

    async fn delete_all(&self, profile_id: &str) -> Result<Vec<Capture>> {
        let captures = self.all_of(profile_id).await?;
        sqlx::query("DELETE FROM captures WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await
            .context("delete a member's captures")?;
        Ok(captures)
    }

    async fn find(&self, id: &str) -> Result<Option<Capture>> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM captures WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("find a capture")?;
        row.as_ref().map(capture_of).transpose()
    }

    async fn pending(&self, limit: usize) -> Result<Vec<Capture>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE state = 'pending' \
             ORDER BY created_at, rowid LIMIT ?"
        ))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("list captures waiting to be read")?;
        rows.iter().map(capture_of).collect()
    }

    async fn pictures_due(&self, now: DateTime<Utc>) -> Result<Vec<Capture>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM captures WHERE picture_file IS NOT NULL \
             AND picture_until IS NOT NULL AND picture_until <= ? ORDER BY rowid"
        ))
        .bind(stamp(now))
        .fetch_all(&self.pool)
        .await
        .context("list captures whose picture's time is up")?;
        rows.iter().map(capture_of).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::domain::capture::{CaptureDetails, PICTURE_KEPT_FOR};
    use pond_core::user_data::mocks::mock_captures::InMemoryCaptureRepository;

    fn capture(member: &str, hash: &str, minutes_old: i64) -> Capture {
        let mut capture = Capture::new(
            member,
            CaptureKind::Screenshot,
            noon() - Duration::minutes(minutes_old),
        );
        capture.picture_file = Some(format!("captures/{member}/{hash}.jpg"));
        capture.content_hash = Some(hash.to_string());
        capture
    }

    fn hashes(found: &[Capture]) -> Vec<&str> {
        found
            .iter()
            .filter_map(|c| c.content_hash.as_deref())
            .collect()
    }

    async fn a_capture_round_trips_and_a_picture_is_kept_once(repo: &dyn CaptureRepository) {
        let mut shot = capture("jerry", "aaa", 10);
        shot.album = Some("Receipts".into());
        shot.taken_at = Some(noon() - Duration::hours(2));
        assert!(repo.add(&shot).await.unwrap());
        assert_eq!(
            repo.get("jerry", &shot.id).await.unwrap(),
            Some(shot.clone())
        );
        assert!(
            !repo.add(&capture("jerry", "aaa", 1)).await.unwrap(),
            "the same picture"
        );
        assert!(repo.add(&capture("liz", "aaa", 1)).await.unwrap());
        assert_eq!(
            repo.by_hash("jerry", "aaa").await.unwrap(),
            Some(shot.clone())
        );
        assert_eq!(repo.by_hash("jerry", "zzz").await.unwrap(), None);
        let mut unhashed = capture("jerry", "x", 1);
        unhashed.content_hash = None;
        let mut another = capture("jerry", "y", 1);
        another.content_hash = None;
        assert!(repo.add(&unhashed).await.unwrap());
        assert!(
            repo.add(&another).await.unwrap(),
            "no hash is no claim to be the same"
        );
        assert_eq!(repo.count_pending("jerry").await.unwrap(), 3);
    }

    async fn a_read_capture_is_kept_and_a_money_capture_is_held_apart(
        repo: &dyn CaptureRepository,
    ) {
        let mut flight = capture("jerry", "aaa", 30);
        let mut receipt = capture("jerry", "bbb", 20);
        let waiting = capture("liz", "ccc", 40);
        for one in [&flight, &receipt, &waiting] {
            assert!(repo.add(one).await.unwrap());
        }
        assert_eq!(
            hashes(&repo.pending(10).await.unwrap()),
            ["ccc", "aaa", "bbb"]
        );
        assert_eq!(hashes(&repo.pending(1).await.unwrap()), ["ccc"]);

        flight.count_attempt(noon());
        flight.read(
            "A flight to Kisumu on 3 December",
            Some("planning a trip"),
            CaptureDetails {
                places: vec!["Kisumu".into()],
                dates: vec!["3 December".into()],
                ..Default::default()
            },
            noon(),
        );
        assert!(repo.save(&flight).await.unwrap());
        receipt.hold_for_budget(noon());
        assert!(repo.save(&receipt).await.unwrap());

        assert_eq!(repo.find(&flight.id).await.unwrap(), Some(flight.clone()));
        assert_eq!(repo.list("jerry").await.unwrap(), vec![flight.clone()]);
        assert_eq!(repo.hot("jerry").await.unwrap(), vec![receipt.clone()]);
        assert_eq!(repo.count_pending("jerry").await.unwrap(), 0);
        assert_eq!(hashes(&repo.pending(10).await.unwrap()), ["ccc"]);

        let month_on = noon() + PICTURE_KEPT_FOR;
        assert!(repo
            .pictures_due(month_on - Duration::seconds(1))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo.pictures_due(month_on).await.unwrap(),
            vec![flight.clone()]
        );
        flight.forget_picture(month_on);
        assert!(repo.save(&flight).await.unwrap());
        assert!(repo.pictures_due(month_on).await.unwrap().is_empty());
        assert_eq!(
            repo.get("jerry", &flight.id)
                .await
                .unwrap()
                .unwrap()
                .description
                .as_deref(),
            Some("A flight to Kisumu on 3 December")
        );
    }

    async fn a_member_never_reads_or_deletes_anothers_captures(repo: &dyn CaptureRepository) {
        let mine = capture("jerry", "aaa", 3);
        let mut held = capture("jerry", "bbb", 2);
        held.hold_for_budget(noon());
        let theirs = capture("liz", "ccc", 1);
        for one in [&mine, &held, &theirs] {
            assert!(repo.add(one).await.unwrap());
        }
        assert_eq!(repo.get("liz", &mine.id).await.unwrap(), None);
        assert_eq!(repo.by_hash("liz", "aaa").await.unwrap(), None);
        assert_eq!(
            repo.by_hash("jerry", "bbb").await.unwrap(),
            Some(held.clone()),
            "a held picture is still the same picture"
        );
        assert_eq!(repo.delete("liz", &mine.id).await.unwrap(), None);
        let mut forged = mine.clone();
        forged.profile_id = "liz".into();
        assert!(!repo.save(&forged).await.unwrap());
        assert_eq!(hashes(&repo.list("liz").await.unwrap()), ["ccc"]);

        assert_eq!(repo.delete("jerry", &mine.id).await.unwrap(), Some(mine));
        let gone = repo.delete_all("jerry").await.unwrap();
        assert_eq!(hashes(&gone), ["bbb"], "the held one goes too");
        assert!(repo.hot("jerry").await.unwrap().is_empty());
        assert_eq!(hashes(&repo.list("liz").await.unwrap()), ["ccc"]);
    }

    async fn sqlite() -> SqliteCaptureRepository {
        SqliteCaptureRepository::new(pool().await)
    }

    #[tokio::test]
    async fn a_capture_round_trips_and_a_members_picture_is_kept_once() {
        a_capture_round_trips_and_a_picture_is_kept_once(&sqlite().await).await;
        a_capture_round_trips_and_a_picture_is_kept_once(&InMemoryCaptureRepository::new()).await;
    }

    #[tokio::test]
    async fn a_read_capture_is_kept_and_a_money_capture_is_held_apart_from_the_list() {
        a_read_capture_is_kept_and_a_money_capture_is_held_apart(&sqlite().await).await;
        a_read_capture_is_kept_and_a_money_capture_is_held_apart(&InMemoryCaptureRepository::new())
            .await;
    }

    #[tokio::test]
    async fn one_member_never_reads_or_deletes_anothers_captures() {
        a_member_never_reads_or_deletes_anothers_captures(&sqlite().await).await;
        a_member_never_reads_or_deletes_anothers_captures(&InMemoryCaptureRepository::new()).await;
    }
}
