use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::{MessageStream, Provider, ProviderUsage};
use goose::session::SessionManager;
use goose_providers::errors::ProviderError;
use goose_providers::model::ModelConfig;
use rmcp::model::Tool;
use std::sync::Arc;

/// A mock provider that simply prints out the payloads it receives
/// and returns a mock tool call request to simulate the agent hallucinating or using a tool.
struct MockInterceptProvider;

#[async_trait]
impl Provider for MockInterceptProvider {
    fn get_name(&self) -> &str {
        "mock_intercept"
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        println!("=== MOCK PROVIDER RECEIVED PAYLOAD ===");
        println!("Model: {}", model_config.model_name);
        println!("System Prompt:\n{}\n", system);

        println!("Tools:");
        for t in tools {
            println!("- {}: {}", t.name, t.description.as_deref().unwrap_or(""));
        }

        println!("\nMessages:");
        for m in messages {
            println!("Role: {:?}", m.role);
            for c in &m.content {
                println!("  {:?}", c);
            }
        }
        println!("======================================\n");

        // We can simulate an empty tool call hallucination here,
        // or just return a dummy response.
        let msg = Message::assistant().with_text(
            "I am a mock response. The real LLM would process the above tools and system prompt.",
        );
        let usage = ProviderUsage::new(
            model_config.model_name.clone(),
            goose::providers::base::Usage::default(),
        );

        Ok(goose::providers::base::stream_from_single_message(
            msg, usage,
        ))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // tracing_subscriber::fmt::init(); // Omitted because it's not a dependency

    let session_manager = Arc::new(SessionManager::instance());
    let permission_manager = goose::config::permission::PermissionManager::instance();

    // Try GooseMode::Auto which is what the fix introduces
    let config = AgentConfig::new(
        session_manager.clone(),
        permission_manager,
        None,
        GooseMode::Auto,
        false,
        GoosePlatform::GooseCli,
    );

    let agent = Arc::new(GooseAgent::with_config(config));

    // Create session
    let session = session_manager
        .create_session(
            std::env::current_dir().unwrap_or_default(),
            "test-session".to_string(),
            goose::session::session_manager::SessionType::User,
            GooseMode::Auto,
        )
        .await?;
    let session_id = session.id.clone();

    // Remove built-in extensions just like the fix
    agent.remove_extension("developer", &session_id).await.ok();
    agent
        .remove_extension("computercontroller", &session_id)
        .await
        .ok();

    // Add the Playwright MCP extension via npx
    let playwright_extension =
        ExtensionConfig::stdio("playwright", "npx", "Playwright browser automation", 60u64)
            .with_args(vec!["-y", "@playwright/mcp"]);

    println!(
        "Adding Playwright MCP extension... (this may take a few seconds to download via npx)"
    );
    agent
        .add_extension(playwright_extension, &session_id)
        .await?;

    // Inject our mock provider!
    let provider = MockInterceptProvider;
    agent
        .update_provider(
            Arc::new(provider),
            ModelConfig::new("mock_model"),
            &session_id,
        )
        .await?;

    // Override the system prompt like `GooseAdapter` does
    agent
        .override_system_prompt("You are a helpful assistant.".to_string())
        .await;

    // Send a message
    let user_msg = Message::user().with_text("Navigate to google.com and take a screenshot.");
    let session_cfg = goose::agents::types::SessionConfig {
        id: session_id,
        schedule_id: None,
        max_turns: Some(5),
        retry_config: None,
    };

    println!("Starting agent reply stream...\n");
    let mut stream = agent.reply(user_msg, session_cfg, None).await?;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                match event {
                    goose::agents::AgentEvent::Message(msg) => {
                        println!("Agent Event -> Message Content:");
                        for c in &msg.content {
                            println!("{:?}", c);
                        }
                    }
                    _ => {} // Ignore other events like HistoryReplaced
                }
            }
            Err(e) => {
                eprintln!("Stream Error: {}", e);
            }
        }
    }

    Ok(())
}
