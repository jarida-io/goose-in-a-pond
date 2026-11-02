import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { api } from "../../api/PondApiClient";
import { Lineage } from "./Lineage";
import type { ContextIndexHealth, EmbedderHealth, MemoryFragment } from "../../api/types";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function memory(id: string, over: Partial<MemoryFragment> = {}): MemoryFragment {
  return {
    id,
    content: `memory ${id}`,
    created_at: "2026-08-01T00:00:00Z",
    ...over,
  } as MemoryFragment;
}

describe("Lineage", () => {
  // Not one hop: otherwise a three-generation merge reads as three one-step merges.
  it("follows a chain to the row nothing replaced", () => {
    const memories = [
      memory("a", { superseded_by: "b", lifecycle: "merged" }),
      memory("b", { superseded_by: "c", lifecycle: "merged" }),
      memory("c", { lifecycle: "active", content: "the surviving fact" }),
    ];
    render(<Lineage memories={memories} health={null} />);

    expect(screen.getByText("the surviving fact")).toBeTruthy();
    // "2" appears in both the stat and the wedge, so each is asserted where it lives.
    expect(screen.getByText(/2 earlier versions folded into this/)).toBeTruthy();
    expect(document.querySelector(".lin__fanCount")?.textContent).toBe("2");
    expect(screen.getByText("Deepest chain").parentElement?.textContent).toContain("2");
  });

  it("groups every ancestor under the one survivor they share", () => {
    const memories = [
      memory("a", { superseded_by: "survivor", lifecycle: "merged" }),
      memory("b", { superseded_by: "survivor", lifecycle: "merged" }),
      memory("c", { superseded_by: "survivor", lifecycle: "merged" }),
      memory("survivor", { lifecycle: "active", content: "kept" }),
    ];
    render(<Lineage memories={memories} health={null} />);
    expect(screen.getByText(/3 earlier versions folded into this/)).toBeTruthy();
  });

  // Counted, not dropped, so the totals agree with the memory count on the tab above.
  it("reports chains that point at a memory the pond no longer has", () => {
    const memories = [memory("orphan", { superseded_by: "long-gone", lifecycle: "merged" })];
    render(<Lineage memories={memories} health={null} />);
    expect(screen.getByText(/point at a memory this pond no longer has/)).toBeTruthy();
  });

  it("does not loop forever on a cycle", () => {
    const memories = [
      memory("x", { superseded_by: "y", lifecycle: "merged" }),
      memory("y", { superseded_by: "x", lifecycle: "merged" }),
    ];
    expect(() => render(<Lineage memories={memories} health={null} />)).not.toThrow();
  });

  it("says so plainly when nothing has been consolidated", () => {
    render(<Lineage memories={[memory("only", { lifecycle: "active" })]} health={null} />);
    expect(
      screen.getAllByText(/Nothing has been folded together yet/).length,
    ).toBeGreaterThan(0);
  });

  // Both read as zero but need opposite fixes.
  it("separates an empty corpus from one that is entirely held back", () => {
    render(
      <Lineage
        memories={[]}
        health={{
          indexed: true,
          model_id: "nomic-embed-text-v1.5",
          dims: 768,
          coverage: 0.5,
          corpora: [
            {
              corpus: "memory",
              rows: 4,
              source_rows: 4,
              structurally_excluded: false,
              indexed_rows: 2,
              missing_rows: 2,
              mismatched: 0,
              coverage: 0.5,
            },
            {
              corpus: "summary",
              rows: 0,
              source_rows: 27,
              structurally_excluded: true,
              indexed_rows: 0,
              missing_rows: 0,
              mismatched: 0,
              coverage: null,
            },
            {
              corpus: "context",
              rows: 0,
              source_rows: 0,
              structurally_excluded: false,
              indexed_rows: 0,
              missing_rows: 0,
              mismatched: 0,
              coverage: null,
            },
          ],
        } as never}
      />,
    );

    expect(screen.getByText("2 of 4")).toBeTruthy();
    // Held back names the count being excluded, which is the actionable number.
    expect(screen.getByText("all 27 held back")).toBeTruthy();
    expect(screen.getByText(/Everything in this corpus is being held back/)).toBeTruthy();
    expect(screen.getByText("nothing here yet")).toBeTruthy();
  });

  it("names an older embedder as the reason rows are unreachable", () => {
    render(
      <Lineage
        memories={[]}
        health={{
          indexed: true,
          model_id: "nomic-embed-text-v1.5",
          dims: 768,
          coverage: 0.2,
          corpora: [
            {
              corpus: "memory",
              rows: 10,
              source_rows: 10,
              structurally_excluded: false,
              indexed_rows: 2,
              missing_rows: 3,
              mismatched: 5,
              coverage: 0.2,
            },
          ],
        } as never}
      />,
    );
    expect(screen.getByText(/5 were read by an older model/)).toBeTruthy();
  });
});

