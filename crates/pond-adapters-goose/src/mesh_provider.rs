//! Wraps an `LlmProvider` as a Goose `Provider` for `chat_provider = "mesh"`. `LlmProvider` has
//! no tools parameter, so tools are dropped and the LAST message says so (it survives best).

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use goose::conversation::message::Message;
use goose::providers::base::{MessageStream, Provider, ProviderUsage, Usage};
use goose_providers::errors::ProviderError;
use goose_providers::model::ModelConfig;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::{LlmProvider, StreamToken};
use rmcp::model::Tool;

/// Appended when non-empty `tools` are dropped. The explicit "don'ts" target observed leaks:
/// an echoed `<answer-contract>` tag and narrated tool decisions.
const NO_TOOLS_OVER_MESH_NOTICE: &str = "\n\n(Tool calls and memory search are not available for \
this response — it is running on a borrowed peer over the mesh. Answer directly and briefly, in \
plain prose. Do not call a tool, do not describe deciding whether one is needed, and do not use or \
repeat any angle-bracket tags — write only the answer itself.)";

pub struct MeshProvider {
    inner: Arc<dyn LlmProvider>,
}

impl MeshProvider {
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }

    /// Inverse of `GooseProviderAdapter::from_goose_message`; Goose has no `System`/`Tool` role.
    fn from_goose_message(msg: &Message) -> ChatMessage {
        let role = match msg.role {
            rmcp::model::Role::User => Role::User,
            rmcp::model::Role::Assistant => Role::Assistant,
        };
        ChatMessage {
            role,
            content: msg.as_concat_text(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[async_trait]
impl Provider for MeshProvider {
    fn get_name(&self) -> &str {
        "mesh"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let mut chat_messages: Vec<ChatMessage> =
            messages.iter().map(Self::from_goose_message).collect();
        let system = system.to_string();
        if !tools.is_empty() {
            tracing::debug!(
                tool_count = tools.len(),
                "mesh provider: dropping tools — MCP tool-calling is not available over the mesh yet"
            );
            // `system` still advertises the tools; with no messages, the notice becomes one.
            match chat_messages.last_mut() {
                Some(last) => last.content.push_str(NO_TOOLS_OVER_MESH_NOTICE),
                None => {
                    chat_messages.push(ChatMessage::user(NO_TOOLS_OVER_MESH_NOTICE.trim_start()))
                }
            }
        }
        // Owned clone so the stream is 'static, as Goose's `MessageStream` requires.
        let inner = self.inner.clone();

        let stream = async_stream::stream! {
            let mut token_stream = inner.stream_complete(&system, chat_messages);
            while let Some(item) = token_stream.next().await {
                match item {
                    Ok(StreamToken::Text(text)) => {
                        yield Ok((Some(Message::assistant().with_text(&text)), None));
                    }
                    Ok(StreamToken::Usage(stats)) => {
                        let usage = Usage {
                            input_tokens: Some(stats.prompt_tokens as i32),
                            output_tokens: Some(stats.completion_tokens as i32),
                            total_tokens: Some((stats.prompt_tokens + stats.completion_tokens) as i32),
                            ..Default::default()
                        };
                        yield Ok((None, Some(ProviderUsage::new("mesh".to_string(), usage))));
                    }
                    Err(err) => {
                        yield Err(ProviderError::ExecutionError(err.to_string()));
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::mocks::mock_provider::MockProvider;

    fn user_message(text: &str) -> Message {
        Message::user().with_text(text)
    }

    #[test]
    fn from_goose_message_translates_user_and_assistant() {
        let user = Message::user().with_text("hello");
        let translated = MeshProvider::from_goose_message(&user);
        assert_eq!(translated.role, Role::User);
        assert_eq!(translated.content, "hello");

        let assistant = Message::assistant().with_text("hi there");
        let translated = MeshProvider::from_goose_message(&assistant);
        assert_eq!(translated.role, Role::Assistant);
        assert_eq!(translated.content, "hi there");
    }

    /// Emits text and `StreamToken::Usage`, which `MockProvider`'s default stream never does.
    struct StreamingStubProvider;

    #[async_trait]
    impl LlmProvider for StreamingStubProvider {
        async fn complete(
            &self,
            _system_prompt: &str,
            _messages: Vec<ChatMessage>,
        ) -> anyhow::Result<ChatMessage> {
            Ok(ChatMessage::assistant("hi there"))
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }

        fn stream_complete<'a>(
            &'a self,
            _system_prompt: &'a str,
            _messages: Vec<ChatMessage>,
        ) -> pond_core::models::ports::provider::TokenStream<'a> {
            Box::pin(async_stream::stream! {
                yield Ok(StreamToken::Text("hi ".to_string()));
                yield Ok(StreamToken::Text("there".to_string()));
                yield Ok(StreamToken::Usage(pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    reasoning_tokens: None,
                }));
            })
        }
    }

    #[tokio::test]
    async fn stream_yields_text_deltas_then_usage() {
        let provider = MeshProvider::new(Arc::new(StreamingStubProvider));
        let cfg = ModelConfig::new("mesh");
        let mut stream = provider
            .stream(&cfg, "You are helpful.", &[user_message("hi")], &[])
            .await
            .unwrap();

        let mut texts = Vec::new();
        let mut saw_usage = false;
        while let Some(item) = stream.next().await {
            let (message, usage) = item.unwrap();
            if let Some(msg) = message {
                texts.push(msg.as_concat_text());
            }
            if let Some(usage) = usage {
                assert_eq!(usage.usage.input_tokens, Some(3));
                assert_eq!(usage.usage.output_tokens, Some(2));
                saw_usage = true;
            }
        }
        // Each StreamToken::Text is its own delta, in order — not accumulated.
        assert_eq!(texts, vec!["hi ".to_string(), "there".to_string()]);
        assert!(saw_usage, "expected a terminal usage item");
    }

    /// Records the `system_prompt` and `messages` that reached the "model".
    struct CapturingProvider {
        seen_system: std::sync::Mutex<Option<String>>,
        seen_messages: std::sync::Mutex<Option<Vec<ChatMessage>>>,
    }

    impl CapturingProvider {
        fn new() -> Self {
            Self {
                seen_system: std::sync::Mutex::new(None),
                seen_messages: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingProvider {
        async fn complete(
            &self,
            system_prompt: &str,
            messages: Vec<ChatMessage>,
        ) -> anyhow::Result<ChatMessage> {
            *self.seen_system.lock().unwrap() = Some(system_prompt.to_string());
            *self.seen_messages.lock().unwrap() = Some(messages);
            Ok(ChatMessage::assistant("ok"))
        }

        fn model_name(&self) -> String {
            "capturing".to_string()
        }
    }

    #[tokio::test]
    async fn a_nonempty_tools_list_gets_a_no_tools_notice_appended_to_the_last_message() {
        let provider = Arc::new(CapturingProvider::new());
        let mesh_provider = MeshProvider::new(provider.clone());
        let cfg = ModelConfig::new("mesh");
        let tool = Tool::new("device_control", "control a device", serde_json::Map::new());

        let mut stream = mesh_provider
            .stream(
                &cfg,
                "You are helpful. You have access to: device_control.",
                &[user_message("turn off the lights")],
                std::slice::from_ref(&tool),
            )
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let seen_system = provider.seen_system.lock().unwrap().clone().unwrap();
        assert_eq!(
            seen_system, "You are helpful. You have access to: device_control.",
            "system must reach the peer unmodified — the notice belongs on the last message"
        );

        let seen_messages = provider.seen_messages.lock().unwrap().clone().unwrap();
        let last = seen_messages.last().expect("at least one message");
        assert!(
            last.content.starts_with("turn off the lights"),
            "the original ask must survive: {}",
            last.content
        );
        assert!(
            last.content.contains("not available for this response"),
            "expected the no-tools-over-mesh notice on the last message, got: {}",
            last.content
        );
    }

    #[tokio::test]
    async fn an_empty_tools_list_leaves_the_conversation_untouched() {
        let provider = Arc::new(CapturingProvider::new());
        let mesh_provider = MeshProvider::new(provider.clone());
        let cfg = ModelConfig::new("mesh");

        let mut stream = mesh_provider
            .stream(&cfg, "You are helpful.", &[user_message("hi")], &[])
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let seen_system = provider.seen_system.lock().unwrap().clone().unwrap();
        assert_eq!(seen_system, "You are helpful.");

        let seen_messages = provider.seen_messages.lock().unwrap().clone().unwrap();
        assert_eq!(seen_messages.last().unwrap().content, "hi");
    }

    #[tokio::test]
    async fn stream_drops_tools_without_erroring() {
        let provider = MeshProvider::new(Arc::new(MockProvider::new()));
        let cfg = ModelConfig::new("mesh");
        let tool = Tool::new("device_control", "control a device", serde_json::Map::new());
        let result = provider
            .stream(
                &cfg,
                "sys",
                &[user_message("hi")],
                std::slice::from_ref(&tool),
            )
            .await;
        assert!(result.is_ok(), "a non-empty tools list must not error");
    }

    #[tokio::test]
    async fn stream_propagates_inner_errors() {
        struct FailingProvider;
        #[async_trait]
        impl LlmProvider for FailingProvider {
            async fn complete(
                &self,
                _system_prompt: &str,
                _messages: Vec<ChatMessage>,
            ) -> anyhow::Result<ChatMessage> {
                Err(anyhow::anyhow!("no peer available"))
            }
            fn model_name(&self) -> String {
                "failing".to_string()
            }
        }

        let provider = MeshProvider::new(Arc::new(FailingProvider));
        let cfg = ModelConfig::new("mesh");
        let mut stream = provider
            .stream(&cfg, "sys", &[user_message("hi")], &[])
            .await
            .unwrap();

        let first = stream.next().await.expect("expected one item");
        assert!(matches!(first, Err(ProviderError::ExecutionError(_))));
    }
}
