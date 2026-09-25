//! `VoiceOutput`s for runs with no TTS engine: print to stdout, or stay silent.

use crate::models::ports::voice_output::VoiceOutput;
use anyhow::Result;
use async_trait::async_trait;

/// VoiceOutput that prints the text to stdout instead of speaking it.
pub struct PrintOutput;

impl Default for PrintOutput {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl VoiceOutput for PrintOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        println!("  🗣  {}", text);
        Ok(())
    }
}

/// Discards all output: `chat --json-events` stdout must carry nothing but NDJSON lines.
pub struct SilentOutput;

impl Default for SilentOutput {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl VoiceOutput for SilentOutput {
    async fn speak(&self, _text: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn print_output_speak_returns_ok() {
        let out = PrintOutput;
        assert!(out.speak("hello world").await.is_ok());
    }

    #[tokio::test]
    async fn print_output_compiles_as_voice_output() {
        let _out: Arc<dyn VoiceOutput> = Arc::new(PrintOutput);
    }

    #[tokio::test]
    async fn silent_output_speaks_nothing() {
        let out = SilentOutput;
        assert!(out.speak("this must not print").await.is_ok());
        let _out: Arc<dyn VoiceOutput> = Arc::new(SilentOutput);
    }
}
