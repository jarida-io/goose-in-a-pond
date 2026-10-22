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
    markSuggestionTaken: vi.fn(),
  },
}));

/** A proposal as the wire sends it: `proposed_action` is a `TaskKind` object, never a string. */
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

function openCard(): string | null {
  return document.querySelector(".sq__summary")?.textContent ?? null;
}

/** The panel row the cursor is on. */
function markedRow(): string | null {
  return document.querySelector(".sq__row[data-active] .sq__row-title")?.textContent ?? null;
}

/** Scoped to the panel: the peek shows the same names. */
function panelRow(summary: string): Element {
  const row = Array.from(document.querySelectorAll(".sq__panel .sq__row")).find(
    (r) => r.querySelector(".sq__row-title")?.textContent === summary,
  );
  if (!row) throw new Error(`no panel row for ${summary}`);
  return row;
}

function approveFromPanel(summary: string): void {
  const approve = Array.from(panelRow(summary).querySelectorAll("button")).find(
    (b) => b.textContent === "Approve",
  );
  if (!approve) throw new Error(`no Approve on the ${summary} row`);
  fireEvent.click(approve);
}

function openPanel(): void {
  fireEvent.click(screen.getByText(/more suggestion/));
}

async function readingBbbWithPanelOpen(): Promise<void> {
  render(<SuggestionQueue sessionId="s-1" />);
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
  // Default: nothing on offer, so the cursor suite sees proposals only.
  vi.mocked(api.listSuggestions).mockResolvedValue({
    suggestions: [],
    considered: [],
    audience: "personal",
  });
});

describe("SuggestionQueue cursor", () => {
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

  it("advances to the successor when the open card itself is answered", async () => {
    render(<SuggestionQueue sessionId="s-1" />);
    await screen.findByText("AAA");
    expect(openCard()).toBe("AAA");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await act(async () => {});

    expect(openCard()).toBe("BBB");
  });

  it("falls back to the last suggestion when the open card was the last one", async () => {
    render(<SuggestionQueue sessionId="s-1" />);
    await screen.findByText("AAA");
    // DDD is past the two-row peek, so the panel is the only way onto it.
    openPanel();
    fireEvent.click(panelRow("DDD").querySelector(".sq__row-title") as Element);
    expect(openCard()).toBe("DDD");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await act(async () => {});

    expect(openCard()).toBe("CCC");
  });

  // The error is drawn on the open card, so it must be the failed one.
  it("returns the cursor to the suggestion whose answer failed to send", async () => {
    vi.mocked(api.decideProposal).mockRejectedValue(new Error("offline"));
    render(<SuggestionQueue sessionId="s-1" />);
    await screen.findByText("AAA");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());

    expect(openCard()).toBe("AAA");
  });

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
    render(<SuggestionQueue sessionId="s-1" />);
    await screen.findByText("AAA");

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await act(async () => {});

    expect(openCard()).toBeNull();
  });
});


// ── Suggestions: the column when nothing is waiting on anybody ──────────────

/** A suggestion as the wire sends one. */
function suggestion(id: string, prompt: string, because: string): Suggestion {
  return { id, prompt, because, answered_by: "giap-memory", composed: false };
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

  it("offers suggestions when nothing is waiting", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    await screen.findByText("What do you remember about me?");
    expect(offerRows()).toEqual([
      "What do you remember about me?",
      "Which of my devices are online?",
    ]);
    expect(document.querySelector(".sq__quiet")).toBeNull();
  });

  // Without this the queue never drains.
  it("tells the pond when a composed suggestion is taken", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.markSuggestionTaken).mockResolvedValue({ id: "q-1", settled: true });
    vi.mocked(api.listSuggestions).mockResolvedValue({
      suggestions: [
        { ...suggestion("q-1", "How did the swim go?", "From something you do regularly, saved 3 days ago."), composed: true },
      ],
      considered: [],
      audience: "personal",
    });
    const asked: string[] = [];
    render(<SuggestionQueue sessionId="s-1" onAsk={(p) => asked.push(p)} />);

    fireEvent.click(await screen.findByText("How did the swim go?"));
    // Sent first and unawaited.
    expect(asked).toEqual(["How did the swim go?"]);
    await waitFor(() => expect(api.markSuggestionTaken).toHaveBeenCalledWith("q-1"));
  });

  // A template suggestion's id is a suggestor name, not a row.
  it("does not try to settle a template suggestion", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.listSuggestions).mockResolvedValue({
      suggestions: [suggestion("memory_recall", "What do you remember about me?", "38 things remembered.")],
      considered: [],
      audience: "personal",
    });
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    fireEvent.click(await screen.findByText("What do you remember about me?"));
    await waitFor(() => expect(api.listSuggestions).toHaveBeenCalled());
    expect(api.markSuggestionTaken).not.toHaveBeenCalled();
  });

  // No destination: the classic composer hides its chips once the transcript has a message.
  it("counts what it could not show without promising where to find it", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.listSuggestions).mockResolvedValue({
      suggestions: [
        suggestion("a", "A?", "because a"),
        suggestion("b", "B?", "because b"),
        suggestion("c", "C?", "because c"),
        suggestion("d", "D?", "because d"),
      ],
      considered: [],
      audience: "personal",
    });
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    await screen.findByText("A?");
    const line = document.querySelector(".sq__unshown");
    expect(line?.textContent).toBe("1 more the pond can answer");
    expect(line?.textContent).not.toMatch(/chat|composer|below|here/i);
  });

  it("shows the measured reason under each offer", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    await screen.findByText("379 things remembered.");
    expect(screen.getByText("19 devices registered here.")).toBeTruthy();
  });

  it("keeps proposals in the column when both exist", async () => {
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    await screen.findByText("AAA");
    expect(openCard()).toBe("AAA");
    expect(offerRows()).toEqual([]);
  });

  it("asks the exact sentence that was shown", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    const asked: string[] = [];
    render(
      <SuggestionQueue sessionId="s-1" onAsk={(p) => asked.push(p)} />,
    );

    fireEvent.click(await screen.findByText("What do you remember about me?"));
    expect(asked).toEqual(["What do you remember about me?"]);
  });

  it("draws no offers when the surface cannot send one", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId="s-1" />);

    await waitFor(() => expect(vi.mocked(api.listSuggestions)).toHaveBeenCalled());
    expect(offerRows()).toEqual([]);
  });

  it("shows no offers when there is nothing to suggest", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.listSuggestions).mockResolvedValue({
      suggestions: [],
      considered: [],
      audience: "shared",
    });
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    await waitFor(() => expect(vi.mocked(api.listSuggestions)).toHaveBeenCalled());
    expect(offerRows()).toEqual([]);
  });

  // The cold launch: the Dashboard mounts with `sessionId` null.
  it("fetches with no session at all", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    render(<SuggestionQueue sessionId={null} onAsk={() => {}} />);

    await screen.findByText("What do you remember about me?");
    expect(vi.mocked(api.listSuggestions)).toHaveBeenCalledWith(null);
  });

  it("goes quiet rather than loud when the fetch fails", async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ profile_id: null, proposals: [] });
    vi.mocked(api.listSuggestions).mockRejectedValue(new Error("offline"));
    render(<SuggestionQueue sessionId="s-1" onAsk={() => {}} />);

    await waitFor(() => expect(vi.mocked(api.listSuggestions)).toHaveBeenCalled());
    expect(document.querySelector(".sq__error")).toBeNull();
  });
});
