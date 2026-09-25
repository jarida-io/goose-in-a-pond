//! SQLite-backed implementation of the ModelRepository port.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Sqlite};

use pond_core::models::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};
use pond_core::models::ports::model_repository::ModelRepository;

// ── Repo struct ───────────────────────────────────────────────────────────────

pub struct SqliteModelRepository {
    pool: Pool<Sqlite>,
}

impl SqliteModelRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

// ── Row → domain helpers ──────────────────────────────────────────────────────

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<ModelRecord> {
    use sqlx::Row;
    let category_str: String = row.try_get("category")?;
    let category = ModelCategory::from_str(&category_str).unwrap_or(ModelCategory::Gguf); // fallback; shouldn't happen for clean data

    Ok(ModelRecord {
        id: row.try_get("id")?,
        category,
        name: row.try_get("name")?,
        filename: row.try_get("filename")?,
        description: row.try_get("description")?,
        size_mb: row.try_get::<i64, _>("size_mb")? as u64,
        url: row.try_get("url")?,
        hf_id: row.try_get("hf_id")?,
        ram_estimate_mb: row
            .try_get::<Option<i64>, _>("ram_estimate_mb")?
            .map(|v| v as u64),
        recommended_role: row.try_get("recommended_role")?,
        context_length: row
            .try_get::<Option<i64>, _>("context_length")?
            .map(|v| v as u32),
        quantization: row.try_get("quantization")?,
        asr_language: row.try_get("asr_language")?,
        asr_size: row.try_get("asr_size")?,
        tts_engine: row.try_get("tts_engine")?,
        tts_voice_name: row.try_get("tts_voice_name")?,
        config_filename: row.try_get("config_filename")?,
        config_url: row.try_get("config_url")?,
        tts_url: row.try_get("tts_url")?,
        sample_rate: row
            .try_get::<Option<i64>, _>("sample_rate")?
            .map(|v| v as u32),
        downloaded: row.try_get::<i64, _>("downloaded")? != 0,
        is_custom: row.try_get::<i64, _>("is_custom")? != 0,
    })
}

// ── ModelRepository impl ──────────────────────────────────────────────────────

#[async_trait]
impl ModelRepository for SqliteModelRepository {
    async fn list_all(&self) -> Result<Vec<ModelRecord>> {
        let rows = sqlx::query("SELECT * FROM models ORDER BY category, name")
            .fetch_all(&self.pool)
            .await?;

        rows.iter().map(row_to_record).collect()
    }

