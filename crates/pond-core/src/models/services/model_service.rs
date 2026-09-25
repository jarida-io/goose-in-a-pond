//! Model lifecycle: catalog, downloads, role assignment. HTTP, SQLite and file paths live in
//! the port implementations wired in `pond-server`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::models::domain::model_record::{
    BinaryRecord, ModelCategory, ModelRecord, ModelRoleAssignment,
};
use crate::models::ports::model_catalog_provider::ModelCatalogProvider;
use crate::models::ports::model_downloader::ModelDownloader;
use crate::models::ports::model_repository::ModelRepository;
use crate::models::ports::model_storage::ModelStorage;

pub struct ModelService {
    repo: Arc<dyn ModelRepository>,
    catalog: Arc<dyn ModelCatalogProvider>,
    downloader: Arc<dyn ModelDownloader>,
    storage: Arc<dyn ModelStorage>,
}

impl ModelService {
    pub fn new(
        repo: Arc<dyn ModelRepository>,
        catalog: Arc<dyn ModelCatalogProvider>,
        downloader: Arc<dyn ModelDownloader>,
        storage: Arc<dyn ModelStorage>,
    ) -> Self {
        Self {
            repo,
            catalog,
            downloader,
            storage,
        }
    }

    // ── Catalog ───────────────────────────────────────────────────────────────

    /// Upsert the fetched catalog on first run; `ModelRepository::upsert` keeps `is_custom` set.
    pub async fn seed_catalog(&self) -> Result<usize> {
        let (models, _binaries) = self.catalog.fetch().await?;
        let count = models.len();
        for mut record in models {
            record.downloaded = self.storage.is_present(&record);
            self.repo.upsert(&record).await?;
        }
        Ok(count)
    }

    /// Re-run `seed_catalog`; safe to repeat.
    pub async fn refresh_catalog(&self) -> Result<usize> {
        self.seed_catalog().await
    }

    /// Fetch tool binaries without touching the model catalog.
    pub async fn fetch_binaries(&self) -> Result<Vec<BinaryRecord>> {
        let (_models, binaries) = self.catalog.fetch().await?;
        Ok(binaries)
    }

    // ── Disk sync ─────────────────────────────────────────────────────────────

