//! Live provider integration tests — gated by environment variables.
//!
//! These tests require real running services and are marked `#[ignore]` by default.
//! They are designed to be run manually to validate full end-to-end provider behaviour.
//!
//! # How to run
//!
//! ## Llamafile (requires a running llamafile server):
//! ```bash
//! GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
//!   cargo test -p pond-server --test live_provider_test -- --ignored llamafile
//! ```
//!
//! ## Ollama (requires `ollama serve` + a pulled model):
//! ```bash
//! GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
//!   cargo test -p pond-server --test live_provider_test -- --ignored ollama
//! ```
//!
//! ## All live tests (both services running):
//! ```bash
//! GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
//! GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
//!   cargo test -p pond-server --test live_provider_test -- --ignored
//! ```
//!
//! ## Known-good models (verified 2026-04):
//! - Llamafile: `gemma-2-2b-it.Q4_K_M.llamafile` (served on port 8080)
//! - Ollama: `gemma3:4b` or `gemma4:latest` (pulled on the local Ollama instance)
//! - GGUF: `gemma-4-E2B-it-Q4_K_M.gguf` (at `$DATA_DIR/models/gguf/`)

use futures::StreamExt;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::{LlmProvider, StreamToken};

// ── Environment helpers ────────────────────────────────────────────────────────

fn llamafile_url() -> Option<String> {
    std::env::var("GIAP_LLAMAFILE_URL").ok()
}
fn ollama_url() -> Option<String> {
    std::env::var("GIAP_OLLAMA_URL").ok()
}
fn ollama_model() -> String {
    std::env::var("GIAP_OLLAMA_MODEL").unwrap_or_else(|_| "gemma3:4b".into())
}
fn gguf_model_path() -> Option<std::path::PathBuf> {
    std::env::var("GIAP_GGUF_MODEL_PATH")
        .ok()
        .map(std::path::PathBuf::from)
}
fn llamafile_bin() -> Option<std::path::PathBuf> {
    std::env::var("GIAP_LLAMAFILE_BIN")
        .ok()
        .map(std::path::PathBuf::from)
}

// ── Llamafile live tests ───────────────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_chat_streams_tokens() {
    let url = match llamafile_url() {
        None => return,
        Some(u) => u,
    };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a concise assistant. Reply in one short sentence.",
        vec![ChatMessage::user("Hello! What is 2 + 2?")],
    );

    let mut text_tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t)) => text_tokens.push(t),
            Ok(StreamToken::Usage(_)) => {}
            Err(e) => panic!("stream error: {e}"),
        }
    }

    assert!(
        !text_tokens.is_empty(),
        "expected at least one text token from llamafile"
    );
    let full = text_tokens.join("");
    println!("llamafile replied: {full}");
    assert!(
        !full.trim().is_empty(),
        "expected non-empty response from llamafile"
    );
}

#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_streaming_yields_incremental_tokens() {
    let url = match llamafile_url() {
        None => return,
        Some(u) => u,
    };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a helpful assistant.",
        vec![ChatMessage::user("Count from 1 to 5, one number per line.")],
    );

    let mut token_count = 0;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(_)) => token_count += 1,
            Ok(StreamToken::Usage(_)) => {}
            Err(e) => panic!("stream error: {e}"),
        }
    }

    assert!(
        token_count >= 2,
        "expected multiple incremental tokens, got {token_count}"
    );
    println!("llamafile streamed {token_count} tokens");
}

#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_done_event_includes_usage() {
    let url = match llamafile_url() {
        None => return,
        Some(u) => u,
    };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a concise assistant.",
        vec![ChatMessage::user("Hello")],
    );

    let mut usage_opt = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(_)) => {}
            Ok(StreamToken::Usage(u)) => usage_opt = Some(u),
            Err(e) => panic!("stream error: {e}"),
        }
    }

    // Not all llamafile builds report usage, so log rather than fail.
    match usage_opt {
        Some(u) => {
            println!(
                "llamafile usage: prompt={} completion={}",
                u.prompt_tokens, u.completion_tokens
            );
            assert!(u.completion_tokens > 0, "completion_tokens should be > 0");
        }
        None => println!("NOTE: this llamafile build does not report usage stats"),
    }
}

