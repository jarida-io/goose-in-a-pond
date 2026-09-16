import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SuggestionQueue } from "./SuggestionQueue";
import { api } from "../../api/PondApiClient";
import type { Proposal } from "../../api/types";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    listProposals: vi.fn(),
    decideProposal: vi.fn(),
  },
}));

/** A proposal as the wire sends one; only summary and id matter to the cursor. */
function proposal(summary: string): Proposal {
  return {
    id: `p-${summary}`,
    summary,
    rationale: `why ${summary}`,
    confidence: 0.8,
    profile_id: null,
    created_at: "2026-09-15T08:00:00Z",
    expires_at: "2026-09-15T20:00:00Z",
    proposed_action: "",
    trigger: { kind: "sensor", source_id: "s1", signal: "idle", observed_at: "2026-09-15T07:59:00Z" },
  };
}

const QUEUE = ["AAA", "BBB", "CCC", "DDD"].map(proposal);

/** The open card: the one suggestion the household is actually reading. */
function openCard(): string | null {
  return document.querySelector(".sq__summary")?.textContent ?? null;
}

/** The panel row the cursor is on, which must agree with the open card. */
function markedRow(): string | null {
  return document.querySelector(".sq__row[data-active] .sq__row-title")?.textContent ?? null;
}

/** A panel row by its summary. Scoped, because the peek shows the same names. */
function panelRow(summary: string): Element {
  const row = Array.from(document.querySelectorAll(".sq__panel .sq__row")).find(
    (r) => r.querySelector(".sq__row-title")?.textContent === summary,
  );
  if (!row) throw new Error(`no panel row for ${summary}`);
  return row;
}

/** Approve the named suggestion from the "Waiting on you" panel. */
function approveFromPanel(summary: string): void {
  const approve = Array.from(panelRow(summary).querySelectorAll("button")).find(
    (b) => b.textContent === "Approve",
  );
  if (!approve) throw new Error(`no Approve on the ${summary} row`);
  fireEvent.click(approve);
}

/** Open the full list, which is the only way to reach a suggestion past the peek. */
function openPanel(): void {
  fireEvent.click(screen.getByText(/more suggestion/));
}

/** Put the cursor on BBB, then open the full list over it. */
async function readingBbbWithPanelOpen(): Promise<void> {
  render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." />);
  await screen.findByText("AAA");
  fireEvent.click(screen.getByText("BBB"));
  expect(openCard()).toBe("BBB");
  openPanel();
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: QUEUE });
  vi.mocked(api.decideProposal).mockResolvedValue(undefined as never);
});

describe("SuggestionQueue cursor", () => {
  /// The cursor used to be a position. Answering anything above it from the
  /// panel shifted the queue up underneath a household that was mid-read, and
  /// the card they were reading was replaced by the one after it -- unanswered,
  /// and now demoted to a peek row with no sign anything had moved.
  it("keeps the household on the suggestion they were reading when a row above it is answered", async () => {
    await readingBbbWithPanelOpen();
    expect(markedRow()).toBe("BBB");

    approveFromPanel("AAA");
    await act(async () => {});

    expect(openCard()).toBe("BBB");
    expect(markedRow()).toBe("BBB");
    expect(screen.queryAllByText("AAA")).toHaveLength(0);
  });

  it("keeps the cursor when a row below it is answered", async () => {
    await readingBbbWithPanelOpen();

    approveFromPanel("DDD");
    await act(async () => {});

    expect(openCard()).toBe("BBB");
    expect(markedRow()).toBe("BBB");
  });

  /// Working down the panel top-to-bottom is the flow the panel was built for,
  /// so the cursor has to survive a run of them, not just one.
  it("survives a run of answers above the cursor", async () => {
    await readingBbbWithPanelOpen();

    approveFromPanel("AAA");
    await act(async () => {});
    approveFromPanel("CCC");
    await act(async () => {});
    approveFromPanel("DDD");
    await act(async () => {});

    expect(openCard()).toBe("BBB");
  });

  /// Answering the open card is the one case that should move the cursor: the
  /// next suggestion takes the answered one's place, as the column promises.
  it("advances to the successor when the open card itself is answered", async () => {
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." />);
    await screen.findByText("AAA");
    expect(openCard()).toBe("AAA");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await act(async () => {});

    expect(openCard()).toBe("BBB");
  });

  it("falls back to the last suggestion when the open card was the last one", async () => {
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." />);
    await screen.findByText("AAA");
    // DDD is past the two-row peek, so the panel is the only way onto it.
    openPanel();
    fireEvent.click(panelRow("DDD").querySelector(".sq__row-title") as Element);
    expect(openCard()).toBe("DDD");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await act(async () => {});

    expect(openCard()).toBe("CCC");
  });

  /// The failure message is drawn on the open card, so a rollback that leaves
  /// the cursor on the successor would blame the wrong suggestion.
  it("returns the cursor to the suggestion whose answer failed to send", async () => {
    vi.mocked(api.decideProposal).mockRejectedValue(new Error("offline"));
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." />);
    await screen.findByText("AAA");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());

    expect(openCard()).toBe("AAA");
  });

  /// A restored proposal must not drag the cursor off the card being read
  /// either -- the rollback splices back in above it.
  it("leaves the cursor alone when a rolled-back answer reappears above it", async () => {
    vi.mocked(api.decideProposal).mockRejectedValue(new Error("offline"));
    await readingBbbWithPanelOpen();

    approveFromPanel("AAA");
    await waitFor(() => expect(screen.getAllByText("AAA").length).toBeGreaterThan(0));

    expect(openCard()).toBe("BBB");
    expect(markedRow()).toBe("BBB");
  });

  it("goes quiet once the last suggestion is answered", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [proposal("AAA")] });
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." />);
    await screen.findByText("AAA");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await act(async () => {});

    expect(openCard()).toBeNull();
    expect(screen.getByText("All quiet.")).toBeTruthy();
  });
});
