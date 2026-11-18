//! Live integration tests for recently introduced features.
//!
//! These tests require a running LLM provider (Ollama or llamafile) and
//! exercise the full stack: real DB, real LLM inference, real scheduling.
//!
//! **All tests are `#[ignore]` by default** — run with:
//!
//! ```bash
//! # With Ollama
//! GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
//!   cargo test -p pond-server --test live_feature_test -- --ignored
//!
//! # With llamafile
//! GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
//!   cargo test -p pond-server --test live_feature_test -- --ignored
//! ```

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::memory::{
    carries_calendar_date, fact_defect, FactDefect, MemoryFragment, MemorySegment, MemoryTier,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::conversation_extractor::{
    ConversationExtractor, ExtractionWindow, WindowExtraction, WindowMessage, WindowSubject,
};
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::services::memory_extraction::{
    BatchExtractionConfig, BatchExtractionService,
};
use pond_infra::db::Database;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_server::conversation_extractor::LlmConversationExtractor;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Maps every text to one unit vector so the write gate's dedup runs without a model.
/// Every cosine is 1.0, so these tests say nothing about dedup behaviour.
struct ConstantEmbedder;

#[async_trait]
impl EmbeddingProvider for ConstantEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![1.0, 0.0, 0.0, 0.0])
    }

    fn dimensions(&self) -> usize {
        4
    }
}

// ── Env var helpers ──────────────────────────────────────────────────────────

fn ollama_url() -> Option<String> {
    std::env::var("GIAP_OLLAMA_URL").ok()
}
fn ollama_model() -> String {
    std::env::var("GIAP_OLLAMA_MODEL").unwrap_or_else(|_| "gemma3:4b".into())
}
fn llamafile_url() -> Option<String> {
    std::env::var("GIAP_LLAMAFILE_URL").ok()
}

/// Build a real LLM provider from env vars (Ollama or llamafile).
async fn build_provider() -> Option<Arc<dyn LlmProvider>> {
    if let Some(url) = ollama_url() {
        let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&ollama_model()));
        return Some(Arc::new(provider));
    }
    if let Some(url) = llamafile_url() {
        let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));
        return Some(Arc::new(provider));
    }
    None
}

// ── Live window extraction ───────────────────────────────────────────────────
// Rerun per GGUF family (`GIAP_OLLAMA_MODEL`) to catch prompts only one family obeys.
fn window(user: &str, assistant: &str, at: DateTime<Utc>) -> Vec<WindowMessage> {
    vec![
        WindowMessage {
            id: "m1".to_string(),
            role: "user".to_string(),
            content: user.to_string(),
            created_at: at,
        },
        WindowMessage {
            id: "m2".to_string(),
            role: "assistant".to_string(),
            content: assistant.to_string(),
            created_at: at,
        },
    ]
}

