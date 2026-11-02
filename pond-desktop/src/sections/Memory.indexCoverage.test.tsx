import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, fireEvent, within } from "@testing-library/react";
import { IndexCoveragePanel } from "./Memory";
import { coverageState, coverageNote, formatCoverage } from "./indexCoverage";
import { ConfirmProvider } from "../components/shared";
import { api } from "../api/PondApiClient";
import type { ContextCorpusCoverage, ContextIndexHealth, EmbedderHealth } from "../api/types";
import { memoryRoles } from "./models/fixtures";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getContextIndexHealth: vi.fn(),
    rebuildContextIndex: vi.fn(),
    getActiveRoles: vi.fn(),
    activateModel: vi.fn(),
    getDownloadProgress: vi.fn(),
  },
}));

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

function corpus(over: Partial<ContextCorpusCoverage> = {}): ContextCorpusCoverage {
  return {
    corpus: "memory",
    rows: 0,
    source_rows: 0,
    structurally_excluded: false,
    indexed_rows: 0,
    missing_rows: 0,
    mismatched: 0,
    coverage: null,
    ...over,
  };
}

/** An index at roughly 2% coverage. */
function thinlyIndexed(): ContextIndexHealth {
  return {
    indexed: true,
    model_id: "nomic-embed-text-v1.5",
    dims: 768,
    rows: 200,
    matching: 4,
    mismatched: 0,
    missing: 196,
    coverage: 0.02,
    corpora: [
      corpus({ corpus: "memory", rows: 17, indexed_rows: 0, missing_rows: 17, coverage: 0 }),
      corpus({ corpus: "context", rows: 183, indexed_rows: 4, missing_rows: 179, coverage: 4 / 183 }),
      corpus({ corpus: "summary", rows: 0, indexed_rows: 0, missing_rows: 0, coverage: null }),
    ],
  };
}

function renderPanel() {
  return render(
    <ConfirmProvider>
      <IndexCoveragePanel />
    </ConfirmProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mocked(api.getContextIndexHealth).mockResolvedValue(thinlyIndexed());
});
afterEach(() => cleanup());

describe("coverageState", () => {
  // Why states, not a percentage: an unwritten store is fine; unreachable rows are the defect.
  it("separates a store nobody has written to from one nobody can reach", () => {
    expect(coverageState({ rows: 0, indexed_rows: 0 })).toBe("vacant");
    expect(coverageState({ rows: 17, indexed_rows: 0 })).toBe("unindexed");
  });

  it("calls anything short of every row partial, and every row complete", () => {
    expect(coverageState({ rows: 17, indexed_rows: 16 })).toBe("partial");
    expect(coverageState({ rows: 17, indexed_rows: 17 })).toBe("complete");
  });

  // A filter that excludes every row also reports zero rows; only `source_rows` tells it from empty.
  it("separates an empty store from one whose every row is filtered out", () => {
    expect(coverageState({ rows: 0, indexed_rows: 0, source_rows: 0 })).toBe("vacant");
    expect(coverageState({ rows: 0, indexed_rows: 0, source_rows: 27 })).toBe("excluded");
  });

  it("does not report more indexed than stored as an unfinished job", () => {
    // A sweep can embed a row after the count was taken.
    expect(coverageState({ rows: 4, indexed_rows: 5 })).toBe("complete");
  });
});

describe("formatCoverage", () => {
  it("keeps a barely-populated index distinguishable from an empty one", () => {
    expect(formatCoverage(0.002)).toBe("<1%");
    expect(formatCoverage(0)).toBe("0%");
    expect(formatCoverage(0.02)).toBe("2%");
    expect(formatCoverage(1)).toBe("100%");
  });

  it("prints no percentage at all when there is nothing to cover", () => {
    expect(formatCoverage(null)).toBe("—");
    expect(formatCoverage(undefined)).toBe("—");
  });
});

