//! SQLite-backed [`QuestionRepository`] over the `questions` table. An answer is the
//! `choices_json` and `other_text` of an answered row.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::question::{Answer, Question, QuestionPurpose, QuestionState};
use pond_core::user_data::ports::questions::QuestionRepository;

use crate::sqlite_rows::{
    flag_in, json_in, json_of, maybe_stamp, maybe_time_of, parse_time, stamp, time_of, word_in,
};

pub struct SqliteQuestionRepository {
    pool: Pool<Sqlite>,
}

impl SqliteQuestionRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, batch_id, purpose, anchor_id, subject_ref, text, \
     options_json, multi, parent_id, state, choices_json, other_text, asked_at, answered_at, \
     expires_at, created_at";

fn question_of(row: &SqliteRow) -> Result<Question> {
    let state = word_in(row, "state", QuestionState::parse)?;
    let answer = if state == QuestionState::Answered {
        Some(Answer {
            choices: json_in(row, "choices_json")?,
            text: row.try_get("other_text")?,
        })
    } else {
        None
    };
    Ok(Question {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        batch_id: row.try_get("batch_id")?,
        purpose: word_in(row, "purpose", QuestionPurpose::parse)?,
        anchor_id: row.try_get("anchor_id")?,
        subject_ref: row.try_get("subject_ref")?,
        text: row.try_get("text")?,
        options: json_in(row, "options_json")?,
        multi: flag_in(row, "multi")?,
        parent_id: row.try_get("parent_id")?,
        state,
        answer,
        asked_at: maybe_time_of(row, "asked_at")?,
        answered_at: maybe_time_of(row, "answered_at")?,
        expires_at: maybe_time_of(row, "expires_at")?,
        created_at: time_of(row, "created_at")?,
    })
}

fn choices_json(question: &Question) -> Result<String> {
    let none = Vec::new();
    json_of(question.answer.as_ref().map_or(&none, |a| &a.choices))
}

fn other_text(question: &Question) -> Option<&str> {
    question.answer.as_ref().and_then(|a| a.text.as_deref())
}

