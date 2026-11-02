import type { Page } from "@playwright/test";

/** Rows as `GET /api/v1/models` sends them, grouped by category the way the server groups them. */

const LLAMA_CPP = { id: "llama_cpp", label: "llama.cpp", file_format: ".gguf", in_process: true };
const LITERT = { id: "litert_lm", label: "LiteRT-LM", file_format: ".litertlm", in_process: true };

export const e4bQat = (over: Record<string, unknown> = {}) => ({
  id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL",
  category: "gguf",
  name: "gemma-4-E4B-it-qat-UD-Q4_K_XL",
  title: "Gemma 4 E4B",
  description: "Gemma 4 E4B Instruct, quantisation-aware 4-bit (~4.2 GB, tool calling + thinking, reads pictures with an add-on)",
  size_mb: 4020,
  downloaded: false,
  active: false,
  url: "https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/8c5a9e4fd5482e2be20fe0bf013b4c262a8f4265/gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
  filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
  ram_estimate_mb: 5025,
  recommended_role: "chat",
  context_length: 131072,
  quantization: "UD-Q4_K_XL",
  engine: LLAMA_CPP,
  provenance: "catalogue",
  kind: "conversation",
  acquire: "download",
  recommended: { rank: "primary", reason: "Best answers this pond can run" },
  companions: [{ kind: "pictures", label: "Gemma 4 E4B", size_bytes: 991_552_320, state: "available" }],
  ...over,
});

export const e2bQat = (over: Record<string, unknown> = {}) => ({
  ...e4bQat(),
  id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL",
  name: "gemma-4-E2B-it-qat-UD-Q4_K_XL",
  title: "Gemma 4 E2B",
  description: "Gemma 4 E2B Instruct, quantisation-aware 4-bit (~2.6 GB, tool calling + thinking, reads pictures with an add-on)",
  size_mb: 2498,
  filename: "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf",
  ram_estimate_mb: 3122,
  recommended: { rank: "lighter", reason: "Faster replies and a smaller download, with a little less depth" },
  companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "available" }],
  ...over,
});

export const litertE4b = (over: Record<string, unknown> = {}) => ({
  ...e4bQat(),
  id: "litert/gemma-4-E4B-it.litertlm",
  category: "litert",
  name: "gemma-4-E4B-it.litertlm",
  title: "Gemma 4 E4B",
  description: "Gemma 4 E4B Instruct for LiteRT-LM (~3.7 GB, tool calling + thinking, text only)",
  size_mb: 3490,
  filename: "gemma-4-E4B-it.litertlm",
  ram_estimate_mb: 4362,
  context_length: 32768,
  quantization: undefined,
  engine: LITERT,
  recommended: { rank: "alternative", reason: "Runs on Google's LiteRT-LM engine; text only" },
  companions: [],
  ...over,
});

export const foundGguf = (over: Record<string, unknown> = {}) => ({
  id: "gguf/Llama-3.2-3B-Instruct-Q4_K_M",
  category: "gguf",
  name: "Llama-3.2-3B-Instruct-Q4_K_M",
  title: "Llama-3.2-3B-Instruct-Q4_K_M",
  description: "(detected on disk)",
  size_mb: 1900,
  downloaded: true,
  active: false,
  filename: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
  recommended_role: "chat",
  quantization: "Q4_K_M",
  engine: LLAMA_CPP,
  provenance: "on_disk",
  kind: "conversation",
  acquire: "unavailable",
  companions: [],
  ...over,
});

export const ollamaModel = (over: Record<string, unknown> = {}) => ({
  id: "ollama/qwen3:4b",
  category: "ollama",
  name: "qwen3:4b",
  title: "qwen3:4b",
  description: "qwen3:4b",
  size_mb: 2400,
  downloaded: true,
  active: false,
  recommended_role: "chat",
  engine: { id: "ollama", label: "Ollama", file_format: null, in_process: false },
  provenance: "ollama",
  kind: "conversation",
  acquire: "external",
  companions: [],
  ...over,
});

export const whisperBase = (over: Record<string, unknown> = {}) => ({
  id: "whisper/base.en",
  category: "whisper",
  name: "base.en",
  title: "Whisper base.en (en)",
  description: "Whisper base.en (en)",
  size_mb: 142,
  downloaded: true,
  active: false,
  filename: "ggml-base.en.bin",
  recommended_role: "asr",
  asr_size: "base.en",
  asr_language: "en",
  provenance: "catalogue",
  kind: "helper",
  acquire: "download",
  companions: [],
  ...over,
});

const EG2_REASON = "Finds facts in Swahili, Sheng or English, whichever language you ask in";

/** A Memory model as the server sends it: its whole description is its title. */
const embeddingRow = (name: string, description: string, over: Record<string, unknown> = {}) => ({
  id: `embedding/${name}`,
  category: "embedding",
  name,
  title: description,
  description,
  downloaded: false,
  active: false,
  recommended_role: "embedding",
  provenance: "catalogue",
  kind: "helper",
  acquire: "download",
  companions: [],
  ...over,
});

