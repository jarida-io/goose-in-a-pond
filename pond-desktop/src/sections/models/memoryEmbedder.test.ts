import { describe, expect, it } from "vitest";
import type { ContextIndexHealth, EmbedderHealth } from "../../api/types";
import {
  embedderLine, embedderTitle, embedderWatching, holdsMemory, memoryChosenText, memoryCost,
  memoryHolder, memoryIsOff, memoryModels, resolvedMemoryName,
} from "./memoryEmbedder";
import { e4b, eg2, memoryRoles, miniLm, nomicEmbed, NO_ROLES } from "./fixtures";
import { modelLabel } from "./modelsView";

function health(embedder: Partial<EmbedderHealth>, over: Partial<ContextIndexHealth> = {}): ContextIndexHealth {
  return {
    indexed: true,
    model_id: "embeddinggemma-2-256",
    dims: 256,
    rows: 95,
    matching: 95,
    mismatched: 0,
    missing: 0,
    coverage: 1,
    corpora: [],
    embedder: { state: "ready", model_id: "embeddinggemma-2-256", reason: null, ...embedder },
    ...over,
  };
}

describe("who holds Memory", () => {
  it("names the model a household chose", () => {
    expect(resolvedMemoryName(memoryRoles("embeddinggemma-2"))).toBe("embeddinggemma-2");
  });

  // The pond runs what the old engine setting implied, and the card must mark that row.
  it("names the default the pond runs when nobody chose", () => {
    expect(resolvedMemoryName(memoryRoles("", "fastembed"))).toBe("all-MiniLM-L6-v2");
    expect(resolvedMemoryName(memoryRoles("", "auto"))).toBe("all-MiniLM-L6-v2");
    expect(resolvedMemoryName(memoryRoles("", "gguf"))).toBe("nomic-embed-text-v1.5");
  });

  it("holds nothing while search by meaning is off, whatever model was named", () => {
    expect(resolvedMemoryName(memoryRoles("embeddinggemma-2", "none"))).toBeNull();
    expect(memoryIsOff(memoryRoles("embeddinggemma-2", "none"))).toBe(true);
    expect(memoryIsOff(memoryRoles("", "auto"))).toBe(false);
    expect(memoryIsOff(NO_ROLES)).toBe(false);
  });

  it("says which row is in use, and whether anyone chose it", () => {
    const models = [miniLm(), eg2()];
    expect(holdsMemory(eg2(), memoryRoles("embeddinggemma-2"))).toBe(true);
    expect(holdsMemory(miniLm(), memoryRoles("embeddinggemma-2"))).toBe(false);
    expect(memoryHolder(models, memoryRoles("", "fastembed"))).toMatchObject({
      name: "all-MiniLM-L6-v2", builtIn: true,
    });
    expect(memoryHolder(models, memoryRoles("embeddinggemma-2"))).toMatchObject({
      name: "embeddinggemma-2", builtIn: false,
    });
    expect(memoryHolder(models, memoryRoles("", "none"))).toBeNull();
  });

  it("lists the recommendation first and keeps the catalogue's order for the rest", () => {
    expect(memoryModels([miniLm(), nomicEmbed(), eg2(), e4b()]).map((m) => m.name)).toEqual([
      "embeddinggemma-2", "all-MiniLM-L6-v2", "nomic-embed-text-v1.5",
    ]);
  });
});

describe("a model's name", () => {
  // The server sends a row's whole description as its title.
  it("is the part of the description before the dash", () => {
    expect(modelLabel(eg2())).toBe("EmbeddingGemma 2");
    expect(modelLabel(miniLm())).toBe("all-MiniLM-L6-v2");
  });

  it("reads EmbeddingGemma 2 for its 256-wide stamp", () => {
    expect(embedderTitle("embeddinggemma-2-256")).toBe("EmbeddingGemma 2");
    expect(embedderTitle("nomic-embed-text-v1.5")).toBe("nomic-embed-text-v1.5");
    expect(embedderTitle(null)).toBe("the Memory model");
  });
});

