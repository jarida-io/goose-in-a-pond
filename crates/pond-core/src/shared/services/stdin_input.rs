//! Text-mode `VoiceInput` adapter over stdin; `ChatService`'s default input source.

use crate::models::ports::voice_input::VoiceInput;
use anyhow::Result;
use async_trait::async_trait;
use std::io::BufRead;

pub struct StdinInput;

impl StdinInput {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinInput {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VoiceInput for StdinInput {
    async fn listen(&self) -> Result<Option<String>> {
        tokio::task::spawn_blocking(|| {
            let stdin = std::io::stdin();
            let mut line = String::new();
            let bytes = stdin.lock().read_line(&mut line)?;
            if bytes == 0 {
                return Ok(None); // EOF (Ctrl-D / piped input exhausted)
            }
            Ok(Some(line.trim().to_string()))
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_input_prompt_is_arrow() {
        assert_eq!(StdinInput::new().prompt(), "> ");
    }
}