#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_multi_turn_conversation() {
    let url = match llamafile_url() {
        None => return,
        Some(u) => u,
    };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let turn1 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![ChatMessage::user("My favourite colour is purple.")],
        )
        .await
        .expect("turn 1 failed");
    println!("turn 1: {}", turn1.content);

    let turn2 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![
                ChatMessage::user("My favourite colour is purple."),
                ChatMessage::assistant(turn1.content.clone()),
                ChatMessage::user("What colour did I just mention?"),
            ],
        )
        .await
        .expect("turn 2 failed");
    println!("turn 2: {}", turn2.content);

    assert!(
        turn2.content.to_lowercase().contains("purple"),
        "expected turn 2 to mention 'purple', got: {}",
        turn2.content
    );
}

// ── Ollama live tests ──────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL pointing to a running Ollama server with a pulled model"]
async fn ollama_live_chat_completes() {
    let url = match ollama_url() {
        None => return,
        Some(u) => u,
    };
    let model = ollama_model();
    let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&model));

    let reply = provider
        .complete(
            "You are a concise assistant. Reply in one sentence.",
            vec![ChatMessage::user("What is the capital of France?")],
        )
        .await
        .expect("live Ollama completion failed");

    println!("Ollama replied: {}", reply.content);
    assert!(
        !reply.content.trim().is_empty(),
        "expected non-empty response from Ollama"
    );
    assert!(
        reply.content.to_lowercase().contains("paris"),
        "expected 'Paris' in response, got: {}",
        reply.content
    );
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL pointing to a running Ollama server with a pulled model"]
async fn ollama_live_usage_reported_in_stream() {
    let url = match ollama_url() {
        None => return,
        Some(u) => u,
    };
    let model = ollama_model();
    let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&model));

    let mut stream = provider.stream_complete(
        "You are a concise assistant.",
        vec![ChatMessage::user("What is 2+2?")],
    );

    let mut text_tokens = Vec::new();
    let mut usage_opt = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t)) => text_tokens.push(t),
            Ok(StreamToken::Usage(u)) => usage_opt = Some(u),
            Err(e) => panic!("Ollama stream error: {e}"),
        }
    }

    let full = text_tokens.join("");
    println!("Ollama replied: {full}");
    assert!(
        !full.trim().is_empty(),
        "expected non-empty response from Ollama"
    );

    let usage = usage_opt.expect("Ollama should always report usage stats");
    println!(
        "Ollama usage: prompt={} completion={}",
        usage.prompt_tokens, usage.completion_tokens
    );
    assert!(usage.prompt_tokens > 0, "expected prompt_tokens > 0");
    assert!(
        usage.completion_tokens > 0,
        "expected completion_tokens > 0"
    );
}

#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL pointing to a running Ollama server with a pulled model"]
async fn ollama_live_multi_turn_conversation() {
    let url = match ollama_url() {
        None => return,
        Some(u) => u,
    };
    let model = ollama_model();
    let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&model));

    let turn1 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![ChatMessage::user("My lucky number is 42.")],
        )
        .await
        .expect("turn 1 failed");
    println!("turn 1: {}", turn1.content);

    let turn2 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![
                ChatMessage::user("My lucky number is 42."),
                ChatMessage::assistant(turn1.content.clone()),
                ChatMessage::user("What number did I just mention?"),
            ],
        )
        .await
        .expect("turn 2 failed");
    println!("turn 2: {}", turn2.content);

    assert!(
        turn2.content.contains("42"),
        "expected turn 2 to mention '42', got: {}",
        turn2.content
    );
}

// ── GGUF in-process live tests ─────────────────────────────────────

