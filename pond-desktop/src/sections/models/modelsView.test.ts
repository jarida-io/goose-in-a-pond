import { describe, it, expect } from "vitest";
import {
  ROLES, roleHolder, formatSize, formatBytes, downloadPercent, isInFlight,
  fitReading, downloadedOnly, availableToDownload, rolesFor, modelLabel, groupByJob,
  modelFacts,
} from "./modelsView";
import type { ModelActiveRoles, ModelEntry, ModelMemoryStatus } from "../../api/types";

function model(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "m1", provider: "gguf", name: "gemma-3-4b-it-q4.gguf",
    is_active: false, downloaded: true, size_mb: 2600, ...over,
  } as ModelEntry;
}

const memory = (over: Partial<ModelMemoryStatus> = {}): ModelMemoryStatus => ({
  total_mb: 8192, available_for_llm_mb: 6144, loaded_model: null, ...over,
});

describe("roles", () => {
  it("names the four jobs a pond fills, in the household's words", () => {
    expect(ROLES.map((r) => r.key)).toEqual(["chat", "asr", "tts", "embedding"]);
    // Jobs, not model categories: nothing here says "LLM" or "ASR".
    for (const r of ROLES) expect(r.label).not.toMatch(/LLM|ASR|TTS|embedding/i);
  });

  it("reports who holds a job, and says nothing when nobody does", () => {
    const roles = {
      chat: { provider: "gguf", model: "gemma-3-4b" },
      asr: null,
      tts: { provider: "piper", model: "  " },
      embedding: null,
      tool: null,
    } as unknown as ModelActiveRoles;

    expect(roleHolder(roles, "chat")).toBe("gemma-3-4b");
    expect(roleHolder(roles, "asr")).toBeNull();
    // A blank name is nobody, not somebody called "".
    expect(roleHolder(roles, "tts")).toBeNull();
    expect(roleHolder(null, "chat")).toBeNull();
  });
});

describe("sizes", () => {
  /// Megabytes past a few thousand stop being a quantity anybody pictures.
  it("switches to gigabytes once megabytes stop being readable", () => {
    expect(formatSize(480)).toBe("480 MB");
    expect(formatSize(1023)).toBe("1023 MB");
    expect(formatSize(1024)).toBe("1.0 GB");
    expect(formatSize(4198)).toBe("4.1 GB");
  });

  it("says nothing rather than '0 MB' when there is no size", () => {
    expect(formatSize(undefined)).toBe("");
    expect(formatSize(null)).toBe("");
    expect(formatSize(0)).toBe("");
  });

  it("converts bytes through the same ladder", () => {
    expect(formatBytes(0)).toBe("0 MB");
    expect(formatBytes(5 * 1_048_576)).toBe("5 MB");
    expect(formatBytes(3 * 1024 * 1_048_576)).toBe("3.0 GB");
  });
});

describe("download progress", () => {
  it("reports a percentage only when the total is known", () => {
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: 200 })).toBe(25);
    expect(downloadPercent({ downloaded_bytes: 200, total_bytes: 200 })).toBe(100);
    // Unknown total: null, so the bar can go indeterminate instead of sitting
    // at zero, which reads as stalled.
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: null })).toBeNull();
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: 0 })).toBeNull();
  });

  it("never exceeds 100 when the server over-reports", () => {
    expect(downloadPercent({ downloaded_bytes: 300, total_bytes: 200 })).toBe(100);
  });

  /// Paused is still in flight — it has a partial file and can be resumed.
  /// Cancelled and errored are not.
  it("counts paused as still in flight, and finished states as not", () => {
    expect(isInFlight({ status: "downloading" })).toBe(true);
    expect(isInFlight({ status: "paused" })).toBe(true);
    expect(isInFlight({ status: "done" })).toBe(false);
    expect(isInFlight({ status: "cancelled" })).toBe(false);
    expect(isInFlight({ status: "error" })).toBe(false);
  });
});

