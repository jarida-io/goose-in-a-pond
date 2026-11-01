//! SQLite-backed implementation of `ProfileInterviewRepository`. Topics and the draft are JSON.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::profile_interview::{Interview, InterviewStatus};
use pond_core::user_data::ports::profile_interview::ProfileInterviewRepository;

pub struct SqliteProfileInterviewRepository {
    pool: Pool<Sqlite>,
}

impl SqliteProfileInterviewRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    async fn one(&self, column: &str, value: &str) -> Result<Option<Interview>> {
        let sql = format!(
            "SELECT profile_id, session_id, status, topics, draft, pending_opening, drafted_at, \
             last_reply_at, started_at, updated_at FROM profile_interviews WHERE {column} = ? \
             ORDER BY updated_at DESC LIMIT 1"
        );
        let row = sqlx::query(&sql)
            .bind(value)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_interview).transpose()
    }
}

fn time(raw: Option<String>) -> Result<Option<DateTime<Utc>>> {
    raw.map(|s| {
        DateTime::parse_from_rfc3339(&s)
            .map(|t| t.with_timezone(&Utc))
            .with_context(|| format!("bad interview timestamp {s:?}"))
    })
    .transpose()
}

fn row_to_interview(row: &sqlx::sqlite::SqliteRow) -> Result<Interview> {
    let status: String = row.try_get("status")?;
    Ok(Interview {
        profile_id: row.try_get("profile_id")?,
        session_id: row.try_get("session_id")?,
        status: InterviewStatus::parse(&status)
            .with_context(|| format!("unknown interview status {status:?}"))?,
        topics: serde_json::from_str(&row.try_get::<String, _>("topics")?)?,
        draft: serde_json::from_str(&row.try_get::<String, _>("draft")?)?,
        pending_opening: row.try_get("pending_opening")?,
        drafted_at: time(row.try_get("drafted_at")?)?,
        last_reply_at: time(row.try_get("last_reply_at")?)?,
        started_at: time(Some(row.try_get("started_at")?))?.unwrap_or_default(),
        updated_at: time(Some(row.try_get("updated_at")?))?.unwrap_or_default(),
    })
}

#[async_trait]
impl ProfileInterviewRepository for SqliteProfileInterviewRepository {
    async fn get(&self, profile_id: &str) -> Result<Option<Interview>> {
        self.one("profile_id", profile_id).await
    }

    async fn get_by_session(&self, session_id: &str) -> Result<Option<Interview>> {
        self.one("session_id", session_id).await
    }

    async fn save(&self, i: &Interview) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO profile_interviews (profile_id, session_id, status, topics, \
             draft, pending_opening, drafted_at, last_reply_at, started_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&i.profile_id)
        .bind(&i.session_id)
        .bind(i.status.as_str())
        .bind(serde_json::to_string(&i.topics)?)
        .bind(serde_json::to_string(&i.draft)?)
        .bind(&i.pending_opening)
        .bind(i.drafted_at.map(|t| t.to_rfc3339()))
        .bind(i.last_reply_at.map(|t| t.to_rfc3339()))
        .bind(i.started_at.to_rfc3339())
        .bind(i.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, profile_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM profile_interviews WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::user_data::domain::profile_interview::{NoteDraft, Topic};

    #[tokio::test]
    async fn an_interview_round_trips_and_is_found_by_its_session() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('amina', 'Amina')")
            .execute(&db.system)
            .await
            .unwrap();
        let repo = SqliteProfileInterviewRepository::new(db.system.clone());
        let now = DateTime::parse_from_rfc3339("2026-10-31T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut i = Interview::new("amina", "s1", now);
        i.record(Topic::Address, "Mama Amina", now).unwrap();
        i.skip_rest(now).unwrap();
        i.propose(
            vec![NoteDraft {
                topic: Topic::Address,
                instruction: "Call her Mama Amina.".into(),
            }],
            &[],
            &[],
            now,
        )
        .unwrap();
        i.note_reply(now);
        i.pending_opening = Some("Hi!".into());
        repo.save(&i).await.unwrap();

        assert_eq!(repo.get("amina").await.unwrap(), Some(i.clone()));
        assert_eq!(repo.get_by_session("s1").await.unwrap(), Some(i));
        assert!(repo.get_by_session("s2").await.unwrap().is_none());
        repo.delete("amina").await.unwrap();
        assert!(repo.get("amina").await.unwrap().is_none());
    }
}
