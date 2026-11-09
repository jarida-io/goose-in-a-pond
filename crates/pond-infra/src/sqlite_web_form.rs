//! SQLite-backed `FormSubmissionStore`. Kept in `pond_system.db`, so a form asked for in the voice
//! child is answered through `serve`, and a restart keeps what is waiting.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::shared::domain::web_form::{FormSubmission, SubmissionState};
use pond_core::shared::ports::web_form::FormSubmissionStore;

pub struct SqliteFormSubmissionStore {
    pool: Pool<Sqlite>,
}

impl SqliteFormSubmissionStore {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, page, action, fields, depth, session_id, state, \
                       result_status, result_address, result_title, failure, asked_at, decided_at";

fn time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("bad web form timestamp {raw:?}"))
}

fn state_of(row: &SqliteRow) -> Result<SubmissionState> {
    let name: String = row.try_get("state")?;
    Ok(match name.as_str() {
        "waiting" => SubmissionState::Waiting,
        "sending" => SubmissionState::Sending,
        "sent" => SubmissionState::Sent {
            status: row
                .try_get::<Option<i64>, _>("result_status")?
                .unwrap_or_default()
                .clamp(0, i64::from(u16::MAX)) as u16,
            address: row
                .try_get::<Option<String>, _>("result_address")?
                .unwrap_or_default(),
            title: row
                .try_get::<Option<String>, _>("result_title")?
                .unwrap_or_default(),
        },
        "declined" => SubmissionState::Declined,
        "timed_out" => SubmissionState::TimedOut,
        _ => SubmissionState::Failed {
            why: row
                .try_get::<Option<String>, _>("failure")?
                .unwrap_or_else(|| format!("The form's record reads \"{name}\".")),
        },
    })
}

fn submission_of(row: &SqliteRow) -> Result<FormSubmission> {
    let decided: Option<String> = row.try_get("decided_at")?;
    Ok(FormSubmission {
        id: row.try_get("id")?,
        owner: row.try_get("profile_id")?,
        page: row.try_get("page")?,
        action: row.try_get("action")?,
        values: serde_json::from_str(&row.try_get::<String, _>("fields")?)?,
        depth: row.try_get::<i64, _>("depth")?.clamp(0, i64::from(u8::MAX)) as u8,
        session_id: row.try_get("session_id")?,
        state: state_of(row)?,
        asked_at: time(&row.try_get::<String, _>("asked_at")?)?,
        decided_at: decided.as_deref().map(time).transpose()?,
    })
}

#[async_trait]
impl FormSubmissionStore for SqliteFormSubmissionStore {
    async fn create(&self, submission: &FormSubmission) -> Result<()> {
        sqlx::query(&format!(
            "INSERT INTO web_form_submissions ({COLUMNS}) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, ?, ?)"
        ))
        .bind(&submission.id)
        .bind(&submission.owner)
        .bind(&submission.page)
        .bind(&submission.action)
        .bind(serde_json::to_string(&submission.values)?)
        .bind(i64::from(submission.depth))
        .bind(&submission.session_id)
        .bind(submission.state.name())
        .bind(submission.asked_at.to_rfc3339())
        .bind(submission.decided_at.map(|t| t.to_rfc3339()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<FormSubmission>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM web_form_submissions WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(submission_of).transpose()
    }

    async fn recent_for(&self, owner: &str, since: DateTime<Utc>) -> Result<Vec<FormSubmission>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM web_form_submissions WHERE profile_id = ? AND asked_at >= ? \
             ORDER BY asked_at DESC"
        ))
        .bind(owner)
        .bind(since.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(submission_of).collect()
    }

