import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Lineage } from "./Lineage";
import type { MemoryFragment } from "../../api/types";

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
