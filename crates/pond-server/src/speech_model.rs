//! Serve's speech model follows the ASR role while it runs: a model not on disk is fetched with its
//! progress on the Models page, and the running Whisper is replaced only once the new one loads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use pond_adapters_whisper::WhisperRsInput;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::speech_model::{SpeechModelSwitch, SpeechSwitch};
use pond_core::user_data::ports::settings::SettingsRepository;

use crate::model_download::{self, DlProgress};
use crate::voice_models;

/// The same map the Models page polls through `/models/download/progress`.
pub type Tracker =
    Arc<tokio::sync::RwLock<std::collections::HashMap<String, pond_api::DownloadEntry>>>;

/// Fetch `url` to `dest`, about `size_mb`, reporting progress.
pub type Fetch =
    Arc<dyn Fn(String, PathBuf, u64, DlProgress) -> BoxFuture<'static, Result<()>> + Send + Sync>;

const CATEGORY: &str = "whisper";

/// The running speech engine, as the switch needs it.
#[async_trait]
pub trait SpeechEngine: Send + Sync {
    async fn current(&self) -> PathBuf;
    /// Load `path` and take it into use; on an error the running model stays.
    async fn load(&self, path: PathBuf) -> Result<()>;
}

#[async_trait]
impl SpeechEngine for WhisperRsInput {
    async fn current(&self) -> PathBuf {
        self.current_model_path().await
    }

    async fn load(&self, path: PathBuf) -> Result<()> {
        self.rebuild_with(path).await
    }
}

pub struct ServeSpeechModel {
    engine: Arc<dyn SpeechEngine>,
    models_dir: PathBuf,
    catalog: Arc<dyn ModelRepository + Send + Sync>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    tracker: Tracker,
    fetch: Fetch,
    /// One switch at a time: a later choice waits, then finds this one done or superseded.
    busy: tokio::sync::Mutex<()>,
}

impl ServeSpeechModel {
    pub fn new(
        engine: Arc<dyn SpeechEngine>,
        data_dir: &Path,
        catalog: Arc<dyn ModelRepository + Send + Sync>,
        settings: Arc<dyn SettingsRepository + Send + Sync>,
        tracker: Tracker,
    ) -> ServeSpeechModel {
        let fetch: Fetch = Arc::new(|url, dest, size_mb, report| {
            Box::pin(async move {
                model_download::download_file_reporting(&url, &dest, size_mb, Some(report)).await
            })
        });
        ServeSpeechModel::with_fetch(engine, data_dir, catalog, settings, tracker, fetch)
    }

    pub fn with_fetch(
        engine: Arc<dyn SpeechEngine>,
        data_dir: &Path,
        catalog: Arc<dyn ModelRepository + Send + Sync>,
        settings: Arc<dyn SettingsRepository + Send + Sync>,
        tracker: Tracker,
        fetch: Fetch,
    ) -> ServeSpeechModel {
        ServeSpeechModel {
            engine,
            models_dir: data_dir.join("models"),
            catalog,
            settings,
            tracker,
            fetch,
            busy: tokio::sync::Mutex::new(()),
        }
    }

    async fn resolve(&self, model: &str) -> Option<voice_models::WhisperModel> {
        let catalog = voice_models::whisper_catalog(&*self.catalog).await;
        voice_models::resolve_whisper(model, &catalog, &self.models_dir)
    }

    /// Download into place, the tracker entry ending done or in plain words.
    async fn fetch_into(&self, url: &str, dest: &Path, size_mb: u64) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let name = dest
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let fetched = (self.fetch)(
            url.to_string(),
            dest.to_path_buf(),
            size_mb,
            reporter(&self.tracker),
        )
        .await;
        let error = match (&fetched, dest.exists()) {
            (Ok(()), true) => None,
            (Ok(()), false) => Some(format!("{} is missing after its download", dest.display())),
            (Err(e), _) => Some(pond_api::download_failure::plain_failure(e)),
        };
        let mut t = self.tracker.write().await;
        let e = t
            .entry(name.clone())
            .or_insert_with(|| pond_api::DownloadEntry::starting(&name, CATEGORY));
        e.status = if error.is_some() { "error" } else { "done" }.into();
        e.error = error.clone();
        e.finished_at = Some(std::time::Instant::now());
        error.map_or(Ok(()), Err)
    }
}

