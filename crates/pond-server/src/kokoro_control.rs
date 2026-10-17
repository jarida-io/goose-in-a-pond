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

const CATEGORY: &str = "tts_kokoro";

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

    /// Fetch whatever `quality` and `voice` lack, then end each file's tracker entry.
    async fn fetch(&self, quality: &str, voice: &str) {
        let tried = model_download::ensure_kokoro_engine_reporting(
            &self.data_dir,
            quality,
            voice,
            Some(reporter(&self.tracker)),
        )
        .await;
        settle(&self.tracker, tried).await;
    }

    fn voices_dir(&self) -> PathBuf {
        model_download::kokoro_dir(&self.data_dir).join("voices")
    }
}

/// Progress only: the last chunk comes before the file is flushed and linked, so `settle` ends
/// the entry. Synchronous, so no write lands after that; a busy lock skips one.
fn reporter(tracker: &Tracker) -> model_download::DlProgress {
    let tracker = tracker.clone();
    Arc::new(move |filename: &str, downloaded: u64, total: u64| {
        let Ok(mut t) = tracker.try_write() else {
            return;
        };
        let e = t
            .entry(filename.to_string())
            // No URL: resuming re-runs `apply`, which re-derives it.
            .or_insert_with(|| pond_api::DownloadEntry::starting(filename, CATEGORY));
        // Bytes for a file whose entry ended are a new attempt at it.
        e.status = "downloading".into();
        e.error = None;
        e.finished_at = None;
        e.downloaded_bytes = downloaded;
        e.total_bytes = Some(total);
    })
}

/// Ends each file's entry: done once the file is in place, error in plain words otherwise.
async fn settle(tracker: &Tracker, tried: Vec<model_download::FileFetch>) {
    let ended: Vec<(String, Option<String>)> = tried
        .into_iter()
        .map(|f| {
            // In place is what counts: another `apply` may have fetched the same file meanwhile.
            let error = (!f.dest.exists()).then(|| {
                let why = f.result.err().unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("{} is missing after its download", f.dest.display()),
                    )
                    .into()
                });
                pond_api::download_failure::plain_failure(&why)
            });
            (f.name, error)
        })
        .collect();

    let now = std::time::Instant::now();
    let mut t = tracker.write().await;
    for (name, error) in ended {
        let e = t
            .entry(name.clone())
            .or_insert_with(|| pond_api::DownloadEntry::starting(&name, CATEGORY));
        e.status = if error.is_some() { "error" } else { "done" }.into();
        e.error = error;
        e.finished_at = Some(now);
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
            self.fetch(quality, voice).await;
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
            self.fetch(quality, name).await;
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

    async fn synthesize_in_voice(&self, voice: &str, text: &str) -> Result<Vec<u8>> {
        let wav = self.engine.synth_wav_in_voice(voice, text).await?;
        if wav.is_empty() {
            anyhow::bail!("Kokoro produced no audio for the sample in {voice:?}");
        }
        Ok(wav)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_download::FileFetch;

    fn tried(name: &str, dest: PathBuf, result: Result<()>) -> FileFetch {
        FileFetch {
            name: name.to_string(),
            dest,
            result,
        }
    }

    #[tokio::test]
    async fn progress_alone_never_ends_an_entry() {
        let tracker = Tracker::default();
        let report = reporter(&tracker);

        // Every byte is in, and the file can still fail to land.
        report("af_heart.bin", 522, 522);
        let e = tracker.read().await["af_heart.bin"].clone();
        assert_eq!(e.status, "downloading");
        assert_eq!((e.downloaded_bytes, e.total_bytes), (522, Some(522)));
        assert_eq!(e.category, "tts_kokoro");
        assert!(e.finished_at.is_none());

        // Bytes for a file whose last fetch failed are a new attempt.
        settle(
            &tracker,
            vec![tried(
                "bm_george.bin",
                PathBuf::from("/nowhere/bm_george.bin"),
                Err(anyhow::anyhow!("boom")),
            )],
        )
        .await;
        report("bm_george.bin", 100, 522);
        let e = tracker.read().await["bm_george.bin"].clone();
        assert_eq!((e.status.as_str(), e.error), ("downloading", None));
        assert!(e.finished_at.is_none());
    }

    #[tokio::test]
    async fn an_entry_is_done_only_once_its_file_is_in_place() {
        let tmp = tempfile::TempDir::new().unwrap();
        let landed = tmp.path().join("af_heart.bin");
        std::fs::write(&landed, b"style table").unwrap();
        let also_landed = tmp.path().join("am_adam.bin");
        std::fs::write(&also_landed, b"style table").unwrap();
        let tracker = Tracker::default();
        let report = reporter(&tracker);
        for name in ["af_heart.bin", "bf_emma.bin", "am_adam.bin"] {
            report(name, 522, 522);
        }

        settle(
            &tracker,
            vec![
                tried("af_heart.bin", landed, Ok(())),
                tried("bf_emma.bin", tmp.path().join("bf_emma.bin"), Ok(())),
                // Another `apply` fetched it meanwhile.
                tried(
                    "am_adam.bin",
                    also_landed,
                    Err(anyhow::anyhow!("lock wait timed out")),
                ),
            ],
        )
        .await;

        let t = tracker.read().await;
        for name in ["af_heart.bin", "am_adam.bin"] {
            assert_eq!(
                (t[name].status.as_str(), t[name].error.as_deref()),
                ("done", None),
                "{name}"
            );
        }
        let missing = &t["bf_emma.bin"];
        assert_eq!(missing.status, "error");
        let said = missing.error.as_deref().unwrap_or_default();
        assert!(said.contains("could not be saved"), "{said}");
        assert!(t.values().all(|e| e.finished_at.is_some()), "all age out");
    }

    #[tokio::test]
    async fn a_failed_fetch_ends_in_error_in_the_households_words() {
        use pond_core::shared::services::egress::{EgressDenied, NetworkMode};
        let tmp = tempfile::TempDir::new().unwrap();
        let tracker = Tracker::default();
        reporter(&tracker)("model_quantized.onnx", 96_468_993, 96_468_993);

        let full = anyhow::Error::new(std::io::Error::from_raw_os_error(28))
            .context("rename /pond/hf_cache/blobs/abc.incomplete -> /pond/hf_cache/blobs/abc");
        let refused = anyhow::Error::new(EgressDenied {
            host: "huggingface.co".into(),
            mode: NetworkMode::Offline,
            reason: "offline",
        })
        .context("hf_cache fetch onnx-community/Kokoro-82M-v1.0-ONNX/tokenizer.json@main");
        settle(
            &tracker,
            vec![
                // Every byte arrived; the rename into the cache did not.
                tried(
                    "model_quantized.onnx",
                    tmp.path().join("model_quantized.onnx"),
                    Err(full),
                ),
                // Refused before its first byte, so only the outcome makes the entry.
                tried(
                    "tokenizer.json",
                    tmp.path().join("tokenizer.json"),
                    Err(refused),
                ),
            ],
        )
        .await;

        let t = tracker.read().await;
        for (name, words) in [
            ("model_quantized.onnx", "no room left"),
            ("tokenizer.json", "network setting"),
        ] {
            let e = &t[name];
            assert_eq!(
                (e.status.as_str(), e.category.as_str()),
                ("error", "tts_kokoro")
            );
            let said = e.error.as_deref().unwrap_or_default();
            assert!(said.contains(words), "{name}: {said}");
            assert!(
                !said.contains('/') && !said.contains("http"),
                "{name}: a path or URL reached the household: {said}"
            );
            assert!(e.finished_at.is_some(), "{name} never ages out");
        }
    }
}