    async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>> {
        let rows = sqlx::query("SELECT * FROM models WHERE category = ? ORDER BY name")
            .bind(category.as_str())
            .fetch_all(&self.pool)
            .await?;

        rows.iter().map(row_to_record).collect()
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>> {
        let row = sqlx::query("SELECT * FROM models WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        row.as_ref().map(row_to_record).transpose()
    }

    async fn upsert(&self, m: &ModelRecord) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO models (
                id, category, name, filename, description, size_mb,
                url, hf_id,
                ram_estimate_mb, recommended_role, context_length, quantization,
                asr_language, asr_size,
                tts_engine, tts_voice_name, config_filename, config_url, tts_url, sample_rate,
                downloaded, is_custom, updated_at
            ) VALUES (
                ?, ?, ?, ?, ?, ?,
                ?, ?,
                ?, ?, ?, ?,
                ?, ?,
                ?, ?, ?, ?, ?, ?,
                ?, ?, datetime('now')
            )
            ON CONFLICT(id) DO UPDATE SET
                category         = excluded.category,
                name             = excluded.name,
                filename         = excluded.filename,
                description      = excluded.description,
                size_mb          = excluded.size_mb,
                url              = excluded.url,
                hf_id            = excluded.hf_id,
                ram_estimate_mb  = excluded.ram_estimate_mb,
                recommended_role = excluded.recommended_role,
                context_length   = excluded.context_length,
                quantization     = excluded.quantization,
                asr_language     = excluded.asr_language,
                asr_size         = excluded.asr_size,
                tts_engine       = excluded.tts_engine,
                tts_voice_name   = excluded.tts_voice_name,
                config_filename  = excluded.config_filename,
                config_url       = excluded.config_url,
                tts_url          = excluded.tts_url,
                sample_rate      = excluded.sample_rate,
                downloaded       = excluded.downloaded,
                -- never overwrite is_custom=1 rows with is_custom=0 from registry
                is_custom        = MAX(models.is_custom, excluded.is_custom),
                updated_at       = datetime('now')
            "#,
        )
        .bind(&m.id)
        .bind(m.category.as_str())
        .bind(&m.name)
        .bind(&m.filename)
        .bind(&m.description)
        .bind(m.size_mb as i64)
        .bind(&m.url)
        .bind(&m.hf_id)
        .bind(m.ram_estimate_mb.map(|v| v as i64))
        .bind(&m.recommended_role)
        .bind(m.context_length.map(|v| v as i64))
        .bind(&m.quantization)
        .bind(&m.asr_language)
        .bind(&m.asr_size)
        .bind(&m.tts_engine)
        .bind(&m.tts_voice_name)
        .bind(&m.config_filename)
        .bind(&m.config_url)
        .bind(&m.tts_url)
        .bind(m.sample_rate.map(|v| v as i64))
        .bind(m.downloaded as i64)
        .bind(m.is_custom as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()> {
        sqlx::query("UPDATE models SET downloaded = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(downloaded as i64)
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    // ── Role assignments ──────────────────────────────────────────────────────

    async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
        use sqlx::Row;
        let rows = sqlx::query("SELECT role, model_id FROM model_role_assignments ORDER BY role")
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .iter()
            .map(|r| ModelRoleAssignment {
                role: r.get("role"),
                model_id: r.get("model_id"),
            })
            .collect())
    }

    async fn get_assignment(&self, role: &str) -> Result<Option<ModelRoleAssignment>> {
        use sqlx::Row;
        let row = sqlx::query("SELECT role, model_id FROM model_role_assignments WHERE role = ?")
            .bind(role)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| ModelRoleAssignment {
            role: r.get("role"),
            model_id: r.get("model_id"),
        }))
    }

    async fn set_assignment(&self, role: &str, model_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO model_role_assignments (role, model_id, updated_at)
            VALUES (?, ?, datetime('now'))
            ON CONFLICT(role) DO UPDATE SET
                model_id   = excluded.model_id,
                updated_at = datetime('now')
            "#,
        )
        .bind(role)
        .bind(model_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn clear_assignment(&self, role: &str) -> Result<()> {
        sqlx::query("DELETE FROM model_role_assignments WHERE role = ?")
            .bind(role)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::TempDir;

    async fn make_repo() -> (SqliteModelRepository, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        let repo = SqliteModelRepository::new(db.system.clone());
        (repo, dir)
    }

    fn gguf_record(name: &str) -> ModelRecord {
        ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::Gguf, name),
            category: ModelCategory::Gguf,
            name: name.to_string(),
            filename: Some(format!("{name}.gguf")),
            description: format!("{name} model"),
            size_mb: 2000,
            url: Some("https://example.com/model.gguf".to_string()),
            hf_id: Some("owner/repo:Q4_K_M".to_string()),
            ram_estimate_mb: Some(2500),
            recommended_role: Some("chat".to_string()),
            context_length: Some(4096),
            quantization: Some("Q4_K_M".to_string()),
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        }
    }

