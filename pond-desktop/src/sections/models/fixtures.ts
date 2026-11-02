// Rows as `api.listModels()` hands them over, written from what the server sends, for tests.

import type { ModelEntry, ModelMemoryStatus, ModelActiveRoles, DownloadEntry } from "../../api/types";

const LLAMA_CPP = { id: "llama_cpp", label: "llama.cpp", file_format: ".gguf", in_process: true };
const LITERT = { id: "litert_lm", label: "LiteRT-LM", file_format: ".litertlm", in_process: true };
const OLLAMA = { id: "ollama", label: "Ollama", file_format: null, in_process: false };
const LLAMAFILE = { id: "llamafile", label: "llamafile", file_format: ".llamafile", in_process: false };

export const ENGINES = { LLAMA_CPP, LITERT, OLLAMA, LLAMAFILE };

export const E4B_ORIN = {
  device: "orin" as const,
  summary: "First reply in about 1 s, 15-16 tokens a second, 16k window",
  first_reply_s: 1,
  tokens_per_second_min: 15,
  tokens_per_second_max: 16,
  window_tokens: 16384,
  measured_on: "2026-10-05",
};

export function e4b(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL",
    provider: "gguf",
    category: "gguf",
    name: "gemma-4-E4B-it-qat-UD-Q4_K_XL",
    display_name: "Gemma 4 E4B Instruct, quantisation-aware 4-bit (~4.2 GB, tool calling + thinking, reads pictures with an add-on)",
    title: "Gemma 4 E4B",
    is_active: false,
    downloaded: false,
    size_mb: 4020,
    ram_estimate_mb: 5025,
    recommended_role: "chat",
    context_length: 131072,
    quantization: "UD-Q4_K_XL",
    filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
    engine: LLAMA_CPP,
    provenance: "catalogue",
    kind: "conversation",
    acquire: "download",
    recommended: { rank: "primary", reason: "Best answers this pond can run" },
    companions: [{ kind: "pictures", label: "Gemma 4 E4B", size_bytes: 991_552_320, state: "available" }],
    ...over,
  };
}

export function e2b(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL",
    provider: "gguf",
    category: "gguf",
    name: "gemma-4-E2B-it-qat-UD-Q4_K_XL",
    display_name: "Gemma 4 E2B Instruct, quantisation-aware 4-bit (~2.6 GB, tool calling + thinking, reads pictures with an add-on)",
    title: "Gemma 4 E2B",
    is_active: false,
    downloaded: false,
    size_mb: 2498,
    ram_estimate_mb: 3122,
    recommended_role: "chat",
    context_length: 131072,
    quantization: "UD-Q4_K_XL",
    filename: "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf",
    engine: LLAMA_CPP,
    provenance: "catalogue",
    kind: "conversation",
    acquire: "download",
    recommended: { rank: "lighter", reason: "Faster replies and a smaller download, with a little less depth" },
    companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "available" }],
    ...over,
  };
}

export function litertE4b(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "litert/gemma-4-E4B-it.litertlm",
    provider: "litert",
    category: "litert",
    name: "gemma-4-E4B-it.litertlm",
    display_name: "Gemma 4 E4B Instruct for LiteRT-LM (~3.7 GB, tool calling + thinking, text only)",
    title: "Gemma 4 E4B",
    is_active: false,
    downloaded: false,
    size_mb: 3490,
    ram_estimate_mb: 4362,
    recommended_role: "chat",
    context_length: 32768,
    filename: "gemma-4-E4B-it.litertlm",
    engine: LITERT,
    provenance: "catalogue",
    kind: "conversation",
    acquire: "download",
    recommended: { rank: "alternative", reason: "Runs on Google's LiteRT-LM engine; text only" },
    companions: [],
    ...over,
  };
}