    async fn advance(
        &self,
        id: &str,
        from: &SubmissionState,
        to: &SubmissionState,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let (status, address, title, failure) = match to {
            SubmissionState::Sent {
                status,
                address,
                title,
            } => (
                Some(i64::from(*status)),
                Some(address.as_str()),
                Some(title.as_str()),
                None,
            ),
            SubmissionState::Failed { why } => (None, None, None, Some(why.as_str())),
            _ => (None, None, None, None),
        };
        let moved = sqlx::query(
            "UPDATE web_form_submissions SET state = ?, result_status = ?, result_address = ?, \
             result_title = ?, failure = ?, decided_at = COALESCE(decided_at, ?) \
             WHERE id = ? AND state = ?",
        )
        .bind(to.name())
        .bind(status)
        .bind(address)
        .bind(title)
        .bind(failure)
        .bind(at.to_rfc3339())
        .bind(id)
        .bind(from.name())
        .execute(&self.pool)
        .await?;
        Ok(moved.rows_affected() == 1)
    }

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64> {
        let gone = sqlx::query("DELETE FROM web_form_submissions WHERE asked_at < ?")
            .bind(before.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(gone.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use chrono::Duration;

    async fn store() -> (SqliteFormSubmissionStore, Database, tempfile::TempDir) {
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
        (SqliteFormSubmissionStore::new(db.system.clone()), db, tmp)
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minutes)
    }

    fn form(id: &str, owner: &str, asked: i64) -> FormSubmission {
        FormSubmission {
            id: id.into(),
            owner: owner.into(),
            page: "https://cars.example/used/9".into(),
            action: "https://cars.example/enquire".into(),
            values: vec![
                ("q".into(), "is it available?".into()),
                ("make".into(), "subaru".into()),
            ],
            depth: 1,
            session_id: Some("s1".into()),
            state: SubmissionState::Waiting,
            asked_at: at(asked),
            decided_at: None,
        }
    }

    #[tokio::test]
    async fn a_form_reads_back_as_kept() {
        let (store, _db, _tmp) = store().await;
        store.create(&form("f1", "amina", 0)).await.unwrap();
        assert_eq!(store.get("f1").await.unwrap(), Some(form("f1", "amina", 0)));
        assert_eq!(store.get("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_form_moves_on_once_and_keeps_its_answer() {
        let (store, _db, _tmp) = store().await;
        store.create(&form("f1", "amina", 0)).await.unwrap();
        let waiting = SubmissionState::Waiting;
        let sending = SubmissionState::Sending;
        assert!(store
            .advance("f1", &waiting, &sending, at(1))
            .await
            .unwrap());
        assert!(
            !store
                .advance("f1", &waiting, &sending, at(2))
                .await
                .unwrap(),
            "a second yes finds it already sending"
        );
        let sent = SubmissionState::Sent {
            status: 200,
            address: "https://cars.example/thanks".into(),
            title: "Thank you".into(),
        };
        assert!(store.advance("f1", &sending, &sent, at(3)).await.unwrap());
        let back = store.get("f1").await.unwrap().unwrap();
        assert_eq!(back.state, sent);
        assert_eq!(
            back.decided_at,
            Some(at(1)),
            "decided when it first left waiting"
        );

        store.create(&form("f2", "amina", 0)).await.unwrap();
        let failed = SubmissionState::Failed {
            why: "The site did not answer.".into(),
        };
        store.advance("f2", &waiting, &failed, at(1)).await.unwrap();
        assert_eq!(store.get("f2").await.unwrap().unwrap().state, failed);
    }

    #[tokio::test]
    async fn recent_forms_are_the_owners_newest_first_and_old_ones_pruned() {
        let (store, _db, _tmp) = store().await;
        store.create(&form("old", "amina", 0)).await.unwrap();
        store.create(&form("new", "amina", 30)).await.unwrap();
        store.create(&form("theirs", "baraka", 30)).await.unwrap();
        let ids = |forms: Vec<FormSubmission>| forms.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(
            ids(store.recent_for("amina", at(0)).await.unwrap()),
            vec!["new", "old"]
        );
        assert_eq!(
            ids(store.recent_for("amina", at(10)).await.unwrap()),
            vec!["new"]
        );
        assert_eq!(store.prune(at(10)).await.unwrap(), 1);
        assert_eq!(store.get("old").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_removed_member_takes_their_forms_with_them() {
        let (store, db, _tmp) = store().await;
        store.create(&form("f1", "baraka", 0)).await.unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = 'baraka'")
            .execute(&db.system)
            .await
            .unwrap();
        assert_eq!(store.get("f1").await.unwrap(), None);
    }
}
