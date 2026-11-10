//! SQLite-backed `SavedItemRepository`. Fields and sources are JSON.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::shared::domain::saved_item::{collection_name, SavedFields, SavedItem};
use pond_core::shared::ports::saved_item::{SavedEdit, SavedItemRepository};

pub struct SqliteSavedItemRepository {
    pool: Pool<Sqlite>,
}

impl SqliteSavedItemRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, kind, title, fields, body, collection, shared, sources, \
     session_id, created_at, updated_at";

/// Rows a member can see: their own, and the household's shared ones.
const VISIBLE: &str = "(profile_id = ? OR shared = 1)";

fn time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("bad saved item timestamp {raw:?}"))
}

fn item_of(row: &SqliteRow) -> Result<SavedItem> {
    let fields: SavedFields =
        serde_json::from_str(&row.try_get::<String, _>("fields")?).unwrap_or(SavedFields::Note);
    Ok(SavedItem {
        id: row.try_get("id")?,
        owner: row.try_get("profile_id")?,
        title: row.try_get("title")?,
        fields,
        body: row.try_get("body")?,
        collection: row.try_get("collection")?,
        shared: row.try_get::<i64, _>("shared")? != 0,
        sources: serde_json::from_str(&row.try_get::<String, _>("sources")?).unwrap_or_default(),
        session_id: row.try_get("session_id")?,
        created_at: time(&row.try_get::<String, _>("created_at")?)?,
        updated_at: time(&row.try_get::<String, _>("updated_at")?)?,
    })
}

