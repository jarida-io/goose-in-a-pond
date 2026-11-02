# P0: EmbeddingGemma 2 on the Orin

The harness behind phase P0 of [the context space](../../../docs/architecture/context-space.md)
(sections 5 and 11): what EmbeddingGemma 2 costs beside the serving pond, how fast it embeds, and
whether it retrieves better than today's MiniLM-L6. Everything runs on the nano in a scratch
directory (default `~/p0-embed`), never in the pond's checkout or data directory, and nothing here
needs a cargo build on the board.

What it measured on 2026-10-31, and the verdict, are in [RESULTS.md](RESULTS.md).

## Setup, once

| Step | Command (on the nano) | Cost |
|---|---|---|
| llama.cpp, CPU tools | `JOBS=2 bash build-llama.sh ~/p0-embed cpu` | about 15 min beside the pond |
| llama.cpp, CUDA module | `JOBS=1 bash build-llama.sh ~/p0-embed cuda` | about 65 min at one job (MemAvailable fell to 62 MB); two jobs slowed the pond's vision events tenfold |
| Weights | `google/embeddinggemma-2` at `f9972ac2` into `~/p0-embed/hf/embeddinggemma-2`; the three `litert-community/embeddinggemma-2-*-litert-lm` files into `~/p0-embed/litert` | 1.5 GB + 1.0 GB, not gated |
| Python | a venv with torch 2.11 (CPU), transformers 5.20.2, numpy, sentencepiece, onnxruntime, litert-lm-api 0.19.0 | about 1 GB |
| GGUF | `bash convert.sh`, then `split_mmproj.py llama.cpp gguf/mmproj-eg2-q8_0.gguf gguf/mmproj-eg2-q8_0` | text Q8_0 304 MB, vision 226 MB, audio 328 MB |
| Inputs | `prep_corpora.py REPO OUT`, `fetch_images.py OUT` | the three text sets, 11 CC0 images |

`build-llama.sh` pins upstream llama.cpp `0ec4027a`, the first tree with `gemma-embedding2` and
its no-cache attention fixes; the pond's own llama.cpp (llama-cpp-rs-giap `8ac8291`) has
`gemma-embedding` (v1) only. The converter pins transformers 4.57.6, which cannot read this
checkpoint's tokenizer config (`extra_special_tokens` is a list); 5.20.2 can, and is the newest
release for the board's Python 3.10.

## Measuring

`measure.sh [workdir] STEP...` runs `recall`, `cpu`, `gpu`, `encoders`, `images` and `litert`.
Every step passes `preflight.sh NEED_MB` first: no build, no active long task, no turn in the last
three minutes, and NEED_MB plus 300 MB free. `memwatch.py` samples RSS, NvMap (through jtop, the
only unprivileged reader of per-process GPU memory on a Jetson) and MemAvailable per stage, and
kills the measured process if MemAvailable falls under 250 MB.

| Tool | Measures |
|---|---|
| `eg2bench` | the text encoder in process on libllama: load, memory per stage, query latency, batch throughput, vectors. `--pageout` drops the model's mapped pages after warm-up, so RSS-file afterwards is the working set rather than the whole GGUF that `MAP_POPULATE` faulted in. `--no-host` keeps the 136 MB token table mapped on CUDA instead of copied into pinned memory |
| `mtmdbench` | one media encoder alone on libmtmd (the text GGUF loads vocab-only) |
| `server_bench.py` | image embeddings end to end through `llama-server`, the only upstream path that pools media tokens |
| `litert_bench.py` | the same measurements on Google's LiteRT-LM embedding engine |
| `minilm_embed.py` | the pond's MiniLM-L6 ONNX model, run as fastembed-rs runs it |
| `recall.py`, `pooled_recall.py` | Recall@1/5/10 and MRR per set, and with all three sets in one index |

Text inputs carry the model card's prompts word for word: `task: search result | query: ` for
queries and `title: {title or none} | text: ` for documents. MiniLM gets the bare text.

## Sets

| Set | Documents | Queries | Source |
|---|---|---|---|
| `reach` | 70 extracted facts | 21 | `crates/pond-infra/tests/fixtures/memory-reachability` |
| `pctx` | 403 household items | 25 | `fixtures/personal-context` |
| `sw` | 44 targets and 25 distractors | 44 | `data/swahili-mixed.jsonl`: Swahili, English, Sheng and code-switched, in both directions |
| `img` | 11 photos | 11 | `data/images.json`: CC0 from Wikimedia Commons, licence re-checked on fetch |