describe("the fit meter", () => {
  /// The page's one real claim: will this run here. Getting it wrong either
  /// scares someone off a model that fits or lets them spend an evening
  /// downloading one that does not.
  it("measures a model against the budget this device gives models", () => {
    const r = fitReading(model({ size_mb: 2048 }), memory({ available_for_llm_mb: 6144 }));
    expect(r.verdict).toBe("fits");
    // 6144 reported minus the 1024 headroom the verdict reserves = 5120 usable.
    expect(r.percent).toBe(40);
    expect(r.label).toContain("40%");
    expect(r.label).toContain("5.0 GB");
  });

  it("says a model larger than the budget would run slowly, not that it cannot", () => {
    const r = fitReading(model({ size_mb: 9000 }), memory({ available_for_llm_mb: 6144 }));
    expect(r.verdict).toBe("spills");
    expect(r.label).toMatch(/slowly/);
    // Reported past 100 so the bar can be pinned full and the number still tell
    // the truth about by how much.
    expect(r.percent).toBeGreaterThan(100);
  });

  /// A confident bar drawn from nothing is worse than no bar. The budget is
  /// absent on dev machines and on builds without the scheduler.
  it("admits when the device has not said what it can spare", () => {
    for (const m of [null, memory({ total_mb: 0 }), memory({ available_for_llm_mb: 0 })]) {
      const r = fitReading(model(), m);
      expect(r.verdict).toBe("unknown");
      expect(r.percent).toBeNull();
      expect(r.label).toMatch(/unknown/i);
    }
  });

  it("admits when the model has not said how big it is", () => {
    const r = fitReading({ size_mb: undefined, ram_estimate_mb: undefined }, memory());
    expect(r.verdict).toBe("unknown");
    expect(r.percent).toBeNull();
  });

  it("falls back to the RAM estimate when there is no file size", () => {
    const r = fitReading({ size_mb: undefined, ram_estimate_mb: 3072 }, memory());
    expect(r.verdict).toBe("fits");
    expect(r.percent).toBe(60);
  });

  /// The bar and the sentence must agree. Measured against the raw budget, a
  /// model at 88% of it was drawn as a comfortable bar under the words "bigger
  /// than" — the picture contradicting the verdict beside it.
  it("draws the bar against the same number the verdict uses", () => {
    const r = fitReading(model({ size_mb: 5400 }), memory({ available_for_llm_mb: 6144 }));
    expect(r.verdict).toBe("spills");
    expect(r.percent).toBeGreaterThan(100);
  });
});

describe("what a model can do", () => {
  it("takes the catalogue's recommendation when it has one", () => {
    expect(rolesFor({ provider: "gguf", recommended_role: "embedding" })).toEqual(["embedding"]);
  });

  /// Driven by the provider the catalogue filed it under, never by parsing the
  /// name — a name heuristic is a copy of a backend rule that has already moved.
  it("otherwise goes by provider, not by reading the filename", () => {
    expect(rolesFor({ provider: "whisper" })).toEqual(["asr"]);
    expect(rolesFor({ provider: "piper" })).toEqual(["tts"]);
    expect(rolesFor({ provider: "embedding" })).toEqual(["embedding"]);
    expect(rolesFor({ provider: "gguf" })).toEqual(["chat"]);
    // A whisper-shaped NAME under the gguf provider is still a chat model.
    expect(rolesFor({ provider: "gguf", recommended_role: undefined })).toEqual(["chat"]);
  });

  it("ignores a recommendation that names no job this pond has", () => {
    expect(rolesFor({ provider: "whisper", recommended_role: "vision" })).toEqual(["asr"]);
  });
});

describe("the list", () => {
  it("shows only what is actually on the disk", () => {
    const models = [model({ name: "here" }), model({ name: "not-here", downloaded: false })];
    expect(downloadedOnly(models).map((m) => m.name)).toEqual(["here"]);
  });

  it("prefers a display name, and falls back to the filename", () => {
    expect(modelLabel({ display_name: "Gemma 3 4B", name: "gemma.gguf" })).toBe("Gemma 3 4B");
    expect(modelLabel({ display_name: "   ", name: "gemma.gguf" })).toBe("gemma.gguf");
    expect(modelLabel({ display_name: undefined, name: "gemma.gguf" })).toBe("gemma.gguf");
  });
});

describe("grouping by job", () => {
  /// The same four words the Jobs band uses, so "Listening" means one thing on
  /// this page rather than "ASR" at the top and "Whisper" further down.
  it("gathers models under the job each one can do, in the Jobs band's order", () => {
    const groups = groupByJob([
      model({ name: "piper.onnx", provider: "piper" }),
      model({ name: "gemma.gguf", provider: "gguf" }),
      model({ name: "whisper.bin", provider: "whisper" }),
      model({ name: "nomic.gguf", provider: "gguf", recommended_role: "embedding" }),
      model({ name: "qwen.gguf", provider: "gguf" }),
    ]);

    expect(groups.map((g) => g.key)).toEqual(["chat", "asr", "tts", "embedding"]);
    expect(groups.map((g) => g.label)).toEqual(["Conversation", "Listening", "Speaking", "Memory"]);
    expect(groups[0].models.map((m) => m.name)).toEqual(["gemma.gguf", "qwen.gguf"]);
    expect(groups[1].models.map((m) => m.name)).toEqual(["whisper.bin"]);
    expect(groups[3].models.map((m) => m.name)).toEqual(["nomic.gguf"]);
  });

  /// A heading over nothing is a question the page cannot answer.
  it("drops jobs nothing on this device can do", () => {
    const groups = groupByJob([model({ provider: "whisper" })]);
    expect(groups.map((g) => g.key)).toEqual(["asr"]);
  });

  it("has nothing to show for an empty device", () => {
    expect(groupByJob([])).toEqual([]);
  });
});

