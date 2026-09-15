//! [`InferenceProvider`] implementation for [`LlamaCppEngine`].
//!
//! The generation loop runs inside `tokio::task::spawn_blocking` because
//! llama-cpp-2 calls are blocking CPU/GPU operations. A `tokio::sync::mpsc`
//! channel bridges the blocking thread to the async [`ChatEventStream`].

use crate::engine::{LlamaCppEngine, ModelSlot};
use crate::memory::effective_context_size;
use crate::sampling::build_sampler;
use crate::tool_calling;

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{AddBos, ChatTemplateResult, GrammarTrigger, GrammarTriggerType};
use llama_cpp_2::openai::OpenAIChatTemplateParams;
use llama_cpp_2::sampling::LlamaSampler;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::inference::{
    ChatEvent, ChatEventStream, InferenceOptions, InferenceProvider, ToolDefinition,
};
use pond_core::models::ports::provider::UsageStats;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

#[async_trait]
impl InferenceProvider for LlamaCppEngine {
    fn stream_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> ChatEventStream {
        let (tx, rx) = mpsc::channel::<Result<ChatEvent>>(64);

        // Clone everything we need to move into the blocking task.
        let system_prompt = system_prompt.to_string();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let options = options.clone();
        let model_slot = self.model_slot();
        let backend = self.backend_arc();

        tokio::task::spawn_blocking(move || {
            generation_task(
                model_slot,
                backend,
                system_prompt,
                messages,
                tools,
                &options,
                tx,
            );
        });

        Box::pin(ReceiverStream { rx })
    }

    fn model_name(&self) -> String {
        // This is sync; use try_lock to avoid blocking.
        self.model_slot()
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.model_id.clone()))
            .unwrap_or_else(|| "none".to_string())
    }

    fn capabilities(&self) -> ModelCapabilities {
        // Use the lock-free capabilities cache. This avoids contending with
        // the model mutex, which is held for the entire duration of generation.
        // The old try_lock() approach silently returned defaults (tool_calling=false)
        // whenever a generation task was in progress.
        self.cached_capabilities()
    }
}

