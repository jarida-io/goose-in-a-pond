import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SuggestionQueue } from "./SuggestionQueue";
import { api } from "../../api/PondApiClient";
import type { Proposal, Suggestion } from "../../api/types";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    listProposals: vi.fn(),
    decideProposal: vi.fn(),
    listSuggestions: vi.fn(),
  },
}));

/**
 * A proposal as the wire really sends one.
 *
 * `proposed_action` used to be `""` here, a value the server cannot produce:
 * `TaskKind` is a tagged union and arrives as an object. The fixture matched
 * the TS type rather than the wire, so every green test in this file was
 * compatible with a shape that never occurs -- and nothing could have caught a
 * regression that bound the field and shipped `[object Object]`. The type is
 * now a union and this is the real shape.
 */
function proposal(summary: string): Proposal {
  return {
    id: `p-${summary}`,
    summary,
    rationale: `why ${summary}`,
    confidence: 0.8,
    profile_id: null,
    created_at: "2026-09-15T08:00:00Z",
    expires_at: "2026-09-15T20:00:00Z",
    proposed_action: { type: "agent_prompt", prompt: summary },
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
  // Default: nothing on offer, so the cursor suite below sees exactly the
  // column it always did.
  vi.mocked(api.listSuggestions).mockResolvedValue({
    suggestions: [],
    considered: [],
    audience: "personal",
  });
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


// ── Suggestions: the column when nothing is waiting on anybody ──────────────

/** A suggestion as the wire sends one. */
function suggestion(id: string, prompt: string, because: string): Suggestion {
  return { id, prompt, because, answered_by: "giap-memory" };
}

const OFFERS = [
  suggestion("memory_recall", "What do you remember about me?", "379 things remembered."),
  suggestion("devices_online", "Which of my devices are online?", "19 devices registered here."),
];

function offerRows(): string[] {
  return Array.from(document.querySelectorAll(".sq__offer-prompt")).map(
    (n) => n.textContent ?? "",
  );
}

describe("SuggestionQueue offers", () => {
  beforeEach(() => {
    vi.mocked(api.listSuggestions).mockResolvedValue({
      suggestions: OFFERS,
      considered: [],
      audience: "personal",
    });
  });

  /// The whole point of the second half: a column that used to be one sentence
  /// about the weather now offers things the pond can actually answer.
  it("offers suggestions when nothing is waiting", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." onAsk={() => {}} />);

    await screen.findByText("What do you remember about me?");
    expect(offerRows()).toEqual([
      "What do you remember about me?",
      "Which of my devices are online?",
    ]);
    expect(document.querySelector(".sq__quiet")).toBeNull();
  });

  /// Every offer shows the fact that produced it. An offer with no reason is
  /// the template this engine exists not to be.
  it("shows the measured reason under each offer", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." onAsk={() => {}} />);

    await screen.findByText("379 things remembered.");
    expect(screen.getByText("19 devices registered here.")).toBeTruthy();
  });

  /// Proposals win. Only they are waiting on somebody, and the design gives the
  /// column one decision at a time.
  it("keeps proposals in the column when both exist", async () => {
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." onAsk={() => {}} />);

    await screen.findByText("AAA");
    expect(openCard()).toBe("AAA");
    expect(offerRows()).toEqual([]);
  });

  /// Tapping sends the prompt VERBATIM. Re-composing it at the client would let
  /// the household read one sentence and send another.
  it("asks the exact sentence that was shown", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    const asked: string[] = [];
    render(
      <SuggestionQueue sessionId="s-1" quietLine="All quiet." onAsk={(p) => asked.push(p)} />,
    );

    fireEvent.click(await screen.findByText("What do you remember about me?"));
    expect(asked).toEqual(["What do you remember about me?"]);
  });

  /// No `onAsk` means no buttons. A surface that cannot send the prompt must
  /// not draw a control that would do nothing -- the same rule that kept the
  /// per-suggestion verb off the proposal card.
  it("draws no offers when the surface cannot send one", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId="s-1" quietLine="All quiet." />);

    await screen.findByText("All quiet.");
    expect(offerRows()).toEqual([]);
  });

  /// The quiet line survives for the pond that genuinely has nothing to offer.
  it("falls back to the quiet line when there is nothing to offer", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.listSuggestions).mockResolvedValue({
      suggestions: [],
      considered: [],
      audience: "shared",
    });
    render(<SuggestionQueue sessionId="s-1" quietLine="Good morning, Jerry." onAsk={() => {}} />);

    await screen.findByText("Good morning, Jerry.");
    expect(offerRows()).toEqual([]);
  });

  /// The fetch does not wait for a session. This is the cold-launch case: the
  /// Dashboard mounts with `sessionId` null and the column must still fill.
  it("fetches with no session at all", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId={null} quietLine="All quiet." onAsk={() => {}} />);

    await screen.findByText("What do you remember about me?");
    expect(vi.mocked(api.listSuggestions)).toHaveBeenCalledWith(null);
  });

  /// A suggestions outage is not a banner, for the same reason a proposals
  /// outage is not: this column's whole job is quiet.
  it("goes quiet rather than loud when the fetch fails", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.listSuggestions).mockRejectedValue(new Error("offline"));
    render(<SuggestionQueue sessionId="s-1" quietLine="Good morning, Jerry." onAsk={() => {}} />);

    await screen.findByText("Good morning, Jerry.");
    expect(document.querySelector(".sq__error")).toBeNull();
  });
});
