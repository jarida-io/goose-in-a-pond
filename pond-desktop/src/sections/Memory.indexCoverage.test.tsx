import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, fireEvent, within } from "@testing-library/react";
import {
  IndexCoveragePanel,
  coverageState,
  coverageNote,
  formatCoverage,
} from "./Memory";
import { ConfirmProvider } from "../components/shared";
import { api } from "../api/PondApiClient";
import type { ContextCorpusCoverage, ContextIndexHealth } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getContextIndexHealth: vi.fn(),
    rebuildContextIndex: vi.fn(),
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
