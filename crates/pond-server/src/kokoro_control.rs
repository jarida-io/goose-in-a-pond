//! Live `TtsControl` for Kokoro: pace, voice and quality, downloading what is missing.
//! Here, not in the adapter, because `pond-adapters-kokoro` has no HTTP client.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_adapters_kokoro::KokoroOutput;
use pond_core::models::ports::tts_control::{TtsApplied, TtsControl};

use crate::model_download;

/// The same map the Models page polls through `/models/download/progress`.
type Tracker = Arc<tokio::sync::RwLock<std::collections::HashMap<String, pond_api::DownloadEntry>>>;

pub struct KokoroTtsControl {
    engine: Arc<KokoroOutput>,
    data_dir: PathBuf,
    tracker: Tracker,
}

impl KokoroTtsControl {
    pub fn new(engine: Arc<KokoroOutput>, data_dir: PathBuf, tracker: Tracker) -> Self {
        Self {
            engine,
            data_dir,
            tracker,
        }
    }

    /// Progress sink into the shared download tracker; writes are spawned, never awaited.
    fn reporter(&self, category: &str) -> model_download::DlProgress {
        let tracker = self.tracker.clone();
        let category = category.to_string();
        Arc::new(move |filename: &str, downloaded: u64, total: u64| {
            let tracker = tracker.clone();
            let filename = filename.to_string();
            let category = category.clone();
            tokio::spawn(async move {
                let mut t = tracker.write().await;
                let e = t
                    .entry(filename.clone())
                    .or_insert_with(|| pond_api::DownloadEntry {
                        filename: filename.clone(),
                        category: category.clone(),
                        downloaded_bytes: 0,
                        total_bytes: None,
                        status: "downloading".into(),
                        finished_at: None,
                        control: Default::default(),
                        // No URL: resuming re-runs `apply`, which re-derives it.
                        url: None,
                    });
                e.downloaded_bytes = downloaded;
                e.total_bytes = Some(total);
                if downloaded >= total && total > 0 {
                    e.status = "done".into();
                    e.finished_at = Some(std::time::Instant::now());
                } else {
                    e.status = "downloading".into();
                    e.finished_at = None;
                }
            });
        })
    }

    fn voices_dir(&self) -> PathBuf {
        model_download::kokoro_dir(&self.data_dir).join("voices")
    }
}

#[async_trait]
impl TtsControl for KokoroTtsControl {
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied> {
        // Resolve the tier first: `q8f16` is digital silence on aarch64 Linux, and substituting
        // once keeps the downloaded, loaded, persisted and shown tier the same string.
        let requested = quality;
        let quality = pond_adapters_kokoro::usable_quality(requested);
        if quality != requested {
            tracing::warn!(
                requested,
                using = quality,
                "requested voice quality does not produce audio on this machine; \
                 substituting the nearest tier that does"
            );
        }

        let mut out = TtsApplied {
            speed_milli: (speed * 1000.0).round().max(0.0) as u32,
            quality: quality.to_string(),
            ..Default::default()
        };

        // ── Weights for the requested tier ──
        // Before the voice: failing after a voice download would waste the fetch.
        let kdir = model_download::kokoro_dir(&self.data_dir);
        let weights = kdir.join(pond_adapters_kokoro::model_filename(quality));
        if !weights.exists() {
            model_download::ensure_kokoro_engine_reporting(
                &self.data_dir,
                quality,
                voice,
                Some(self.reporter("tts_kokoro")),
            )
            .await;
            out.downloaded_weights = weights.exists();
            if !out.downloaded_weights {
                anyhow::bail!(
                    "could not fetch the {quality} voice engine; the current one is unchanged"
                );
            }
        }
        // Reload only on a changed file: `set_model` drops the session.
        if self.engine.model_path().await != weights {
            self.engine.set_model(weights).await?;
            out.engine_reloaded = true;
        }

        // ── The voice ──
        let wanted = voice.trim();
        let name = if wanted.is_empty() {
            pond_adapters_kokoro::DEFAULT_VOICE
        } else {
            wanted
        };
        let path = pond_adapters_kokoro::voices::voice_path(&self.voices_dir(), name)
            .with_context(|| format!("{name:?} is not a usable Kokoro voice id"))?;
        if !path.exists() {
            // A missing voice is a download (half a megabyte), not an error.
            model_download::ensure_kokoro_engine_reporting(
                &self.data_dir,
                quality,
                name,
                Some(self.reporter("tts_kokoro")),
            )
            .await;
            out.downloaded_voice = path.exists();
            if !out.downloaded_voice {
                anyhow::bail!("could not fetch the voice {name:?}; the current one is unchanged");
            }
        }
        self.engine.set_voice(name).await?;
        out.voice = self.engine.voice().await;

        // ── Pace ──
        out.speed_milli = (self.engine.set_speed(speed) * 1000.0).round() as u32;

        tracing::info!(
            voice = %out.voice,
            quality,
            reloaded = out.engine_reloaded,
            fetched_voice = out.downloaded_voice,
            fetched_weights = out.downloaded_weights,
            "TTS settings applied to the running engine"
        );
        Ok(out)
    }

    async fn installed_voices(&self) -> Vec<String> {
        self.engine.installed_voices()
    }
}
