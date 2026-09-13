//! #132 Milestone 4: drives a real `GooseAdapter` turn through the "mesh" arm of
//! `ensure_provider_current` with a real `MeshInferenceProvider` over mocked transport and
//! ledger, so it runs in CI unlike the `*_live_test.rs` files here. It proves that
//! `chat_provider = "mesh"` reaches a real chat turn, not just `AppState.llm_provider`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use pond_adapters_goose::GooseAdapter;
use pond_adapters_mesh_inference::MeshInferenceService;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger;
use pond_core::mesh::mocks::mock_mesh_transport::MockMeshTransport;
use pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory;
use pond_core::mesh::mocks::mock_usage_tally::MockUsageTally;
use pond_core::models::mocks::mock_provider::MockProvider;
use pond_core::shared::domain::agent::{AgentRequest, AgentStreamEvent};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::settings::SettingsRepository;

struct MeshSettingsRepo;

#[async_trait]
impl SettingsRepository for MeshSettingsRepo {
    async fn get(&self) -> anyhow::Result<Settings> {
        let mut s = Settings::default();
        s.chat_provider = "mesh".to_string();
        s.agent_max_turns = 5;
        Ok(s)
    }
    async fn update(&self, _settings: &Settings) -> anyhow::Result<()> {
        Ok(())
    }
    async fn get_key(&self, _key: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    async fn set_key(&self, _key: &str, _value: String) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_chat_turn_over_an_unavailable_mesh_surfaces_a_clean_error() {
    // No trusted peer configured — deterministically exercises the
    // "no trusted, connected, funded mesh peer is available" path,
    // mirroring exactly what a fresh Pond with mesh_enabled=true but no
    // Circle yet would see on a real chat turn.
    let mesh_service = MeshInferenceService::spawn(
        Arc::new(MockMeshTransport::new(PeerId::from([1u8; 32]))),
        Arc::new(MockPeerDirectory::new()),
        Arc::new(MockCreditLedger::new()),
        Arc::new(MockUsageTally::new()),
        Arc::new(MeshSettingsRepo),
        Arc::new(MockProvider::new()),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(15 * 60),
        None,
    );
    let mesh_provider: Arc<dyn pond_core::models::ports::provider::LlmProvider> =
        Arc::new(mesh_service.provider());

    let adapter = GooseAdapter::new(
        Arc::new(MeshSettingsRepo),
        Arc::new(
            pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository::default(),
        ),
        Arc::new(pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_skill::MockSkillRepository::default()),
        Arc::new(pond_core::user_data::mocks::mock_memory::MockMemoryRepository::default()),
        "http://127.0.0.1:8080".to_string(),
        None,
        None, // tool_registry
    )
    .await
    .unwrap()
    .with_mesh_provider(Arc::new(tokio::sync::RwLock::new(Some(mesh_provider))));

    let request = AgentRequest {
        message: "hello".to_string(),
        session_id: "mesh-test-session".to_string(),
        model_role: "chat".to_string(),
        images: Vec::new(),
        voice_mode: false,
        canvas_mode: false,
        profile_scope: ProfileScope::Household,
        profile_context: None,
        // Not a recipe run, so nothing restricts this turn's tool groups.
        tool_group_allowlist: None,
        warmup: false,
    };

    let mut stream = adapter.chat_stream(request).await.unwrap();

    let mut saw_mesh_error = false;
    while let Some(event_result) = stream.next().await {
        match event_result {
            // Goose's own agent loop catches a provider error and surfaces it
            // as a normal assistant Text event ("Ran into this error: ...
            // Please retry..."), not a distinct Error event — confirmed by
            // running this test and observing the real event stream.
            Ok(AgentStreamEvent::Text { content }) => {
                assert!(
                    content.to_lowercase().contains("mesh")
                        || content.to_lowercase().contains("peer"),
                    "expected the mesh unavailability error to surface honestly, \
                     not a wrong-provider answer — got: {content}"
                );
                saw_mesh_error = true;
            }
            Ok(AgentStreamEvent::Error { content }) => {
                assert!(
                    content.to_lowercase().contains("mesh")
                        || content.to_lowercase().contains("peer"),
                    "expected the mesh unavailability error to surface, got: {content}"
                );
                saw_mesh_error = true;
            }
            _ => {}
        }
    }

    assert!(
        saw_mesh_error,
        "expected the mesh-unavailable error to surface through the chat stream, \
         as either a Text or Error event"
    );
}
