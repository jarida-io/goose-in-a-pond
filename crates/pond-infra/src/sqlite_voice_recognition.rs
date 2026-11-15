//! Voice Match's prints and the wake CNN's enrolment results, in `pond_system.db`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::voice_print::{VoicePrint, WakeTake};
use pond_core::user_data::ports::voice_print::VoicePrintRepository;
use sqlx::{Pool, Sqlite};

pub struct SqliteVoiceRecognition {
    pool: Pool<Sqlite>,
}

impl SqliteVoiceRecognition {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn parse_stamp(s: &str) -> DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|t| t.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn pack(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn unpack(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(anyhow!("a voice print of {} bytes", bytes.len()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[async_trait]
impl VoicePrintRepository for SqliteVoiceRecognition {
    async fn add_print(&self, print: &VoicePrint) -> Result<()> {
        sqlx::query(
            "INSERT INTO voice_prints (id, profile_id, embedding, model_dims, model_name, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&print.id)
        .bind(&print.profile_id)
        .bind(pack(&print.embedding))
        .bind(print.embedding.len() as i64)
        .bind(&print.model_name)
        .bind(stamp(print.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn prints(&self, model_name: &str) -> Result<Vec<VoicePrint>> {
        let rows: Vec<(String, String, Vec<u8>, String, String)> = sqlx::query_as(
            "SELECT id, profile_id, embedding, model_name, created_at FROM voice_prints \
             WHERE model_name = ? ORDER BY created_at, rowid",
        )
        .bind(model_name)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(id, profile_id, blob, model_name, created_at)| {
                Ok(VoicePrint {
                    id,
                    profile_id,
                    embedding: unpack(&blob)?,
                    model_name,
                    created_at: parse_stamp(&created_at),
                })
            })
            .collect()
    }

    async fn delete_prints(&self, profile_id: &str) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM voice_prints WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    async fn add_wake_take(&self, take: &WakeTake) -> Result<()> {
        sqlx::query(
            "INSERT INTO voice_wake_takes (id, profile_id, phrase, cnn_heard, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&take.id)
        .bind(&take.profile_id)
        .bind(&take.phrase)
        .bind(take.cnn_heard as i64)
        .bind(stamp(take.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn wake_takes(&self, phrase: &str) -> Result<Vec<WakeTake>> {
        let rows: Vec<(String, Option<String>, String, i64, String)> = sqlx::query_as(
            "SELECT id, profile_id, phrase, cnn_heard, created_at FROM voice_wake_takes \
             WHERE phrase = ? ORDER BY created_at, rowid",
        )
        .bind(phrase)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, profile_id, phrase, heard, created_at)| WakeTake {
                id,
                profile_id,
                phrase,
                cnn_heard: heard != 0,
                created_at: parse_stamp(&created_at),
            })
            .collect())
    }

    async fn clear_wake_takes(&self) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM voice_wake_takes")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    async fn delete_wake_takes(&self, profile_id: &str) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM voice_wake_takes WHERE profile_id = ?")
                .bind(profile_id)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    async fn delete_wake_take(&self, id: &str) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM voice_wake_takes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    async fn store() -> (SqliteVoiceRecognition, Database, tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        let profile = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, 'Amina')")
            .bind(&profile)
            .execute(&db.system)
            .await
            .unwrap();
        (
            SqliteVoiceRecognition::new(db.system.clone()),
            db,
            dir,
            profile,
        )
    }

    #[tokio::test]
    async fn a_print_reads_back_and_goes_with_its_member() {
        let (store, db, _dir, amina) = store().await;
        let print = VoicePrint {
            id: "p1".into(),
            profile_id: amina.clone(),
            embedding: vec![0.25, -1.5, 3.0],
            model_name: "resnet34".into(),
            created_at: Utc::now(),
        };
        store.add_print(&print).await.unwrap();
        let read = store.prints("resnet34").await.unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].embedding, print.embedding);
        assert!(store.prints("another-model").await.unwrap().is_empty());
        store
            .add_wake_take(&WakeTake {
                id: "t1".into(),
                profile_id: Some(amina.clone()),
                phrase: "hey goose".into(),
                cnn_heard: true,
                created_at: Utc::now(),
            })
            .await
            .unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = ?")
            .bind(&amina)
            .execute(&db.system)
            .await
            .unwrap();
        assert!(
            store.prints("resnet34").await.unwrap().is_empty(),
            "cascade"
        );
        assert!(store.wake_takes("hey goose").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn takes_are_kept_per_phrase_and_cleared_together() {
        let (store, _db, _dir, amina) = store().await;
        for (i, phrase) in ["hey goose", "hey goose", "goose"].into_iter().enumerate() {
            store
                .add_wake_take(&WakeTake {
                    id: format!("t{i}"),
                    profile_id: (i != 1).then(|| amina.clone()),
                    phrase: phrase.into(),
                    cnn_heard: i == 0,
                    created_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let takes = store.wake_takes("hey goose").await.unwrap();
        assert_eq!(takes.len(), 2);
        assert!(takes[0].cnn_heard && takes[1].profile_id.is_none());
        assert_eq!(store.delete_wake_takes(&amina).await.unwrap(), 2);
        assert_eq!(store.clear_wake_takes().await.unwrap(), 1);
    }
}
