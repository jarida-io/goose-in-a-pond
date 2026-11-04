import { describe, it, expect } from "vitest";
import {
  ROLES, roleHolder, formatSize, formatBytes, fitReading, budgetReading, downloadedOnly,
  rolesFor, modelLabel, groupBySection, modelFacts, sourceChip, addOnOf,
  offersPictures, downloadSummary, recommendedPicks, measuredOf, raisedPick, askingPick,
  isInUse, holderEntry, sortModels, fitsOnlyWithoutPictures, fileFitsOnlyWithoutPictures, PICTURES_LEFT_OUT,
  unavailableNote, usesOf, sectionOf, jobLabel,
} from "./modelsView";
import type { HelperModel, ModelActiveRoles, ModelEntry, ModelMemoryStatus } from "../../api/types";
import {
  DESKTOP_MEMORY, E4B_ORIN, ORIN_MEMORY, e2b, e4b, foundOnDisk, functionGemma, litertE2b, litertE4b,
  llamafile, ollama, rolesWith, voice, whisper,
} from "./fixtures";

function model(over: Partial<ModelEntry> = {}): ModelEntry {
  return {
    id: "gguf/gemma-3-4b-it-q4", provider: "gguf", name: "gemma-3-4b-it-q4.gguf",
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
  it("writes them as the pond's own download messages do: decimal GB, whole MB", () => {
    // What the server announces for the pick: "Downloading Gemma 4 E4B (4.2 GB)".
    expect(formatSize(4020)).toBe("4.2 GB");
    expect(formatSize(2498)).toBe("2.6 GB");
    expect(formatSize(480)).toBe("480 MB");
    expect(formatSize(941)).toBe("941 MB");
    expect(formatBytes(986_833_728)).toBe("941 MB");
    expect(formatBytes(991_552_320)).toBe("945 MB");
    expect(formatBytes(4_215_695_776)).toBe("4.2 GB");
  });

  it("changes unit at 1 GB of bytes, where the server does", () => {
    expect(formatBytes(999_999_999)).toBe("953 MB");
    expect(formatBytes(1_000_000_000)).toBe("1.0 GB");
  });

  it("says nothing rather than '0 MB' when there is no size", () => {
    expect(formatSize(undefined)).toBe("");
    expect(formatSize(null)).toBe("");
    expect(formatSize(0)).toBe("");
  });

  it("writes a byte count of nothing as 0 MB", () => {
    expect(formatBytes(0)).toBe("0 MB");
    expect(formatBytes(undefined)).toBe("0 MB");
  });
});

