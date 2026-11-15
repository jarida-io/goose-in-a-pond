//! SQLite store for Goose's own lessons and follow-ups (`agent_notes`).

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::shared::domain::agent_note::{AgentNote, NoteKind};
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::{Pool, Sqlite};

pub struct SqliteAgentNotes {
    pool: Pool<Sqlite>,
}

impl SqliteAgentNotes {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

type Row = (String, Option<String>, String, String, String);

const COLUMNS: &str = "id, profile_id, kind, text, created_at";

fn note(row: Row) -> AgentNote {
    let (id, profile_id, kind, text, created_at) = row;
    AgentNote {
        id,
        profile_id,
        kind: NoteKind::parse(&kind),
        text,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    }
}

#[async_trait]
impl AgentNotesRepository for SqliteAgentNotes {
    async fn add(&self, note: AgentNote) -> Result<()> {
        sqlx::query(
            "INSERT INTO agent_notes (id, profile_id, kind, text, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&note.id)
        .bind(&note.profile_id)
        .bind(note.kind.as_str())
        .bind(&note.text)
        .bind(note.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list(&self, scope: &ProfileScope, kind: NoteKind) -> Result<Vec<AgentNote>> {
        let rows: Vec<Row> = match scope {
            ProfileScope::Guest => Vec::new(),
            ProfileScope::Household => {
                sqlx::query_as(&format!(
                    "SELECT {COLUMNS} FROM agent_notes WHERE kind = ? \
                     ORDER BY created_at DESC, rowid DESC"
                ))
                .bind(kind.as_str())
                .fetch_all(&self.pool)
                .await?
            }
            ProfileScope::Owner(id) => {
                sqlx::query_as(&format!(
                    "SELECT {COLUMNS} FROM agent_notes \
                     WHERE kind = ? AND (profile_id = ? OR profile_id IS NULL) \
                     ORDER BY created_at DESC, rowid DESC"
                ))
                .bind(kind.as_str())
                .bind(id)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(note).collect())
    }

    async fn owned(&self, profile_id: Option<&str>, kind: NoteKind) -> Result<Vec<AgentNote>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM agent_notes WHERE kind = ? AND profile_id IS ? \
             ORDER BY created_at DESC, rowid DESC"
        ))
        .bind(kind.as_str())
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(note).collect())
    }

    async fn remove(&self, id: &str, scope: &ProfileScope) -> Result<bool> {
        let removed = match scope {
            ProfileScope::Guest => 0,
            ProfileScope::Household => sqlx::query("DELETE FROM agent_notes WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected(),
            ProfileScope::Owner(owner) => sqlx::query(
                "DELETE FROM agent_notes WHERE id = ? AND (profile_id = ? OR profile_id IS NULL)",
            )
            .bind(id)
            .bind(owner)
            .execute(&self.pool)
            .await?
            .rows_affected(),
        };
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    async fn store() -> (SqliteAgentNotes, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for id in ["liz", "jerry"] {
            sqlx::query("INSERT INTO profiles (id, display_name, avatar_emoji) VALUES (?, ?, ?)")
                .bind(id)
                .bind(id)
                .bind("duck")
                .execute(&db.system)
                .await
                .unwrap();
        }
        (SqliteAgentNotes::new(db.system), tmp)
    }

    fn a_note(id: &str, owner: Option<&str>, kind: NoteKind, minutes_old: i64) -> AgentNote {
        AgentNote {
            id: id.to_string(),
            profile_id: owner.map(str::to_string),
            kind,
            text: format!("note {id}"),
            created_at: Utc::now() - chrono::Duration::minutes(minutes_old),
        }
    }

    fn ids(notes: Vec<AgentNote>) -> Vec<String> {
        notes.into_iter().map(|n| n.id).collect()
    }

    #[tokio::test]
    async fn a_member_reads_their_own_and_the_households_newest_first_and_never_anothers() {
        let (store, _tmp) = store().await;
        for note in [
            a_note("liz-old", Some("liz"), NoteKind::Lesson, 30),
            a_note("liz-new", Some("liz"), NoteKind::Lesson, 1),
            a_note("jerry-1", Some("jerry"), NoteKind::Lesson, 5),
            a_note("house-1", None, NoteKind::Lesson, 10),
            a_note("liz-follow-up", Some("liz"), NoteKind::Note, 2),
        ] {
            store.add(note).await.unwrap();
        }
        let liz = ProfileScope::Owner("liz".into());
        assert_eq!(
            ids(store.list(&liz, NoteKind::Lesson).await.unwrap()),
            ["liz-new", "house-1", "liz-old"]
        );
        assert_eq!(
            ids(store.list(&liz, NoteKind::Note).await.unwrap()),
            ["liz-follow-up"]
        );
        assert_eq!(
            store
                .list(&ProfileScope::Household, NoteKind::Lesson)
                .await
                .unwrap()
                .len(),
            4
        );
        assert!(store
            .list(&ProfileScope::Guest, NoteKind::Lesson)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            ids(store.owned(None, NoteKind::Lesson).await.unwrap()),
            ["house-1"]
        );
        assert_eq!(
            ids(store.owned(Some("liz"), NoteKind::Lesson).await.unwrap()),
            ["liz-new", "liz-old"]
        );
    }

    #[tokio::test]
    async fn a_note_is_removed_only_by_someone_it_reaches() {
        let (store, _tmp) = store().await;
        store
            .add(a_note("liz-1", Some("liz"), NoteKind::Note, 1))
            .await
            .unwrap();
        let jerry = ProfileScope::Owner("jerry".into());
        assert!(!store.remove("liz-1", &jerry).await.unwrap());
        assert!(!store.remove("liz-1", &ProfileScope::Guest).await.unwrap());
        assert!(store
            .remove("liz-1", &ProfileScope::Owner("liz".into()))
            .await
            .unwrap());
        assert!(!store
            .remove("liz-1", &ProfileScope::Household)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn a_members_notes_go_with_the_member() {
        let (store, _tmp) = store().await;
        store
            .add(a_note("liz-1", Some("liz"), NoteKind::Lesson, 1))
            .await
            .unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(store
            .list(&ProfileScope::Household, NoteKind::Lesson)
            .await
            .unwrap()
            .is_empty());
    }
}