describe("coverageNote", () => {
  it("says out loud that an unindexed corpus is unreachable", () => {
    expect(coverageNote(corpus({ rows: 17, indexed_rows: 0, missing_rows: 17, coverage: 0 })))
      .toMatch(/Not searchable/);
  });

  it("names vectors left behind by a previous embedder", () => {
    const note = coverageNote(
      corpus({ rows: 10, indexed_rows: 6, missing_rows: 2, mismatched: 2, coverage: 0.6 }),
    );
    expect(note).toMatch(/2 still to embed/);
    expect(note).toMatch(/another model/);
  });

  it("does not scold a store nobody has written to", () => {
    expect(coverageNote(corpus())).toBe("Nothing stored yet.");
  });

  it("says an excluded corpus is unreachable and that embedding will not help", () => {
    const note = coverageNote(
      corpus({ corpus: "summary", rows: 0, source_rows: 27, structurally_excluded: true }),
    );
    expect(note).toContain("27 stored");
    // "Not indexed" would invite waiting for a sweep that can never pick these up.
    expect(note).toMatch(/will not fix/i);
  });
});

describe("IndexCoveragePanel", () => {
  it("shows the overall figure, the model it belongs to, and every corpus", async () => {
    renderPanel();

    expect(await screen.findByText("2%")).toBeTruthy();
    expect(screen.getByText("4 of 200 rows searchable")).toBeTruthy();
    expect(screen.getByText(/nomic-embed-text-v1\.5/)).toBeTruthy();
    expect(screen.getByText("Memories")).toBeTruthy();
    expect(screen.getByText("Context")).toBeTruthy();
    expect(screen.getByText("Summaries")).toBeTruthy();
  });

  it("makes a corpus at zero unmissable without alarming about an empty one", async () => {
    renderPanel();

    expect(await screen.findByText("0 / 17")).toBeTruthy();
    expect(screen.getByText(/Not searchable/)).toBeTruthy();

    // The summary corpus: empty, not failing.
    expect(screen.getByText("0 / 0")).toBeTruthy();
    expect(screen.getByText("Nothing stored yet.")).toBeTruthy();
  });

  it("renders the server's own reason when the pond has no index, and no percentage", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue({
      indexed: false,
      reason: "no embedding model is configured, so nothing has been indexed and retrieval falls back to recency",
      model_id: null,
      dims: null,
      coverage: null,
      corpora: [],
    } satisfies ContextIndexHealth);

    renderPanel();

    expect(await screen.findByText("Nothing is indexed")).toBeTruthy();
    expect(screen.getByText(/no embedding model is configured/)).toBeTruthy();
    // A 0% here would read as breakage rather than as a switched-off feature.
    expect(screen.queryByText("0%")).toBeNull();
  });

  it("clears the index on confirmation and says the refill happens in the background", async () => {
    mocked(api.rebuildContextIndex).mockResolvedValue({
      indexed: true,
      cleared: 3,
      corpora: [
        { corpus: "memory", cleared: 3 },
        { corpus: "context", cleared: 0 },
        { corpus: "summary", cleared: 0 },
      ],
    });

    renderPanel();
    await screen.findByText("2%");

    fireEvent.click(screen.getByRole("button", { name: /Reindex/ }));

    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Reindex" }));

    await waitFor(() => expect(mocked(api.rebuildContextIndex)).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(mocked(api.getContextIndexHealth)).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/Cleared 3 vectors/)).toBeTruthy();
    expect(screen.getByText(/in the background/)).toBeTruthy();
  });

  it("does not touch the index when the confirmation is declined", async () => {
    renderPanel();
    await screen.findByText("2%");

    fireEvent.click(screen.getByRole("button", { name: /Reindex/ }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(mocked(api.rebuildContextIndex)).not.toHaveBeenCalled();
  });
});

/** The thin index with the embedder's own report beside it. */
function switching(embedder: Partial<EmbedderHealth>, over: Partial<ContextIndexHealth> = {}): ContextIndexHealth {
  return {
    ...thinlyIndexed(),
    model_id: "embeddinggemma-2-256",
    dims: 256,
    embedder: { state: "ready", model_id: "embeddinggemma-2-256", reason: null, ...embedder },
    ...over,
  };
}

