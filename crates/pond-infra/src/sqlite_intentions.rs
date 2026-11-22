//! SQLite-backed [`IntentionRepository`] over the `intentions` table. A condition is its kind
//! and the kind's JSON spec.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::intention::{
    Condition, ConditionKind, IntentAct, Intention, IntentionState,
};
use pond_core::user_data::ports::intentions::IntentionRepository;

use crate::sqlite_rows::{maybe_stamp, maybe_time_of, stamp, time_of, word_in};

pub struct SqliteIntentionRepository {
    pool: Pool<Sqlite>,
}

impl SqliteIntentionRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, act, text, why, condition_kind, condition_json, \
     condition_label, anchor_id, until, state, outcome, created_at, settled_at";

fn intention_of(row: &SqliteRow) -> Result<Intention> {
    let kind = word_in(row, "condition_kind", ConditionKind::parse)?;
    let spec: String = row.try_get("condition_json")?;
    let condition = serde_json::from_str(&spec)
        .ok()
        .and_then(|spec| Condition::from_stored(kind, &spec))
        .with_context(|| format!("unreadable {} condition {spec:?}", kind.as_str()))?;
    Ok(Intention {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        act: word_in(row, "act", IntentAct::parse)?,
        text: row.try_get("text")?,
        why: row.try_get("why")?,
        condition,
        condition_label: row.try_get("condition_label")?,
        anchor_id: row.try_get("anchor_id")?,
        until: time_of(row, "until")?,
        state: word_in(row, "state", IntentionState::parse)?,
        outcome: row.try_get("outcome")?,
        created_at: time_of(row, "created_at")?,
        settled_at: maybe_time_of(row, "settled_at")?,
    })
}