/// `new_with_data_dir` ignores `data_dir` for an absolute `.gguf` path (Goose registers it).
#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_chat_completes() {
    let path = match gguf_model_path() {
        None => return,
        Some(p) => p,
    };
    let path_str = path.to_string_lossy().into_owned();

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter::new_with_data_dir(
        &path_str,
        std::path::Path::new("/tmp"),
    )
    .await
    .expect("GGUF adapter init failed — check that the model file exists");

    let reply = adapter
        .complete(
            "You are a concise assistant. Reply in one short sentence.",
            vec![ChatMessage::user("What is 2 + 2?")],
        )
        .await
        .expect("GGUF complete() failed");

    println!("GGUF replied: {}", reply.content);
    assert!(
        !reply.content.trim().is_empty(),
        "expected non-empty GGUF response"
    );
}

#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_stream_yields_tokens() {
    let path = match gguf_model_path() {
        None => return,
        Some(p) => p,
    };
    let path_str = path.to_string_lossy().into_owned();

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter::new_with_data_dir(
        &path_str,
        std::path::Path::new("/tmp"),
    )
    .await
    .expect("GGUF adapter init failed");

    let mut stream = adapter.stream_complete(
        "You are a helpful assistant.",
        vec![ChatMessage::user("Say hello in one word.")],
    );

    let mut tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t)) => tokens.push(t),
            Ok(StreamToken::Usage(_)) => {}
            Err(e) => panic!("GGUF stream error: {e}"),
        }
    }

    assert!(
        !tokens.is_empty(),
        "expected at least one text token from GGUF"
    );
    println!(
        "GGUF streamed {} token(s): {}",
        tokens.len(),
        tokens.join("")
    );
}

#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_multi_turn_conversation() {
    let path = match gguf_model_path() {
        None => return,
        Some(p) => p,
    };
    let path_str = path.to_string_lossy().into_owned();

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter::new_with_data_dir(
        &path_str,
        std::path::Path::new("/tmp"),
    )
    .await
    .expect("GGUF adapter init failed");

    let turn1 = adapter
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![ChatMessage::user("My favourite number is 7.")],
        )
        .await
        .expect("GGUF turn 1 failed");
    println!("GGUF turn 1: {}", turn1.content);

    let turn2 = adapter
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![
                ChatMessage::user("My favourite number is 7."),
                ChatMessage::assistant(turn1.content.clone()),
                ChatMessage::user("What number did I just mention?"),
            ],
        )
        .await
        .expect("GGUF turn 2 failed");
    println!("GGUF turn 2: {}", turn2.content);

    assert!(
        turn2.content.contains("7"),
        "expected turn 2 to mention '7', got: {}",
        turn2.content
    );
}

// ── Llamafile auto-start live tests ────────────────────────────────
// Spawn logic is duplicated here: `llamafile_process` is in main.rs, out of a test's reach.

/// RAII guard — kills the llamafile child process on drop.
struct LlamafileGuard(tokio::process::Child);

impl Drop for LlamafileGuard {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// Spawn `bin` on `port` (not 8080, which prod may hold) and wait up to 60 s for it.
async fn spawn_llamafile_for_test(bin: &std::path::Path, port: u16) -> Option<LlamafileGuard> {
    let child = tokio::process::Command::new(bin)
        .arg("--server")
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .arg("--nobrowser")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let guard = LlamafileGuard(child);
    let client = reqwest::Client::new();

    for _ in 0..60u32 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if client
            .get(format!("http://127.0.0.1:{}", port))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .is_ok()
        {
            return Some(guard);
        }
    }
    None // did not become ready within 60 s
}