describe("the fit column", () => {
  it("measures a model against the room this pond has for one model", () => {
    const r = fitReading(model({ size_mb: 2048 }), memory({ available_for_llm_mb: 6144 }));
    expect(r.state).toBe("fits");
    // 6144 free minus the 1024 headroom = 5120 usable.
    expect(r.percent).toBe(40);
    expect(r.text).toBe("40%");
    expect(r.label).toContain("5.4 GB");
  });

  it("reads 'In use' for the model in use, never a percentage", () => {
    const r = fitReading(model({ size_mb: 2048 }), memory(), { inUse: true });
    expect(r.state).toBe("in_use");
    expect(r.text).toBe("In use");
    expect(r.percent).toBeNull();
  });

  it("reads 'Too big for this pond' over budget, and never a percentage that runs into the thousands", () => {
    // 4 MB of budget, once headroom is set aside: the old reading was 300,000%.
    const r = fitReading(model({ size_mb: 9000 }), memory({ available_for_llm_mb: 1028 }));
    expect(r.state).toBe("too_big");
    expect(r.text).toBe("Too big for this pond");
    expect(r.percent).toBeNull();
    expect(r.text).not.toMatch(/\d%/);
    expect(r.label).toMatch(/Needs 9\.4 GB/);
  });

  it("caps a model that only just fits at 100", () => {
    const r = fitReading(model({ size_mb: 5120 }), memory({ available_for_llm_mb: 6144 }));
    expect(r.state).toBe("fits");
    expect(r.percent).toBe(100);
  });

  it("counts what a switch frees, so the model in use never blocks its replacement", () => {
    const loaded = memory({ available_for_llm_mb: 1000, reclaimable_mb: 2600 });
    expect(fitReading(model({ size_mb: 2400 }), loaded).state).toBe("fits");
    expect(fitReading(model({ size_mb: 2400 }), { ...loaded, reclaimable_mb: 0 }).state).toBe("too_big");
  });

  it("counts the picture add-on a download will bring, and only where it is included", () => {
    // E4B: 4020 MB of weights plus 945 MB of add-on, against 4800 MB usable.
    const room = memory({ available_for_llm_mb: 5824 });
    expect(fitReading(e4b(), room).state).toBe("fits");
    expect(fitReading(e4b(), room, { withPictures: true }).state).toBe("too_big");
  });

  it("admits when the device has not said what it can spare", () => {
    for (const m of [null, memory({ total_mb: 0 }), memory({ available_for_llm_mb: 0 })]) {
      const r = fitReading(model(), m);
      expect(r.state).toBe("unknown");
      expect(r.percent).toBeNull();
      expect(r.text).toBe("—");
      expect(r.label).toMatch(/does not report a memory budget/);
    }
  });

  it("admits when the model has not said how big it is", () => {
    const r = fitReading({ size_mb: undefined, ram_estimate_mb: undefined, provider: "gguf" }, memory());
    expect(r.state).toBe("unknown");
  });

  it("falls back to the RAM estimate when there is no file size", () => {
    const r = fitReading({ size_mb: undefined, ram_estimate_mb: 3072, provider: "gguf" }, memory());
    expect(r.state).toBe("fits");
    expect(r.percent).toBe(60);
  });

  it("makes no claim for a model another program runs, whose memory is not the pond's", () => {
    const r = fitReading(ollama(), memory({ available_for_llm_mb: 500 }));
    expect(r.state).toBe("outside");
    expect(r.text).toBe("Runs in Ollama");
    expect(fitReading(llamafile(), memory()).text).toBe("Runs in llamafile");
  });
});