    /// Refresh `downloaded` flags from disk on every later startup; returns how many changed.
    pub async fn sync_disk_flags(&self) -> Result<usize> {
        let records = self.repo.list_all().await?;
        let mut changed = 0usize;
        for record in &records {
            let on_disk = self.storage.is_present(record);
            if on_disk != record.downloaded {
                self.repo.set_downloaded(&record.id, on_disk).await?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    // ── Role assignments ──────────────────────────────────────────────────────

    pub async fn model_for_role(&self, role: &str) -> Result<Option<ModelRecord>> {
        let assignment = self.repo.get_assignment(role).await?;
        match assignment {
            None => Ok(None),
            Some(a) => self.repo.get_by_id(&a.model_id).await,
        }
    }

    pub async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
        self.repo.list_assignments().await
    }

    /// Assign `model_id` to `role` if its category suits the role.
    pub async fn assign_role(&self, role: &str, model_id: &str) -> Result<()> {
        let record = self
            .repo
            .get_by_id(model_id)
            .await?
            .ok_or_else(|| anyhow!("Model '{}' not found in catalog", model_id))?;

        if !ModelRoleAssignment::category_matches_role(&record.category, role) {
            return Err(anyhow!(
                "Model category '{}' is not valid for role '{}'",
                record.category.as_str(),
                role
            ));
        }

        self.repo.set_assignment(role, model_id).await
    }

    /// Remove the assignment for `role` (no-op if unset).
    pub async fn clear_role(&self, role: &str) -> Result<()> {
        self.repo.clear_assignment(role).await
    }

    // ── Download ──────────────────────────────────────────────────────────────

    /// Path of `model_id`'s file, downloading it from the record's `url` if missing.
    pub async fn ensure_downloaded(&self, model_id: &str) -> Result<PathBuf> {
        let record = self
            .repo
            .get_by_id(model_id)
            .await?
            .ok_or_else(|| anyhow!("Model '{}' not found in catalog", model_id))?;

        let path = self.storage.path_for(&record).ok_or_else(|| {
            anyhow!(
                "Model '{}' has no local file path (it is server-side only)",
                model_id
            )
        })?;

        if path.exists() {
            return Ok(path);
        }

        let url = record
            .url
            .as_deref()
            .ok_or_else(|| anyhow!("Model '{}' has no download URL in the catalog", model_id))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.downloader.download(url, &path, record.size_mb).await?;
        self.repo.set_downloaded(model_id, true).await?;

        Ok(path)
    }

    /// Ensure a tool binary is on disk. Returns its path.
    pub async fn ensure_binary_downloaded(&self, record: &BinaryRecord) -> Result<PathBuf> {
        let path = self.storage.binary_path(record);
        if path.exists() {
            return Ok(path);
        }

        let url = record.url_for_current_platform().ok_or_else(|| {
            anyhow!(
                "Binary '{}' has no download URL for platform '{}'",
                record.name,
                BinaryRecord::current_platform_key()
            )
        })?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.downloader.download(url, &path, 0).await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            std::fs::set_permissions(&path, perms)?;
        }

        Ok(path)
    }

    // ── Listing ───────────────────────────────────────────────────────────────

    pub async fn list_all(&self) -> Result<Vec<ModelRecord>> {
        self.repo.list_all().await
    }

    pub async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>> {
        self.repo.list_by_category(category).await
    }

    /// Look up a model by its stable id (`"{category}/{name}"`).
    pub async fn get(&self, model_id: &str) -> Result<Option<ModelRecord>> {
        self.repo.get_by_id(model_id).await
    }

    /// Register a user-added model (sets `is_custom = true`).
    pub async fn add_custom(&self, mut record: ModelRecord) -> Result<()> {
        record.is_custom = true;
        record.downloaded = self.storage.is_present(&record);
        self.repo.upsert(&record).await
    }

    /// Mark a model as downloaded (or not). Used by download progress handlers.
    pub async fn set_downloaded(&self, model_id: &str, downloaded: bool) -> Result<()> {
        self.repo.set_downloaded(model_id, downloaded).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── Mock ModelRepository ──────────────────────────────────────────────

    struct MockModelRepository {
        records: Mutex<HashMap<String, ModelRecord>>,
    }

    impl MockModelRepository {
        fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl ModelRepository for MockModelRepository {
        async fn list_all(&self) -> Result<Vec<ModelRecord>> {
            let mut v: Vec<_> = self.records.lock().unwrap().values().cloned().collect();
            v.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(v)
        }
        async fn list_by_category(&self, cat: &ModelCategory) -> Result<Vec<ModelRecord>> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .values()
                .filter(|r| &r.category == cat)
                .cloned()
                .collect())
        }
        async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>> {
            Ok(self.records.lock().unwrap().get(id).cloned())
        }
        async fn upsert(&self, model: &ModelRecord) -> Result<()> {
            self.records
                .lock()
                .unwrap()
                .insert(model.id.clone(), model.clone());
            Ok(())
        }
        async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()> {
            if let Some(r) = self.records.lock().unwrap().get_mut(id) {
                r.downloaded = downloaded;
            }
            Ok(())
        }
        async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
            Ok(vec![])
        }
        async fn get_assignment(&self, _: &str) -> Result<Option<ModelRoleAssignment>> {
            Ok(None)
        }
        async fn set_assignment(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn clear_assignment(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    // ── Mock ModelCatalogProvider ─────────────────────────────────────────

    struct MockCatalog;
    #[async_trait]
    impl ModelCatalogProvider for MockCatalog {
        async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
            Ok((vec![], vec![]))
        }
    }

    // ── Mock ModelDownloader ──────────────────────────────────────────────

    struct MockDownloader {
        called_urls: Mutex<Vec<String>>,
    }
    impl MockDownloader {
        fn new() -> Self {
            Self {
                called_urls: Mutex::new(vec![]),
            }
        }
    }
    #[async_trait]
    impl ModelDownloader for MockDownloader {
        async fn download(&self, url: &str, _dest: &std::path::Path, _size: u64) -> Result<()> {
            self.called_urls.lock().unwrap().push(url.to_string());
            Ok(())
        }
    }

    // ── Mock ModelStorage (uses a real tempdir) ──────────────────────────

    struct MockStorage {
        base: PathBuf,
    }
    impl MockStorage {
        fn new(base: PathBuf) -> Self {
            Self { base }
        }
    }
    impl ModelStorage for MockStorage {
        fn path_for(&self, record: &ModelRecord) -> Option<PathBuf> {
            record.filename.as_ref().map(|f| self.base.join(f))
        }
        fn binary_path(&self, _record: &BinaryRecord) -> PathBuf {
            self.base.join("bin")
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn stub_model(id: &str, filename: &str, downloaded: bool, url: Option<&str>) -> ModelRecord {
        ModelRecord {
            id: id.to_string(),
            category: ModelCategory::Llamafile,
            name: id.to_string(),
            filename: Some(filename.to_string()),
            description: String::new(),
            size_mb: 100,
            url: url.map(|s| s.to_string()),
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded,
            is_custom: false,
        }
    }

    fn make_service(
        repo: Arc<MockModelRepository>,
        dl: Arc<MockDownloader>,
        base_dir: &std::path::Path,
    ) -> ModelService {
        ModelService::new(
            repo,
            Arc::new(MockCatalog),
            dl,
            Arc::new(MockStorage::new(base_dir.to_path_buf())),
        )
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ensure_downloaded_triggers_download_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl.clone(), tmp.path());

        // Seed a model with a URL, not yet on disk
        let m = stub_model(
            "llamafile/qwen",
            "qwen.llamafile",
            false,
            Some("https://example.com/qwen.llamafile"),
        );
        repo.upsert(&m).await.unwrap();

        let path = svc.ensure_downloaded("llamafile/qwen").await.unwrap();
        assert_eq!(path, tmp.path().join("qwen.llamafile"));

        let urls = dl.called_urls.lock().unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://example.com/qwen.llamafile");

        let record = repo.get_by_id("llamafile/qwen").await.unwrap().unwrap();
        assert!(
            record.downloaded,
            "downloaded flag should be true after download"
        );
    }

    #[tokio::test]
    async fn ensure_downloaded_skips_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl.clone(), tmp.path());

        let model_path = tmp.path().join("existing.llamafile");
        std::fs::File::create(&model_path).unwrap();

        let m = stub_model(
            "llamafile/existing",
            "existing.llamafile",
            true,
            Some("https://example.com/existing.llamafile"),
        );
        repo.upsert(&m).await.unwrap();

        let path = svc.ensure_downloaded("llamafile/existing").await.unwrap();
        assert_eq!(path, model_path);

        let urls = dl.called_urls.lock().unwrap();
        assert!(
            urls.is_empty(),
            "downloader should not have been called, but got: {:?}",
            *urls
        );
    }

    #[tokio::test]
    async fn ensure_downloaded_errors_when_model_not_in_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo, dl, tmp.path());

