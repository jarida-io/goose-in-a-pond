//! What serve's Voice Match does with an enrolment take: the wake CNN's verdict on it and the
//! speaker model's embedding of it. The speaker model is fetched on first need.

use async_trait::async_trait;
use serde::Serialize;

/// The credit WeSpeaker's VoxCeleb models carry (CC BY 4.0), shown wherever the model is offered.
pub const VOICE_MATCH_CREDIT: &str = "Speaker embeddings by WeSpeaker ResNet34-LM (H. Wang et al., \
\"WeSpeaker: A Research and Production oriented Speaker Embedding Learning Toolkit\", ICASSP 2023), \
trained on VoxCeleb (A. Nagrani, J. S. Chung, A. Zisserman, \"VoxCeleb: a large-scale speaker \
identification dataset\", Interspeech 2017; J. S. Chung, A. Nagrani, A. Zisserman, \"VoxCeleb2: \
Deep Speaker Recognition\", Interspeech 2018). Licensed under CC BY 4.0, \
https://creativecommons.org/licenses/by/4.0/.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceModelState {
    Missing,
    Downloading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct VoiceModelStatus {
    pub state: VoiceModelState,
    /// 0 to 1 while downloading.
    pub progress: Option<f32>,
}

/// What one take told the pond.
#[derive(Debug, Clone, PartialEq)]
pub struct HeardTake {
    /// The "hey goose" CNN fired somewhere in it.
    pub cnn_heard: bool,
    /// The speaker model's embedding of the whole utterance, when one was asked for.
    pub embedding: Option<Vec<f32>>,
    /// The phrase's fbank frames, and the frames after it, where the phrase was found in time.
    pub template: Option<TakeTemplate>,
}

/// A take's wake template: features only, never audio.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeTemplate {
    pub phrase: Vec<f32>,
    pub rest: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TakeError {
    #[error("the speaker model is not ready")]
    ModelNotReady,
    #[error("{0}")]
    Failed(String),
}

#[async_trait]
pub trait VoiceMatchEngine: Send + Sync {
    fn model_status(&self) -> VoiceModelStatus;

    /// The prints this model makes, so prints from another model are never compared.
    fn model_name(&self) -> &str;

    /// Fetch the speaker model again (after a failure, or never started); returns at once.
    fn fetch_model(&self);

    /// 16 kHz mono PCM of one take; `embed` asks for the speaker embedding too, and `triggers`
    /// (the phrase as it may be heard) for the wake template.
    async fn hear_take(
        &self,
        pcm: Vec<f32>,
        embed: bool,
        triggers: Vec<String>,
    ) -> Result<HeardTake, TakeError>;
}