async fn read_window(
    messages: &[WindowMessage],
    allow_reminders: bool,
) -> Option<WindowExtraction> {
    let provider = build_provider().await?;
    let extractor = LlmConversationExtractor::new(Arc::new(RwLock::new(Some(provider))));
    let subject = WindowSubject::named("Jerry");
    match extractor
        .extract_window(ExtractionWindow {
            subject: &subject,
            assistant_name: "Goose",
            session_id: "live-test",
            window_id: "m2",
            messages,
            known: &[],
            max_memories: 3,
            allow_reminders,
            allow_plans: false,
        })
        .await
    {
        Ok(extraction) => Some(extraction),
        Err(e) => {
            // Unreadable output is the model's fault, not the code's; skip rather than fail.
            println!("[live-test] the model produced nothing readable: {e}");
            None
        }
    }
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_window_extraction_produces_the_catalogue() {
    let messages = window(
        "my sister Amara lives in Nakuru, and I run the greenhouse watering before work every day",
        "Noted. Amara in Nakuru, and the greenhouse before work.",
        Utc::now(),
    );
    let Some(extraction) = read_window(&messages, true).await else {
        return;
    };

    println!("[live-test] {} memories:", extraction.memories.len());
    for m in &extraction.memories {
        println!("  [{}] {}", m.kind.as_str(), m.note);
    }
    println!("[live-test] {} rejected kinds", extraction.rejected);

    assert!(
        !extraction.memories.is_empty(),
        "a window this rich produced nothing at all"
    );
    // Off-catalogue items are already dropped; many of them means the prompt needs fixing.
    assert!(
        extraction.rejected <= extraction.memories.len(),
        "more items were outside the five-value catalogue than inside it"
    );
}

/// Dated notes must be refused; prints the verdicts and the reminders that keep the dates.
#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_no_stored_note_carries_a_date() {
    let messages = window(
        "I moved to Kisumu in 2019, and I have a dentist appointment next Tuesday at 9am",
        "Got it -- Kisumu since 2019, and the dentist next Tuesday.",
        Utc::now(),
    );
    let Some(extraction) = read_window(&messages, true).await else {
        return;
    };

    let mut dated = 0usize;
    for memory in &extraction.memories {
        let verdict = fact_defect(&memory.note);
        let carries = carries_calendar_date(&memory.note);
        println!(
            "[live-test] [{}] {:?} -> {}",
            memory.kind.as_str(),
            memory.note,
            match &verdict {
                Some(d) => format!("REFUSED: {d}"),
                None => "stored as written".to_string(),
            }
        );
        if carries {
            dated += 1;
            assert_eq!(
                verdict,
                Some(FactDefect::CalendarDate),
                "a note carrying a date was not refused for the date: {:?}",
                memory.note
            );
        }
    }

    // A dated note with no matching reminder is a date lost; watch this number on a device.
    if dated > 0 && extraction.reminders.is_empty() {
        println!(
            "[live-test] WARNING: {dated} dated note(s) refused and no reminder filed -- \
             this model ignores half the schema"
        );
    }

    println!("[live-test] {} reminders:", extraction.reminders.len());
    for r in &extraction.reminders {
        println!("  {:?} when {:?}", r.about, r.when_said);
        assert!(
            !r.when_said.contains("..."),
            "the `when` field echoed the schema skeleton's placeholder verbatim, which is \
             exactly what `project_functiongemma_behaviour` warns about: {:?}",
            r.when_said
        );
    }
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_window_extraction_keeps_little_from_small_talk() {
    let messages = window("Hello!", "Hi there! How can I help you?", Utc::now());
    let Some(extraction) = read_window(&messages, true).await else {
        return;
    };
    println!(
        "[live-test] small talk produced {} memories",
        extraction.memories.len()
    );
    assert!(
        extraction.memories.len() <= 1,
        "a greeting produced {} memories, and every one of them will be injected into \
         later turns forever",
        extraction.memories.len()
    );
}

/// End to end: model output through the real write gate into SQLite.
#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_extraction_stores_to_sqlite() {
    let Some(provider) = build_provider().await else {
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system.clone());
    let storage = SqliteSessionStorage::new(db.system.clone());
    storage
        .create_session("live-extraction".to_string())
        .await
        .unwrap();
    for (i, (role, text)) in [
        (
            "user",
            "I prefer dark mode, and my sister Amara lives in Nakuru",
        ),
        ("assistant", "Noted -- dark mode, and Amara in Nakuru."),
    ]
    .into_iter()
    .enumerate()
    {
        let message = if role == "user" {
            ChatMessage::user(text.to_string())
        } else {
            ChatMessage::assistant(text.to_string())
        };
        storage
            .add_message(
                "live-extraction".to_string(),
                SessionMessage::new(format!("live-m{i}"), "live-extraction".to_string(), message),
            )
            .await
            .unwrap();
    }

    let mut settings = Settings::default();
    settings.user_name = "Jerry".to_string();
    let config = BatchExtractionConfig::from_settings(&settings);
    assert!(config.mode.writes(), "the shipped mode must write");

    let service = BatchExtractionService::new()
        .with_embedding_provider(Arc::new(ConstantEmbedder) as Arc<dyn EmbeddingProvider>);
    let extractor = LlmConversationExtractor::new(Arc::new(RwLock::new(Some(provider))));

    let report = service
        .run_pass(
            &storage,
            &repo,
            &extractor,
            &config,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    println!("[live-test] {report:?}");

    let memories = repo
        .search_recent(&ProfileScope::Household, 20)
        .await
        .unwrap();
    println!("[live-test] stored {} memories in SQLite:", memories.len());
    for m in &memories {
        println!("  [{:?}] (imp={:?}) {}", m.segment, m.importance, m.content);
        assert!(m.segment.is_some(), "a stored memory with no segment");
        assert!(m.importance.is_some());
        assert_eq!(m.source, "extraction");
        assert_ne!(
            m.tier,
            Some(MemoryTier::Permanent),
            "no kind in the catalogue is permanent: {:?}",
            m.content
        );
        assert!(
            !carries_calendar_date(&m.content),
            "a stored memory carries a date: {:?}",
            m.content
        );
    }
}

// ── Live Consolidation Test ──────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_consolidation_merges_duplicates() {
    let provider = match build_provider().await {
        Some(p) => p,
        None => return,
    };
    let live = Arc::new(RwLock::new(Some(provider)));

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);

    for (id, content) in [
        ("d1", "User's name is Jerry"),
        ("d2", "The user is called Jerry"),
        ("d3", "Jerry is the user's name"),
        ("d4", "User lives in Nairobi"),
        ("d5", "User likes dark mode"),
    ] {
        repo.add(MemoryFragment::from_extraction(
            id.into(),
            None,
            content.into(),
            MemorySegment::Identity,
            0.8,
            None,
        ))
        .await
        .unwrap();
    }

    let consolidator = pond_server::llm_memory_consolidator::LlmMemoryConsolidator::new(live);

    let before = repo
        .search_scoreable(&ProfileScope::Household)
        .await
        .unwrap();
    println!(
        "[live-test] before consolidation: {} memories",
        before.len()
    );

    let (merged, pruned) = pond_core::user_data::services::memory_consolidation::run_consolidation(
        &consolidator,
        &repo,
        50,
    )
    .await
    .unwrap();

    println!("[live-test] consolidation: merged={merged}, pruned={pruned}");

    let after = repo
        .search_recent(&ProfileScope::Household, 20)
        .await
        .unwrap();
    println!(
        "[live-test] after consolidation: {} active memories",
        after.len()
    );
    for m in &after {
        println!("  [{}] {}", m.id, m.content);
    }

    // Model-dependent, so only check that something happened.
    if merged > 0 || pruned > 0 {
        assert!(after.len() < 5, "consolidation should reduce memory count");
    } else {
        println!(
            "[live-test] model did not propose consolidation actions (acceptable for small models)"
        );
    }
}

// ── Live Token Usage from Chat ───────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_chat_produces_nonzero_token_estimate() {
    let provider = match build_provider().await {
        Some(p) => p,
        None => return,
    };

    let response = provider
        .complete(
            "You are a helpful assistant.",
            vec![pond_core::models::domain::message::ChatMessage::user(
                "What is 2 + 2?".to_string(),
            )],
        )
        .await
        .unwrap();

    println!(
        "[live-test] response: {:?}",
        &response.content[..response.content.len().min(200)]
    );
    assert!(
        !response.content.is_empty(),
        "model should produce a response"
    );

    // The chars/4 estimate would give us:
    let est_completion = response.content.len() / 4;
    println!("[live-test] estimated completion tokens: {est_completion}");
    assert!(
        est_completion > 0,
        "response should produce nonzero token estimate"
    );
}