#[async_trait]
impl IntentionRepository for SqliteIntentionRepository {
    async fn add(&self, intention: &Intention) -> Result<bool> {
        let key = intention.key();
        // The table has no unique key on the text: a settled intention may be made again.
        let done = sqlx::query(
            "INSERT INTO intentions (id, profile_id, act, text, text_key, why, condition_kind, \
             condition_json, condition_label, anchor_id, until, state, outcome, created_at, \
             settled_at) \
             SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ? \
             WHERE NOT EXISTS (SELECT 1 FROM intentions \
                 WHERE profile_id = ? AND text_key = ? AND state = 'waiting') \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&intention.id)
        .bind(&intention.profile_id)
        .bind(intention.act.as_str())
        .bind(&intention.text)
        .bind(&key)
        .bind(&intention.why)
        .bind(intention.condition.kind().as_str())
        .bind(intention.condition.spec().to_string())
        .bind(&intention.condition_label)
        .bind(&intention.anchor_id)
        .bind(stamp(intention.until))
        .bind(intention.state.as_str())
        .bind(&intention.outcome)
        .bind(stamp(intention.created_at))
        .bind(maybe_stamp(intention.settled_at))
        .bind(&intention.profile_id)
        .bind(&key)
        .execute(&self.pool)
        .await
        .context("keep an intention")?;
        Ok(done.rows_affected() > 0)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Intention>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM intentions WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read an intention")?;
        row.as_ref().map(intention_of).transpose()
    }

    async fn list(
        &self,
        profile_id: &str,
        state: Option<IntentionState>,
    ) -> Result<Vec<Intention>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM intentions WHERE profile_id = ? AND (? IS NULL OR state = ?) \
             ORDER BY created_at DESC, rowid DESC"
        ))
        .bind(profile_id)
        .bind(state.map(IntentionState::as_str))
        .bind(state.map(IntentionState::as_str))
        .fetch_all(&self.pool)
        .await
        .context("list a member's intentions")?;
        rows.iter().map(intention_of).collect()
    }

    async fn waiting(&self, kind: ConditionKind) -> Result<Vec<Intention>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM intentions WHERE state = 'waiting' AND condition_kind = ? \
             ORDER BY until, rowid"
        ))
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await
        .context("list waiting intentions by their condition")?;
        rows.iter().map(intention_of).collect()
    }

    async fn waiting_past(&self, now: DateTime<Utc>) -> Result<Vec<Intention>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM intentions WHERE state = 'waiting' AND until < ? \
             ORDER BY until, rowid"
        ))
        .bind(stamp(now))
        .fetch_all(&self.pool)
        .await
        .context("list waiting intentions past their deadline")?;
        rows.iter().map(intention_of).collect()
    }

    async fn save(&self, intention: &Intention) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE intentions SET state = ?, outcome = ?, settled_at = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(intention.state.as_str())
        .bind(&intention.outcome)
        .bind(maybe_stamp(intention.settled_at))
        .bind(&intention.id)
        .bind(&intention.profile_id)
        .execute(&self.pool)
        .await
        .context("save an intention")?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::shared::domain::todo::Transition;
    use pond_core::user_data::mocks::mock_intentions::InMemoryIntentionRepository;

    fn intention(member: &str, text: &str, condition: Condition, until_hours: i64) -> Intention {
        Intention::new(
            member,
            IntentAct::Push,
            text,
            condition,
            "when it holds",
            noon() + Duration::hours(until_hours),
            noon(),
        )
        .unwrap()
    }

    fn at(hours: i64) -> Condition {
        Condition::Time {
            at: noon() + Duration::hours(hours),
        }
    }

    fn texts(found: &[Intention]) -> Vec<&str> {
        found.iter().map(|i| i.text.as_str()).collect()
    }

    async fn every_condition_round_trips(repo: &dyn IntentionRepository) {
        let mut timed = intention("jerry", "Leave for the summit", at(20), 24);
        timed.why = "The doors open at 8.".into();
        let arriving = intention(
            "jerry",
            "Ask about the office printer",
            Condition::Place {
                place_id: "office".into(),
                transition: Transition::Arrive,
            },
            48,
        );
        let package = intention(
            "jerry",
            "Say the parcel is at the gate",
            Condition::Thing {
                source_id: "camera:gate".into(),
                event: "package".into(),
            },
            72,
        );
        let next_chat = intention("jerry", "Ask how the talk went", Condition::Chat, 96);
        for one in [&timed, &arriving, &package, &next_chat] {
            assert!(repo.add(one).await.unwrap());
            assert_eq!(
                repo.get("jerry", &one.id).await.unwrap().as_ref(),
                Some(one)
            );
        }
        assert_eq!(
            repo.waiting(ConditionKind::Time).await.unwrap(),
            vec![timed]
        );
        assert_eq!(
            repo.waiting(ConditionKind::Place).await.unwrap(),
            vec![arriving]
        );
        assert_eq!(
            repo.waiting(ConditionKind::Thing).await.unwrap(),
            vec![package]
        );
        assert_eq!(
            repo.waiting(ConditionKind::Chat).await.unwrap(),
            vec![next_chat]
        );
    }

    async fn one_waiting_intention_per_key_until_it_settles(repo: &dyn IntentionRepository) {
        let mut first = intention("jerry", "Remind about the rain", at(1), 5);
        assert!(repo.add(&first).await.unwrap());
        let twin = intention("jerry", "remind about the RAIN!", Condition::Chat, 9);
        assert!(!repo.add(&twin).await.unwrap(), "the same key is waiting");
        assert!(repo
            .add(&intention("liz", "Remind about the rain", at(1), 5))
            .await
            .unwrap());

        first
            .done(
                "Told Jerry it will rain at four",
                noon() + Duration::hours(1),
            )
            .unwrap();
        assert!(repo.save(&first).await.unwrap());
        assert_eq!(
            repo.get("jerry", &first.id).await.unwrap(),
            Some(first.clone())
        );
        assert!(repo.add(&twin).await.unwrap(), "the first one is settled");
        assert_eq!(
            texts(
                &repo
                    .list("jerry", Some(IntentionState::Waiting))
                    .await
                    .unwrap()
            ),
            ["remind about the RAIN!"]
        );
        assert_eq!(repo.list("jerry", None).await.unwrap().len(), 2);
    }

    async fn the_runner_sees_every_members_and_a_member_only_their_own(
        repo: &dyn IntentionRepository,
    ) {
        let mine = intention("jerry", "Leave for the summit", at(2), 3);
        let theirs = intention("liz", "Water the plants", at(1), 30);
        let mut late = intention("liz", "Call the plumber", Condition::Chat, 1);
        for one in [&mine, &theirs, &late] {
            assert!(repo.add(one).await.unwrap());
        }
        assert_eq!(
            texts(&repo.waiting(ConditionKind::Time).await.unwrap()),
            ["Leave for the summit", "Water the plants"],
            "nearest deadline first"
        );
        assert_eq!(
            texts(
                &repo
                    .waiting_past(noon() + Duration::hours(2))
                    .await
                    .unwrap()
            ),
            ["Call the plumber"]
        );
        assert_eq!(
            texts(&repo.list("jerry", None).await.unwrap()),
            ["Leave for the summit"]
        );
        assert_eq!(repo.get("jerry", &theirs.id).await.unwrap(), None);

        let mut forged = theirs.clone();
        forged.profile_id = "jerry".into();
        forged.withdraw(noon()).unwrap();
        assert!(!repo.save(&forged).await.unwrap());
        late.expire(noon() + Duration::hours(2)).unwrap();
        assert!(repo.save(&late).await.unwrap());
        assert!(repo
            .waiting_past(noon() + Duration::hours(2))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo.list("liz", Some(IntentionState::Waiting))
                .await
                .unwrap(),
            vec![theirs]
        );
    }

    async fn sqlite() -> SqliteIntentionRepository {
        SqliteIntentionRepository::new(pool().await)
    }

    #[tokio::test]
    async fn an_intention_round_trips_with_each_kind_of_condition() {
        every_condition_round_trips(&sqlite().await).await;
        every_condition_round_trips(&InMemoryIntentionRepository::new()).await;
    }

    #[tokio::test]
    async fn a_member_has_one_waiting_intention_per_key_until_it_settles() {
        one_waiting_intention_per_key_until_it_settles(&sqlite().await).await;
        one_waiting_intention_per_key_until_it_settles(&InMemoryIntentionRepository::new()).await;
    }

    #[tokio::test]
    async fn the_runner_sees_every_members_intentions_and_a_member_only_their_own() {
        the_runner_sees_every_members_and_a_member_only_their_own(&sqlite().await).await;
        the_runner_sees_every_members_and_a_member_only_their_own(
            &InMemoryIntentionRepository::new(),
        )
        .await;
    }
}