export const miniLmRow = (over: Record<string, unknown> = {}) =>
  embeddingRow(
    "all-MiniLM-L6-v2",
    "all-MiniLM-L6-v2 \u2014 fast, lightweight sentence embeddings. 23 MB, 384 dimensions. Best for domain classification and semantic search.",
    { size_mb: 23, acquire: "external", ...over },
  );

export const nomicRow = (over: Record<string, unknown> = {}) =>
  embeddingRow(
    "nomic-embed-text-v1.5",
    "Nomic Embed Text v1.5 \u2014 8-bit GGUF run by llama.cpp. 768 dimensions, tuned for search.",
    { size_mb: 139, filename: "nomic-embed-text-v1.5.Q8_0.gguf", ...over },
  );

export const eg2Row = (over: Record<string, unknown> = {}) =>
  embeddingRow(
    "embeddinggemma-2",
    "EmbeddingGemma 2 \u2014 8-bit GGUF run by llama.cpp. Over 100 languages, so a fact said in Swahili is found by a question in English. 256 dimensions.",
    {
      size_mb: 295,
      filename: "embeddinggemma-2-Q8_0.gguf",
      recommended: { rank: "primary", reason: EG2_REASON },
      ...over,
    },
  );

/** The three Memory models a pond with the default on it offers. */
export const memoryRows = () => [eg2Row(), miniLmRow(), nomicRow()];

/** What the index reports with Memory on EmbeddingGemma 2; the embedder and the rows reached are the dials. */
export function memoryHealth(
  embedder: { state?: string; model_id?: string | null; reason?: string | null } = {},
  over: Record<string, unknown> = {},
) {
  const model_id = embedder.model_id === undefined ? "embeddinggemma-2-256" : embedder.model_id;
  return {
    indexed: true,
    embedder: { state: "ready", model_id, reason: null, ...embedder },
    model_id,
    dims: 256,
    rows: 95,
    matching: 95,
    mismatched: 0,
    missing: 0,
    coverage: 1,
    corpora: [
      { corpus: "memory", rows: 45, source_rows: 45, indexed_rows: 45, missing_rows: 0, mismatched: 0, coverage: 1, structurally_excluded: false },
      { corpus: "context", rows: 0, source_rows: 0, indexed_rows: 0, missing_rows: 0, mismatched: 0, coverage: null, structurally_excluded: false },
      { corpus: "summary", rows: 50, source_rows: 50, indexed_rows: 50, missing_rows: 0, mismatched: 0, coverage: 1, structurally_excluded: false },
    ],
    ...over,
  };
}

/** A pond with Gemma 4 E2B on it and the rest of the picks still to get. */
export function typicalModels(over: Record<string, unknown[]> = {}) {
  return {
    gguf: [e4bQat(), e2bQat({ downloaded: true }), foundGguf()],
    litert: [litertE4b()],
    llamafile: [],
    whisper: [whisperBase()],
    tts: [],
    ollama: [ollamaModel()],
    embedding: [],
    ...over,
  };
}

/** The Orin's reading: 4.2 GB fits, 4.2 GB with its picture support does not. */
export const ORIN_MEMORY = { total_mb: 7620, available_for_llm_mb: 5820, loaded_model: null, reclaimable_mb: 0 };

/** A desktop with room to spare: the primary pick fits with its picture support. */
export const ROOMY_MEMORY = { total_mb: 16384, available_for_llm_mb: 12288, loaded_model: null, reclaimable_mb: 0 };

export interface ModelMocks {
  models?: ReturnType<typeof typicalModels>;
  roles?: Record<string, unknown>;
  memory?: Record<string, unknown>;
  downloads?: unknown[];
  /** The index report, with the embedder's own state in it. */
  health?: Record<string, unknown>;
}

/** Specific models routes, to register AFTER `mockAllApiRoutes`: the last registered wins. */
export async function mockModels(page: Page, opts: ModelMocks = {}): Promise<void> {
  await page.route("**/api/v1/models", (r) => r.fulfill({ json: opts.models ?? typicalModels() }));
  await page.route("**/api/v1/models/active-roles", (r) =>
    r.fulfill({
      json: opts.roles ?? {
        chat: { provider: "local", model: "gemma-4-E2B-it-qat-UD-Q4_K_XL", model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL" },
        tool: { model: null },
        asr: { model_id: null },
        tts: { model_id: null },
        embedding: { model_id: null, model: "", provider: "fastembed" },
        router_name: "LLaMA_CPP",
      },
    }),
  );
  if (opts.health) {
    await page.route("**/api/v1/context/index/health", (r) => r.fulfill({ json: opts.health }));
  }
  await page.route("**/api/v1/models/memory-status", (r) => r.fulfill({ json: opts.memory ?? ORIN_MEMORY }));
  await page.route("**/api/v1/models/download/progress", (r) =>
    r.fulfill({ json: { downloads: opts.downloads ?? [] } }),
  );
  await page.route("**/api/v1/models/disk-usage", (r) =>
    r.fulfill({
      json: { total_bytes: 2_620_370_976, by_category: { gguf: 2_620_370_976 }, hf_cache_bytes: 0, incomplete_bytes: 0 },
    }),
  );
}