/** The index with the embedder's own report beside it. */
function indexWith(embedder: Partial<EmbedderHealth> | null, over: Partial<ContextIndexHealth> = {}): ContextIndexHealth {
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
    ...(embedder ? { embedder: { state: "ready", model_id: "embeddinggemma-2-256", reason: null, ...embedder } } : {}),
    ...over,
  };
}

describe("Lineage: the embedder", () => {
  it("says it searches by keyword while the model loads", () => {
    render(<Lineage memories={[]} health={indexWith({ state: "loading" }, { matching: 0 })} />);
    expect(screen.getByRole("status").textContent).toBe("Searching by keyword while EmbeddingGemma 2 loads");
  });

  it("counts reading memories again, in items, because the total spans every store", () => {
    render(<Lineage memories={[]} health={indexWith({}, { matching: 40 })} />);
    expect(screen.getByRole("status").textContent).toBe("Reading memories again: 40 of 95 items");
    expect(screen.getByRole("progressbar", { name: "Items read again" }).getAttribute("aria-valuenow")).toBe("42");
  });

  it("gives the action one name before and while it runs", async () => {
    vi.spyOn(api, "rebuildContextIndex").mockImplementation(() => new Promise(() => {}));
    render(<Lineage memories={[]} health={indexWith({})} />);
    const button = screen.getByRole("button", { name: "Read memories again" });
    fireEvent.click(button);
    expect(await screen.findByRole("button", { name: "Reading memories again" })).toBeTruthy();
    expect(screen.queryByText(/Clearing|Read everything again|Re-indexing/)).toBeNull();
  });

  it("names the model as the household reads it, not its stamp and dimensions", () => {
    render(<Lineage memories={[]} health={indexWith({})} />);
    expect(screen.getByText(/^Read by/).textContent).toBe(
      "Read by EmbeddingGemma 2. Anything not indexed is still found by word, just less well.",
    );
    expect(screen.queryByText(/dimensions|embeddinggemma-2-256/)).toBeNull();
  });

  it("does not say a model that failed or is loading has read anything", () => {
    render(<Lineage memories={[]} health={indexWith({ state: "failed", reason: "bad magic number" }, { matching: 0 })} />);
    expect(screen.queryByText(/^Read by/)).toBeNull();
    expect(screen.getByText(/^Chosen:/).textContent).toBe(
      "Chosen: EmbeddingGemma 2. Anything not indexed is still found by word, just less well.",
    );
  });

  it("keeps quiet once every row is reachable", () => {
    render(<Lineage memories={[]} health={indexWith({})} />);
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says a failed load in plain words and offers another go", () => {
    const onRetry = vi.fn();
    render(
      <Lineage
        memories={[]}
        health={indexWith({ state: "failed", reason: "out of memory" }, { matching: 0 })}
        onRetry={onRetry}
      />,
    );
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toContain("There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword.");
    expect(alert.textContent).toContain("all-MiniLM-L6-v2 (23 MB)");
    // The pond's own words wait behind Details.
    expect(screen.queryByText("out of memory")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByText("out of memory")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("says any other failed load in plain words too", () => {
    render(<Lineage memories={[]} health={indexWith({ state: "failed", reason: "bad magic number" }, { matching: 0 })} />);
    expect(screen.getByRole("alert").textContent).toContain(
      "EmbeddingGemma 2 did not load, so Memory is searching by keyword. Try again, or choose another Memory model.",
    );
  });

  it("says search by meaning is off, instead of the server's sentence about a configured model", () => {
    render(
      <Lineage
        memories={[]}
        health={indexWith({ state: "off", model_id: null }, { indexed: false, reason: "no embedding model is configured" })}
      />,
    );
    expect(screen.getByRole("status").textContent).toBe("Search by meaning is off. Memory searches by keyword.");
    expect(screen.queryByText(/no embedding model is configured/)).toBeNull();
  });

  it("reads a pond that predates the embedder report as before", () => {
    render(<Lineage memories={[]} health={indexWith(null)} />);
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByText(/Read by/)).toBeTruthy();
  });
});