/// Blocking generation task that runs inside `spawn_blocking`.
///
/// Acquires the model lock, builds the prompt, creates a context, and runs
/// the autoregressive generation loop, sending [`ChatEvent`]s through the
/// channel.
///
/// When `cache_path` is provided, the context state is saved after generation
/// and loaded before the next call. Prefix matching skips re-decoding tokens
/// already in the KV cache, saving 5-15s on Jetson for stable system prompts.
fn generation_task(
    model_slot: ModelSlot,
    backend: Arc<LlamaBackend>,
    system_prompt: String,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    options: &InferenceOptions,
    tx: mpsc::Sender<Result<ChatEvent>>,
) {
    let gen_start = std::time::Instant::now();

    // Acquire the model lock (blocking). Mutable for in-memory KV-cache persistence.
    let mut model_guard = model_slot.blocking_lock();
    let Some(loaded) = model_guard.as_mut() else {
        let _ = tx.blocking_send(Err(anyhow::anyhow!("no model loaded")));
        return;
    };

    // ── Build the prompt ─────────────────────────────────────────────────

    let oai_messages_json = build_openai_messages_json(&system_prompt, &messages);
    let compact_tools = options
        .compact_tools_json_override
        .clone()
        .or_else(|| tool_calling::compact_tools_json(&tools));

    // On small-context platforms (Jetson ≤4096), full tool schemas always exceed
    // the budget. Skip directly to compact to avoid wasting time on serialization,
    // Jinja rendering, and tokenization that will be discarded immediately.
    let n_ctx_train = loaded.model.n_ctx_train() as usize;
    let use_compact_directly = n_ctx_train <= 4096;

    // Prefer pre-formatted JSON from the dispatcher (matches Goose's format_tools() exactly).
    // Fall back to re-serializing ToolDefinition objects only when no override is provided.
    let full_tools_json = if let Some(ref override_json) = options.tools_json_override {
        tracing::info!(
            schema_len = override_json.len(),
            tools = tools.len(),
            "using pre-formatted tools_json from dispatcher (Goose-compatible)"
        );
        Some(override_json.clone())
    } else if tools.is_empty() {
        tracing::warn!("no tools passed to generation_task — model will have no tool schemas");
        None
    } else if use_compact_directly {
        tracing::debug!(
            n_ctx_train,
            tools = tools.len(),
            "small context — skipping full tool schema serialization"
        );
        None
    } else {
        let json = tool_calling::tools_to_json(&tools);
        tracing::info!(
            tools = tools.len(),
            schema_len = json.as_ref().map(|j| j.len()).unwrap_or(0),
            n_ctx_train,
            "full tool schemas serialized for Jinja template"
        );
        json
    };

    let template_result = match apply_template(
        &loaded.model,
        &loaded.chat_template,
        &oai_messages_json,
        full_tools_json.as_deref(),
        compact_tools.as_deref(),
        options.enable_thinking,
    ) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.blocking_send(Err(e));
            return;
        }
    };

    let prompt = &template_result.prompt;
    let additional_stops = &template_result.additional_stops;

    // Dump full rendered prompt to file when GIAP_DUMP_PROMPT is set.
    // This lets you see exactly what the model receives (system prompt +
    // tool declarations + user message) rendered through the Jinja template.
    if std::env::var("GIAP_DUMP_PROMPT").is_ok() {
        let dump_val = std::env::var("GIAP_DUMP_PROMPT").unwrap_or_default();
        let dump_path = if dump_val == "1" || dump_val.is_empty() {
            "/tmp/giap-rendered-prompt.txt".to_string()
        } else {
            dump_val
        };
        if let Err(e) = std::fs::write(&dump_path, prompt) {
            tracing::warn!(path = %dump_path, error = %e, "failed to dump rendered prompt");
        } else {
            tracing::info!(
                path = %dump_path,
                prompt_len = prompt.len(),
                tools_count = tools.len(),
                "rendered prompt dumped to file"
            );
        }
    }

    // Debug: log the formatted prompt and tool state so we can diagnose
    // whether the chat template is including tools properly.
    tracing::debug!(
        tools_count = tools.len(),
        has_tools_json = full_tools_json.is_some(),
        additional_stops_count = additional_stops.len(),
        has_grammar = template_result.grammar.is_some(),
        grammar_lazy = template_result.grammar_lazy,
        grammar_triggers_count = template_result.grammar_triggers.len(),
        prompt_len = prompt.len(),
        "template applied"
    );
    if !additional_stops.is_empty() {
        tracing::debug!(stops = ?additional_stops, "additional stop sequences");
    }
    if let Some(ref grammar) = template_result.grammar {
        let preview = if grammar.len() > 200 {
            &grammar[..200]
        } else {
            grammar.as_str()
        };
        tracing::debug!(grammar_preview = %preview, "tool-call grammar from template");
    }
    if !template_result.grammar_triggers.is_empty() {
        let trigger_values: Vec<&str> = template_result
            .grammar_triggers
            .iter()
            .map(|t| t.value.as_str())
            .collect();
        tracing::debug!(triggers = ?trigger_values, "grammar triggers");
    }
    // Log the last 500 chars of the prompt to see if tools are included.
    let prompt_tail = if prompt.len() > 500 {
        &prompt[prompt.len() - 500..]
    } else {
        prompt.as_str()
    };
    tracing::debug!(prompt_tail = %prompt_tail, "prompt tail (last 500 chars)");

    // ── Tokenize ─────────────────────────────────────────────────────────

    let tokens = match loaded.model.str_to_token(prompt, AddBos::Never) {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.blocking_send(Err(anyhow::anyhow!("tokenization failed: {}", e)));
            return;
        }
    };

    let prompt_token_count = tokens.len();
    let ctx_size = effective_context_size(&loaded.model, prompt_token_count);

    tracing::info!(
        target: "giap::trace",
        kind = "inference_start",
        model = %loaded.model_id,
        prompt_tokens = prompt_token_count,
        ctx_size,
    );

    if prompt_token_count >= ctx_size {
        let _ = tx.blocking_send(Err(anyhow::anyhow!(
            "prompt ({} tokens) exceeds context limit ({} tokens)",
            prompt_token_count,
            ctx_size
        )));
        return;
    }

    // ── In-memory KV-cache reuse ───────────────────────────────────────────
    // Try to reuse the persistent context from the previous turn. If the prefix
    // matches, we skip re-decoding thousands of tokens (system prompt + tools).
    // If no cached context exists (first call), create a fresh one.

    // Helper: create a fresh context sized for the current prompt.
    let create_fresh_ctx = |model: &llama_cpp_2::model::LlamaModel,
                            backend: &LlamaBackend,
                            ctx_size: usize|
     -> Result<llama_cpp_2::context::LlamaContext<'static>> {
        let mut params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(ctx_size as u32));
        params = params.with_n_batch(512);
        params = params.with_flash_attention_policy(1);
        let ctx = model
            .new_context(backend, params)
            .map_err(|e| anyhow::anyhow!("failed to create context: {}", e))?;
        // SAFETY: Context borrows from model which lives in the same LoadedModel struct.
        // We guarantee it's dropped before the model (see LoadedModel docs).
        Ok(unsafe { std::mem::transmute(ctx) })
    };

    // Check if we can reuse the persistent context.
    let (mut ctx, tokens_to_decode_start) = if let Some(ref cached) = loaded.cached_ctx {
        // Check if the new prompt fits in the cached context's allocation.
        // The context was sized for an earlier (smaller) prompt — if the conversation
        // grew beyond it, we must create a fresh context with the right size.
        // Reserve 512 tokens for generation headroom.
        let cached_n_ctx = cached.ctx.n_ctx() as usize;
        if tokens.len() + 512 > cached_n_ctx {
            tracing::info!(
                prompt_tokens = tokens.len(),
                cached_n_ctx,
                new_ctx_size = ctx_size,
                "KV cache too small for current prompt — creating larger context"
            );
            loaded.cached_ctx = None;
            match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
                Ok(ctx) => (ctx, 0),
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        } else {
            // Context fits — check prefix match.
            let action = crate::kv_cache::plan_cache_reuse(&cached.tokens_in_cache, &tokens);
            match action {
                crate::kv_cache::CacheAction::FullHit => {
                    tracing::info!(
                        cached = cached.tokens_in_cache.len(),
                        "KV cache full hit (in-memory) — skipping all prefill"
                    );
                    let ctx = loaded.cached_ctx.take().unwrap().ctx;
                    (ctx, tokens.len())
                }
                crate::kv_cache::CacheAction::IncrementalDecode {
                    trim_from,
                    decode_from,
                    tokens_saved,
                } => {
                    let mut ctx = loaded.cached_ctx.take().unwrap().ctx;
                    let _ = ctx.clear_kv_cache_seq(None, Some(trim_from as u32), None);
                    tracing::info!(
                        tokens_saved,
                        decode_from,
                        total = tokens.len(),
                        "KV cache partial hit (in-memory) — decoding only delta"
                    );
                    (ctx, decode_from)
                }
                crate::kv_cache::CacheAction::FullPrefill => {
                    tracing::info!("KV cache miss (in-memory, no prefix match) — full prefill");
                    loaded.cached_ctx = None;
                    match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
                        Ok(ctx) => (ctx, 0),
                        Err(e) => {
                            let _ = tx.blocking_send(Err(e));
                            return;
                        }
                    }
                }
            }
        }
    } else {
        tracing::info!("KV cache cold start (in-memory) — creating fresh context");
        match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
            Ok(ctx) => (ctx, 0),
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        }
    };

    let tokens_to_decode = &tokens[tokens_to_decode_start..];

    // Prefill tokens in batches (only the delta when cache hit).
    // If decode fails (NoKvCacheSlot), drop the cache and retry with a fresh context.
    if !tokens_to_decode.is_empty() {
        let n_batch = ctx.n_batch() as usize;
        let mut decode_failed = false;
        for chunk in tokens_to_decode.chunks(n_batch) {
            let mut batch = match LlamaBatch::get_one(chunk) {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx.blocking_send(Err(anyhow::anyhow!("batch creation failed: {}", e)));
                    return;
                }
            };
            if let Err(e) = ctx.decode(&mut batch) {
                tracing::warn!("decode failed ({}), retrying with fresh context", e);
                decode_failed = true;
                break;
            }
        }
        // Retry: create fresh context and full prefill if cached decode failed.
        if decode_failed {
            drop(ctx);
            ctx = match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            };
            let n_batch = ctx.n_batch() as usize;
            for chunk in tokens.chunks(n_batch) {
                let mut batch = match LlamaBatch::get_one(chunk) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ =
                            tx.blocking_send(Err(anyhow::anyhow!("batch creation failed: {}", e)));
                        return;
                    }
                };
                if let Err(e) = ctx.decode(&mut batch) {
                    let _ = tx.blocking_send(Err(anyhow::anyhow!("prefill decode failed: {}", e)));
                    return;
                }
            }
        }
    }

    // ── Generation loop with streaming parser (matches Goose's approach) ──
    //
    // Uses llama-cpp-2's ChatParseStateOaicompat to parse tool calls from the
    // model's native format (e.g. Gemma 4's <|tool_call>call:NAME{...}<tool_call|>).
    // This is the SAME parser Goose uses — it handles all the native escape
    // formats correctly, including <|"|> string delimiters.

    let mut sampler = build_sampler(options.temperature);

    // Initialize the streaming parser from the template result.
    // This understands the model's chat format and extracts structured deltas
    // (content, reasoning_content, tool_calls) from raw token output.
    let mut stream_parser = match template_result.streaming_state_oaicompat() {
        Ok(parser) => {
            tracing::debug!(
                "streaming parser initialized (same as Goose's native tool call parser)"
            );
            Some(parser)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to init streaming parser — falling back to manual parsing");
            None
        }
    };

    // Feed the generation prompt to the parser so it knows the context.
    if let Some(ref mut parser) = stream_parser {
        if !template_result.generation_prompt.is_empty() {
            let _ = parser.update(&template_result.generation_prompt, true);
        }
    }

    let max_output = if let Some(max) = options.max_tokens {
        ctx_size
            .saturating_sub(prompt_token_count)
            .min(max as usize)
    } else {
        ctx_size.saturating_sub(prompt_token_count)
    };

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut generated_text = String::new();
    let mut output_token_count: u32 = 0;
    let mut ttft_ms: Option<u64> = None;
    // Accumulate RAW tool call deltas — merge by index after generation completes.
    // Arguments arrive as partial strings across multiple deltas and must be
    // concatenated before JSON parsing (same approach as Goose).
    let mut raw_tool_deltas: Vec<serde_json::Value> = Vec::new();

    for _ in 0..max_output {
        let token = sampler.sample(&ctx, -1);
        sampler.accept(token);

        if loaded.model.is_eog_token(token) {
            break;
        }

        output_token_count += 1;
        if output_token_count == 1 {
            ttft_ms = Some(gen_start.elapsed().as_millis() as u64);
        }

        let piece = match loaded.model.token_to_piece(token, &mut decoder, true, None) {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.blocking_send(Err(anyhow::anyhow!("token decode failed: {}", e)));
                break;
            }
        };

        if !piece.is_empty() {
            generated_text.push_str(&piece);

            if let Some(ref mut parser) = stream_parser {
                // Feed token to the streaming parser (same as Goose).
                match parser.update(&piece, true) {
                    Ok(deltas) => {
                        for delta_json in deltas {
                            if let Ok(delta) =
                                serde_json::from_str::<serde_json::Value>(&delta_json)
                            {
                                // Stream text content immediately.
                                if let Some(content) = delta.get("content").and_then(|v| v.as_str())
                                {
                                    if !content.is_empty() {
                                        let _ = tx.blocking_send(Ok(ChatEvent::Text(
                                            content.to_string(),
                                        )));
                                    }
                                }
                                // Accumulate tool call deltas — DON'T parse args yet.
                                // Arguments arrive as partial strings across deltas.
                                if let Some(tool_calls) =
                                    delta.get("tool_calls").and_then(|v| v.as_array())
                                {
                                    for tc in tool_calls {
                                        raw_tool_deltas.push(tc.clone());
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "streaming parser error");
                    }
                }
            } else {
                // Fallback: stream text with safe boundary (old approach).
                let stream_up_to = tool_calling::safe_stream_end(&generated_text);
                let streamed_so_far = generated_text.len() - piece.len();
                if stream_up_to > streamed_so_far {
                    #[allow(clippy::string_slice)]
                    let new_text = &generated_text[streamed_so_far..stream_up_to];
                    if !new_text.is_empty() {
                        let _ = tx.blocking_send(Ok(ChatEvent::Text(new_text.to_string())));
                    }
                }
            }

            // Check additional stop sequences from the template.
            let should_stop = additional_stops
                .iter()
                .any(|stop| generated_text.ends_with(stop));
            if should_stop {
                break;
            }
        }

        // Decode next token.
        let next_tokens = [token];
        let mut next_batch = match LlamaBatch::get_one(&next_tokens) {
            Ok(b) => b,
            Err(e) => {
                let _ = tx.blocking_send(Err(anyhow::anyhow!("batch creation failed: {}", e)));
                break;
            }
        };
        if let Err(e) = ctx.decode(&mut next_batch) {
            let _ = tx.blocking_send(Err(anyhow::anyhow!("decode failed: {}", e)));
            break;
        }
    }

    // ── Finalize: flush parser and emit tool calls ──────────────────────

    let total_latency_ms = gen_start.elapsed().as_millis() as u64;
    tracing::info!(
        target: "giap::trace",
        kind = "inference_end",
        model = %loaded.model_id,
        prompt_tokens = prompt_token_count,
        output_tokens = output_token_count,
        ttft_ms = ttft_ms.unwrap_or(0),
        total_latency_ms,
    );
    tracing::debug!(
        generated_len = generated_text.len(),
        output_tokens = output_token_count,
        "generation complete"
    );
    let output_preview = if generated_text.len() > 300 {
        &generated_text[..300]
    } else {
        &generated_text
    };
    tracing::debug!(output = %output_preview, "raw model output (first 300 chars)");

    if let Some(ref mut parser) = stream_parser {
        // Finalize with is_partial=false to flush any remaining deltas.
        if let Ok(final_deltas) = parser.update("", false) {
            for delta_json in final_deltas {
                if let Ok(delta) = serde_json::from_str::<serde_json::Value>(&delta_json) {
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        if !content.is_empty() {
                            let _ = tx.blocking_send(Ok(ChatEvent::Text(content.to_string())));
                        }
                    }
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tool_calls {
                            raw_tool_deltas.push(tc.clone());
                        }
                    }
                }
            }
        }

        // Merge accumulated deltas by index — concatenate argument strings,
        // THEN parse the complete JSON. Same as Goose's extract_oai_tool_call_contents.
        if !raw_tool_deltas.is_empty() {
            let mut merged: std::collections::BTreeMap<u64, (String, String, String)> =
                std::collections::BTreeMap::new();

            for delta in &raw_tool_deltas {
                let index = delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let entry = merged
                    .entry(index)
                    .or_insert_with(|| (String::new(), String::new(), String::new()));

                if let Some(id) = delta.get("id").and_then(|v| v.as_str()) {
                    if !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                }
                if let Some(func) = delta.get("function") {
                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                        if !name.is_empty() {
                            entry.1 = name.to_string();
                        }
                    }
                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                        entry.2.push_str(args); // ACCUMULATE, don't parse yet
                    }
                }
            }

            let tool_count = merged.values().filter(|(_, n, _)| !n.is_empty()).count();
            tracing::debug!(
                deltas = raw_tool_deltas.len(),
                tools = tool_count,
                "tool call deltas merged by index"
            );

            for (_, (id, name, args_str)) in merged {
                if name.is_empty() {
                    continue;
                }
                let call_id = if id.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    id
                };
                let arguments: serde_json::Value = if args_str.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&args_str).unwrap_or_else(|e| {
                        tracing::warn!(
                            tool = %name,
                            args = %args_str,
                            error = %e,
                            "failed to parse merged tool arguments"
                        );
                        serde_json::json!({})
                    })
                };
                tracing::debug!(tool = %name, args = %arguments, "emitting tool call");
                let _ = tx.blocking_send(Ok(ChatEvent::ToolCall {
                    id: call_id,
                    name,
                    arguments,
                }));
            }
        }
    } else {
        // Fallback: manual parsing (for models without streaming parser support).
        let tool_calls = tool_calling::parse_tool_calls(&generated_text);
        if !tool_calls.is_empty() {
            for tc in tool_calls {
                let _ = tx.blocking_send(Ok(ChatEvent::ToolCall {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: tc.name,
                    arguments: tc.arguments,
                }));
            }
        }
    }

    // ── Persist context in memory for next turn ─────────────────────────────
    // Store the context + token log back into LoadedModel so the next call
    // can skip re-prefilling the stable prefix. Zero disk I/O.
    loaded.cached_ctx = Some(crate::engine::CachedInferenceContext {
        ctx,
        tokens_in_cache: tokens,
    });
    tracing::debug!(
        tokens_cached = loaded.cached_ctx.as_ref().unwrap().tokens_in_cache.len(),
        "KV cache persisted in memory for next turn"
    );

    // Emit usage stats.
    let _ = tx.blocking_send(Ok(ChatEvent::Usage(UsageStats {
        prompt_tokens: prompt_token_count as u32,
        completion_tokens: output_token_count,
        // The oaicompat delta loop here reads `content` and `tool_calls` only —
        // `reasoning_content` is documented as parsed and is not consumed. This
        // engine feeds the quarantined PondAgent loop (Q2-05), so PAI-5 P2 left
        // it alone rather than counting a channel nothing reads.
        reasoning_tokens: None,
    })));
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Apply the chat template using the OpenAI-compat API with jinja.
///
/// Tries full tool schema first; falls back to compact (name+description only)
/// if the full version fails or exceeds the token budget.
///
/// Returns the full [`ChatTemplateResult`] which includes the rendered prompt,
/// grammar constraints for tool calling, additional stop sequences, and trigger
/// information for lazy grammar sampling.
fn apply_template(
    model: &llama_cpp_2::model::LlamaModel,
    template: &llama_cpp_2::model::LlamaChatTemplate,
    messages_json: &str,
    full_tools_json: Option<&str>,
    compact_tools: Option<&str>,
    enable_thinking: bool,
) -> Result<ChatTemplateResult> {
    let apply = |tools: Option<&str>| {
        let params = OpenAIChatTemplateParams {
            messages_json,
            tools_json: tools,
            tool_choice: None,
            json_schema: None,
            grammar: None,
            reasoning_format: if enable_thinking { Some("auto") } else { None },
            chat_template_kwargs: None,
            add_generation_prompt: true,
            use_jinja: true,
            parallel_tool_calls: false,
            enable_thinking,
            add_bos: false,
            add_eos: false,
            parse_tool_calls: true,
        };
        model.apply_chat_template_oaicompat(template, &params)
    };

    // Try full tools first.
    match apply(full_tools_json) {
        Ok(result) => {
            // Check token count -- if too large, fall back to compact.
            let token_count = model
                .str_to_token(&result.prompt, AddBos::Never)
                .map(|t| t.len())
                .unwrap_or(0);
            let n_ctx_train = model.n_ctx_train() as usize;
            if token_count > n_ctx_train.saturating_sub(512) {
                tracing::info!(
                    token_count,
                    n_ctx_train,
                    "full tool schema exceeds budget, trying compact"
                );
                match apply(compact_tools) {
                    Ok(r) => Ok(r),
                    Err(_) => Ok(result),
                }
            } else {
                Ok(result)
            }
        }
        Err(e) => {
            tracing::warn!("full template failed: {}, trying compact", e);
            apply(compact_tools)
                .map_err(|e2| anyhow::anyhow!("chat template failed: {} (compact: {})", e, e2))
        }
    }
}

