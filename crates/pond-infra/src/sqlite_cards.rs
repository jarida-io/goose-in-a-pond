//! SQLite-backed [`CardRepository`] over the `cards` table.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::card::{Card, CardAnchor, CardKind, CardState};
use pond_core::user_data::ports::cards::CardRepository;

use crate::sqlite_rows::{count_in, json_in, json_of, stamp, time_of, word_in};

pub struct SqliteCardRepository {
    pool: Pool<Sqlite>,
}

impl SqliteCardRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, batch_id, rank, kind, title, why_json, when_text, \
     where_text, cost, effort, action_json, sources_json, notes_json, anchor_id, anchor_title, \
     state, created_at, fades_at";

const NEWEST_FIRST: &str = "ORDER BY created_at DESC, rank, rowid";

fn card_of(row: &SqliteRow) -> Result<Card> {
    let why: String = row.try_get("why_json")?;
    let anchor_id: Option<String> = row.try_get("anchor_id")?;
    let anchor_title: Option<String> = row.try_get("anchor_title")?;
    Ok(Card {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        kind: word_in(row, "kind", CardKind::parse)?,
        title: row.try_get("title")?,
        why: serde_json::from_str(&why).context("read a card's why")?,
        when: row.try_get("when_text")?,
        where_: row.try_get("where_text")?,
        cost: row.try_get("cost")?,
        effort: row.try_get("effort")?,
        action: json_in(row, "action_json")?,
        sources: json_in(row, "sources_json")?,
        notes: json_in(row, "notes_json")?,
        anchor: anchor_id.map(|id| CardAnchor {
            id,
            title: anchor_title.unwrap_or_default(),
        }),
        state: word_in(row, "state", CardState::parse)?,
        batch_id: row.try_get("batch_id")?,
        rank: count_in(row, "rank")?,
        created_at: time_of(row, "created_at")?,
        fades_at: time_of(row, "fades_at")?,
    })
}

async fn insert_card<'e, E>(executor: E, card: &Card, now: DateTime<Utc>) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(&format!(
        "INSERT INTO cards ({COLUMNS}, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(&card.id)
    .bind(&card.profile_id)
    .bind(&card.batch_id)
    .bind(i64::from(card.rank))
    .bind(card.kind.as_str())
    .bind(&card.title)
    .bind(json_of(&card.why)?)
    .bind(&card.when)
    .bind(&card.where_)
    .bind(&card.cost)
    .bind(&card.effort)
    .bind(card.action.as_ref().map(json_of).transpose()?)
    .bind(json_of(&card.sources)?)
    .bind(json_of(&card.notes)?)
    .bind(card.anchor.as_ref().map(|a| a.id.clone()))
    .bind(card.anchor.as_ref().map(|a| a.title.clone()))
    .bind(card.state.as_str())
    .bind(stamp(card.created_at))
    .bind(stamp(card.fades_at))
    .bind(stamp(now))
    .execute(executor)
    .await
    .context("store a card")?;
    Ok(())
}

#[async_trait]
impl CardRepository for SqliteCardRepository {
    async fn add_set(&self, profile_id: &str, cards: &[Card], now: DateTime<Utc>) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE cards SET fades_at = ?, updated_at = ? \
             WHERE profile_id = ? AND state = 'new' AND fades_at > ?",
        )
        .bind(stamp(now))
        .bind(stamp(now))
        .bind(profile_id)
        .bind(stamp(now))
        .execute(&mut *tx)
        .await
        .context("fade a member's earlier cards")?;
        for card in cards {
            insert_card(&mut *tx, card, now).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn add_one(&self, card: &Card) -> Result<()> {
        insert_card(&self.pool, card, card.created_at).await
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cards WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read a card")?;
        row.as_ref().map(card_of).transpose()
    }

    async fn shown(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Card>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cards WHERE profile_id = ? \
             AND (state = 'saved' OR (state = 'new' AND fades_at > ?)) {NEWEST_FIRST}"
        ))
        .bind(profile_id)
        .bind(stamp(now))
        .fetch_all(&self.pool)
        .await
        .context("list a member's cards")?;
        rows.iter().map(card_of).collect()
    }

    async fn since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<Card>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM cards WHERE profile_id = ? AND created_at >= ? {NEWEST_FIRST}"
        ))
        .bind(profile_id)
        .bind(stamp(since))
        .fetch_all(&self.pool)
        .await
        .context("list a member's recent cards")?;
        rows.iter().map(card_of).collect()
    }

    async fn save(&self, card: &Card) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE cards SET state = ?, updated_at = ? WHERE profile_id = ? AND id = ?",
        )
        .bind(card.state.as_str())
        .bind(stamp(Utc::now()))
        .bind(&card.profile_id)
        .bind(&card.id)
        .execute(&self.pool)
        .await
        .context("save a card")?;
        Ok(done.rows_affected() > 0)
    }

    async fn purge_before(&self, before: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query("DELETE FROM cards WHERE state <> 'saved' AND created_at < ?")
            .bind(stamp(before))
            .execute(&self.pool)
            .await
            .context("purge old cards")?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::domain::card::{
        CardNotes, CardSource, CardSourceKind, Why, WhySource,
    };

    fn card(member: &str, title: &str, rank: u32, made: DateTime<Utc>) -> Card {
        Card {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: member.to_string(),
            kind: CardKind::Do,
            title: title.to_string(),
            why: Why {
                text: "You run".to_string(),
                source: WhySource {
                    kind: CardSourceKind::Fact,
                    label: "Runs".to_string(),
                    url: None,
                },
            },
            when: Some("Saturday".to_string()),
            where_: None,
            cost: None,
            effort: None,
            action: None,
            sources: vec![CardSource {
                kind: CardSourceKind::Fact,
                title: "Runs".to_string(),
                url: None,
            }],
            notes: CardNotes::default(),
            anchor: Some(CardAnchor {
                id: "a1".to_string(),
                title: "Summit".to_string(),
            }),
            state: CardState::New,
            batch_id: "b".to_string(),
            rank,
            created_at: made,
            fades_at: Card::fade_time(made),
        }
    }

    #[tokio::test]
    async fn a_new_set_fades_the_last_one_but_what_was_saved_stays() {
        let store = SqliteCardRepository::new(pool().await);
        let (first, later) = (noon(), noon() + Duration::hours(1));
        let old = card("jerry", "Old", 0, first);
        let mut kept = card("jerry", "Kept", 1, first);
        store
            .add_set("jerry", &[old.clone(), kept.clone()], first)
            .await
            .unwrap();
        kept.state = CardState::Saved;
        assert!(store.save(&kept).await.unwrap());
        store
            .add_set(
                "jerry",
                &[card("jerry", "B", 1, later), card("jerry", "A", 0, later)],
                later,
            )
            .await
            .unwrap();
        let titles: Vec<String> = store
            .shown("jerry", later)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.title)
            .collect();
        assert_eq!(titles, ["A", "B", "Kept"]);
        assert_eq!(store.since("jerry", first).await.unwrap().len(), 4);
        assert!(store.get("liz", &old.id).await.unwrap().is_none());
        let read = store.get("jerry", &old.id).await.unwrap().unwrap();
        assert_eq!(read.anchor.unwrap().title, "Summit");
        assert_eq!(store.purge_before(later).await.unwrap(), 1);
    }
}