export function litertE2b(over: Partial<ModelEntry> = {}): ModelEntry {
  return litertE4b({
    id: "litert/gemma-4-E2B-it.litertlm",
    name: "gemma-4-E2B-it.litertlm",
    filename: "gemma-4-E2B-it.litertlm",
    display_name: "Gemma 4 E2B Instruct for LiteRT-LM (~2.6 GB, tool calling + thinking, text only)",
    title: "Gemma 4 E2B",
    size_mb: 2468,
    ram_estimate_mb: 3085,
    recommended: undefined,
    ...over,
  });
}

export function ollama(name = "qwen3:4b", over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: `ollama/${name}`,
    provider: "ollama",
    category: "ollama",
    name,
    display_name: name,
    title: name,
    is_active: false,
    downloaded: true,
    size_mb: 2400,
    recommended_role: "chat",
    engine: OLLAMA,
    provenance: "ollama",
    kind: "conversation",
    acquire: "external",
    companions: [],
    ...over,
  };
}

export function llamafile(name = "mistral-7b-instruct", over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: `llamafile/${name}`,
    provider: "llamafile",
    category: "llamafile",
    name,
    display_name: name,
    title: name,
    is_active: false,
    downloaded: true,
    size_mb: 4100,
    recommended_role: "chat",
    filename: `${name}.llamafile`,
    engine: LLAMAFILE,
    provenance: "added",
    kind: "conversation",
    acquire: "download",
    companions: [],
    ...over,
  };
}

/** A GGUF the scan found on disk: no source, a name from its own header, no recommendation. */
export function foundOnDisk(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "gguf/Llama-3.2-3B-Instruct-Q4_K_M",
    provider: "gguf",
    category: "gguf",
    name: "Llama-3.2-3B-Instruct-Q4_K_M",
    display_name: "(detected on disk)",
    title: "Llama-3.2-3B-Instruct-Q4_K_M",
    is_active: false,
    downloaded: true,
    size_mb: 1900,
    quantization: "Q4_K_M",
    recommended_role: "chat",
    filename: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
    engine: LLAMA_CPP,
    provenance: "on_disk",
    kind: "conversation",
    acquire: "unavailable",
    companions: [],
    ...over,
  };
}

export function functionGemma(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "gguf/functiongemma-270m-it-Q8_0",
    provider: "gguf",
    category: "gguf",
    name: "functiongemma-270m-it-Q8_0",
    display_name: "FunctionGemma 270M",
    title: "FunctionGemma 270M",
    is_active: false,
    downloaded: true,
    size_mb: 278,
    recommended_role: "tool",
    engine: LLAMA_CPP,
    provenance: "catalogue",
    kind: "helper",
    acquire: "download",
    companions: [],
    ...over,
  };
}

export function whisper(name = "base.en", over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: `whisper/${name}`,
    provider: "whisper",
    category: "whisper",
    name,
    display_name: `Whisper ${name} (en)`,
    title: `Whisper ${name} (en)`,
    is_active: false,
    downloaded: false,
    size_mb: 142,
    recommended_role: "asr",
    asr_size: name,
    asr_language: "en",
    provenance: "catalogue",
    kind: "helper",
    acquire: "download",
    companions: [],
    ...over,
  };
}

export function voice(name = "af_heart", over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: `tts_kokoro/${name}`,
    provider: "tts",
    category: "tts_kokoro",
    name,
    display_name: "Af_Heart",
    title: "American female — Heart",
    is_active: false,
    downloaded: true,
    size_mb: 1,
    recommended_role: "tts",
    tts_engine: "kokoro",
    provenance: "catalogue",
    kind: "helper",
    acquire: "download",
    companions: [],
    ...over,
  };
}

/** The numbers the server sends for EmbeddingGemma 2, only on a pond that keeps the Orin budget. */
export const EG2_ORIN = {
  device: "orin" as const,
  summary:
    "Searches memory in 18 ms on the GPU or 41 ms on the CPU. Puts the right Swahili or mixed-language fact in its top five 73% of the time, against 39% for the default",
  measured_on: "2026-10-31",
};