describe("IndexCoveragePanel: the embedder", () => {
  it("says it searches by keyword while the model loads", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue(switching({ state: "loading" }, { matching: 0 }));
    renderPanel();
    const status = await screen.findByRole("status");
    expect(status.textContent).toBe("Searching by keyword while EmbeddingGemma 2 loads");
  });

  it("counts the re-index from the numbers the panel already shows", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue(
      switching({}, { rows: 95, matching: 40, missing: 0, mismatched: 55, coverage: 40 / 95 }),
    );
    renderPanel();
    expect((await screen.findByRole("status")).textContent).toBe("Reading memories again: 40 of 95 items");
    expect(screen.getByText("40 of 95 rows searchable")).toBeTruthy();
  });

  it("keeps quiet once every row is reachable: the figure below already says so", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue(
      switching({}, { rows: 95, matching: 95, missing: 0, coverage: 1 }),
    );
    renderPanel();
    await screen.findByText("95 of 95 rows searchable");
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says a failed load in plain words, keeps the pond's own reason behind Details, and asks again without clearing anything", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue(
      switching({ state: "failed", reason: "loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory" }, { matching: 0 }),
    );
    mocked(api.getActiveRoles).mockResolvedValue(memoryRoles("embeddinggemma-2"));
    mocked(api.activateModel).mockResolvedValue(undefined as never);
    renderPanel();

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword.");
    expect(within(alert).queryByText(/on the Gpu/)).toBeNull();
    fireEvent.click(within(alert).getByRole("button", { name: "Details" }));
    expect(within(alert).getByText(/on the Gpu: out of memory/)).toBeTruthy();

    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    await waitFor(() =>
      expect(mocked(api.activateModel)).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"),
    );
    expect(mocked(api.rebuildContextIndex)).not.toHaveBeenCalled();
  });

  it("reads a file still coming down as progress, not a failure", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue(
      switching(
        { state: "failed", reason: "embeddinggemma-2-Q8_0.gguf is not on disk; the models pipeline is downloading it" },
        { matching: 0 },
      ),
    );
    mocked(api.getDownloadProgress).mockResolvedValue({
      downloads: [{ filename: "embeddinggemma-2-Q8_0.gguf", category: "embedding", downloaded_bytes: 1, total_bytes: 3, status: "downloading" }],
    });
    renderPanel();
    expect((await screen.findByRole("status")).textContent).toBe(
      "Searching by keyword until EmbeddingGemma 2 finishes downloading",
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says a model whose file is neither here nor coming down is not here yet, and fetches it on request", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue(
      switching(
        { state: "failed", reason: "embeddinggemma-2-Q8_0.gguf is not on disk; the models pipeline is downloading it" },
        { matching: 0 },
      ),
    );
    mocked(api.getDownloadProgress).mockResolvedValue({ downloads: [] });
    mocked(api.getActiveRoles).mockResolvedValue(memoryRoles("embeddinggemma-2"));
    mocked(api.activateModel).mockResolvedValue(undefined as never);
    renderPanel();
    const status = await screen.findByRole("status");
    expect(status.textContent).toContain("EmbeddingGemma 2 is not on this device yet, so Memory is searching by keyword.");
    fireEvent.click(within(status).getByRole("button", { name: "Download" }));
    await waitFor(() =>
      expect(mocked(api.activateModel)).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"),
    );
  });

  it("says search by meaning is off in the pond's household words, and no figure", async () => {
    mocked(api.getContextIndexHealth).mockResolvedValue({
      indexed: false,
      reason: "no embedding model is configured, so nothing has been indexed and retrieval falls back to recency",
      model_id: null,
      dims: null,
      coverage: null,
      corpora: [],
      embedder: { state: "off", model_id: null, reason: null },
    });
    renderPanel();
    expect((await screen.findByRole("status")).textContent).toBe(
      "Search by meaning is off. Memory searches by keyword.",
    );
    // The line is the whole answer: the server's sentence about a configured model would repeat it in jargon.
    expect(screen.queryByText("Nothing is indexed")).toBeNull();
    expect(screen.queryByText("0%")).toBeNull();
  });
});