/// Progress only, synchronous so nothing lands after the entry ends; a busy lock skips one.
fn reporter(tracker: &Tracker) -> DlProgress {
    let tracker = tracker.clone();
    Arc::new(move |filename: &str, downloaded: u64, total: u64| {
        let Ok(mut t) = tracker.try_write() else {
            return;
        };
        let e = t
            .entry(filename.to_string())
            .or_insert_with(|| pond_api::DownloadEntry::starting(filename, CATEGORY));
        e.status = "downloading".into();
        e.error = None;
        e.finished_at = None;
        e.downloaded_bytes = downloaded;
        e.total_bytes = Some(total);
    })
}

#[async_trait]
impl SpeechModelSwitch for ServeSpeechModel {
    async fn switch_to(&self, model: &str) -> SpeechSwitch {
        let _one = self.busy.lock().await;
        let Some(target) = self.resolve(model).await else {
            return SpeechSwitch::Kept {
                reason: format!("'{model}' matches no speech model in the catalogue or on disk"),
            };
        };
        if self.engine.current().await == target.path {
            return SpeechSwitch::Unchanged;
        }
        let mut fetched = false;
        if !target.path.exists() {
            let Some(download) = target.download.as_ref() else {
                return SpeechSwitch::Kept {
                    reason: format!(
                        "{} is not on disk and has no download",
                        target.path.display()
                    ),
                };
            };
            tracing::info!(
                kind = "speech_model_fetch",
                model,
                size_mb = download.size_mb,
                "fetching the chosen speech model; the current one keeps transcribing"
            );
            if let Err(reason) = self
                .fetch_into(&download.url, &target.path, download.size_mb)
                .await
            {
                return SpeechSwitch::Kept { reason };
            }
            fetched = true;
            let file = target.path.file_name().and_then(|f| f.to_str());
            let row = voice_models::whisper_catalog(&*self.catalog)
                .await
                .into_iter()
                .find(|m| m.filename.as_deref() == file);
            if let Some(row) = row {
                let _ = self.catalog.set_downloaded(&row.id, true).await;
            }
            // The household may have chosen again while this downloaded: the newest choice wins.
            let now = self
                .settings
                .get()
                .await
                .map(|s| s.active_whisper_model)
                .unwrap_or_default();
            if self.resolve(&now).await.map(|w| w.path) != Some(target.path.clone()) {
                return SpeechSwitch::Superseded;
            }
        }
        match self.engine.load(target.path.clone()).await {
            Ok(()) => SpeechSwitch::Switched {
                model: model.to_string(),
                fetched,
            },
            Err(e) => SpeechSwitch::Kept {
                reason: format!("{e:#}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
    use pond_core::models::mocks::mock_model_repository::MockModelRepository;
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
    use std::sync::Mutex;

    struct Engine {
        current: Mutex<PathBuf>,
        loads: Mutex<Vec<PathBuf>>,
        fails: bool,
    }

    #[async_trait]
    impl SpeechEngine for Engine {
        async fn current(&self) -> PathBuf {
            self.current.lock().unwrap().clone()
        }
        async fn load(&self, path: PathBuf) -> Result<()> {
            self.loads.lock().unwrap().push(path.clone());
            if self.fails {
                anyhow::bail!("no room on the GPU");
            }
            *self.current.lock().unwrap() = path;
            Ok(())
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        engine: Arc<Engine>,
        switch: ServeSpeechModel,
        fetched: Arc<Mutex<Vec<String>>>,
        settings: Arc<MockSettingsRepository>,
        tracker: Tracker,
    }

    fn row(name: &str, filename: &str) -> ModelRecord {
        let mut r = crate::composite_model_catalog_provider::whisper_models()
            .into_iter()
            .next()
            .unwrap();
        r.id = format!("whisper/{name}");
        r.name = name.into();
        r.filename = Some(filename.into());
        r.url = Some(format!("https://example.test/{filename}"));
        r.category = ModelCategory::Whisper;
        r
    }

    async fn rig(fetch_works: bool, load_fails: bool) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("ggml-base.bin"), b"base").unwrap();
        let catalog = Arc::new(MockModelRepository::new());
        catalog.upsert(&row("base", "ggml-base.bin")).await.unwrap();
        let engine = Arc::new(Engine {
            current: Mutex::new(models.join("ggml-base.bin")),
            loads: Mutex::default(),
            fails: load_fails,
        });
        let settings = Arc::new(MockSettingsRepository::new());
        let tracker: Tracker = Arc::default();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let log = fetched.clone();
        let fetch: Fetch = Arc::new(move |url, dest: PathBuf, _mb, report: DlProgress| {
            log.lock().unwrap().push(url);
            Box::pin(async move {
                if !fetch_works {
                    anyhow::bail!("connection reset");
                }
                report(&dest.file_name().unwrap().to_string_lossy(), 5, 10);
                std::fs::write(&dest, b"model")?;
                Ok(())
            })
        });
        let switch = ServeSpeechModel::with_fetch(
            engine.clone(),
            dir.path(),
            catalog,
            settings.clone(),
            tracker.clone(),
            fetch,
        );
        Rig {
            dir,
            engine,
            switch,
            fetched,
            settings,
            tracker,
        }
    }

    async fn choose(r: &Rig, model: &str) {
        use pond_core::user_data::ports::settings::SettingsRepository as _;
        r.settings
            .set_key("active_whisper_model", model.to_string())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_model_not_on_disk_is_fetched_with_progress_then_taken_into_use() {
        let r = rig(true, false).await;
        choose(&r, "small.en-q5_1").await;
        let out = r.switch.switch_to("small.en-q5_1").await;
        assert_eq!(
            out,
            SpeechSwitch::Switched {
                model: "small.en-q5_1".into(),
                fetched: true
            }
        );
        let q5 = r.dir.path().join("models/ggml-small.en-q5_1.bin");
        assert_eq!(*r.engine.loads.lock().unwrap(), vec![q5.clone()]);
        let url = r.fetched.lock().unwrap()[0].clone();
        assert!(
            url.contains(pond_core::models::domain::whisper_models::REVISION),
            "{url}"
        );
        let t = r.tracker.read().await;
        let entry = t
            .get("ggml-small.en-q5_1.bin")
            .expect("progress on the Models page");
        assert_eq!((entry.status.as_str(), entry.downloaded_bytes), ("done", 5));
    }

    #[tokio::test]
    async fn the_running_model_keeps_transcribing_when_the_fetch_or_the_load_fails() {
        let r = rig(false, false).await;
        choose(&r, "small.en-q5_1").await;
        let out = r.switch.switch_to("small.en-q5_1").await;
        assert!(matches!(out, SpeechSwitch::Kept { .. }), "{out:?}");
        assert!(
            r.engine.loads.lock().unwrap().is_empty(),
            "nothing replaced"
        );
        let t = r.tracker.read().await;
        assert_eq!(t["ggml-small.en-q5_1.bin"].status, "error");
        drop(t);

        let r = rig(true, true).await;
        choose(&r, "small.en-q5_1").await;
        let out = r.switch.switch_to("small.en-q5_1").await;
        assert!(matches!(out, SpeechSwitch::Kept { .. }), "{out:?}");
        assert_eq!(
            r.engine.current().await,
            r.dir.path().join("models/ggml-base.bin")
        );
    }

    #[tokio::test]
    async fn the_running_model_or_an_unknown_name_changes_nothing() {
        let r = rig(true, false).await;
        assert_eq!(r.switch.switch_to("base").await, SpeechSwitch::Unchanged);
        assert!(matches!(
            r.switch.switch_to("whisper-from-mars").await,
            SpeechSwitch::Kept { .. }
        ));
        assert!(r.engine.loads.lock().unwrap().is_empty());
        assert!(r.fetched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_choice_changed_during_the_download_is_not_loaded() {
        let r = rig(true, false).await;
        choose(&r, "base").await;
        let out = r.switch.switch_to("small.en-q5_1").await;
        assert_eq!(out, SpeechSwitch::Superseded);
        assert!(r.engine.loads.lock().unwrap().is_empty());
        assert!(
            r.dir.path().join("models/ggml-small.en-q5_1.bin").exists(),
            "the download is kept for the next time it is chosen"
        );
    }
}