/// `LIKE` with its own wildcards taken literally.
fn like(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

impl SqliteSavedItemRepository {
    /// The member's spelling of a collection they already have, so "cars" joins "Cars".
    async fn existing_collection(&self, member: &str, name: &str) -> Result<String> {
        let found: Option<(String,)> = sqlx::query_as(&format!(
            "SELECT collection FROM saved_items WHERE {VISIBLE} AND collection = ? COLLATE NOCASE \
             ORDER BY created_at LIMIT 1"
        ))
        .bind(member)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(found.map(|(c,)| c).unwrap_or_else(|| name.to_string()))
    }
}

#[async_trait]
impl SavedItemRepository for SqliteSavedItemRepository {
    async fn create(&self, item: &SavedItem) -> Result<()> {
        let collection = match &item.collection {
            Some(name) => Some(self.existing_collection(&item.owner, name).await?),
            None => None,
        };
        sqlx::query(&format!(
            "INSERT INTO saved_items ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(&item.id)
        .bind(&item.owner)
        .bind(item.fields.kind())
        .bind(&item.title)
        .bind(serde_json::to_string(&item.fields)?)
        .bind(&item.body)
        .bind(collection)
        .bind(i64::from(item.shared))
        .bind(serde_json::to_string(&item.sources)?)
        .bind(&item.session_id)
        .bind(item.created_at.to_rfc3339())
        .bind(item.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<SavedItem>> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM saved_items WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(item_of).transpose()
    }

    async fn list_visible(
        &self,
        member: &str,
        collection: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<SavedItem>> {
        let mut sql = format!("SELECT {COLUMNS} FROM saved_items WHERE {VISIBLE}");
        if collection.is_some() {
            sql.push_str(" AND collection = ? COLLATE NOCASE");
        }
        if kind.is_some() {
            sql.push_str(" AND kind = ?");
        }
        sql.push_str(" ORDER BY updated_at DESC");
        let mut query = sqlx::query(&sql).bind(member);
        if let Some(collection) = collection {
            query = query.bind(collection);
        }
        if let Some(kind) = kind {
            query = query.bind(kind);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(item_of).collect()
    }

    async fn search(&self, member: &str, query: &str, limit: usize) -> Result<Vec<SavedItem>> {
        let words: Vec<&str> = query.split_whitespace().take(8).collect();
        let mut sql = format!("SELECT {COLUMNS} FROM saved_items WHERE {VISIBLE}");
        for _ in &words {
            sql.push_str(
                " AND (title LIKE ? ESCAPE '\\' OR body LIKE ? ESCAPE '\\' \
                 OR fields LIKE ? ESCAPE '\\' OR collection LIKE ? ESCAPE '\\')",
            );
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ?");
        let mut q = sqlx::query(&sql).bind(member);
        for word in &words {
            let pattern = like(word);
            q = q
                .bind(pattern.clone())
                .bind(pattern.clone())
                .bind(pattern.clone())
                .bind(pattern);
        }
        let rows = q.bind(limit as i64).fetch_all(&self.pool).await?;
        rows.iter().map(item_of).collect()
    }

    async fn count_for(&self, owner: &str) -> Result<usize> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM saved_items WHERE profile_id = ?")
            .bind(owner)
            .fetch_one(&self.pool)
            .await?;
        Ok(n.max(0) as usize)
    }

    async fn collections(&self, member: &str) -> Result<Vec<(String, usize)>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(&format!(
            "SELECT MIN(collection), COUNT(*) FROM saved_items WHERE {VISIBLE} \
             AND collection IS NOT NULL GROUP BY collection COLLATE NOCASE \
             ORDER BY MIN(collection) COLLATE NOCASE"
        ))
        .bind(member)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(c, n)| (c, n.max(0) as usize))
            .collect())
    }

    async fn edit(&self, owner: &str, id: &str, edit: &SavedEdit) -> Result<bool> {
        let Some(mut item) = self.get(id).await?.filter(|i| i.owner == owner) else {
            return Ok(false);
        };
        if let Some(title) = edit
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            item.title = title
                .chars()
                .take(pond_core::shared::domain::saved_item::MAX_TITLE_CHARS)
                .collect();
        }
        if let Some(body) = &edit.body {
            item.body = body
                .chars()
                .take(pond_core::shared::domain::saved_item::MAX_BODY_CHARS)
                .collect();
        }
        if let Some(collection) = &edit.collection {
            item.collection = match collection_name(collection.as_deref()) {
                Some(name) => Some(self.existing_collection(owner, &name).await?),
                None => None,
            };
        }
        if let Some(shared) = edit.shared {
            item.shared = shared;
        }
        let done = sqlx::query(
            "UPDATE saved_items SET title = ?, body = ?, collection = ?, shared = ?, updated_at = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(&item.title)
        .bind(&item.body)
        .bind(&item.collection)
        .bind(i64::from(item.shared))
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete(&self, owner: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM saved_items WHERE id = ? AND profile_id = ?")
            .bind(id)
            .bind(owner)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    async fn repo() -> (SqliteSavedItemRepository, tempfile::TempDir) {
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
        (SqliteSavedItemRepository::new(db.system.clone()), tmp)
    }

    fn card(id: &str, owner: &str, title: &str, collection: Option<&str>, body: &str) -> SavedItem {
        SavedItem::new(
            id.into(),
            owner.into(),
            title,
            SavedFields::Recipe {
                servings: Some("4".into()),
                time: None,
                ingredients: vec!["rice".into()],
                steps: vec![],
            },
            body,
            collection,
            vec![],
            None,
            Utc::now(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_member_sees_their_own_and_shared_cards_and_changes_only_their_own() {
        let (repo, _tmp) = repo().await;
        repo.create(&card("a1", "amina", "Pilau", Some("Dinners"), "with beef"))
            .await
            .unwrap();
        repo.create(&card("b1", "baraka", "Chapati", Some("dinners"), "soft"))
            .await
            .unwrap();
        assert_eq!(
            repo.list_visible("amina", None, None).await.unwrap().len(),
            1
        );
        assert!(!repo
            .edit(
                "amina",
                "b1",
                &SavedEdit {
                    shared: Some(true),
                    ..Default::default()
                }
            )
            .await
            .unwrap());
        assert!(repo
            .edit(
                "baraka",
                "b1",
                &SavedEdit {
                    shared: Some(true),
                    ..Default::default()
                }
            )
            .await
            .unwrap());
        let seen = repo.list_visible("amina", None, None).await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            repo.list_visible("amina", Some("DINNERS"), Some("recipe"))
                .await
                .unwrap()
                .len(),
            2
        );
        let got = repo.get("a1").await.unwrap().unwrap();
        assert_eq!(got.fields.kind(), "recipe");
        assert!(!repo.delete("amina", "b1").await.unwrap());
        assert!(repo.delete("baraka", "b1").await.unwrap());
    }

    #[tokio::test]
    async fn collections_join_by_name_without_case_and_search_needs_every_word() {
        let (repo, _tmp) = repo().await;
        repo.create(&card(
            "a1",
            "amina",
            "Pilau",
            Some("Dinners"),
            "beef and spices",
        ))
        .await
        .unwrap();
        repo.create(&card(
            "a2",
            "amina",
            "Chapati",
            Some("dinners"),
            "flour 100%",
        ))
        .await
        .unwrap();
        let a2 = repo.get("a2").await.unwrap().unwrap();
        assert_eq!(
            a2.collection.as_deref(),
            Some("Dinners"),
            "joins the existing spelling"
        );
        assert_eq!(
            repo.collections("amina").await.unwrap(),
            vec![("Dinners".to_string(), 2)]
        );
        let found = repo.search("amina", "beef spices", 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "a1");
        assert!(repo
            .search("amina", "beef flour", 5)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo.search("amina", "100%", 5).await.unwrap().len(),
            1,
            "% is literal"
        );
        assert!(
            repo.search("baraka", "beef", 5).await.unwrap().is_empty(),
            "not shared"
        );
        assert_eq!(repo.count_for("amina").await.unwrap(), 2);
        repo.edit(
            "amina",
            "a2",
            &SavedEdit {
                collection: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(repo.get("a2").await.unwrap().unwrap().collection.is_none());
    }
}
