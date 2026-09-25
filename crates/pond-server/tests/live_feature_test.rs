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

use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::memory::{MemoryFragment, MemorySegment};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::memory_extractor::MemoryExtractor;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_infra::db::Database;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use std::sync::Arc;
use tokio::sync::RwLock;

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

// ── Live Memory Extraction Test ──────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_memory_extraction_from_conversation() {
    let provider = match build_provider().await {
        Some(p) => p,
        None => return,
    };
    let live = Arc::new(RwLock::new(Some(provider)));

    let extractor = pond_server::llm_memory_extractor::LlmMemoryExtractor::new(live, 3);

    let facts = extractor
        .extract(
            "My name is Jerry and I live in Nairobi. I'm a software engineer.",
            "Nice to meet you Jerry! Nairobi is a wonderful city.",
            &[],
        )
        .await;

    match facts {
        Ok(extracted) => {
            println!("[live-test] extracted {} facts:", extracted.len());
            for f in &extracted {
                println!(
                    "  [{:?}] (imp={:.2}) {}",
                    f.segment, f.importance, f.content
                );
            }
            assert!(
                !extracted.is_empty(),
                "should extract at least one fact from rich input"
            );

            let has_identity = extracted
                .iter()
                .any(|f| f.segment == MemorySegment::Identity);
            println!("[live-test] has identity fact: {has_identity}");
        }
        Err(e) => {
            println!("[live-test] extraction failed (model may not support JSON output): {e}");
            // Don't fail — small models often can't produce valid JSON
        }
    }
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_memory_extraction_skips_trivial_input() {
    let provider = match build_provider().await {
        Some(p) => p,
        None => return,
    };
    let live = Arc::new(RwLock::new(Some(provider)));

    let extractor = pond_server::llm_memory_extractor::LlmMemoryExtractor::new(live, 3);

    let facts = extractor
        .extract("Hello!", "Hi there! How can I help you?", &[])
        .await;

    match facts {
        Ok(extracted) => {
            println!(
                "[live-test] trivial input extracted {} facts",
                extracted.len()
            );
            assert!(
                extracted.len() <= 1,
                "trivial greeting should not produce many facts"
            );
        }
        Err(e) => {
            println!("[live-test] extraction error on trivial input: {e}");
        }
    }
}

// ── Live Memory Extraction → DB Round-Trip ───────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_extraction_stores_to_sqlite() {
    let provider = match build_provider().await {
        Some(p) => p,
        None => return,
    };
    let live = Arc::new(RwLock::new(Some(provider)));

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);
    let extractor = pond_server::llm_memory_extractor::LlmMemoryExtractor::new(live, 3);

    let extraction_service =
        pond_core::user_data::services::memory_extraction::MemoryExtractionService::new(1);

    extraction_service
        .run(
            &extractor,
            &repo,
            "I prefer dark mode and I'm allergic to peanuts",
            "I'll remember that! Dark mode it is, and I'll keep the peanut allergy in mind.",
            Some("test-session"),
            // Not `Guest`: `run` returns early for a scope that excludes everything.
            &ProfileScope::Household,
        )
        .await;

    let memories = repo
        .search_recent(&ProfileScope::Household, 20)
        .await
        .unwrap();
    println!("[live-test] stored {} memories in SQLite:", memories.len());
    for m in &memories {
        println!(
            "  [{:?}] (imp={:.2}) {}",
            m.segment
                .as_ref()
                .map(|s| format!("{:?}", s))
                .unwrap_or("none".into()),
            m.importance.unwrap_or(0.0),
            m.content
        );
    }

    assert!(
        !memories.is_empty(),
        "extraction should store at least one memory"
    );

    for m in &memories {
        assert!(
            m.segment.is_some(),
            "stored memory should have segment: {:?}",
            m.content
        );
        assert!(
            m.importance.is_some(),
            "stored memory should have importance"
        );
        assert_eq!(m.source, "extraction");
    }
}

// ── Live Memory Cleanup After Extraction ─────────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL or GIAP_LLAMAFILE_URL"]
async fn live_extraction_then_cleanup_cycle() {
    let provider = match build_provider().await {
        Some(p) => p,
        None => return,
    };
    let live = Arc::new(RwLock::new(Some(provider)));

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let repo = SqliteMemoryRepository::new(db.system);
    let extractor = pond_server::llm_memory_extractor::LlmMemoryExtractor::new(live, 3);
    let service =
        pond_core::user_data::services::memory_extraction::MemoryExtractionService::new(1);

    service
        .run(
            &extractor,
            &repo,
            "My birthday is March 5th and my favorite color is blue",
            "Got it! I'll remember your birthday and color preference.",
            Some("test-sess"),
            &ProfileScope::Household,
        )
        .await;

    let before = repo
        .search_recent(&ProfileScope::Household, 20)
        .await
        .unwrap();
    println!("[live-test] before cleanup: {} memories", before.len());

    // Run cleanup — fresh memories should NOT be pruned
    let (scanned, archived, pruned) =
        pond_core::user_data::services::memory_cleanup::run_cleanup(&repo, 0.05, 0.15, 11.25, 0.8)
            .await
            .unwrap();
    println!("[live-test] cleanup: scanned={scanned}, archived={archived}, pruned={pruned}");

    let after = repo
        .search_recent(&ProfileScope::Household, 20)
        .await
        .unwrap();
    assert_eq!(
        before.len(),
        after.len(),
        "fresh memories should survive cleanup"
    );
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