/// Build a lazy grammar sampler from the template's grammar and triggers.
///
/// Lazy grammar sampling means the grammar constraints only activate when
/// specific trigger words/tokens/patterns are encountered in the output.
/// This allows the model to generate free-form text normally, and only
/// constrains output to valid tool-call JSON when a tool-call pattern starts.
///
/// The trigger classification:
/// - `GrammarTriggerType::Word` -> passed as `trigger_words` to `grammar_lazy()`
/// - `GrammarTriggerType::Token` -> passed as `trigger_tokens` to `grammar_lazy()`
/// - `GrammarTriggerType::Pattern` / `PatternFull` -> passed as patterns to
///   `grammar_lazy_patterns()`
#[allow(dead_code)]
fn build_grammar_sampler_lazy(
    model: &llama_cpp_2::model::LlamaModel,
    grammar_str: &str,
    triggers: &[GrammarTrigger],
) -> Result<LlamaSampler> {
    let mut word_triggers: Vec<Vec<u8>> = Vec::new();
    let mut token_triggers: Vec<llama_cpp_2::token::LlamaToken> = Vec::new();
    let mut pattern_triggers: Vec<String> = Vec::new();

    for trigger in triggers {
        match trigger.trigger_type {
            GrammarTriggerType::Word => {
                word_triggers.push(trigger.value.as_bytes().to_vec());
            }
            GrammarTriggerType::Token => {
                if let Some(tok) = trigger.token {
                    token_triggers.push(tok);
                }
            }
            GrammarTriggerType::Pattern | GrammarTriggerType::PatternFull => {
                pattern_triggers.push(trigger.value.clone());
            }
        }
    }

    // Use pattern-based lazy grammar if any regex patterns are present.
    if !pattern_triggers.is_empty() {
        tracing::debug!(
            patterns = ?pattern_triggers,
            token_count = token_triggers.len(),
            "building lazy grammar sampler with regex patterns"
        );
        LlamaSampler::grammar_lazy_patterns(
            model,
            grammar_str,
            "root",
            &pattern_triggers,
            &token_triggers,
        )
        .map_err(|e| anyhow::anyhow!("lazy grammar (patterns) failed: {}", e))
    } else if !word_triggers.is_empty() || !token_triggers.is_empty() {
        tracing::debug!(
            word_count = word_triggers.len(),
            token_count = token_triggers.len(),
            "building lazy grammar sampler with word/token triggers"
        );
        LlamaSampler::grammar_lazy(
            model,
            grammar_str,
            "root",
            word_triggers.iter().map(|w| w.as_slice()),
            &token_triggers,
        )
        .map_err(|e| anyhow::anyhow!("lazy grammar (words) failed: {}", e))
    } else {
        // No triggers provided -- fall back to always-on grammar.
        tracing::debug!("no grammar triggers, using strict grammar");
        LlamaSampler::grammar(model, grammar_str, "root")
            .map_err(|e| anyhow::anyhow!("strict grammar failed: {}", e))
    }
}