        let result = svc.ensure_downloaded("gguf/nonexistent").await;
        assert!(result.is_err(), "should error when model id is unknown");
    }

    #[tokio::test]
    async fn ensure_downloaded_errors_when_no_url() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl, tmp.path());

        // Model with no URL and file not present
        let m = stub_model("llamafile/no-url", "no-url.llamafile", false, None);
        repo.upsert(&m).await.unwrap();

        let result = svc.ensure_downloaded("llamafile/no-url").await;
        assert!(
            result.is_err(),
            "should error when model has no download URL"
        );
    }

    /// The shared mocks used by integration tests work with `ModelService`.
    mod shared_mocks {
        use crate::models::domain::model_record::{ModelCategory, ModelRecord};
        use crate::models::mocks::mock_model_catalog_provider::MockModelCatalogProvider;
        use crate::models::mocks::mock_model_downloader::MockModelDownloader;
        use crate::models::mocks::mock_model_repository::MockModelRepository as SharedMockRepo;
        use crate::models::mocks::mock_model_storage::MockModelStorage;
        use crate::models::ports::{
            model_repository::ModelRepository, model_storage::ModelStorage,
        };
        use crate::models::services::model_service::ModelService;
        use std::sync::Arc;

        fn gguf(name: &str, downloaded: bool, url: Option<&str>) -> ModelRecord {
            ModelRecord {
                id: format!("gguf/{name}"),
                category: ModelCategory::Gguf,
                name: name.to_string(),
                filename: Some(format!("{name}.gguf")),
                description: String::new(),
                size_mb: 10,
                url: url.map(str::to_string),
                hf_id: None,
                ram_estimate_mb: None,
                recommended_role: None,
                context_length: None,
                quantization: None,
                asr_language: None,
                asr_size: None,
                tts_engine: None,
                tts_voice_name: None,
                config_filename: None,
                config_url: None,
                tts_url: None,
                sample_rate: None,
                downloaded,
                is_custom: false,
            }
        }

        #[tokio::test]
        async fn shared_never_present_forces_download() {
            let tmp = tempfile::tempdir().unwrap();
            let repo = Arc::new(SharedMockRepo::new());
            let dl = Arc::new(MockModelDownloader::new(true)); // write placeholder
            let storage: Arc<dyn ModelStorage> = Arc::new(MockModelStorage::file_system_backed(
                tmp.path().to_path_buf(),
            ));
            let svc = ModelService::new(
                repo.clone() as Arc<dyn ModelRepository>,
                Arc::new(MockModelCatalogProvider::default()),
                dl.clone(),
                storage,
            );

            let m = gguf("llama3", false, Some("https://example.com/llama3.gguf"));
            repo.upsert(&m).await.unwrap();

            svc.ensure_downloaded("gguf/llama3").await.unwrap();
            assert!(dl.was_downloaded_any().await);
            assert!(
                repo.get_by_id("gguf/llama3")
                    .await
                    .unwrap()
                    .unwrap()
                    .downloaded
            );
        }

        #[tokio::test]
        async fn shared_file_present_skips_download() {
            let tmp = tempfile::tempdir().unwrap();
            let repo = Arc::new(SharedMockRepo::new());
            let dl = Arc::new(MockModelDownloader::new(false));
            let storage: Arc<dyn ModelStorage> = Arc::new(MockModelStorage::file_system_backed(
                tmp.path().to_path_buf(),
            ));
            let svc = ModelService::new(
                repo.clone() as Arc<dyn ModelRepository>,
                Arc::new(MockModelCatalogProvider::default()),
                dl.clone(),
                storage,
            );

            // Pre-create the file on disk so path.exists() returns true
            let model_dir = tmp.path().join("models/gguf");
            std::fs::create_dir_all(&model_dir).unwrap();
            std::fs::write(model_dir.join("llama3.gguf"), b"fake weights").unwrap();

            let m = gguf("llama3", true, Some("https://example.com/llama3.gguf"));
            repo.upsert(&m).await.unwrap();

            svc.ensure_downloaded("gguf/llama3").await.unwrap();
            assert!(
                !dl.was_downloaded_any().await,
                "should not download when file already present"
            );
        }
    }

    #[tokio::test]
    async fn sync_disk_flags_corrects_stale_records() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl, tmp.path());

        // model_a: file exists on disk, but DB says downloaded=false
        std::fs::File::create(tmp.path().join("a.llamafile")).unwrap();
        repo.upsert(&stub_model("a", "a.llamafile", false, None))
            .await
            .unwrap();

        // model_b: file does NOT exist, but DB says downloaded=true
        repo.upsert(&stub_model("b", "b.llamafile", true, None))
            .await
            .unwrap();

        let changed = svc.sync_disk_flags().await.unwrap();
        assert_eq!(changed, 2, "both records should have been corrected");

        assert!(
            repo.get_by_id("a").await.unwrap().unwrap().downloaded,
            "model_a should now be downloaded=true"
        );
        assert!(
            !repo.get_by_id("b").await.unwrap().unwrap().downloaded,
            "model_b should now be downloaded=false"
        );
    }
}
