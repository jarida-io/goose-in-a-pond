# P0: EmbeddingGemma 2 on the Orin Nano (2026-10-31)

## Verdict

| Half of the gate | Result |
|---|---|
| Beats MiniLM on recall | **Pass.** It is better or tied on every set and metric, and the gap is large on Swahili and mixed-language text (R@5 0.73 vs 0.39) and in one pooled 542-document index (R@5 0.74 vs 0.53). |
| Fits the memory budget beside E4B and its drafter | **Pass for the text encoder only if it replaces MiniLM, and only narrowly. The vision and audio encoders fail.** The text encoder takes about 270 MB. At the pond's steady 450-518 MB free, that leaves 180-250 MB, under the 300 MB margin, if it sits beside MiniLM. Removing MiniLM in the same change (about 130 MB standalone) brings the net cost to about +140 MB, which leaves about 310-380 MB. Vision (+290 MB) and audio (+371 MB) do not fit beside the serving pond, so they run per batch only. |

**Overall:** P0's gate passes for a text-only EmbeddingGemma 2 that **replaces** fastembed MiniLM-L6. The margin is thin, so it needs its own measured constant in `device_budget.rs`. Vision and audio stay per batch (P6), as section 5 already plans.

**Runtime:** llama.cpp in process, through goose's llama-cpp-2. Use a Q8_0 GGUF converted from Google's weights. Details are below.

## Conditions (all numbers are Orin-measured unless marked otherwise)

- **Board:** the nano, JetPack R36.4.7, MAXN_SUPER, CPU at 1.728 GHz.
- **Pond, serving throughout:** `gemma-4-E4B-it-qat-UD-Q4_K_XL` with the MTP drafter loaded (`mtp-gemma-4-E4B-it.gguf`), NvMap about 3.7-4.0 GB. Live background work during the runs:
  - the camera pipeline, with face ID on TensorRT;
  - the inference lane: summary_refresh about every 30 s, plus titling, suggestion generation, memory extraction and index maintenance.
- **Not stopped:** the service was never stopped or restarted.
- **Preflight before every run:** no build, no active long task, no turn in the last 3 minutes, and need + 300 MB free.
- **Floor:** `memwatch.py` kills the measured process if MemAvailable drops below 250 MB. It never fired.
- **Memory sources:**
  - RSS comes from `/proc`.
  - NvMap comes per process from jtop's root service; there is no sudo for nvmap debugfs.
  - MemAvailable is the system figure. My builds pushed 1.5-3.3 GB of the pond into swap, so absolute free memory was inflated during the runs (650-1450 MB). The per-process deltas are the reliable figures; the absolute free is not.
- **Weights:**
  - `google/embeddinggemma-2` @ f9972ac2: safetensors, not gated, sha256 verified. Converted with upstream llama.cpp 0ec4027a.
  - LiteRT files from `litert-community` @ pinned revisions, sha256 verified. Google's own LiteRT-LM and MediaPipe repos point to them.
- **Prompts:** the model card's, word for word:
  - queries: `task: search result | query: `
  - documents: `title: {title or none} | text: `
  - MiniLM gets bare text, as fastembed does.

## Text encoder (query embed; 200 queries averaging 17.3 tokens)

