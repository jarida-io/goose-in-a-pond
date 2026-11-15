//! What the engine path asks of the rest of the pond when memory is too short to take in a turn's
//! recording or picture.

use async_trait::async_trait;

#[async_trait]
pub trait MediaRoom: Send + Sync {
    /// Gives back the memory of models that load again by themselves when next needed. Returns
    /// the names of what was given back.
    async fn make_room(&self) -> Vec<&'static str>;

    /// The words of a WAV recording, for a turn that cannot hear it. `None` without a way to
    /// transcribe, or when no words were made out.
    async fn words_of(&self, wav: Vec<u8>) -> Option<String>;
}
