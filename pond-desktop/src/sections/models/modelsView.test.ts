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
    // Unknown total: null, so the bar goes indeterminate rather than reading as stalled at zero.
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: null })).toBeNull();
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: 0 })).toBeNull();
  });

  it("never exceeds 100 when the server over-reports", () => {
    expect(downloadPercent({ downloaded_bytes: 300, total_bytes: 200 })).toBe(100);
  });

  // Paused has a partial file and can resume.
  it("counts paused as still in flight, and finished states as not", () => {
    expect(isInFlight({ status: "downloading" })).toBe(true);
    expect(isInFlight({ status: "paused" })).toBe(true);
    expect(isInFlight({ status: "done" })).toBe(false);
    expect(isInFlight({ status: "cancelled" })).toBe(false);
    expect(isInFlight({ status: "error" })).toBe(false);
  });
});

describe("the fit meter", () => {
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
    // Past 100: the bar pins full while the number still says by how much.
    expect(r.percent).toBeGreaterThan(100);
  });

  // The budget is absent on dev machines and builds without the scheduler.
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

  it("otherwise goes by provider, not by reading the filename", () => {
    expect(rolesFor({ provider: "whisper" })).toEqual(["asr"]);
    expect(rolesFor({ provider: "piper" })).toEqual(["tts"]);
    expect(rolesFor({ provider: "embedding" })).toEqual(["embedding"]);
    expect(rolesFor({ provider: "gguf" })).toEqual(["chat"]);
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

  it("drops jobs nothing on this device can do", () => {
    const groups = groupByJob([model({ provider: "whisper" })]);
    expect(groups.map((g) => g.key)).toEqual(["asr"]);
  });

  it("has nothing to show for an empty device", () => {
    expect(groupByJob([])).toEqual([]);
  });
});

describe("names the scan invents", () => {
  // The disk scan writes "(detected on disk)" as the description, which the client shows as the name.
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
  it("lays out quantisation and context window", () => {
    expect(modelFacts({ quantization: "Q4_K_M", context_length: 131072 }))
      .toEqual(["Q4_K_M", "131,072 ctx"]);
  });

  it("spells a whisper build in words rather than codes", () => {
    expect(modelFacts({ asr_size: "base", asr_language: "en" })).toEqual(["base", "English"]);
    expect(modelFacts({ asr_size: "large", asr_language: "multilingual" }))
      .toEqual(["large", "Multilingual"]);
  });

  it("says nothing at all when the header carried nothing", () => {
    expect(modelFacts({})).toEqual([]);
    expect(modelFacts({ quantization: undefined, context_length: 0 })).toEqual([]);
  });
});

describe("what can still be added", () => {
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
    // An unstated flag counts as absent: at worst it offers a download already present.
    expect(availableToDownload(catalogue).map((m) => m.name)).toEqual(["not-here", "unstated"]);
  });
});