#[async_trait]
impl QuestionRepository for SqliteQuestionRepository {
    async fn add_batch(&self, questions: &[Question]) -> Result<Vec<Question>> {
        let mut stored = Vec::new();
        let mut tx = self.pool.begin().await?;
        for question in questions {
            let done = sqlx::query(
                "INSERT INTO questions (id, profile_id, batch_id, purpose, anchor_id, \
                 subject_ref, text, text_key, options_json, multi, parent_id, state, \
                 choices_json, other_text, asked_at, answered_at, expires_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&question.id)
            .bind(&question.profile_id)
            .bind(&question.batch_id)
            .bind(question.purpose.as_str())
            .bind(&question.anchor_id)
            .bind(&question.subject_ref)
            .bind(&question.text)
            .bind(question.key())
            .bind(json_of(&question.options)?)
            .bind(question.multi as i64)
            .bind(&question.parent_id)
            .bind(question.state.as_str())
            .bind(choices_json(question)?)
            .bind(other_text(question))
            .bind(maybe_stamp(question.asked_at))
            .bind(maybe_stamp(question.answered_at))
            .bind(maybe_stamp(question.expires_at))
            .bind(stamp(question.created_at))
            .execute(&mut *tx)
            .await
            .context("store a question")?;
            if done.rows_affected() > 0 {
                stored.push(question.clone());
            }
        }
        tx.commit().await?;
        Ok(stored)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Question>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read a question")?;
        row.as_ref().map(question_of).transpose()
    }

    async fn open(&self, profile_id: &str) -> Result<Vec<Question>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? AND state = 'open' \
             ORDER BY created_at, rowid"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's open questions")?;
        rows.iter().map(question_of).collect()
    }

    async fn for_anchor(&self, profile_id: &str, anchor_id: &str) -> Result<Vec<Question>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? AND anchor_id = ? \
             ORDER BY created_at, rowid"
        ))
        .bind(profile_id)
        .bind(anchor_id)
        .fetch_all(&self.pool)
        .await
        .context("list the questions asked for an anchor")?;
        rows.iter().map(question_of).collect()
    }

    async fn answered(&self, profile_id: &str, limit: usize) -> Result<Vec<Question>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? AND state = 'answered' \
             ORDER BY answered_at DESC, rowid DESC LIMIT ?"
        ))
        .bind(profile_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("list a member's answers")?;
        rows.iter().map(question_of).collect()
    }

    async fn save(&self, question: &Question) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE questions SET state = ?, choices_json = ?, other_text = ?, asked_at = ?, \
             answered_at = ?, expires_at = ? WHERE id = ? AND profile_id = ?",
        )
        .bind(question.state.as_str())
        .bind(choices_json(question)?)
        .bind(other_text(question))
        .bind(maybe_stamp(question.asked_at))
        .bind(maybe_stamp(question.answered_at))
        .bind(maybe_stamp(question.expires_at))
        .bind(&question.id)
        .bind(&question.profile_id)
        .execute(&self.pool)
        .await
        .context("save a question")?;
        Ok(done.rows_affected() > 0)
    }

    async fn mark_asked(&self, profile_id: &str, batch_id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "UPDATE questions SET asked_at = ? \
             WHERE profile_id = ? AND batch_id = ? AND state = 'open'",
        )
        .bind(stamp(at))
        .bind(profile_id)
        .bind(batch_id)
        .execute(&self.pool)
        .await
        .context("stamp a batch of questions as asked")?;
        Ok(())
    }

    async fn last_batch_at(
        &self,
        profile_id: &str,
        purpose: QuestionPurpose,
    ) -> Result<Option<DateTime<Utc>>> {
        let latest: Option<String> = sqlx::query_scalar(
            "SELECT MAX(created_at) FROM questions WHERE profile_id = ? AND purpose = ?",
        )
        .bind(profile_id)
        .bind(purpose.as_str())
        .fetch_one(&self.pool)
        .await
        .context("read when a member was last asked a batch")?;
        latest.as_deref().map(parse_time).transpose()
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE questions SET state = 'expired', answered_at = ? \
             WHERE state = 'open' AND expires_at IS NOT NULL AND expires_at <= ?",
        )
        .bind(stamp(now))
        .bind(stamp(now))
        .execute(&self.pool)
        .await
        .context("expire questions past their deadline")?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::mocks::mock_questions::InMemoryQuestionRepository;

    fn question(member: &str, batch: &str, text: &str, minutes_old: i64) -> Question {
        Question::new(
            member,
            batch,
            QuestionPurpose::Profile,
            text,
            &["Yes".to_string(), "No".to_string(), "Sometimes".to_string()],
            false,
            noon() - Duration::minutes(minutes_old),
        )
        .unwrap()
    }

    fn texts(found: &[Question]) -> Vec<&str> {
        found.iter().map(|q| q.text.as_str()).collect()
    }

    async fn a_batch_round_trips_and_a_question_is_asked_once(repo: &dyn QuestionRepository) {
        let mut drive = question("jerry", "b1", "Do you drive?", 5);
        drive.subject_ref = Some("gap:transport".into());
        drive.expires_at = Some(noon() + Duration::days(3));
        let cook = question("jerry", "b1", "Do you cook?", 5);
        let stored = repo
            .add_batch(&[drive.clone(), cook.clone()])
            .await
            .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(
            repo.get("jerry", &drive.id).await.unwrap(),
            Some(drive.clone())
        );
        assert_eq!(
            texts(&repo.open("jerry").await.unwrap()),
            ["Do you drive?", "Do you cook?"]
        );

        let again = repo
            .add_batch(&[
                question("jerry", "b2", "do you DRIVE", 1),
                question("jerry", "b2", "Do you travel for work?", 1),
            ])
            .await
            .unwrap();
        assert_eq!(
            texts(&again),
            ["Do you travel for work?"],
            "asked once, by key"
        );
        assert_eq!(
            repo.last_batch_at("jerry", QuestionPurpose::Profile)
                .await
                .unwrap(),
            Some(noon() - Duration::minutes(1))
        );
        assert_eq!(
            repo.last_batch_at("jerry", QuestionPurpose::Ledger)
                .await
                .unwrap(),
            None
        );
    }

    async fn an_answer_a_skip_and_an_expiry_are_kept(repo: &dyn QuestionRepository) {
        let mut drive = question("jerry", "b1", "Do you drive?", 5);
        drive.anchor_id = None;
        let mut cook = question("jerry", "b1", "Do you cook?", 4);
        let mut late = question("jerry", "b1", "Do you fly often?", 3);
        late.expires_at = Some(noon() - Duration::hours(1));
        let wait = question("jerry", "b1", "Do you run?", 2);
        repo.add_batch(&[drive.clone(), cook.clone(), late.clone(), wait.clone()])
            .await
            .unwrap();
        repo.mark_asked("jerry", "b1", noon()).await.unwrap();

        drive.asked_at = Some(noon());
        drive
            .answer(
                Answer {
                    choices: vec!["yes".into()],
                    text: Some("a Fielder, mostly weekends".into()),
                },
                noon() + Duration::minutes(2),
            )
            .unwrap();
        assert!(repo.save(&drive).await.unwrap());
        cook.asked_at = Some(noon());
        assert!(cook.skip(noon() + Duration::minutes(3)));
        assert!(repo.save(&cook).await.unwrap());
        assert_eq!(
            repo.expire_due(noon() + Duration::minutes(4))
                .await
                .unwrap(),
            1
        );

        assert_eq!(
            repo.get("jerry", &drive.id).await.unwrap(),
            Some(drive.clone())
        );
        assert_eq!(repo.get("jerry", &cook.id).await.unwrap(), Some(cook));
        let expired = repo.get("jerry", &late.id).await.unwrap().unwrap();
        assert_eq!(expired.state, QuestionState::Expired);
        assert_eq!(texts(&repo.open("jerry").await.unwrap()), ["Do you run?"]);
        assert_eq!(repo.answered("jerry", 5).await.unwrap(), vec![drive]);
        assert_eq!(
            repo.open("jerry").await.unwrap()[0].asked_at,
            Some(noon()),
            "the batch was stamped when it was pushed"
        );
    }

    async fn a_member_never_reads_or_answers_anothers_questions(repo: &dyn QuestionRepository) {
        let mine = question("jerry", "b1", "Do you drive?", 1);
        let theirs = question("liz", "b9", "Do you drive?", 1);
        assert_eq!(
            repo.add_batch(&[mine.clone(), theirs.clone()])
                .await
                .unwrap()
                .len(),
            2,
            "the same question may be asked of two members"
        );
        assert_eq!(texts(&repo.open("jerry").await.unwrap()), ["Do you drive?"]);
        assert_eq!(repo.get("liz", &mine.id).await.unwrap(), None);
        let mut forged = mine.clone();
        forged.profile_id = "liz".into();
        assert!(forged.skip(noon()));
        assert!(!repo.save(&forged).await.unwrap());
        repo.mark_asked("liz", "b1", noon()).await.unwrap();
        assert_eq!(repo.get("jerry", &mine.id).await.unwrap(), Some(mine));
        assert!(repo.answered("liz", 5).await.unwrap().is_empty());
    }

    async fn sqlite() -> SqliteQuestionRepository {
        SqliteQuestionRepository::new(pool().await)
    }

    #[tokio::test]
    async fn a_batch_of_questions_round_trips_and_a_member_is_asked_a_question_once() {
        a_batch_round_trips_and_a_question_is_asked_once(&sqlite().await).await;
        a_batch_round_trips_and_a_question_is_asked_once(&InMemoryQuestionRepository::new()).await;
    }

    #[tokio::test]
    async fn an_answer_a_skip_and_an_expiry_are_kept_on_the_question() {
        an_answer_a_skip_and_an_expiry_are_kept(&sqlite().await).await;
        an_answer_a_skip_and_an_expiry_are_kept(&InMemoryQuestionRepository::new()).await;
    }

    #[tokio::test]
    async fn one_member_never_reads_or_answers_anothers_questions() {
        a_member_never_reads_or_answers_anothers_questions(&sqlite().await).await;
        a_member_never_reads_or_answers_anothers_questions(&InMemoryQuestionRepository::new())
            .await;
    }

    #[tokio::test]
    async fn an_anchors_questions_are_read_together_and_go_with_the_anchor() {
        use crate::sqlite_anchors::SqliteAnchorRepository;
        use pond_core::user_data::domain::anchor::{Anchor, AnchorKind, AnchorOrigin};
        use pond_core::user_data::ports::anchors::AnchorRepository;

        let pool = pool().await;
        let anchors = SqliteAnchorRepository::new(pool.clone());
        let repo = SqliteQuestionRepository::new(pool.clone());
        let trip = Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Kisumu",
            AnchorOrigin::You,
            noon(),
        )
        .unwrap();
        anchors.add(&trip).await.unwrap();
        let mut budget = question("jerry", "b1", "What is your budget?", 2);
        budget.purpose = QuestionPurpose::Anchor;
        budget.anchor_id = Some(trip.id.clone());
        repo.add_batch(&[budget.clone(), question("jerry", "b2", "Do you drive?", 1)])
            .await
            .unwrap();
        assert_eq!(
            repo.for_anchor("jerry", &trip.id).await.unwrap(),
            vec![budget]
        );
        assert!(repo.for_anchor("liz", &trip.id).await.unwrap().is_empty());
        sqlx::query("DELETE FROM anchors WHERE id = ?")
            .bind(&trip.id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(texts(&repo.open("jerry").await.unwrap()), ["Do you drive?"]);
    }
}
