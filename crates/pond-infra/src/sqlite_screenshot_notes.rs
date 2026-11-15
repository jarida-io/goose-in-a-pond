//! SQLite [`ScreenshotNoteRepository`] over the `screenshot_notes` table.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteRow, Pool, Row, Sqlite};

use pond_core::user_data::domain::member_profile::same_sentence;
use pond_core::user_data::ports::screenshot_notes::ScreenshotNoteRepository;
use pond_core::user_data::services::screenshot_notes::{ScreenshotNote, NOTES_KEPT};

pub struct SqliteScreenshotNoteRepository {
    pool: Pool<Sqlite>,
}

impl SqliteScreenshotNoteRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn time(raw: Option<String>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.as_deref()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn note_of(row: &SqliteRow) -> Result<ScreenshotNote> {
    let created_at: String = row.try_get("created_at")?;
    Ok(ScreenshotNote {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        note: row.try_get("note")?,
        taken_at: time(row.try_get("taken_at")?),
        created_at: time(Some(created_at)).context("a note's created_at is not a time")?,
        used_at: time(row.try_get("used_at")?),
    })
}

const COLUMNS: &str = "id, profile_id, note, taken_at, created_at, used_at";

#[async_trait]
impl ScreenshotNoteRepository for SqliteScreenshotNoteRepository {
    async fn add(&self, note: &ScreenshotNote) -> Result<bool> {
        let held = self.list(&note.profile_id).await?;
        if held.iter().any(|h| same_sentence(&h.note, &note.note)) {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO screenshot_notes (id, profile_id, note, taken_at, created_at, used_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&note.id)
        .bind(&note.profile_id)
        .bind(&note.note)
        .bind(note.taken_at.map(stamp))
        .bind(stamp(note.created_at))
        .bind(note.used_at.map(stamp))
        .execute(&self.pool)
        .await
        .context("keep a screenshot note")?;
        sqlx::query(
            "DELETE FROM screenshot_notes WHERE profile_id = ? AND id NOT IN \
             (SELECT id FROM screenshot_notes WHERE profile_id = ? \
              ORDER BY created_at DESC, rowid DESC LIMIT ?)",
        )
        .bind(&note.profile_id)
        .bind(&note.profile_id)
        .bind(NOTES_KEPT as i64)
        .execute(&self.pool)
        .await
        .context("drop a member's oldest screenshot notes")?;
        Ok(true)
    }

    async fn list(&self, profile_id: &str) -> Result<Vec<ScreenshotNote>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM screenshot_notes WHERE profile_id = ? \
             ORDER BY created_at DESC, rowid DESC"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's screenshot notes")?;
        rows.iter().map(note_of).collect()
    }

    async fn unused_since(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<ScreenshotNote>> {
        Ok(self
            .list(profile_id)
            .await?
            .into_iter()
            .filter(|n| n.used_at.is_none() && n.created_at >= since)
            .collect())
    }

    async fn mark_used(&self, profile_id: &str, id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE screenshot_notes SET used_at = ? WHERE profile_id = ? AND id = ?")
            .bind(stamp(at))
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("mark a screenshot note used")?;
        Ok(())
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM screenshot_notes WHERE profile_id = ? AND id = ?")
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete a screenshot note")?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete_all(&self, profile_id: &str) -> Result<u64> {
        let done = sqlx::query("DELETE FROM screenshot_notes WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await
            .context("delete a member's screenshot notes")?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo() -> SqliteScreenshotNoteRepository {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();
        for member in ["jerry", "amina"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(member)
                .bind(member)
                .execute(&pool)
                .await
                .unwrap();
        }
        SqliteScreenshotNoteRepository::new(pool)
    }

    fn note(id: &str, owner: &str, text: &str, age_days: i64) -> ScreenshotNote {
        ScreenshotNote {
            id: id.into(),
            profile_id: owner.into(),
            note: text.into(),
            taken_at: None,
            created_at: Utc::now() - chrono::Duration::days(age_days),
            used_at: None,
        }
    }

    #[tokio::test]
    async fn a_note_is_its_members_and_is_kept_once() {
        let repo = repo().await;
        assert!(repo
            .add(&note("n1", "jerry", "Saved a recipe for dal", 0))
            .await
            .unwrap());
        assert!(
            !repo
                .add(&note("n2", "jerry", "saved a recipe for dal.", 0))
                .await
                .unwrap(),
            "the same line twice is one note"
        );
        assert!(repo
            .add(&note("n3", "amina", "Saved a recipe for dal", 0))
            .await
            .unwrap());
        assert_eq!(repo.list("jerry").await.unwrap().len(), 1);
        assert!(!repo.delete("amina", "n1").await.unwrap());
        assert!(repo.delete("jerry", "n1").await.unwrap());
        assert!(repo.list("jerry").await.unwrap().is_empty());
        assert_eq!(repo.list("amina").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_used_or_old_note_seeds_nothing_and_all_can_be_deleted() {
        let repo = repo().await;
        repo.add(&note("old", "jerry", "Looking at a tent for camping", 20))
            .await
            .unwrap();
        repo.add(&note("new", "jerry", "Looking at the Ngong Hills trail", 1))
            .await
            .unwrap();
        repo.add(&note("used", "jerry", "Reading about the Safari Rally", 2))
            .await
            .unwrap();
        repo.mark_used("jerry", "used", Utc::now()).await.unwrap();
        let since = Utc::now() - chrono::Duration::days(14);
        let fresh: Vec<String> = repo
            .unused_since("jerry", since)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(fresh, vec!["new"]);
        assert_eq!(repo.delete_all("jerry").await.unwrap(), 3);
        assert!(repo.list("jerry").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_newest_notes_are_kept() {
        let repo = repo().await;
        for n in 0..(NOTES_KEPT + 3) {
            repo.add(&note(
                &format!("n{n}"),
                "jerry",
                &format!("Looking at product number {n} in a shop"),
                (NOTES_KEPT + 3 - n) as i64,
            ))
            .await
            .unwrap();
        }
        let kept = repo.list("jerry").await.unwrap();
        assert_eq!(kept.len(), NOTES_KEPT);
        assert_eq!(kept[0].id, format!("n{}", NOTES_KEPT + 2));
        assert!(!kept.iter().any(|n| n.id == "n0"));
    }
}