/** A Memory model, as the server sends it. */
function embeddingRow(name: string, title: string, description: string, over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: `embedding/${name}`,
    provider: "embedding",
    category: "embedding",
    name,
    display_name: description,
    title,
    is_active: false,
    downloaded: false,
    recommended_role: "embedding",
    provenance: "catalogue",
    kind: "helper",
    acquire: "download",
    companions: [],
    ...over,
  };
}

export function miniLm(over: Partial<ModelEntry> = {}): ModelEntry {
  return embeddingRow(
    "all-MiniLM-L6-v2",
    "all-MiniLM-L6-v2",
    "all-MiniLM-L6-v2 \u2014 fast, lightweight sentence embeddings. 23 MB, 384 dimensions. Best for domain classification and semantic search.",
    { size_mb: 23, ram_estimate_mb: 50, acquire: "external", ...over },
  );
}

export function nomicEmbed(over: Partial<ModelEntry> = {}): ModelEntry {
  return embeddingRow(
    "nomic-embed-text-v1.5",
    "Nomic Embed Text v1.5",
    "Nomic Embed Text v1.5 \u2014 8-bit GGUF run by llama.cpp. 768 dimensions, tuned for search.",
    { size_mb: 139, ram_estimate_mb: 173, filename: "nomic-embed-text-v1.5.Q8_0.gguf", ...over },
  );
}

export function eg2(over: Partial<ModelEntry> = {}): ModelEntry {
  return embeddingRow(
    "embeddinggemma-2",
    "EmbeddingGemma 2",
    "EmbeddingGemma 2 \u2014 8-bit GGUF run by llama.cpp. Over 100 languages, so a fact said in Swahili is found by a question in English. 256 dimensions.",
    {
      size_mb: 295,
      ram_estimate_mb: 368,
      filename: "embeddinggemma-2-Q8_0.gguf",
      recommended: { rank: "primary", reason: "Finds facts in Swahili, Sheng or English, whichever language you ask in" },
      ...over,
    },
  );
}

/** The roles reply with Memory on `model` ("" means nobody chose: the pond runs its default). */
export function memoryRoles(model: string, provider = "auto"): ModelActiveRoles {
  return { ...NO_ROLES, embedding: { provider, model } };
}

/** The Orin's reading with nothing loaded: 4.2 GB fits, 4.2 GB with its picture support does not. */
export const ORIN_MEMORY: ModelMemoryStatus = {
  total_mb: 7620,
  available_for_llm_mb: 5820,
  loaded_model: null,
  reclaimable_mb: 0,
};

/** The live nano at the re-score: E4B and a Memory model running. 309 MB available leaves a Memory
 *  model 189 MB of room once EmbeddingGemma 2's share comes back. */
export const ORIN_MEMORY_SERVING: ModelMemoryStatus = {
  total_mb: 7620,
  available_for_llm_mb: 309,
  budget_mb: 5820,
  loaded_model: null,
  reclaimable_mb: 4965,
  embedder_room_mb: 189,
};

/** A desktop with room to spare: the primary pick fits with its picture support. */
export const DESKTOP_MEMORY: ModelMemoryStatus = {
  total_mb: 16384,
  available_for_llm_mb: 12288,
  loaded_model: null,
  reclaimable_mb: 0,
};

export const NO_ROLES: ModelActiveRoles = {
  chat: null,
  tool: null,
  asr: null,
  tts: null,
  embedding: null,
};

export function rolesWith(chat: { provider: string; model: string } | null): ModelActiveRoles {
  return { ...NO_ROLES, chat };
}

export function entry(over: Partial<DownloadEntry> & Pick<DownloadEntry, "filename">): DownloadEntry {
  return {
    category: "gguf",
    downloaded_bytes: 0,
    total_bytes: null,
    status: "downloading",
    resumable: true,
    ...over,
  };
}