#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_BIN pointing to a .llamafile binary"]
async fn llamafile_autostart_chat_streams_tokens() {
    let bin = match llamafile_bin() {
        None => return,
        Some(p) => p,
    };
    let test_port: u16 = 8081;

    let _guard = spawn_llamafile_for_test(&bin, test_port)
        .await
        .expect("llamafile did not start within 60 s — check GIAP_LLAMAFILE_BIN");

    let url = format!("http://127.0.0.1:{}", test_port);
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a concise assistant. Reply in one short sentence.",
        vec![ChatMessage::user("What is 3 + 3?")],
    );

    let mut tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t)) => tokens.push(t),
            Ok(StreamToken::Usage(_)) => {}
            Err(e) => panic!("auto-start stream error: {e}"),
        }
    }

    assert!(
        !tokens.is_empty(),
        "expected tokens from auto-started llamafile"
    );
    println!("auto-started llamafile replied: {}", tokens.join(""));
}

#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_BIN pointing to a .llamafile binary"]
async fn llamafile_autostart_guard_kills_on_drop() {
    let bin = match llamafile_bin() {
        None => return,
        Some(p) => p,
    };
    let test_port: u16 = 8082;
    let client = reqwest::Client::new();

    {
        let _guard = spawn_llamafile_for_test(&bin, test_port)
            .await
            .expect("llamafile did not start");

        assert!(
            client
                .get(format!("http://127.0.0.1:{}", test_port))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .is_ok(),
            "expected llamafile to be reachable while guard is alive"
        );
    } // guard dropped here — process killed

    // Give the OS a moment to reap the process
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let still_up = client
        .get(format!("http://127.0.0.1:{}", test_port))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    assert!(!still_up, "expected llamafile to be down after guard drop");
}

// ── GGUF diagnostic: thinking-token detection ──────────────────────
// Prints only: run with `--ignored gguf_live_diagnose --nocapture` to see the output.

/// Highlight every `<` in a string so tag boundaries are visible in test output.
fn highlight_tags(s: &str) -> String {
    s.replace('<', "\n  «<").replace('>', ">»")
}

