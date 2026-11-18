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

const COLUMNS: &str = "id, profile_id, batch_id, purpose, anchor_id, subject_ref, header, text, \
     options_json, recommended, multi, gap, session_id, round, parent_id, state, choices_json, \
     other_text, asked_at, answered_at, expires_at, created_at";

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
        header: row.try_get("header")?,
        text: row.try_get("text")?,
        options: json_in(row, "options_json")?,
        recommended: row
            .try_get::<Option<i64>, _>("recommended")?
            .and_then(|at| usize::try_from(at).ok()),
        multi: flag_in(row, "multi")?,
        gap: row.try_get("gap")?,
        session_id: row.try_get("session_id")?,
        round: u32::try_from(row.try_get::<i64, _>("round")?).unwrap_or(1),
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
                 subject_ref, header, text, text_key, options_json, recommended, multi, gap, \
                 session_id, round, parent_id, state, choices_json, other_text, asked_at, \
                 answered_at, expires_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&question.id)
            .bind(&question.profile_id)
            .bind(&question.batch_id)
            .bind(question.purpose.as_str())
            .bind(&question.anchor_id)
            .bind(&question.subject_ref)
            .bind(&question.header)
            .bind(&question.text)
            .bind(question.key())
            .bind(json_of(&question.options)?)
            .bind(question.recommended.map(|at| at as i64))
            .bind(question.multi as i64)
            .bind(&question.gap)
            .bind(&question.session_id)
            .bind(i64::from(question.round))
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
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? \
             AND state IN ('open', 'waiting') ORDER BY created_at, rowid"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's open questions")?;
        rows.iter().map(question_of).collect()
    }

    async fn in_batch(&self, profile_id: &str, batch_id: &str) -> Result<Vec<Question>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? AND batch_id = ? \
             ORDER BY created_at, rowid"
        ))
        .bind(profile_id)
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await
        .context("read a batch of questions")?;
        rows.iter().map(question_of).collect()
    }

    async fn for_session(&self, profile_id: &str, session_id: &str) -> Result<Vec<Question>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? AND session_id = ? \
             ORDER BY created_at, rowid"
        ))
        .bind(profile_id)
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .context("list the questions a chat asked")?;
        rows.iter().map(question_of).collect()
    }

    async fn recent(&self, profile_id: &str, limit: usize) -> Result<Vec<Question>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM questions WHERE profile_id = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT ?"
        ))
        .bind(profile_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("list a member's recent questions")?;
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

    async fn set_waiting(&self, profile_id: &str, batch_id: &str) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE questions SET state = 'waiting' \
             WHERE profile_id = ? AND batch_id = ? AND state = 'open'",
        )
        .bind(profile_id)
        .bind(batch_id)
        .execute(&self.pool)
        .await
        .context("mark a batch of questions as waiting")?;
        Ok(done.rows_affected())
    }

    async fn expire_session(&self, session_id: &str, now: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE questions SET state = 'expired', answered_at = ? \
             WHERE session_id = ? AND state IN ('open', 'waiting')",
        )
        .bind(stamp(now))
        .bind(session_id)
        .execute(&self.pool)
        .await
        .context("expire the questions of a deleted chat")?;
        Ok(done.rows_affected())
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE questions SET state = 'expired', answered_at = ? \
             WHERE state IN ('open', 'waiting') AND expires_at IS NOT NULL AND expires_at <= ?",
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

    async fn a_chat_batch_waits_and_goes_with_its_chat(repo: &dyn QuestionRepository) {
        use pond_core::user_data::domain::question::{Preview, QuestionDraft, QuestionOption};
        let mut train = QuestionOption::new("Train", "Four hours, scenic");
        train.preview = Some(Preview::Place {
            name: "Madaraka Express".into(),
            price: Some("KSh 3,000".into()),
            url: Some("https://metickets.krc.co.ke".into()),
            image_url: None,
        });
        let draft = QuestionDraft {
            header: "Travel".into(),
            text: "How do you want to get to Diani?".into(),
            options: vec![train, QuestionOption::new("Fly", "An hour from Wilson")],
            recommended: Some(1),
            multi: false,
            gap: None,
        };
        let mut asked =
            Question::from_draft("jerry", "c1", QuestionPurpose::Chat, &draft, noon()).unwrap();
        asked.session_id = Some("s1".into());
        asked.round = 2;
        let other = question("jerry", "c2", "Do you drive?", 1);
        repo.add_batch(&[asked.clone(), other.clone()])
            .await
            .unwrap();
        assert_eq!(
            repo.get("jerry", &asked.id).await.unwrap(),
            Some(asked.clone())
        );
        assert_eq!(
            repo.in_batch("jerry", "c1").await.unwrap(),
            vec![asked.clone()]
        );
        assert_eq!(
            repo.for_session("jerry", "s1").await.unwrap(),
            vec![asked.clone()]
        );
        assert!(repo.for_session("liz", "s1").await.unwrap().is_empty());
        assert_eq!(repo.set_waiting("liz", "c1").await.unwrap(), 0);
        assert_eq!(repo.set_waiting("jerry", "c1").await.unwrap(), 1);
        let waiting = repo.get("jerry", &asked.id).await.unwrap().unwrap();
        assert_eq!(waiting.state, QuestionState::Waiting);
        assert_eq!(
            repo.open("jerry").await.unwrap().len(),
            2,
            "waiting is still open"
        );
        assert_eq!(
            texts(&repo.recent("jerry", 1).await.unwrap()),
            ["How do you want to get to Diani?"]
        );
        assert_eq!(repo.expire_session("s1", noon()).await.unwrap(), 1);
        assert_eq!(texts(&repo.open("jerry").await.unwrap()), ["Do you drive?"]);
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
    async fn a_chat_batch_waits_and_expires_with_its_chat() {
        a_chat_batch_waits_and_goes_with_its_chat(&sqlite().await).await;
        a_chat_batch_waits_and_goes_with_its_chat(&InMemoryQuestionRepository::new()).await;
    }

    #[tokio::test]
    async fn options_stored_as_plain_strings_read_as_labels() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO questions (id, profile_id, batch_id, purpose, text, text_key, \
             options_json, created_at) VALUES ('q1', 'jerry', 'b1', 'profile', \
             'Do you drive?', 'do you drive', '[\"Yes\",\"No\"]', '2026-11-16T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let read = SqliteQuestionRepository::new(pool)
            .get("jerry", "q1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.labels(), ["Yes", "No"]);
        assert_eq!((read.header.as_str(), read.round), ("", 1));
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