/// Build OpenAI-compatible messages JSON array.
///
/// Emits `tool_calls` on assistant messages and `tool_call_id` on tool messages
/// per the OpenAI tools API. This is required for multi-turn tool round-tripping
/// — without it the model only sees a flat history and forgets it called a tool.
fn build_openai_messages_json(system_prompt: &str, messages: &[ChatMessage]) -> String {
    let mut arr: Vec<serde_json::Value> = vec![serde_json::json!({
        "role": "system",
        "content": system_prompt
    })];

    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            // Tool results use "tool" role so the model recognizes them as
            // tool responses (not user input). This prevents the model from
            // asking follow-up questions about tool results.
            Role::Tool => "tool",
        };

        let mut obj = serde_json::json!({
            "role": role,
            "content": msg.content
        });

        if !msg.tool_calls.is_empty() {
            let tool_calls: Vec<serde_json::Value> = msg
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments,
                        }
                    })
                })
                .collect();
            obj["tool_calls"] = serde_json::Value::Array(tool_calls);
        }

        if let Some(ref tc_id) = msg.tool_call_id {
            obj["tool_call_id"] = serde_json::Value::String(tc_id.clone());
        }

        arr.push(obj);
    }

    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
}

// ── Stream adapter ───────────────────────────────────────────────────────────