    fn whisper_record(size: &str) -> ModelRecord {
        ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::Whisper, size),
            category: ModelCategory::Whisper,
            name: size.to_string(),
            filename: Some(format!("ggml-{size}.en.bin")),
            description: format!("Whisper {size}"),
            size_mb: 74,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: Some("en".to_string()),
            asr_size: Some(size.to_string()),
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        }
    }

    #[tokio::test]
    async fn upsert_and_get_by_id() {
        let (repo, _dir) = make_repo().await;
        let m = gguf_record("llama-3b");

        repo.upsert(&m).await.unwrap();
        let fetched = repo.get_by_id("gguf/llama-3b").await.unwrap().unwrap();

        assert_eq!(fetched.name, "llama-3b");
        assert_eq!(fetched.category, ModelCategory::Gguf);
        assert_eq!(fetched.ram_estimate_mb, Some(2500));
        assert_eq!(fetched.quantization, Some("Q4_K_M".to_string()));
        assert!(!fetched.downloaded);
        assert!(!fetched.is_custom);
    }

    #[tokio::test]
    async fn set_downloaded_updates_flag() {
        let (repo, _dir) = make_repo().await;
        let m = gguf_record("gemma-2b");
        repo.upsert(&m).await.unwrap();

        repo.set_downloaded("gguf/gemma-2b", true).await.unwrap();
        let fetched = repo.get_by_id("gguf/gemma-2b").await.unwrap().unwrap();
        assert!(fetched.downloaded);

        repo.set_downloaded("gguf/gemma-2b", false).await.unwrap();
        let fetched = repo.get_by_id("gguf/gemma-2b").await.unwrap().unwrap();
        assert!(!fetched.downloaded);
    }

    #[tokio::test]
    async fn list_by_category_filters_correctly() {
        let (repo, _dir) = make_repo().await;
        repo.upsert(&gguf_record("llama-3b")).await.unwrap();
        repo.upsert(&gguf_record("gemma-2b")).await.unwrap();
        repo.upsert(&whisper_record("base")).await.unwrap();

        let gguf_list = repo.list_by_category(&ModelCategory::Gguf).await.unwrap();
        assert_eq!(gguf_list.len(), 2);
        assert!(gguf_list.iter().all(|m| m.category == ModelCategory::Gguf));

        let whisper_list = repo
            .list_by_category(&ModelCategory::Whisper)
            .await
            .unwrap();
        assert_eq!(whisper_list.len(), 1);
        assert_eq!(whisper_list[0].asr_language, Some("en".to_string()));
        assert_eq!(whisper_list[0].asr_size, Some("base".to_string()));
    }

    #[tokio::test]
    async fn list_all_returns_all_categories() {
        let (repo, _dir) = make_repo().await;
        repo.upsert(&gguf_record("llama-3b")).await.unwrap();
        repo.upsert(&whisper_record("tiny")).await.unwrap();

        let all = repo.list_all().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn is_custom_preserved_on_upsert() {
        let (repo, _dir) = make_repo().await;
        let mut m = gguf_record("custom-model");
        m.is_custom = true;
        repo.upsert(&m).await.unwrap();

        // Upsert again with is_custom=false (simulating registry re-seed)
        let m2 = gguf_record("custom-model");
        assert!(!m2.is_custom);
        repo.upsert(&m2).await.unwrap();

        let fetched = repo.get_by_id("gguf/custom-model").await.unwrap().unwrap();
        assert!(fetched.is_custom, "is_custom should be preserved");
    }

    #[tokio::test]
    async fn set_and_get_assignment() {
        let (repo, _dir) = make_repo().await;
        repo.upsert(&gguf_record("llama-3b")).await.unwrap();

        repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();
        let a = repo.get_assignment("chat").await.unwrap().unwrap();
        assert_eq!(a.role, "chat");
        assert_eq!(a.model_id, "gguf/llama-3b");
    }

    #[tokio::test]
    async fn clear_assignment() {
        let (repo, _dir) = make_repo().await;
        repo.upsert(&gguf_record("llama-3b")).await.unwrap();
        repo.set_assignment("think", "gguf/llama-3b").await.unwrap();

        repo.clear_assignment("think").await.unwrap();
        let a = repo.get_assignment("think").await.unwrap();
        assert!(a.is_none());
    }

    #[tokio::test]
    async fn set_assignment_updates_existing() {
        let (repo, _dir) = make_repo().await;
        repo.upsert(&gguf_record("llama-3b")).await.unwrap();
        repo.upsert(&gguf_record("gemma-2b")).await.unwrap();

        repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();
        repo.set_assignment("chat", "gguf/gemma-2b").await.unwrap();

        let a = repo.get_assignment("chat").await.unwrap().unwrap();
        assert_eq!(a.model_id, "gguf/gemma-2b");
    }

    #[tokio::test]
    async fn list_assignments_returns_all() {
        let (repo, _dir) = make_repo().await;
        repo.upsert(&gguf_record("llama-3b")).await.unwrap();
        repo.upsert(&whisper_record("base")).await.unwrap();

        repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();
        repo.set_assignment("asr", "whisper/base").await.unwrap();

        let all = repo.list_assignments().await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|a| a.role == "chat"));
        assert!(all.iter().any(|a| a.role == "asr"));
    }
}
