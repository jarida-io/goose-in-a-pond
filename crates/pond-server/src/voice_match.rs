//! Serve's Voice Match: the speaker model fetched on first need, and enrolment takes heard by an
//! ear of their own. The room's ear loads the same model once someone is enrolled.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pond_adapters_ear::{ModelFile, TakeEar};
use pond_core::models::domain::voice_match_model as model;
use pond_core::models::ports::voice_match::{
    HeardTake, TakeError, TakeTemplate, VoiceMatchEngine, VoiceModelState, VoiceModelStatus,
};

use crate::model_download;

/// Words heard in 16 kHz PCM and when, as (`text`, start, end) seconds; blocking.
pub type WordTimer = Arc<dyn Fn(&[f32]) -> anyhow::Result<Vec<(String, f32, f32)>> + Send + Sync>;

pub struct ServeVoiceMatch {
    data_dir: PathBuf,
    status: Arc<Mutex<VoiceModelStatus>>,
    /// Where in a take the phrase was said, for its wake template; none without Whisper.
    words: Option<WordTimer>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ServeVoiceMatch {
    pub fn new(data_dir: &Path, words: Option<WordTimer>) -> Arc<ServeVoiceMatch> {
        let engine = Arc::new(ServeVoiceMatch {
            data_dir: data_dir.to_path_buf(),
            status: Arc::new(Mutex::new(VoiceModelStatus {
                state: VoiceModelState::Missing,
                progress: None,
            })),
            words,
        });
        if engine.onnx_path().exists() {
            lock(&engine.status).state = VoiceModelState::Ready;
        }
        engine
    }

    pub fn onnx_path(&self) -> PathBuf {
        self.data_dir.join(model::LOCAL_PATH)
    }

    /// The speaker model as the ear loads it, once it is here; blocking.
    pub fn speaker_model(&self) -> Option<ModelFile> {
        if lock(&self.status).state != VoiceModelState::Ready {
            return None;
        }
        match pond_adapters_ear::load_speaker(&self.onnx_path()) {
            Ok(file) => Some(file),
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "voice match: the speaker model does not load"
                );
                None
            }
        }
    }

    async fn fetch(status: Arc<Mutex<VoiceModelStatus>>, dest: PathBuf) {
        let report_to = status.clone();
        let report: model_download::DlProgress = Arc::new(move |_name, done, total| {
            let progress = (total > 0).then(|| (done as f32 / total as f32).min(1.0));
            *lock(&report_to) = VoiceModelStatus {
                state: VoiceModelState::Downloading,
                progress,
            };
        });
        let mb = model::SIZE_BYTES.div_ceil(1_048_576);
        let fetched =
            model_download::download_file_reporting(&model::url(), &dest, mb, Some(report)).await;
        let converted = match fetched {
            // Converted once here, so no later load parses the ONNX.
            Ok(()) => tokio::task::spawn_blocking(move || {
                pond_adapters_ear::load_speaker(&dest).map(|_| ())
            })
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r),
            Err(e) => Err(e),
        };
        let state = match converted {
            Ok(()) => {
                tracing::info!(
                    kind = "voice_match_model",
                    "voice match: the speaker model is ready"
                );
                VoiceModelState::Ready
            }
            Err(e) => {
                tracing::warn!(
                    kind = "voice_match_model",
                    error = format!("{e:#}"),
                    "voice match: the speaker model could not be fetched"
                );
                VoiceModelState::Failed
            }
        };
        *lock(&status) = VoiceModelStatus {
            state,
            progress: None,
        };
    }
}

#[async_trait]
impl VoiceMatchEngine for ServeVoiceMatch {
    fn model_status(&self) -> VoiceModelStatus {
        *lock(&self.status)
    }

    fn model_name(&self) -> &str {
        model::MODEL_NAME
    }

    fn fetch_model(&self) {
        {
            let mut status = lock(&self.status);
            if matches!(
                status.state,
                VoiceModelState::Ready | VoiceModelState::Downloading
            ) {
                return;
            }
            *status = VoiceModelStatus {
                state: VoiceModelState::Downloading,
                progress: Some(0.0),
            };
        }
        let dest = self.onnx_path();
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        tokio::spawn(Self::fetch(self.status.clone(), dest));
    }

    async fn hear_take(
        &self,
        pcm: Vec<f32>,
        embed: bool,
        triggers: Vec<String>,
    ) -> Result<HeardTake, TakeError> {
        let speaker = match embed {
            true => {
                if self.model_status().state != VoiceModelState::Ready {
                    self.fetch_model();
                    return Err(TakeError::ModelNotReady);
                }
                let onnx = self.onnx_path();
                let loaded =
                    tokio::task::spawn_blocking(move || pond_adapters_ear::load_speaker(&onnx))
                        .await
                        .map_err(|e| TakeError::Failed(e.to_string()))?;
                Some(loaded.map_err(|e| TakeError::Failed(format!("{e:#}")))?)
            }
            false => None,
        };
        let silero = model_download::ensure_silero_model(&self.data_dir)
            .await
            .ok_or_else(|| TakeError::Failed("the speech detector is not installed".into()))?;
        let words = self.words.clone().filter(|_| embed);
        tokio::task::spawn_blocking(move || {
            // Only with Voice Match on: a template is the member's voice.
            let span = words.and_then(|timer| match timer(&pcm) {
                Ok(words) => {
                    let timed: Vec<(&str, f32, f32)> =
                        words.iter().map(|w| (w.0.as_str(), w.1, w.2)).collect();
                    pond_voice::text::wake_span(&timed, &triggers)
                }
                Err(e) => {
                    tracing::warn!(
                        error = format!("{e:#}"),
                        "enrol: no word times for the take"
                    );
                    None
                }
            });
            let vad = pond_adapters_ear::load_vad(&silero)?;
            let mut ear = TakeEar::new(vad, speaker)?;
            ear.hear(&pcm, embed, span)
        })
        .await
        .map_err(|e| TakeError::Failed(e.to_string()))?
        .map(|heard| HeardTake {
            cnn_heard: heard.cnn_heard,
            embedding: heard.embedding,
            template: heard
                .template
                .map(|(phrase, rest)| TakeTemplate { phrase, rest }),
        })
        .map_err(|e| TakeError::Failed(format!("{e:#}")))
    }
}