describe("the header's budget", () => {
  it("is the same room the rows are judged against", () => {
    const mem = memory({ available_for_llm_mb: 6144 });
    expect(budgetReading(mem).text).toBe("5.4 GB");
    expect(fitReading(model({ size_mb: 5120 }), mem).label).toContain(budgetReading(mem).text);
  });

  it("never renders blank, however empty the budget", () => {
    expect(budgetReading(null).text).toBe("—");
    expect(budgetReading(memory({ total_mb: 0, available_for_llm_mb: 0 })).text).toBe("—");
    // A budget the model in use has used up reads 0 MB, not "".
    expect(budgetReading(memory({ available_for_llm_mb: 900 })).text).toBe("0 MB");
    expect(budgetReading(null).label).toMatch(/does not report/);
  });

  it("counts what leaving the model in use returns", () => {
    expect(budgetReading(memory({ available_for_llm_mb: 1000, reclaimable_mb: 4000 })).text).toBe("4.2 GB");
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

  it("never offers a helper as conversation", () => {
    // The tool-call helper has the role "tool"; before `kind`, it fell through to chat.
    expect(rolesFor(functionGemma())).toEqual([]);
    expect(rolesFor({ provider: "gguf", kind: "helper", recommended_role: "chat" })).toEqual([]);
    expect(rolesFor({ provider: "gguf", recommended_role: "tool" })).toEqual(["chat"]);
  });

  it("keeps speech and voices on their jobs though the server calls them helpers", () => {
    expect(rolesFor(whisper())).toEqual(["asr"]);
    expect(rolesFor(voice())).toEqual(["tts"]);
  });
});

describe("the list", () => {
  it("shows only what is actually on the disk", () => {
    const models = [model({ name: "here" }), model({ name: "not-here", downloaded: false })];
    expect(downloadedOnly(models).map((m) => m.name)).toEqual(["here"]);
  });
});

describe("a model's name", () => {
  it("is its title, never the catalogue's sentence", () => {
    expect(modelLabel(e4b())).toBe("Gemma 4 E4B");
  });

  it("never shows the placeholder the scan writes", () => {
    expect(modelLabel(foundOnDisk())).toBe("Llama-3.2-3B-Instruct-Q4_K_M");
    // An older server sends no title: the placeholder still never reads as a name.
    expect(modelLabel({ display_name: "(detected on disk)", name: "gemma-4-e2b.gguf", provider: "gguf" }))
      .toBe("gemma-4-e2b.gguf");
  });

  it("falls back to a real description, then the file name, for a server that sends no title", () => {
    expect(modelLabel({ display_name: "Gemma 3 4B", name: "gemma.gguf", provider: "gguf" })).toBe("Gemma 3 4B");
    expect(modelLabel({ display_name: "   ", name: "gemma.gguf", provider: "gguf" })).toBe("gemma.gguf");
    expect(modelLabel({ display_name: undefined, name: "gemma.gguf", provider: "gguf" })).toBe("gemma.gguf");
  });

  it("keeps a voice's own title, which the catalogue spells better than its description", () => {
    expect(modelLabel(voice())).toBe("Af_Heart");
  });
});

describe("where a row came from", () => {
  it("chips only what informs, and the catalogue earns none", () => {
    expect(sourceChip(e4b())).toBe("Recommended");
    expect(sourceChip(litertE2b())).toBeNull();
    expect(sourceChip(llamafile())).toBe("Added");
    expect(sourceChip(foundOnDisk())).toBe("Found on disk");
    expect(sourceChip(ollama())).toBe("Ollama");
  });
});

describe("what the file says about itself", () => {
  it("lays out quantisation and window", () => {
    expect(modelFacts({ quantization: "Q4_K_M", context_length: 131072 }))
      .toEqual(["Q4_K_M", "131,072-token window"]);
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

describe("picture support", () => {
  const withState = (state: "installed" | "available" | "downloading" | "verifying" | "not_on_this_device") =>
    e2b({ companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state }] });

  it("reads each state in the household's words", () => {
    expect(addOnOf(withState("installed")).text).toBe("Pictures included");
    expect(addOnOf(withState("available")).text).toBe("Add pictures · 941 MB");
    expect(addOnOf(withState("downloading")).kind).toBe("adding");
    expect(addOnOf(withState("verifying")).text).toBe("Checking picture support");
    expect(addOnOf(withState("not_on_this_device")).text).toBe("Pictures aren't available on this device");
  });

  it("reads 'Text only' for a model with no add-on, LiteRT-LM included", () => {
    expect(addOnOf(litertE4b()).text).toBe("Text only");
    expect(addOnOf(foundOnDisk()).text).toBe("Text only");
  });

  it("claims nothing for engines that read pictures their own way, or for speech", () => {
    expect(addOnOf(ollama()).kind).toBe("none");
    expect(addOnOf(llamafile()).kind).toBe("none");
    expect(addOnOf(whisper()).kind).toBe("none");
  });

  it("offers the add-on as a choice only before the model is on the device", () => {
    expect(offersPictures(e4b())).toBe(true);
    expect(offersPictures(e4b({ downloaded: true }))).toBe(false);
    expect(offersPictures(withState("installed"))).toBe(false);
    expect(offersPictures(withState("verifying"))).toBe(false);
    expect(offersPictures(withState("not_on_this_device"))).toBe(false);
    expect(offersPictures(litertE4b())).toBe(false);
  });

  it("says the number before it is spent, and drops the add-on when it is unticked", () => {
    expect(downloadSummary(e4b(), true)).toBe("4.2 GB + 945 MB for pictures");
    expect(downloadSummary(e4b(), false)).toBe("4.2 GB");
    expect(downloadSummary(litertE4b(), true)).toBe("3.7 GB");
    expect(downloadSummary(withState("not_on_this_device"), true)).toBe("2.6 GB");
    expect(downloadSummary(withState("installed"), true)).toBe("2.6 GB");
  });
});

describe("when the picture box starts unticked", () => {
  it("says why in the sentence the household reads beside it", () => {
    expect(PICTURES_LEFT_OUT).toBe("Left out: with pictures it would not fit this pond. Tick to include it anyway.");
  });

  it("is only when the model fits this pond and the model with its add-on does not", () => {
    // 4020 MB of weights; 945 MB of add-on; 4796 MB left for one model on the Orin's reading.
    expect(fitsOnlyWithoutPictures(e4b(), ORIN_MEMORY)).toBe(true);
  });

  it("is not when the add-on fits too: it stays in, ticked", () => {
    expect(fitsOnlyWithoutPictures(e4b(), DESKTOP_MEMORY)).toBe(false);
  });

  it("is not when the model is too big on its own: dropping the add-on would not help", () => {
    expect(fitsOnlyWithoutPictures(e4b(), memory({ available_for_llm_mb: 1030 }))).toBe(false);
  });

  it("is not when the pond cannot say how much room there is: nothing drops silently", () => {
    expect(fitsOnlyWithoutPictures(e4b(), memory({ total_mb: 0, available_for_llm_mb: 0 }))).toBe(false);
    expect(fitsOnlyWithoutPictures(e4b(), null)).toBe(false);
  });

  it("counts what switching away from the model in use returns, as the card does", () => {
    // 5820 MB free is the Orin's; 900 MB more comes back, so the add-on fits.
    expect(fitsOnlyWithoutPictures(e4b(), { ...ORIN_MEMORY, reclaimable_mb: 900 })).toBe(false);
  });

  it("is not for a model with no add-on to bring, or one already here", () => {
    expect(fitsOnlyWithoutPictures(litertE4b(), ORIN_MEMORY)).toBe(false);
    expect(fitsOnlyWithoutPictures(e4b({ downloaded: true }), ORIN_MEMORY)).toBe(false);
    expect(fitsOnlyWithoutPictures(ollama(), ORIN_MEMORY)).toBe(false);
  });

  it("agrees with the card: whenever it is true, ticking the box reads 'Too big' and leaving it out reads a fit", () => {
    expect(fitReading(e4b(), ORIN_MEMORY).state).toBe("fits");
    expect(fitReading(e4b(), ORIN_MEMORY, { withPictures: true }).state).toBe("too_big");
  });

  describe("for a file Hugging Face lists", () => {
    const addOn = { size_bytes: 991_552_320, label: "Gemma 4 E4B" };

    it("is the same question, asked of the sizes the listing gives", () => {
      expect(fileFitsOnlyWithoutPictures({ size_mb: 4020, pictures: addOn }, ORIN_MEMORY)).toBe(true);
      expect(fileFitsOnlyWithoutPictures({ size_mb: 1000, pictures: addOn }, ORIN_MEMORY)).toBe(false);
      expect(fileFitsOnlyWithoutPictures({ size_mb: 4020, pictures: addOn }, DESKTOP_MEMORY)).toBe(false);
    });

    it("cannot weigh a file whose size is not listed, so its add-on stays in", () => {
      expect(fileFitsOnlyWithoutPictures({ pictures: addOn }, ORIN_MEMORY)).toBe(false);
    });

    it("has nothing to leave out when the file brings no add-on", () => {
      expect(fileFitsOnlyWithoutPictures({ size_mb: 4020, pictures: null }, ORIN_MEMORY)).toBe(false);
    });
  });
});

describe("the recommended picks", () => {
  const catalogue = [litertE4b(), foundOnDisk(), e2b(), e4b(), functionGemma(), whisper()];

  it("are GIAP's suggestions for conversation, best first", () => {
    expect(recommendedPicks(catalogue).map((m) => m.title)).toEqual([
      "Gemma 4 E4B", "Gemma 4 E2B", "Gemma 4 E4B",
    ]);
    expect(recommendedPicks(catalogue).map((m) => m.recommended!.rank)).toEqual([
      "primary", "lighter", "alternative",
    ]);
  });

  it("show measured numbers only where the server sent them", () => {
    expect(measuredOf(e4b().recommended)).toBeNull();
    const orin = measuredOf({ ...e4b().recommended!, measured: E4B_ORIN });
    expect(orin?.text).toBe("First reply in about 1 s, 15-16 tokens a second, 16k window");
    expect(orin?.caption).toBe("Measured on this kind of device, 5 Oct 2026");
    expect(measuredOf(undefined)).toBeNull();
  });

  it("raise the one in use, else the one put first", () => {
    const picks = recommendedPicks(catalogue);
    expect(raisedPick(picks, null)?.id).toBe(e4b().id);
    expect(raisedPick(picks, rolesWith({ provider: "local", model: e2b().name }))?.id).toBe(e2b().id);
    expect(raisedPick([], null)).toBeNull();
  });

  it("ask the page's one question only while nothing converses, and not while it comes down", () => {
    const picks = recommendedPicks(catalogue);
    expect(askingPick(picks, null, () => false)?.id).toBe(e4b().id);
    expect(askingPick(picks, rolesWith({ provider: "local", model: "x" }), () => false)).toBeNull();
    expect(askingPick(picks, null, (m) => m.id === e4b().id)).toBeNull();
  });
});

describe("who holds the conversation", () => {
  const roles = rolesWith({ provider: "local", model: "gemma-4-E4B-it.litertlm" });

  it("matches the row the role names, though the role reads local", () => {
    expect(isInUse(litertE4b(), roles)).toBe(true);
    expect(isInUse(e4b(), roles)).toBe(false);
    expect(holderEntry([e4b(), litertE4b()], roles, "chat")?.id).toBe(litertE4b().id);
  });

  it("does not mistake another engine's model of the same name", () => {
    const other = rolesWith({ provider: "ollama", model: "qwen3:4b" });
    expect(isInUse(ollama(), other)).toBe(true);
    expect(isInUse(ollama(), roles)).toBe(false);
    expect(isInUse(model({ name: "qwen3:4b" }), other)).toBe(false);
  });

  it("finds nothing when nobody holds the job", () => {
    expect(holderEntry([e4b()], null, "chat")).toBeNull();
    expect(isInUse(e4b(), null)).toBe(false);
  });
});

describe("sorting and grouping", () => {
  it("lists picks first, best first, then by name", () => {
    const sorted = sortModels([foundOnDisk(), litertE2b(), e2b(), e4b(), llamafile("a-model")]);
    expect(sorted.map((m) => m.name)).toEqual([
      e4b().name, e2b().name, "a-model", "gemma-4-E2B-it.litertlm", "Llama-3.2-3B-Instruct-Q4_K_M",
    ]);
  });

  it("gathers a pond older than sections under the job each model can do, in the Jobs band's order", () => {
    const groups = groupBySection([
      model({ name: "piper.onnx", provider: "piper" }),
      model({ name: "gemma.gguf", provider: "gguf" }),
      model({ name: "whisper.bin", provider: "whisper" }),
      model({ name: "nomic.gguf", provider: "gguf", recommended_role: "embedding" }),
      model({ name: "qwen.gguf", provider: "gguf" }),
    ], [], null);

    expect(groups.map((g) => g.key)).toEqual(["conversation", "listening", "speaking", "memory"]);
    expect(groups.map((g) => g.label)).toEqual(["Conversation", "Listening", "Speaking", "Memory"]);
    expect(groups[0].models.map((m) => m.name)).toEqual(["gemma.gguf", "qwen.gguf"]);
    expect(groups[1].models.map((m) => m.name)).toEqual(["whisper.bin"]);
    expect(groups[3].models.map((m) => m.name)).toEqual(["nomic.gguf"]);
  });

  it("keeps a helper out of Conversation, under Other", () => {
    const groups = groupBySection([e2b({ downloaded: true }), functionGemma()], [], null);
    expect(groups.map((g) => g.key)).toEqual(["conversation", "other"]);
    expect(groups[0].models.map((m) => m.title)).toEqual(["Gemma 4 E2B"]);
  });

  it("drops sections nothing on this device is in", () => {
    expect(groupBySection([model({ provider: "whisper" })], [], null).map((g) => g.key)).toEqual(["listening"]);
    expect(groupBySection([], [], null)).toEqual([]);
  });

});

describe("sections and the jobs each model is doing", () => {
  const helper = (over: Partial<HelperModel> = {}): HelperModel => ({
    id: "gguf/e4b#pictures", title: "Picture support", uses: ["pictures"], section: "pictures",
    serves: "gguf/e4b", ...over,
  });

  it("reads a row's jobs from the pond, whatever the assignment flag says", () => {
    expect(usesOf(model({ provider: "embedding", is_active: false, roles: ["embedding"] }), null))
      .toEqual(["embedding"]);
    expect(usesOf(model({ roles: ["chat", "tool"] }), null)).toEqual(["chat", "tool"]);
    expect(usesOf(model({ roles: [] }), rolesWith({ provider: "local", model: "gemma-3-4b-it-q4.gguf" })))
      .toEqual([]);
  });

  it("works a job out from the holders on a pond that does not say", () => {
    const roles = rolesWith({ provider: "local", model: "gemma-3-4b-it-q4.gguf" });
    expect(usesOf(model(), roles)).toEqual(["chat"]);
    expect(usesOf(model({ name: "other.gguf" }), roles)).toEqual([]);
  });

  it("leaves off a job it has no words for", () => {
    expect(usesOf(model({ roles: ["chat", "reranking" as never] }), null)).toEqual(["chat"]);
  });

  it("names every job in the household's words", () => {
    expect(jobLabel("chat")).toBe("Conversation");
    expect(jobLabel("embedding")).toBe("Memory");
    expect(jobLabel("pictures")).toBe("Pictures");
    expect(jobLabel("drafter")).toBe("Faster answers");
    for (const use of ["tool", "asr", "tts", "camera", "faces"] as const) {
      expect(jobLabel(use)).not.toMatch(/^models\./);
    }
  });

  it("lists a row where the pond says, else under its job", () => {
    expect(sectionOf(model({ section: "other" }))).toBe("other");
    expect(sectionOf(model({ provider: "whisper" }))).toBe("listening");
    expect(sectionOf(functionGemma())).toBe("other");
  });

  it("shows the memory model in use though the pond fetched it itself, first in its section", () => {
    const groups = groupBySection([
      model({ name: "bge-base", provider: "embedding", downloaded: true, section: "memory", roles: [] }),
      model({ name: "minilm", provider: "embedding", downloaded: false, section: "memory", roles: ["embedding"] }),
      model({ name: "bge-small", provider: "embedding", downloaded: false, section: "memory", roles: [] }),
    ], [], null);
    expect(groups.map((g) => g.key)).toEqual(["memory"]);
    expect(groups[0].models.map((m) => m.name)).toEqual(["minilm", "bge-base"]);
  });

  it("puts the models in use beside the rows under their own sections", () => {
    const groups = groupBySection([
      model({ name: "e4b", section: "conversation", roles: ["chat"] }),
    ], [
      helper(),
      helper({ id: "camera_detector", title: "YOLOX-Nano", uses: ["camera"], serves: null }),
      helper({ id: "mtp", title: null, uses: ["drafter"], section: "other" }),
      helper({ id: "later", section: "reranking" as never }),
    ], null);
    expect(groups.map((g) => g.key)).toEqual(["conversation", "pictures", "other"]);
    expect(groups[1].helpers.map((h) => h.id)).toEqual(["gguf/e4b#pictures", "camera_detector"]);
    expect(groups[2].helpers.map((h) => h.id)).toEqual(["mtp", "later"]);
  });
});

describe("a model this pond cannot run", () => {
  it("says what the missing engine needs, and nothing for a model that runs", () => {
    expect(unavailableNote(litertE4b({ available: false, unavailable_reason: "litert_library_missing" })))
      .toBe("Needs the LiteRT-LM library, which this Pond does not have yet");
    expect(unavailableNote(litertE4b({ available: false }))).toBe("This pond cannot run this model");
    expect(unavailableNote(litertE4b({ available: true }))).toBeNull();
    // A pond older than the field lists every model as one it can run.
    expect(unavailableNote(e4b())).toBeNull();
  });
});