/// Adapts a `tokio::sync::mpsc::Receiver` into a `futures::Stream`.
struct ReceiverStream {
    rx: mpsc::Receiver<Result<ChatEvent>>,
}

impl Stream for ReceiverStream {
    type Item = Result<ChatEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_messages_json_basic() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];
        let json = build_openai_messages_json("You are helpful.", &messages);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 3); // system + user + assistant
        assert_eq!(parsed[0]["role"], "system");
        assert_eq!(parsed[1]["role"], "user");
        assert_eq!(parsed[1]["content"], "Hello");
        assert_eq!(parsed[2]["role"], "assistant");
    }

    #[test]
    fn build_messages_json_empty() {
        let json = build_openai_messages_json("sys", &[]);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["content"], "sys");
    }

    #[test]
    fn build_messages_json_emits_tool_calls_and_tool_call_id() {
        use pond_core::models::domain::message::ToolCallRecord;

        let messages = vec![
            ChatMessage::user("weather?"),
            ChatMessage::assistant_with_tool_calls(
                "checking",
                vec![ToolCallRecord {
                    id: "call-1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{\"city\":\"NBO\"}".to_string(),
                }],
            ),
            ChatMessage::tool_result("sunny", "call-1"),
            ChatMessage::assistant("It's sunny."),
        ];
        let json = build_openai_messages_json("sys", &messages);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        // system + user + assistant(with calls) + tool + assistant
        assert_eq!(parsed.len(), 5);
        // Assistant tool-call entry
        let asst = &parsed[2];
        assert_eq!(asst["role"], "assistant");
        let calls = asst["tool_calls"].as_array().expect("tool_calls array");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call-1");
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert_eq!(calls[0]["function"]["arguments"], "{\"city\":\"NBO\"}");
        // Tool result entry
        let tool = &parsed[3];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "call-1");
        // Plain assistant has no tool_calls field
        assert!(parsed[4].get("tool_calls").is_none());
    }
}
