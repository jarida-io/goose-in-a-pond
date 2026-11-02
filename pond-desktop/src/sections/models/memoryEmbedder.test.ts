import { describe, expect, it } from "vitest";
import type { ContextIndexHealth, EmbedderHealth } from "../../api/types";
import {
  embedderLine, embedderTitle, embedderWatching, FIT_WORD, holderBadge, holderText, holdsMemory, isOutOfMemory,
  jobState, memoryChosenText, memoryCost, memoryFit, memoryHolder, memoryIsOff, memoryModels, memoryNeed,
  memoryPress, NEEDS_CONNECTION, resolvedMemoryName,
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

  it("reads EmbeddingGemma 2 for its 256-wide stamp, and names the others as the catalogue does", () => {
    expect(embedderTitle("embeddinggemma-2-256")).toBe("EmbeddingGemma 2");
    expect(embedderTitle("nomic-embed-text-v1.5")).toBe("Nomic Embed Text v1.5");
    expect(embedderTitle("all-MiniLM-L6-v2")).toBe("all-MiniLM-L6-v2");
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

  // The totals add up every store (memories, connected items, summaries), so they are counted as items.
  it("counts reading again from the items reached against the items there are", () => {
    const line = embedderLine(health({}, { rows: 95, matching: 40, mismatched: 55, missing: 0, coverage: 40 / 95 }));
    expect(line).toMatchObject({
      kind: "reindexing", text: "Reading memories again: 40 of 95 items", progress: { done: 40, total: 95 },
    });
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

  it("explains a failed load in plain words, keeps the pond's own words for Details, and offers another go", () => {
    const line = embedderLine(health({ state: "failed", reason: "loading x.gguf: bad magic number" }));
    expect(line).toMatchObject({ kind: "failed", retry: true, detail: "loading x.gguf: bad magic number" });
    expect(line?.text).toBe(
      "EmbeddingGemma 2 did not load, so Memory is searching by keyword. Try again, or choose another Memory model.",
    );
  });

  it("says plainly that there is not enough memory, and names the lighter model, keeping the raw reason aside", () => {
    const line = embedderLine(health({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }));
    expect(line).toMatchObject({ kind: "failed", retry: true, detail: "loading x.gguf on the Gpu: out of memory" });
    expect(line?.text).toBe(
      "There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword. " +
        "Choose the lighter all-MiniLM-L6-v2 (23 MB), or try again.",
    );
    expect(line?.text).not.toMatch(/Gpu|gguf|out of memory/);
  });

  it("does not send the lightest model to itself when it is the one that ran out of room", () => {
    const line = embedderLine(
      health({ state: "failed", model_id: "all-MiniLM-L6-v2", reason: "out of memory" }, { model_id: "all-MiniLM-L6-v2" }),
    );
    expect(line?.text).toBe(
      "There is not enough free memory to load all-MiniLM-L6-v2, so Memory is searching by keyword. Try again later.",
    );
  });

  it("recognises a load that ran out of room by the words engines use for it", () => {
    expect(isOutOfMemory("loading x.gguf on the Gpu: out of memory")).toBe(true);
    expect(isOutOfMemory("CUDA OOM while loading")).toBe(true);
    expect(isOutOfMemory("failed to allocate 400 MiB")).toBe(true);
    expect(isOutOfMemory("bad magic number")).toBe(false);
    expect(isOutOfMemory(null)).toBe(false);
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

  it("says a model already here is now in use, and that the pond reads memories again", () => {
    expect(memoryCost(eg2({ downloaded: true }))).toBe("On this device, 295 MB");
    expect(memoryChosenText(eg2({ downloaded: true }))).toBe(
      "Now using EmbeddingGemma 2 for memory. The pond reads memories again at the next quiet moment.",
    );
  });

  it("says a built-in model is fetched by the library, and is not a download here", () => {
    expect(memoryCost(miniLm())).toBe("23 MB, fetched the first time it is used");
    expect(memoryChosenText(miniLm())).toMatch(/^Now using all-MiniLM-L6-v2 for memory/);
  });

  // Nothing loads, downloads or reads again while the switch is off, so none of that may be promised.
  it("says the model is set and used once search by meaning is on, without promising a read or a fetch", () => {
    const here = memoryChosenText(eg2({ downloaded: true }), { off: true });
    expect(here).toBe("EmbeddingGemma 2 is set for memory. The pond uses it once you turn on search by meaning.");
    expect(here).not.toMatch(/reads|Now using|once it downloads/);
    expect(memoryChosenText(eg2(), { off: true })).toBe(
      "EmbeddingGemma 2 is set for memory. Once you turn on search by meaning, the pond downloads it (295 MB) and uses it.",
    );
  });
});

describe("the button on a Memory row", () => {
  const on = { off: false, offline: false };

  it("says Download and use only where the press starts a download", () => {
    expect(memoryPress(eg2(), on)).toMatchObject({ label: "Download and use", download: true, size: "295 MB", blocked: null });
    expect(memoryPress(eg2({ downloaded: true }), on)).toMatchObject({ label: "Use", download: false });
    expect(memoryPress(miniLm(), on)).toMatchObject({ label: "Use", download: false });
  });

  it("stays a plain Use while search by meaning is off, because nothing is fetched then", () => {
    expect(memoryPress(eg2(), { off: true, offline: false })).toMatchObject({ label: "Use", download: false, blocked: null });
  });

  it("rests, and says why, when the file is not here and the pond reaches nothing outside the house", () => {
    expect(memoryPress(eg2(), { off: false, offline: true })).toMatchObject({ label: "Download and use", blocked: NEEDS_CONNECTION });
    expect(NEEDS_CONNECTION).toBe("Needs a connection to download");
  });

  it("never rests a model that needs no download, offline or not", () => {
    expect(memoryPress(eg2({ downloaded: true }), { off: false, offline: true }).blocked).toBeNull();
    expect(memoryPress(miniLm(), { off: false, offline: true }).blocked).toBeNull();
  });
});

describe("what the model's row and line may claim", () => {
  const said = (embedder: Partial<EmbedderHealth>, opts: { downloading?: boolean } = {}, over: Partial<ContextIndexHealth> = {}) =>
    embedderLine(health(embedder, over), opts);

  it("says In use only once the model runs, reading again included", () => {
    expect(holderBadge(said({}))).toEqual({ label: "In use", tone: "ok" });
    expect(holderBadge(said({}, {}, { matching: 40 }))).toEqual({ label: "In use", tone: "ok" });
    // The pond answered with nothing to say: the household's choice stands.
    expect(holderBadge(null)).toEqual({ label: "In use", tone: "ok" });
  });

  it("adds a word to the Jobs card only while the model is not running", () => {
    expect(jobState(said({}))).toBeNull();
    expect(jobState(null)).toBeNull();
    expect(jobState(said({ state: "failed", reason: "out of memory" }))).toEqual({ label: "Chosen, not loaded", tone: "stopped" });
    expect(jobState(said({ state: "loading" }))).toEqual({ label: "Loading", tone: "busy" });
    expect(jobState(null, { checking: true })).toEqual({ label: "Checking", tone: "busy" });
  });

  it("claims nothing before the pond's first answer", () => {
    expect(holderBadge(null, { checking: true })).toEqual({ label: "Checking", tone: "busy" });
    // An answer that arrived wins over the flag.
    expect(holderBadge(said({ state: "failed", reason: "out of memory" }), { checking: true }).label).toBe("Chosen, not loaded");
  });

  it("says a chosen model that failed is chosen and not loaded", () => {
    expect(holderBadge(said({ state: "failed", reason: "out of memory" }))).toEqual({
      label: "Chosen, not loaded", tone: "stopped",
    });
    expect(holderBadge(said({ state: "failed", reason: "x.gguf is not on disk; the models pipeline is downloading it" }))).toEqual({
      label: "Chosen, not downloaded", tone: "stopped",
    });
  });

  it("says a model still on its way is not in use yet", () => {
    expect(holderBadge(said({ state: "loading" }))).toEqual({ label: "Loading", tone: "busy" });
    expect(holderBadge(said({ state: "chosen" }))).toEqual({ label: "Waiting to load", tone: "wait" });
    expect(
      holderBadge(said({ state: "failed", reason: "is not on disk; the models pipeline is downloading it" }, { downloading: true })),
    ).toEqual({ label: "Downloading", tone: "busy" });
  });

  it("names the hub's Model line for what the embedder has done with it", () => {
    expect(holderText(said({}), "embeddinggemma-2-256")).toBe("Using EmbeddingGemma 2.");
    expect(holderText(said({ state: "failed", reason: "out of memory" }), "embeddinggemma-2-256")).toBe(
      "EmbeddingGemma 2 is chosen but did not load.",
    );
    expect(holderText(said({ state: "loading" }), "embeddinggemma-2-256")).toBe("EmbeddingGemma 2 is loading.");
    expect(holderText(said({ state: "chosen" }), "embeddinggemma-2-256")).toBe(
      "EmbeddingGemma 2 is chosen and loads at a quiet moment.",
    );
    expect(holderText(null, null)).toBe("Chosen on the Models screen.");
  });
});

describe("what a Memory model needs, and whether this pond has room", () => {
  it("says what a model needs while it runs, from the pond's estimate", () => {
    expect(memoryNeed(eg2())).toBe("Needs about 368 MB while it runs");
    expect(memoryNeed(miniLm())).toBe("Needs about 50 MB while it runs");
    expect(memoryNeed(eg2({ ram_estimate_mb: undefined }))).toBeNull();
  });

  // The live nano at the re-score: 309 MB available, so 189 MB of room once EmbeddingGemma 2 gives its share back.
  it("reads the need against the room in a word", () => {
    expect(memoryFit(miniLm(), 189)).toBe("fits");
    expect(memoryFit(eg2(), 189)).toBe("tight");
    expect(memoryFit(eg2(), 368)).toBe("fits");
    expect(FIT_WORD.fits).toBe("Fits");
    expect(FIT_WORD.tight).toBe("Tight on this pond");
  });

  it("claims nothing where the pond read no room or the catalogue lists no need", () => {
    expect(memoryFit(eg2(), null)).toBeNull();
    expect(memoryFit(eg2(), undefined)).toBeNull();
    expect(memoryFit(eg2({ ram_estimate_mb: undefined }), 1000)).toBeNull();
    expect(memoryFit(eg2(), 0)).toBe("tight");
  });
});