/// Prints output raw, after `strip_thinking_tokens()`, per stream chunk, and after ThoughtFilter.
#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_diagnose_thinking_tokens() {
    let path = match gguf_model_path() {
        None => return,
        Some(p) => p,
    };
    let path_str = path.to_string_lossy().into_owned();
    let model_name = path.file_name().unwrap_or_default().to_string_lossy();

    println!("\n{}", "=".repeat(70));
    println!("  GGUF THINKING TOKEN DIAGNOSTIC");
    println!("  Model: {}", model_name);
    println!("{}\n", "=".repeat(70));

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter::new_with_data_dir(
        &path_str,
        std::path::Path::new("/tmp"),
    )
    .await
    .expect("GGUF adapter init failed — check model path");

    // ── 1. Non-streaming: RAW complete() (bypass strip_thinking_tokens) ──
    println!("─── 1. RAW complete() — BEFORE strip_thinking_tokens ───");
    let raw_reply = adapter.raw_complete(
        "You are a helpful, thoughtful assistant. Think step by step before answering.",
        vec![ChatMessage::user("If a train travels at 60 mph for 2.5 hours, how far does it go? Show your reasoning.")],
    ).await;

    match raw_reply {
        Ok(reply) => {
            println!("  Raw content ({} chars):", reply.content.len());
            println!("  {}", highlight_tags(&reply.content));
            println!();
        }
        Err(e) => {
            println!(
                "  raw_complete unavailable ({}), using stripped complete()...",
                e
            );
            // Fallback: use the normal complete() which applies stripping
            let reply = adapter
                .complete(
                    "You are a helpful, thoughtful assistant. Think step by step.",
                    vec![ChatMessage::user(
                        "If a train travels at 60 mph for 2.5 hours, how far does it go?",
                    )],
                )
                .await
                .expect("GGUF complete() failed");
            println!("  Stripped content ({} chars):", reply.content.len());
            println!("  {}", highlight_tags(&reply.content));
            println!();
        }
    }

    println!("─── 1b. complete() — AFTER strip_thinking_tokens ───");
    let reply = adapter
        .complete(
            "You are a helpful, thoughtful assistant. Think step by step before answering.",
            vec![ChatMessage::user("If a train travels at 60 mph for 2.5 hours, how far does it go? Show your reasoning.")],
        )
        .await
        .expect("GGUF complete() failed");

    println!("  Stripped content ({} chars):", reply.content.len());
    println!("  {}", highlight_tags(&reply.content));
    println!();

    // complete() has already stripped; any tags left are ones strip_thinking_tokens() misses.
    let has_tags = reply.content.contains('<') && reply.content.contains('>');
    if has_tags {
        println!("  ⚠️  TAGS STILL PRESENT after strip_thinking_tokens()!");
        for (i, part) in reply.content.split('<').enumerate() {
            if i == 0 {
                continue;
            }
            if let Some(close) = part.find('>') {
                println!("  LEAKED TAG: <{}>", &part[..close]);
            }
        }
    } else {
        println!("  ✅ No tags detected in complete() output.");
    }
    println!();

    // ── 2. Streaming: stream_complete() ──────────────────────────────────
    println!("─── 2. stream_complete() — STREAMING CHUNKS ───");
    let mut stream = adapter.stream_complete(
        "You are a helpful, thoughtful assistant. Think carefully before answering.",
        vec![ChatMessage::user("What is 17 * 23? Show your work.")],
    );

    let mut chunks = Vec::new();
    let mut chunk_idx = 0u32;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t)) => {
                let display = t.replace('\n', "\\n");
                let has_angle = t.contains('<');
                let marker = if has_angle { " ⚠️" } else { "" };
                println!("  [CHUNK {:3}] {:?}{}", chunk_idx, display, marker);
                chunks.push(t);
                chunk_idx += 1;
            }
            Ok(StreamToken::Usage(u)) => {
                println!(
                    "  [USAGE] prompt={} completion={}",
                    u.prompt_tokens, u.completion_tokens
                );
            }
            Err(e) => {
                println!("  [ERROR] {}", e);
                break;
            }
        }
    }

    let raw_stream = chunks.join("");
    println!();
    println!(
        "  Full streamed text ({} chunks, {} chars):",
        chunks.len(),
        raw_stream.len()
    );
    println!("  {}", highlight_tags(&raw_stream));
    println!();

    // ── 3. Simulate ThoughtFilter on the streaming chunks ────────────────
    println!("─── 3. ThoughtFilter SIMULATION ───");
    let mut thought = pond_api::thought_filter::ThoughtFilter::new();
    let mut filtered_output = String::new();

    for (i, chunk) in chunks.iter().enumerate() {
        let visible = thought.push(chunk);
        if !visible.is_empty() {
            filtered_output.push_str(&visible);
            println!("  [FILTER {:3}] → {:?}", i, visible);
        }
    }
    let tail = thought.flush();
    if !tail.is_empty() {
        filtered_output.push_str(&tail);
        println!("  [FLUSH] → {:?}", tail);
    }

    println!();
    println!("  Filtered output ({} chars):", filtered_output.len());
    println!("  {}", &filtered_output);

    let leaked = filtered_output.contains('<') && filtered_output.contains('>');
    if leaked {
        println!();
        println!("  ⚠️  TAGS LEAKED THROUGH ThoughtFilter!");
        for (i, part) in filtered_output.split('<').enumerate() {
            if i == 0 {
                continue;
            }
            if let Some(close) = part.find('>') {
                println!("  LEAKED: <{}>", &part[..close]);
            }
        }
    } else {
        println!("  ✅ ThoughtFilter caught all tags.");
    }

    println!();
    println!("─── SUMMARY ───");
    println!("  Model: {}", model_name);
    println!("  complete() has tags: {}", has_tags);
    println!("  stream ThoughtFilter leaked: {}", leaked);
    println!("  Raw stream tags: {}", raw_stream.contains('<'));
}
