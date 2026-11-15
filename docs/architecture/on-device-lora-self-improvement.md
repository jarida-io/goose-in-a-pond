# Autonomous On-Device Model Self-Improvement via QLoRA & Domestic SFT Mining

**Status:** Proposed Architecture & Implementation Plan  
**Target Device:** NVIDIA Jetson Orin Nano (8 GB Unified LPDDR5, ~7,620 MB usable ceiling)  
**Target Engine:** `llama.cpp` GGUF backend (`goose-local-inference` / `GiapProviderShim`)  
**Core Invariant:** Sovereign local learning without cloud API costs or privacy leakage.

---

## 1. Motivation & Context

In Goose In A Pond (GIAP), small local language models (SLMs, 3B–8B parameters) run directly on-device under tight memory budgets. While these models handle direct single-turn requests reasonably well, domestic home environments introduce unique edge challenges:
1. **Tool Schema Hallucination**: Small models occasionally invent tool names or produce malformed JSON arguments instead of adhering strictly to GIAP's MCP servers (`giap-device`, `giap-sensors`, `giap-schedule`, `giap-weather`, `giap-memory`, `giap-vision`).
2. **Household Alignment & Shorthand**: Every household develops its own vernacular, device nicknames, quiet hours, and preference hierarchies (e.g. "turn off Jerry's room" referring to a specific group of Matter smart plugs).
3. **Prompt Prefix Overhead**: Injecting lengthy tool definitions, domestic rules, and few-shot examples into every turn consumes precious KV cache context on the 8 GB Jetson.

By training a lightweight, parameter-efficient LoRA adapter on local domestic interaction traces, the model internalizes the home's device topology, tool schemas, and soul guidelines directly into low-rank weight matrices.

---

## 2. Memory Budget & Mathematical Feasibility

### The 8 GB Unified Memory Ceiling
Full backpropagation across a 3.8B–8B model is completely impossible on the Jetson Orin Nano:
- 8B model in FP16 = 16 GB for weights alone.
- AdamW optimizer state = 8 bytes per parameter (16 GB $\times 2 = 32$ GB).
- Activations and gradients = 10–20 GB.
- Total required: >60 GB VRAM.

### Why QLoRA Succeeds on the Orin Nano
Under **QLoRA (Quantized Low-Rank Adaptation)**:
1. **Base Model ($W_0$)**: Frozen in 4-bit (`Q4_K_M`) precision ($3.8\text{B} \approx 2.5\text{ GB}$; $7\text{B} \approx 4.3\text{ GB}$).
2. **Trainable Low-Rank Matrices ($A$ and $B$)**:
   $$\Delta W = B \times A, \quad \text{where } B \in \mathbb{R}^{d \times r}, \; A \in \mathbb{R}^{r \times d}$$
   With rank $r = 8$ and alpha $\alpha = 16$ targeting attention projections ($W_q, W_v$):
   - Total trainable parameters: $\approx 2\text{M} - 6\text{M}$ parameters (only **15 MB to 35 MB** of adapter weights).
   - Optimizer memory for 4M parameters in 16-bit: $< 32\text{ MB}$.
3. **Training Envelope**:
   - Batch size: 1.
   - Gradient checkpointing: enabled.
   - Maximum sequence length: clamped to 1,024 tokens.
   - **Peak Training VRAM: ~4.2 GB to 4.8 GB**.
   - This fits comfortably within the Jetson's 7,620 MB budget when audio and chat services yield to the background lane.

---

## 3. Autonomous Training Data Mining (Forming Datasets on the Device)

Training on edge cannot rely on human annotators. The Pond autonomously harvests and synthesizes SFT (Supervised Fine-Tuning) data from three native domestic interaction streams:

```
┌────────────────────────────────────────────────────────────────────────┐
│                   Goose Interaction Streams (Daytime)                  │
├────────────────────┬───────────────────────┬───────────────────────────┤
│  Stream A: Human   │  Stream B: Tool Call  │  Stream C: Overnight      │
│  Corrections & Tone│  Execution Traces     │  Soul Self-Distillation   │
└─────────┬──────────┴───────────┬───────────┴─────────────┬─────────────┘
          │                      │                         │
          ▼                      ▼                         ▼
┌────────────────────────────────────────────────────────────────────────┐
│             Autonomous Extraction & Quality Curation Filter            │
│  - Redact personal secrets/tokens (redaction_chokepoints)              │
│  - Length clamp (<= 1024 tokens to safeguard training VRAM)            │
│  - Verification gate (positive outcome verified in SQLite)             │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Prompt-Masked SFT Formatting                      │
│     Target tokens (Assistant turn / Tool calls) = Loss Weight 1.0       │
│     Context tokens (System Soul / User Turn)   = Loss Weight 0.0       │
└──────────────────────────────────┬─────────────────────────────────────┘
                                   │
                                   ▼
┌────────────────────────────────────────────────────────────────────────┐
│            Quiet-Hours QLoRA Training (llama.cpp finetune)             │
│            ~4.5 GB Peak VRAM, 2 Epochs, Rank r=8, 25-35 Minutes        │
└────────────────────────────────────────────────────────────────────────┘
```

### Stream A: Implicit Human Corrections & Redirections
When a human interacts with Goose, errors are followed by immediate corrective feedback.
- **Detection Triggers**:
  1. *Linguistic Markers*: Follow-up turn contains negation or correction tokens ("No", "Not that", "Actually", "I meant", "Stop", "Don't").
  2. *Barge-In Interruption*: User interrupts assistant speech synthesis (`InterruptWatch` fires before sentence 1 completes).
  3. *Regenerations & Edits*: User modifies a staged to-do item or re-prompts after an unhelpful reply.
- **Sample Generation**:
  The failed intermediate turn is discarded. The initial prompt $P_0$ is paired with the corrected outcome $A_{\text{resolved}}$:
  $$\text{Input: } [P_0 + \text{Context}] \quad \longrightarrow \quad \text{Target: } A_{\text{resolved}}$$

### Stream B: Verified Tool Execution Traces (Tool Calling Discipline)
To ensure the model never invents tools or produces invalid parameters:
- Every turn where the model issues an MCP call (`giap-device`, `giap-sensors`, `giap-schedule`, `giap-weather`, `giap-memory`) is monitored.
- If the call succeeds (valid JSON, matching schema, executed without error) and the user confirms or acknowledges it, the raw turn is saved as a positive gold sample:
  ```json
  <tool_call>
  {"name": "giap-device", "arguments": {"room": "Living Room", "action": "set_level", "level": 30}}
  </tool_call>
  ```
- If a tool call fails with a validation error, but the model self-repairs in the subsequent step, the prompt is paired with the repaired syntax.

### Stream C: Overnight "Soul" Self-Distillation Pass
During quiet hours (e.g. 02:00 AM – 03:00 AM), the inference lane runs an offline distillation sweep:
1. It retrieves recent multi-turn transcripts from `pond_system.db`.
2. It evaluates them against the prompt guidelines in `soul.md` (concise speech, home grounding, domestic agency).
3. The model critiques its own past responses and generates a refined version:
   - *Past Response:* "I found 10 articles on the web about Egusi soup. Would you like me to read all of them to you?" (Verbose, unhelpful).
   - *Distilled Response:* "Here are the ingredients for Egusi Soup. I can add the smoked fish and melon seeds to your to-do list, or schedule prep time for Saturday." (Actionable, domestic).

---

## 4. Dataset Formatting, Loss Masking & Guardrails

### Prompt Masking (Zero Loss on Context)
In auto-regressive fine-tuning, calculating loss on the prompt tokens teaches the model to predict user questions rather than answers.
- In `llama.cpp` format, all prompt tokens (system prompt, few-shot headers, user message) are given a loss weight of `0.0`.
- Only the **assistant's completion tokens and `<tool_call>` JSON blocks** receive a loss weight of `1.0`.

```text
[WEIGHT: 0.0] <|im_start|>system
[WEIGHT: 0.0] You are Goose, an autonomous domestic AI on local hardware...<|im_end|>
[WEIGHT: 0.0] <|im_start|>user
[WEIGHT: 0.0] What's the climate in the nursery right now?<|im_end|>
[WEIGHT: 1.0] <|im_start|>assistant
[WEIGHT: 1.0] <tool_call>
[WEIGHT: 1.0] {"name": "giap-sensors", "arguments": {"room": "nursery", "metric": "environment"}}
[WEIGHT: 1.0] </tool_call><|im_end|>
```