describe("what the pond says while it switches", () => {
  it("says nothing it does not know", () => {
    expect(embedderLine(null)).toBeNull();
    const older = health({}) as Partial<ContextIndexHealth>;
    delete older.embedder;
    expect(embedderLine(older as ContextIndexHealth)).toBeNull();
  });

  it("says it is searching by keyword while the model loads", () => {
    expect(embedderLine(health({ state: "loading" }))?.text).toBe(
      "Searching by keyword while EmbeddingGemma 2 loads",
    );
  });

  it("says a chosen model is waiting for a quiet moment", () => {
    const line = embedderLine(health({ state: "chosen" }));
    expect(line?.kind).toBe("waiting");
    expect(line?.text).toContain("Searching by keyword");
  });

  it("counts the re-index from the rows reached against the rows there are", () => {
    const line = embedderLine(health({}, { rows: 95, matching: 40, mismatched: 55, missing: 0, coverage: 40 / 95 }));
    expect(line).toMatchObject({ kind: "reindexing", text: "Re-indexing: 40 of 95", progress: { done: 40, total: 95 } });
  });

  it("calls a finished re-index ready", () => {
    expect(embedderLine(health({}))).toMatchObject({ kind: "ready" });
    expect(embedderLine(health({}, { rows: 0, matching: 0 }))).toMatchObject({ kind: "ready" });
  });

  it("does not count more reached than there are rows", () => {
    expect(embedderLine(health({}, { rows: 4, matching: 5 }))?.kind).toBe("ready");
  });

  it("is quiet while the pond has no index to count", () => {
    expect(embedderLine(health({}, { indexed: false }))).toBeNull();
  });

  it("says search by meaning is off", () => {
    expect(embedderLine(health({ state: "off", model_id: null }, { indexed: false }))).toMatchObject({
      kind: "off", text: "Search by meaning is off. Memory searches by keyword.",
    });
  });

  // The pond marks a model whose file is still coming down as failed until the file arrives.
  it("reads a missing file that is on its way as progress, not a failure", () => {
    const fetching = health({
      state: "failed",
      reason: "embeddinggemma-2-Q8_0.gguf is not on disk; the models pipeline is downloading it",
    });
    const line = embedderLine(fetching, { downloading: true });
    expect(line).toMatchObject({ kind: "downloading", retry: false, detail: null });
    expect(line?.text).toBe("Searching by keyword until EmbeddingGemma 2 finishes downloading");
    const other = health({ state: "failed", reason: "loading it failed" });
    expect(embedderLine(other, { downloading: true })?.kind).toBe("downloading");
  });

  // Stopping the download leaves the model chosen and its file absent: the household must be able to ask again.
  it("says a file that is neither here nor on its way is not here yet, and offers to fetch it", () => {
    const stopped = health({
      state: "failed",
      reason: "embeddinggemma-2-Q8_0.gguf is not on disk; the models pipeline is downloading it",
    });
    expect(embedderLine(stopped)).toMatchObject({
      kind: "missing",
      text: "EmbeddingGemma 2 is not on this device yet, so Memory is searching by keyword.",
      retry: true,
      retryLabel: "Download",
      retryingLabel: "Starting",
      detail: null,
    });
  });

  it("explains a failed load in plain words, keeps the pond's own words, and offers another go", () => {
    const line = embedderLine(health({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }));
    expect(line).toMatchObject({ kind: "failed", retry: true, detail: "loading x.gguf on the Gpu: out of memory" });
    expect(line?.text).toBe(
      "EmbeddingGemma 2 did not load, so Memory is searching by keyword. Try again, or choose another Memory model.",
    );
  });

  it("offers no retry where the build cannot run the model at all", () => {
    const line = embedderLine(
      health({ state: "failed", reason: "embeddinggemma-2 runs on llama.cpp, which this build leaves out (the local-inference feature)" }),
    );
    expect(line).toMatchObject({ kind: "failed", retry: false });
    expect(line?.text).toBe("This build of the pond cannot run EmbeddingGemma 2. Choose another Memory model.");
  });
});

describe("when the page keeps asking", () => {
  it("watches a model on its way to ready, and a re-index in progress", () => {
    expect(embedderWatching(health({ state: "chosen" }))).toBe(true);
    expect(embedderWatching(health({ state: "loading" }))).toBe(true);
    expect(embedderWatching(health({}, { rows: 95, matching: 40 }))).toBe(true);
    expect(embedderWatching(health({ state: "failed", reason: "x is not on disk; the models pipeline is downloading it" }))).toBe(true);
  });

  it("stops once it is settled, off or failed for good", () => {
    expect(embedderWatching(null)).toBe(false);
    expect(embedderWatching(health({}))).toBe(false);
    expect(embedderWatching(health({ state: "off" }))).toBe(false);
    expect(embedderWatching(health({ state: "failed", reason: "out of memory" }))).toBe(false);
    expect(embedderWatching(health({}, { indexed: false, rows: undefined }))).toBe(false);
  });
});

describe("pressing Use", () => {
  it("says the size before a download is spent, and what Memory does until it arrives", () => {
    expect(memoryCost(eg2())).toBe("295 MB to download");
    expect(memoryChosenText(eg2())).toBe(
      "Using EmbeddingGemma 2 for memory once it downloads (295 MB). Memory searches by keyword until then.",
    );
  });

  it("says a model already here is now in use, and that the pond reads again", () => {
    expect(memoryCost(eg2({ downloaded: true }))).toBe("On this device, 295 MB");
    expect(memoryChosenText(eg2({ downloaded: true }))).toBe(
      "Now using EmbeddingGemma 2 for memory. The pond reads your memories again at the next quiet moment.",
    );
  });

  it("says a built-in model is fetched by the library, and is not a download here", () => {
    expect(memoryCost(miniLm())).toBe("23 MB, fetched the first time it is used");
    expect(memoryChosenText(miniLm())).toMatch(/^Now using all-MiniLM-L6-v2 for memory/);
  });
});
