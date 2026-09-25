//! Context-window sizing from GGUF KV-cache dimensions and free device memory.

use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};

/// Free inference memory in bytes, preferring accelerators over CPU RAM; 0 if none report.
pub(crate) fn available_inference_memory_bytes() -> u64 {
    let devices = list_llama_ggml_backend_devices();

    let accel_memory = devices
        .iter()
        .filter(|d| {
            matches!(
                d.device_type,
                LlamaBackendDeviceType::Gpu
                    | LlamaBackendDeviceType::IntegratedGpu
                    | LlamaBackendDeviceType::Accelerator
            )
        })
        .map(|d| d.memory_free as u64)
        .max()
        .unwrap_or(0);

    if accel_memory > 0 {
        return accel_memory;
    }

    devices
        .iter()
        .filter(|d| d.device_type == LlamaBackendDeviceType::Cpu)
        .map(|d| d.memory_free as u64)
        .max()
        .unwrap_or(0)
}

/// Max context whose KV cache fits in half the free memory; `None` if the model lacks the dims.
pub(crate) fn estimate_max_context(model: &LlamaModel) -> Option<usize> {
    let available = available_inference_memory_bytes();
    if available == 0 {
        return None;
    }

    // Reserve 50% for compute scratch buffers.
    let usable = (available as f64 * 0.5) as u64;

    let n_layer = model.n_layer() as u64;
    let n_head_kv = model.n_head_kv() as u64;
    let n_head = model.n_head() as u64;
    let n_embd = model.n_embd() as u64;

    if n_head == 0 || n_layer == 0 || n_head_kv == 0 || n_embd == 0 {
        return None;
    }

    // MLA models (DeepSeek/GLM) have KV dims other than n_head_kv * head_dim; read them from GGUF.
    let head_dim = n_embd / n_head;
    let arch = model
        .meta_val_str("general.architecture")
        .unwrap_or_default();

    let k_per_head = model
        .meta_val_str(&format!("{arch}.attention.key_length"))
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(head_dim);

    let v_per_head = model
        .meta_val_str(&format!("{arch}.attention.value_length"))
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(head_dim);

    // Bytes per KV token; the trailing 2 is bytes per f16 element.
    let bytes_per_token = (k_per_head + v_per_head) * n_head_kv * n_layer * 2;

    if bytes_per_token == 0 {
        return None;
    }

    Some((usable / bytes_per_token) as usize)
}

/// Context size for a request: prompt plus 512 tokens, capped by `n_ctx_train` and free memory.
pub(crate) fn effective_context_size(model: &LlamaModel, prompt_token_count: usize) -> usize {
    let n_ctx_train = model.n_ctx_train() as usize;
    let memory_max = estimate_max_context(model);

    let cap = match memory_max {
        Some(mem_max) if mem_max < n_ctx_train => {
            tracing::info!(n_ctx_train, mem_max, "capping context to memory estimate");
            mem_max
        }
        _ => n_ctx_train,
    };

    let min_generation_headroom = 512;
    let needed = prompt_token_count + min_generation_headroom;

    if needed > cap {
        tracing::warn!(
            prompt_token_count,
            cap,
            "prompt + headroom exceeds context limit, capping"
        );
    }

    needed.min(cap)
}
