use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use pond_adapters_goose::GooseAdapter;
use pond_core::shared::domain::agent::AgentRequest;
use pond_core::shared::domain::agent::AgentStreamEvent;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::settings::SettingsRepository;
use std::sync::Arc;

struct TestSettingsRepo {
    provider: String,
    model: String,
}

#[async_trait]
impl SettingsRepository for TestSettingsRepo {
    async fn get(&self) -> Result<Settings> {
        let mut s = Settings::default();
        s.chat_provider = self.provider.clone();
        s.chat_model = self.model.clone();
        s.agent_max_turns = 5;
        Ok(s)
    }

    async fn update(&self, _settings: &Settings) -> Result<()> {
        Ok(())
    }
    async fn get_key(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_key(&self, _key: &str, _value: String) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires live Ollama agent at GIAP_OLLAMA_URL"]
async fn live_action_loop_ollama_executes_tool_call() {
    let url =
        std::env::var("GIAP_OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    std::env::set_var("OLLAMA_HOST", &url);

    let settings_repo = Arc::new(TestSettingsRepo {
        provider: "ollama".to_string(),
        model: "llama3.2".to_string(),
    });

    let adapter = GooseAdapter::new(
        settings_repo,
        Arc::new(
            pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository::default(),
        ),
        Arc::new(pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_skill::MockSkillRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_memory::MockMemoryRepository::default()),
        url,
        None,
        None, // tool_registry
    )
    .await
    .unwrap();

    let request = AgentRequest {
        message: "List registered devices".to_string(),
        session_id: "test-session".to_string(),
        model_role: "task".to_string(),
        images: Vec::new(),
        voice_mode: false,
        canvas_mode: false,
        // Household is what a test with no speaker means.
        profile_scope: ProfileScope::Household,
        profile_context: None,
        tool_group_allowlist: None,
        warmup: false,
    };

    let mut stream = adapter.chat_stream(request).await.unwrap();

    let mut saw_tool_call = false;
    let mut saw_text = false;

    while let Some(event_result) = stream.next().await {
        if let Ok(event) = event_result {
            match event {
                AgentStreamEvent::ToolCall { tool, .. } => {
                    println!("Saw tool call: {}", tool);
                    if tool.contains("device") {
                        saw_tool_call = true;
                    }
                }
                AgentStreamEvent::Text { content } => {
                    println!("Saw text: {}", content);
                    saw_text = true;
                }
                AgentStreamEvent::Error { content } => {
                    panic!("Loop returned an error: {}", content);
                }
                _ => {}
            }
        }
    }

    assert!(saw_tool_call, "Agent did not invoke any device tool");
    assert!(saw_text, "Agent did not return any text");
}

#[tokio::test]
#[ignore = "requires live Llamafile agent at GIAP_LLAMAFILE_URL"]
async fn live_action_loop_llamafile_executes_tool_call() {
    let url =
        std::env::var("GIAP_LLAMAFILE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    std::env::set_var("OLLAMA_HOST", &url);

    let settings_repo = Arc::new(TestSettingsRepo {
        provider: "llamafile".to_string(),
        model: "llamafile".to_string(),
    });

    let adapter = GooseAdapter::new(
        settings_repo,
        Arc::new(
            pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository::default(),
        ),
        Arc::new(pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_skill::MockSkillRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_memory::MockMemoryRepository::default()),
        url,
        None,
        None, // tool_registry
    )
    .await
    .unwrap();

    let request = AgentRequest {
        message: "List registered devices".to_string(),
        session_id: "test-session-llamafile".to_string(),
        model_role: "task".to_string(),
        images: Vec::new(),
        voice_mode: false,
        canvas_mode: false,
        // Household is what a test with no speaker means.
        profile_scope: ProfileScope::Household,
        profile_context: None,
        tool_group_allowlist: None,
        warmup: false,
    };

    let mut stream = adapter.chat_stream(request).await.unwrap();

    let mut saw_tool_call = false;
    let mut saw_text = false;

    while let Some(event_result) = stream.next().await {
        if let Ok(event) = event_result {
            match event {
                AgentStreamEvent::ToolCall { tool, .. } => {
                    if tool.contains("device") {
                        saw_tool_call = true;
                    }
                }
                AgentStreamEvent::Text { content: _ } => {
                    saw_text = true;
                }
                AgentStreamEvent::Error { content } => {
                    panic!("Loop returned an error: {}", content);
                }
                _ => {}
            }
        }
    }

    assert!(saw_tool_call, "Agent did not invoke any device tool");
    assert!(saw_text, "Agent did not return any text");
}

#[tokio::test]
#[ignore = "requires local GGUF setup"]
async fn live_action_loop_local_executes_tool_call() {
    let url = "http://127.0.0.1:8080".to_string();

    let settings_repo = Arc::new(TestSettingsRepo {
        provider: "local".to_string(),
        model: "".to_string(),
    });

    let adapter = GooseAdapter::new(
        settings_repo,
        Arc::new(
            pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository::default(),
        ),
        Arc::new(pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_skill::MockSkillRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_memory::MockMemoryRepository::default()),
        url,
        None,
        None, // tool_registry
    )
    .await
    .unwrap();

    let request = AgentRequest {
        message: "List registered devices".to_string(),
        session_id: "test-session-local".to_string(),
        model_role: "task".to_string(),
        images: Vec::new(),
        voice_mode: false,
        canvas_mode: false,
        // Household is what a test with no speaker means.
        profile_scope: ProfileScope::Household,
        profile_context: None,
        tool_group_allowlist: None,
        warmup: false,
    };

    let mut stream = adapter.chat_stream(request).await.unwrap();

    let mut saw_tool_call = false;
    let mut saw_text = false;

    while let Some(event_result) = stream.next().await {
        if let Ok(event) = event_result {
            match event {
                AgentStreamEvent::ToolCall { tool, .. } => {
                    if tool.contains("device") {
                        saw_tool_call = true;
                    }
                }
                AgentStreamEvent::Text { content: _ } => {
                    saw_text = true;
                }
                AgentStreamEvent::Error { content } => {
                    panic!("Loop returned an error: {}", content);
                }
                _ => {}
            }
        }
    }

    assert!(saw_tool_call, "Agent did not invoke any device tool");
    assert!(saw_text, "Agent did not return any text");
}
