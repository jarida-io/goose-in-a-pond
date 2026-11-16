//! SQLite-backed [`DontListRepository`] over the `dont_list` table.

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::dont_list::{sort_rules, DontRule, RuleOrigin, Strength};
use pond_core::user_data::ports::dont_list::DontListRepository;

use crate::sqlite_rows::{json_in, json_of, stamp, time_of, word_in};

pub struct SqliteDontListRepository {
    pool: Pool<Sqlite>,
}

impl SqliteDontListRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str =
    "id, profile_id, rule, strength, reasons_json, said, origin, origin_ref, created_at";

fn rule_of(row: &SqliteRow) -> Result<DontRule> {
    Ok(DontRule {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        rule: row.try_get("rule")?,
        strength: word_in(row, "strength", Strength::parse)?,
        reasons: json_in(row, "reasons_json")?,
        said: row.try_get("said")?,
        origin: word_in(row, "origin", RuleOrigin::parse)?,
        origin_ref: row.try_get("origin_ref")?,
        created_at: time_of(row, "created_at")?,
    })
}

impl SqliteDontListRepository {
    async fn by_key(&self, profile_id: &str, key: &str) -> Result<Option<DontRule>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM dont_list WHERE profile_id = ? AND rule_key = ?"
        ))
        .bind(profile_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .context("read a don't rule by its key")?;
        row.as_ref().map(rule_of).transpose()
    }
}

#[async_trait]
impl DontListRepository for SqliteDontListRepository {
    async fn list(&self, profile_id: &str, strength: Option<Strength>) -> Result<Vec<DontRule>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM dont_list WHERE profile_id = ? AND (? IS NULL OR strength = ?)"
        ))
        .bind(profile_id)
        .bind(strength.map(Strength::as_str))
        .bind(strength.map(Strength::as_str))
        .fetch_all(&self.pool)
        .await
        .context("list a member's don't rules")?;
        let mut rules = rows.iter().map(rule_of).collect::<Result<Vec<_>>>()?;
        sort_rules(&mut rules);
        Ok(rules)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<DontRule>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM dont_list WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read a don't rule")?;
        row.as_ref().map(rule_of).transpose()
    }

    async fn add(&self, rule: &DontRule) -> Result<DontRule> {
        let key = rule.key();
        match self.by_key(&rule.profile_id, &key).await? {
            Some(held) => {
                let merged = held.merged_with(rule);
                sqlx::query(
                    "UPDATE dont_list SET rule = ?, strength = ?, reasons_json = ?, said = ? \
                     WHERE id = ? AND profile_id = ?",
                )
                .bind(&merged.rule)
                .bind(merged.strength.as_str())
                .bind(json_of(&merged.reasons)?)
                .bind(&merged.said)
                .bind(&merged.id)
                .bind(&merged.profile_id)
                .execute(&self.pool)
                .await
                .context("merge a don't rule into the one held")?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO dont_list (id, profile_id, rule, rule_key, strength, \
                     reasons_json, said, origin, origin_ref, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&rule.id)
                .bind(&rule.profile_id)
                .bind(&rule.rule)
                .bind(&key)
                .bind(rule.strength.as_str())
                .bind(json_of(&rule.reasons)?)
                .bind(&rule.said)
                .bind(rule.origin.as_str())
                .bind(&rule.origin_ref)
                .bind(stamp(rule.created_at))
                .execute(&self.pool)
                .await
                .context("keep a don't rule")?;
            }
        }
        self.by_key(&rule.profile_id, &key)
            .await?
            .context("a don't rule just written is not there")
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM dont_list WHERE profile_id = ? AND id = ?")
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete a don't rule")?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::domain::dont_list::Reason;
    use pond_core::user_data::mocks::mock_dont_list::InMemoryDontListRepository;

    fn never(member: &str, rule: &str, minutes_old: i64) -> DontRule {
        let mut rule = DontRule::new(
            member,
            rule,
            Strength::Never,
            RuleOrigin::You,
            noon() - Duration::minutes(minutes_old),
        )
        .unwrap();
        rule.said = Some("no more of these".into());
        rule
    }

    fn not_this(member: &str, title: &str, minutes_old: i64) -> DontRule {
        DontRule::avoid_title(
            member,
            title,
            &[Reason::TooGeneric],
            RuleOrigin::Reject,
            "d1",
            noon() - Duration::minutes(minutes_old),
        )
        .unwrap()
    }

    fn rules(found: &[DontRule]) -> Vec<&str> {
        found.iter().map(|r| r.rule.as_str()).collect()
    }

    async fn rules_round_trip_never_first_then_newest(repo: &dyn DontListRepository) {
        let old = never("jerry", "Celebrity gossip", 60);
        assert_eq!(repo.add(&old).await.unwrap(), old);
        repo.add(&not_this("jerry", "A duck recipe in teal", 5))
            .await
            .unwrap();
        repo.add(&never("jerry", "Recipes built on a colour", 1))
            .await
            .unwrap();
        let all = repo.list("jerry", None).await.unwrap();
        assert_eq!(
            rules(&all),
            [
                "Recipes built on a colour",
                "Celebrity gossip",
                "A duck recipe in teal"
            ]
        );
        assert_eq!(
            repo.list("jerry", Some(Strength::Avoid))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(repo.get("jerry", &old.id).await.unwrap(), Some(old));
    }

    async fn a_rule_said_twice_is_one_rule_and_stays_a_never(repo: &dyn DontListRepository) {
        let first = repo.add(&never("jerry", "Duck recipes", 9)).await.unwrap();
        let again = repo
            .add(&not_this("jerry", "duck recipes!", 1))
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.strength, Strength::Never);
        assert_eq!(again.reasons, [Reason::TooGeneric]);
        assert_eq!(again.rule, "Duck recipes");
        assert_eq!(repo.list("jerry", None).await.unwrap().len(), 1);
    }

    async fn a_member_never_reads_or_deletes_anothers_rules(repo: &dyn DontListRepository) {
        let mine = repo.add(&never("jerry", "Duck recipes", 1)).await.unwrap();
        repo.add(&never("liz", "Duck recipes", 1)).await.unwrap();
        assert_eq!(repo.list("jerry", None).await.unwrap().len(), 1);
        assert_eq!(repo.get("liz", &mine.id).await.unwrap(), None);
        assert!(!repo.delete("liz", &mine.id).await.unwrap());
        assert!(repo.delete("jerry", &mine.id).await.unwrap());
        assert!(repo.list("jerry", None).await.unwrap().is_empty());
        assert_eq!(repo.list("liz", None).await.unwrap().len(), 1);
    }

    async fn sqlite() -> SqliteDontListRepository {
        SqliteDontListRepository::new(pool().await)
    }

    #[tokio::test]
    async fn dont_rules_round_trip_and_read_never_first_then_newest() {
        rules_round_trip_never_first_then_newest(&sqlite().await).await;
        rules_round_trip_never_first_then_newest(&InMemoryDontListRepository::new()).await;
    }

    #[tokio::test]
    async fn a_dont_rule_said_twice_is_one_rule_and_stays_a_never() {
        a_rule_said_twice_is_one_rule_and_stays_a_never(&sqlite().await).await;
        a_rule_said_twice_is_one_rule_and_stays_a_never(&InMemoryDontListRepository::new()).await;
    }

    #[tokio::test]
    async fn one_member_never_reads_or_deletes_anothers_dont_rules() {
        a_member_never_reads_or_deletes_anothers_rules(&sqlite().await).await;
        a_member_never_reads_or_deletes_anothers_rules(&InMemoryDontListRepository::new()).await;
    }
}