describe("names the scan invents", () => {
  /// The filesystem scan stamps every file it finds with "(detected on disk)",
  /// and the client maps description to display name — so those arrived as a
  /// card called "(detected on disk)", a name identifying nothing, on exactly
  /// the models a person is least likely to recognise.
  it("falls back to the filename when the catalogue only has a placeholder", () => {
    expect(modelLabel({ display_name: "(detected on disk)", name: "gemma-4-e2b.gguf" }))
      .toBe("gemma-4-e2b.gguf");
  });

  it("still prefers a real description", () => {
    expect(modelLabel({ display_name: "Gemma 4 E2B Instruct", name: "gemma-4-e2b.gguf" }))
      .toBe("Gemma 4 E2B Instruct");
  });
});

describe("what the file says about itself", () => {
  /// Read from the structured fields the server now fills from a GGUF header,
  /// not re-parsed out of a description somebody else formatted.
  it("lays out quantisation and context window", () => {
    expect(modelFacts({ quantization: "Q4_K_M", context_length: 131072 }))
      .toEqual(["Q4_K_M", "131,072 ctx"]);
  });

  it("spells a whisper build in words rather than codes", () => {
    expect(modelFacts({ asr_size: "base", asr_language: "en" })).toEqual(["base", "English"]);
    expect(modelFacts({ asr_size: "large", asr_language: "multilingual" }))
      .toEqual(["large", "Multilingual"]);
  });

  /// A row reading "unknown · unknown" is worse than one with just a name and
  /// a size. Absent facts are absent.
  it("says nothing at all when the header carried nothing", () => {
    expect(modelFacts({})).toEqual([]);
    expect(modelFacts({ quantization: undefined, context_length: 0 })).toEqual([]);
  });

  it("adds Reads pictures only when the server said exactly true", () => {
    expect(modelFacts({ quantization: "Q4_K_M", reads_images: true }))
      .toEqual(["Q4_K_M", "Reads pictures"]);
    expect(modelFacts({ reads_images: false })).toEqual([]);
    expect(modelFacts({ reads_images: undefined })).toEqual([]);
  });
});

describe("what can still be added", () => {
  /// The catalogue ships whisper builds and piper voices with download URLs
  /// already attached. Showing only what was downloaded dropped every one of
  /// them, so a pond could add a chat model from Hugging Face but had no route
  /// at all to a second voice or a better transcriber.
  it("offers the speech models the catalogue knows about", () => {
    const catalogue = [
      model({ name: "ggml-base.en", provider: "whisper", downloaded: false, asr_size: "base" }),
      model({ name: "ggml-small.en", provider: "whisper", downloaded: true }),
      model({ name: "en_US-amy-medium", provider: "piper", downloaded: false }),
      model({ name: "gemma.gguf", provider: "gguf", downloaded: false }),
    ];

    const groups = groupByJob(availableToDownload(catalogue));
    expect(groups.map((g) => g.label)).toEqual(["Conversation", "Listening", "Speaking"]);
    expect(groups.find((g) => g.key === "asr")!.models.map((m) => m.name))
      .toEqual(["ggml-base.en"]);
    expect(groups.find((g) => g.key === "tts")!.models.map((m) => m.name))
      .toEqual(["en_US-amy-medium"]);
  });

  it("splits the catalogue cleanly in two", () => {
    const catalogue = [
      model({ name: "here", downloaded: true }),
      model({ name: "not-here", downloaded: false }),
      model({ name: "unstated", downloaded: undefined }),
    ];
    expect(downloadedOnly(catalogue).map((m) => m.name)).toEqual(["here"]);
    // An unstated flag counts as absent — the safe direction, since the worst
    // it costs is offering a download for something already present.
    expect(availableToDownload(catalogue).map((m) => m.name)).toEqual(["not-here", "unstated"]);
  });
});