### Safety & Integrity Guardrails

| Guardrail | Enforcement Point | Technical Detail |
| :--- | :--- | :--- |
| **Secret Scrubbing** | Pre-export filter | Redacts PAKE pairing codes, Wi-Fi keys, HTTPS tokens, and passwords using `redaction_chokepoints.rs`. |
| **Token Budget Ceiling** | Sequence clamp | Samples exceeding 1,024 tokens are trimmed or dropped to hold backprop memory $< 4.5\text{ GB}$. |
| **Replay Anchors** | Anti-catastrophic forgetting | 20% of every training batch consists of fixed general instruction/reasoning samples. |
| **Evaluation Gate** | Pre-activation check | Evaluates perplexity and JSON syntax against 20 held-out validation queries before activating the adapter. |

---

## 5. Runtime Serving: Dynamic LoRA Adapters in GIAP

`llama.cpp` natively supports dynamic LoRA loading without modifying the immutable quantized base model on disk.

### 1. Adapter Storage Layout
Adapters are stored in the pond data directory under:
```
/var/lib/goose-in-a-pond/
  ├── models/
  │    └── qwen2.5-7b-instruct-q4_k_m.gguf       # 4.3 GB (Immutable base)
  └── adapters/
       ├── household_soul_v1.gguf                # 28 MB (Trained adapter)
       └── members/
            ├── jerry_personal.gguf              # 18 MB (Personal preference layer)
            └── guest_shared.gguf
```

### 2. Runtime Mounting
When initializing the local inference engine in `crates/pond-adapters-local-inference/src/lib.rs`:
```rust
// Dynamic adapter attachment in llama-cpp-2
let mut model_params = LlamaModelParams::default();
let model = LlamaModel::load_from_file(&model_path, model_params)?;

// Mount household LoRA adapter with scaling factor 1.0
let adapter = LlamaAdapterLora::init(&model, &adapter_path)?;
```

- Adapters can be hot-swapped or scaled ($0.0 \dots 1.0$) per speaker based on voice enrollment or user session tokens.

---

## 6. Implementation Roadmap

### Phase 1: Interaction Logger & Dataset Extractor (`pond-core`)
- [ ] Add `training_interactions` table to `pond_system.db` tracking:
  - `session_id`, `turn_index`, `user_prompt`, `assistant_turn`, `tool_calls_json`.
  - `feedback_score` (+1 for confirmed/used, -1 for contradicted/barged-in).
- [ ] Implement `DatasetMinerService`:
  - Extracts positive pairs and resolved correction triplets.
  - Applies `redaction_chokepoints`.
  - Writes prompt-masked training text to `<data_dir>/training/sft_dataset.txt`.

### Phase 2: Background Training Runner in Inference Lane (`pond-server`)
- [ ] Add `LlamaFinetuneChore` to `crates/pond-core/src/user_data/services/inference_lane.rs`:
  - Runs only during quiet hours (02:00 – 04:00) when room voice and user sessions are idle.
  - Invokes `llama-cpp-finetune` subprocess with:
    `--model-base <base.gguf> --train-data <sft_dataset.txt> --lora-out <adapter_temp.gguf> --rank 8 --threads 4`.
  - Enforces `cgroups` ceiling and yields immediately if user activity occurs.

### Phase 3: Validation Gate & Adapter Promotion
- [ ] Implement `AdapterValidator`:
  - Evaluates validation loss on 20 benchmark turns.
  - Tests 5 tool-calling queries; validates that JSON schemas parse correctly.
  - If validation passes, atomically renames `adapter_temp.gguf` $\rightarrow$ `household_soul.gguf`.
  - Signals `GooseAdapter` to reload inference context.

### Phase 4: UI Transparency in Goose On The Go & Desktop
- [ ] Surface adapter status in the "Pond / About" tab:
  - "Household Adapter: Version 4 (Trained Nov 12, 02:45 AM, 210 domestic turns)".
  - One-click toggle: "Reset to Factory Soul" (instantly unmounts LoRA adapter).