| Runtime and configuration | Load | p50 / p95 | RSS (anon + file) | NvMap | MemAvailable before → after |
|---|---|---|---|---|---|
| llama.cpp Q8_0, CPU, 4 threads, ctx 256, 4 seqs, repack | 1.41 s | **40.9 / 55.9 ms** | **270 MB** (253 + 17) | 0 | 778 → 562 (−216) |
| same, no repack | 1.32 s | 74.6 / 100.2 ms | 266 MB (118 + 155 file-backed) | 0 | 818 → 703 (−115) |
| same, repack, 2 threads | | 68.6 / 92.3 ms | | | |
| same, repack, 6 threads | | 43.5 / 106.2 ms | | | |
| llama.cpp Q8_0, CUDA (token table pinned, the default) | 1.84 s | **18.0 / 36.5 ms** | 492 MB (307 + 197 libs) | 400 MB | 989 → 726 (−262) |
| llama.cpp Q8_0, CUDA, token table mapped (`--no-host`) | | 20.2 / 33.4 ms | 413 MB (307 + 116) | 264 MB | 1248 → 864 (−384) |
| LiteRT-LM 0.19 int4 QAT, CPU, 4 threads | 0.47 s | 169.3 / 177.4 ms | 380 MB (260 anon) | 0 | 820 → 513 (−307) |
| LiteRT-LM int4, GPU (WebGPU on Vulkan, fp16 activations) | 7.36 s | 87.0 / 93.2 ms | 326 MB | 208 MB (377 during load) | 800 → 409 (−391; 344 at the load peak) |
| MiniLM-L6 ONNX, CPU, 4 threads (PyPI ORT, not the pond's build) | | 6.7 / 8.9 ms | +132 MB over a bare Python+ORT import | 0 | |

- **Against the warm TTFT (~0.45 s):**
  - llama.cpp CPU adds 9% at p50 (12% at p95).
  - CUDA adds 4% (8%).
  - LiteRT CPU adds 38%; LiteRT GPU adds 19%.
  - Today's typed turns showed engine TTFT of 1.4-1.6 s on 9.5-10.5k-token prompts.
- **CPU RSS breakdown:** the 262k-token vocab 97 MB (measured vocab-only), repacked weights 139 MB, compute buffer 27 MB, output buffer 4 MB, token-table working set about 12 MB (file-backed, after paging the GGUF out).
- **CUDA, standalone process:** the anon figure includes about 75 MB of CUDA runtime and about 90 MB of loaded kernel images; the NvMap figure includes the CUDA context. In process, the pond's existing CUDA context would carry those, so I estimate about 300 MB. That is **derived, not measured.**
- **Cold-start costs (Orin-measured):**

  | Runtime | First encode |
  |---|---|
  | llama.cpp CPU | 64 ms |
  | llama.cpp CUDA | 282 ms |
  | LiteRT CPU | 265 ms |
  | LiteRT GPU | 224 ms |

## Indexing throughput (pctx + reach documents, about 28-31 tokens each; median of 5)

| Batch | CPU Q8_0 ms/item (batch ms) | CUDA Q8_0 ms/item (batch ms) | LiteRT CPU ms/item |
|---|---|---|---|
| 1 | 78.3 (78) | 34.6 (35) | 175.9 |
| 2 | 117.3 (235), noisy | 19.7 (39) | 174.0 |
| 4 | 63.3 (253) | 13.4 (54) | 170.2 |
| 8 | 60.0 (480) | 9.6 (77) | 166.3 |
| 16 | 59.4 (950) | **7.5 (120)** | 165.3 |
| 24 | 60.9 (1461) | 8.6 (**207**) | |
| 32 | 60.2 (1926) | 9.5 (304) | |
| about 37 (1024-token ubatch cap) | | 9.0-9.2 (440-575) | |

- **Largest batch under 250 ms:**

  | Runtime | Largest batch |
  |---|---|
  | CUDA | **24 items** (about 640 tokens, 207 ms) |
  | CPU | **3 items** (a batch of 4 is 253 ms) |
  | LiteRT CPU | 1 item (it pads every input to its 128-token signature and does not batch) |

- **Whole sets (embed command, 32 sequences, ctx 1024), for 403 household items:**

  | Runtime | Time | Per item |
  |---|---|---|
  | CUDA | 4.0 s | 10 ms |
  | CPU | 25.0 s | 62 ms |
  | LiteRT CPU | 66.1 s | 164 ms |

- **Indexing context (ctx 1024, 64 sequences):**
  - CPU RSS 610 MB (compute buffer 110 MB, output buffer 64 MB).
  - CUDA RSS 577 MB + NvMap 429 MB.
  - Most of that is the logits-sized output buffer and the per-layer-input tensor, which scales with the ubatch. Use 16 or fewer sequences and a 512-token ubatch in production.

## Media encoders

**Each encoder alone (libmtmd, Q8_0; deltas over the 104 MB vocab-only stage):**

| Encoder | Device | Memory | Time |
|---|---|---|---|
| Vision, 140 soft tokens | CPU | **+290 MB RSS** (weights 216 MB, compute 51 MB) | **6.06 s/image** p50 (6.21 s first) |
| Vision, 140 soft tokens | CUDA | +198 MB RSS + 328 MB NvMap (standalone, CUDA context included) | **0.67 s/image** p50 (1.06 s first) |
| Audio, 11 s clip (275 soft tokens) | CPU | **+371 MB RSS** (weights 313 MB, compute 37 MB) | **2.12 s** (0.19x real time) |
| Audio, 11 s clip | CUDA | +184 MB RSS + 390 MB NvMap (standalone) | **0.17 s** (0.56 s first) |

**End to end:**

| Pipeline | Memory | Time |
|---|---|---|
| Image through `llama-server`: text + vision, CPU, 140 soft tokens, no repack, FA on | server RSS 628 MB; MemAvailable 879 → 372 (lowest 324) | 6.32 s/image p50 |
| LiteRT text + vision (440m file), CPU | RSS 873 MB (+about 490 over its text-only 380) | 3.39 s/image |
| LiteRT text + audio (740m file, vision off), CPU | RSS 589 MB (+about 210) | 2.56 s per 11 s clip |

## Recall (Recall@k = any labelled item in the top k, the memory-reachability harness's definition)

| Set (docs / queries) | Embedder | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|---|
| reach (70 / 21) | MiniLM-L6 (384) | 0.619 | 0.952 | 0.952 | 0.765 |
| | EG2 llama.cpp Q8_0, 768 | **0.762** | 0.952 | 0.952 | **0.859** |
| | EG2 llama.cpp Q8_0, 256 | 0.762 | 0.952 | 0.952 | 0.859 |
| | EG2 BF16, 768 (reference) | 0.762 | 0.952 | 0.952 | 0.859 |
| | EG2 LiteRT int4, 768 / 256 | 0.714 | 0.952 | 0.952 | 0.835 |
| pctx (403 / 25) | MiniLM-L6 | 0.600 | 0.800 | 0.880 | 0.679 |
| | EG2 Q8_0, 768 | **0.720** | **0.840** | **0.960** | **0.801** |
| | EG2 Q8_0, 256 | 0.760 | 0.880 | 0.960 | 0.827 |
| | EG2 BF16, 768 | 0.720 | 0.840 | 0.960 | 0.799 |
| | EG2 LiteRT, 768 / 256 | 0.760 / 0.720 | 0.880 / 0.920 | 0.960 | 0.823 / 0.809 |
| sw: Swahili, Sheng, mixed (69 / 44) | MiniLM-L6 | 0.273 | 0.386 | 0.455 | 0.335 |
| | EG2 Q8_0, 768 | **0.409** | **0.727** | **0.841** | **0.566** |
| | EG2 Q8_0, 256 | 0.432 | 0.773 | 0.841 | 0.584 |
| | EG2 BF16, 768 | 0.409 | 0.727 | 0.841 | 0.565 |
| | EG2 LiteRT, 768 / 256 | 0.364 / 0.432 | 0.705 | 0.795 | 0.519 / 0.567 |
| pooled: all three sets in one index (542 / 90) | MiniLM-L6 | 0.333 | 0.533 | 0.622 | 0.430 |
| | EG2 Q8_0, 768 | **0.511** | **0.744** | **0.833** | **0.628** |
| | EG2 Q8_0, 256 | 0.511 | 0.756 | 0.822 | 0.632 |
| | EG2 LiteRT, 768 / 256 | 0.478 / 0.489 | 0.711 / 0.733 | 0.800 / 0.767 | 0.600 / 0.610 |
| img: text to image (11 / 11) | MiniLM | n/a (text only) | | | |
| | EG2 llama.cpp, 140 soft tokens, 768 = 256 | **0.909** | 1.000 | 1.000 | 0.955 |
| | EG2 LiteRT 440m | 1.000 | 1.000 | 1.000 | 1.000 |

**Swahili set, R@5 by kind (EG2 Q8_0 vs MiniLM):**

| Kind | EG2 | MiniLM |
|---|---|---|
| Swahili fact, English question | 15/22 | 7/22 |
| English fact, Swahili question | 5/8 | 0/8 |
| Mixed fact, English question | 5/7 | 4/7 |
| Mixed fact, mixed question | 3/3 | 3/3 |
| Swahili fact, Swahili question | 4/4 | 3/4 |

**Pooled index:** MiniLM's reach R@5 falls from 0.952 alone to 0.762 among 542 documents. EG2 holds 0.952.

**The card's question-answering prompt** (`task: question answering | query: `):

| Set | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| reach | 0.762 | 0.952 | 0.952 | 0.850 |
| pctx | 0.720 | 0.920 | 0.960 | 0.804 |
| sw | 0.477 | 0.750 | 0.841 | 0.602 |

It is equal or slightly better, which is worth a follow-up on the real store.

**Matryoshka 256 equals or beats 768 on every set here.** Storing 256 dimensions costs nothing measurable on this data and cuts vectors to a third.

**Small sets:** one query is worth 4.8 points (reach), 4.0 (pctx), 2.3 (sw) and 9.1 (img).

**Agreement checks:**
- Tokenization is identical to the HF tokenizer (BOS…EOS).
- Q8_0 vs BF16: cos ≥ 0.9987 (mean 0.9997).
- CPU vs CUDA: cos ≥ 0.9987.
- llama.cpp Q8_0 vs Google's LiteRT int4 QAT: mean cos 0.975-0.985 (min 0.917).
- Image vectors, llama.cpp vs LiteRT: mean 0.981.

Two independent runtimes agree, so the GGUF port is sound.

## Runtime recommendation

**llama.cpp in process, through goose's llama-cpp-2. Q8_0 converted from Google's safetensors. Text encoder on CUDA, sharing the pond's context.**

**Why llama.cpp:**
- **Speed:** queries at 18 ms (CUDA) or 41 ms (CPU).
- **Indexing:** 24 items per 250 ms batch on CUDA.
- **Recall:** the best measured.
- **Footprint:** the smallest. Q8_0 is indistinguishable from BF16.
- **One runtime:** it is the runtime the chat model already uses, with mtmd already enabled for vision and audio (0.67 s/image and 0.17 s/clip on CUDA).
- **CPU fallback:** if NvMap is the scarcer resource, the same GGUF on CPU costs 270 MB and 41 ms. Indexing then drops to 3 items per batch, and a re-embed of the store becomes minutes instead of seconds.

**LiteRT-LM is the alternative, not the pick:**
- 4x slower on CPU and no lighter.
- Its GPU path runs fp16 activations, which the card warns against.
- It does not batch.
- Its one real advantage is distribution: Google ships a ready 165 MB file that a pond can download by default. Google ships no GGUF.

**ONNX Runtime with TensorRT: not measured, and not recommended now.**
- Google publishes no ONNX.
- The card forbids fp16, which is TensorRT's main win.
- A self-export needs torch and transformers ≥ 5.18 on a host with gigabytes free.
- Engine builds beside E4B have peaked at 3.2 GB before.

**Wiring into `EmbeddingProvider`:**
1. **Bump llama.cpp.** Bump the llama.cpp vendored in llama-cpp-rs-giap (8ac8291 has only `gemma-embedding` v1) past upstream #30054 (`4fbc76de`, gemma-embedding2). Include #30416 (no-cache attention padding) and #30255/#30445 (no logits or LM head for pooled graphs). 0ec4027a is the tree measured here. This bump touches the chat path, so it needs the Orin regression run (E4B, MTP drafter, KV snapshots).
2. **Add a spec.** Add an `EmbeddingModelSpec` to `GgufEmbeddingProvider` (`crates/pond-inference/src/embedding.rs`):
   - Mean pooling, 768 dimensions.
   - `content_prefix` `title: none | text: `, or a title-aware form for items that have titles.
   - `query_prefix` `task: search result | query: `.
   - A distinct `model_id`. Nomic's vectors are also 768 wide, so width cannot tell the two spaces apart.
3. **Fix the width check.** The loader compares `model.n_embd()` (512 for this model) with the spec, so it would refuse EG2. Compare `n_embd_out` (768); the fork's llama-cpp-2 already reads pooled vectors at `n_embd_out`.
4. **Keep one context.** Retain one context (n_ctx 256-512, 4-16 sequences, kv_unified) instead of a fresh context per embed. llama.cpp still sizes a 1 MB-per-sequence logits buffer for pooled contexts at this revision, and non-unified KV rounds n_ctx up to 256 per sequence.
5. **Map the token table on CUDA.** Use `no_host` and an mmap load mode on CUDA, so the 136 MB token table stays mapped instead of pinned.
6. **Re-embed.** Re-embed the store on the switch, as P2 plans.

**Where a pond downloads the GGUF is open:** Google ships safetensors only, and converting them
needs torch, which cannot run beside the pond. See decision 11 in
[the context space](../../../docs/architecture/context-space.md).

## Not measured, and why

- **ONNX Runtime with TensorRT:** see above. There are no official weights, fp16 is unsafe, and exporting needs a machine I may not use.
- **The full image pipeline on CUDA** (text + vision in one `llama-server`): it would need about 1.1 GB beside the pond, over the 300 MB margin. The vision encoder alone was measured on CUDA (0.67 s/image), and full-pipeline image recall was measured on CPU.
- **The in-process cost inside `pond-server`:** this needs the llama.cpp bump and a cargo build on the board. The about-300 MB CUDA figure is derived.
- **MiniLM's share inside the pond:** I measured it standalone (+132 MB over a Python+ORT import, PyPI ORT 4 threads). The pond loads it at startup (`fastembed embedding model loaded`), but its in-process share was not isolated.
- **Recall on the pond's real store:** I did not read the household's data, so the fixtures stood in.
- **Audio retrieval quality and video:** I measured audio memory and speed only, and video not at all.
- **BF16 on CPU:** unusable at 1.9 s/doc, so the CPU BF16 run was stopped after reach. BF16 recall comes from CUDA.

## What the measuring cost the pond

- **Builds beside the serving pond:**

  | Build | Duration | Jobs | Lowest MemAvailable |
  |---|---|---|---|
  | llama.cpp, CPU stage | about 15 min | 2 | 108 MB |
  | llama.cpp, CUDA module | 64 min | 1 | 62 MB |

  The build script pauses itself below 150 MB. A first CUDA attempt at 2 jobs raised the pond's vision_classifier p95 from 8.6 to 102.6 ms, so I stopped it and reran at 1 job. There was no OOM. The pond stayed healthy (health 200), and turns completed during the builds (10:38Z, 11:55Z). No measurement ran during a build.
- **Service:** never stopped or restarted, and healthy at the end. The builds left about 1.1 GB of the pond in swap afterwards. The pond's settings and checkout were untouched.
