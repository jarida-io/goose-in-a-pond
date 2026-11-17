import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ApiError } from "../api/types";
import { anchor } from "./myGooseFixtures";

const api = vi.hoisted(() => ({
  getAnchor: vi.fn(),
  stopAnchor: vi.fn(),
  answerQuestion: vi.fn(),
  skipQuestion: vi.fn(),
  confirmAnchor: vi.fn(),
  dismissAnchor: vi.fn(),
  listProfiles: vi.fn(),
  shareAnchor: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { AnchorPage } from "./AnchorPage";

const RUNNING = anchor({
  id: "a-summit",
  kind: "event",
  title: "AI Summit Nairobi",
  facts: [{ label: "Venue", value: "KICC", source: "the event page" }],
  deliverables: [
    { id: "d-1", kind: "dossier", slot: "dossier", title: "Dossier for AI Summit Nairobi", state: "ready", hook: "Who to meet", version: 2, changed: "", unseen: true },
  ],
  questions: [{ id: "q-1", text: "Will you drive or take a matatu?", options: ["Drive", "Matatu"], multi: false }],
  open_questions: 1,
  job: {
    kind: "deliverable",
    subject: "Dossier for AI Summit Nairobi",
    state: "running",
    rounds: 1,
    steps: [
      { key: "plan", role: "Point Gander", label: "Planning the work", state: "completed", summary: null },
      { key: "research-1", role: "Forager Gosling", label: "Researching", state: "running", summary: null },
      { key: "compose", role: "Nestmaker Goose", label: "Writing it up", state: "pending", summary: null },
    ],
  },
});

function renderPage() {
  const said: string[] = [];
  const back = vi.fn();
  const opened: string[] = [];
  render(<AnchorPage id="a-summit" variant="classic" say={(t) => said.push(t)} onBack={back} onOpenDeliverable={(d) => opened.push(d.id)} />);
  return { said, back, opened };
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
beforeEach(() => {
  vi.clearAllMocks();
  api.getAnchor.mockResolvedValue(RUNNING);
});

describe("the anchor page", () => {
  it("draws the running job as a pipeline of plain steps", async () => {
    renderPage();
    const pipeline = await screen.findByRole("list", { name: "Working on it" });
    const steps = within(pipeline).getAllByRole("listitem");

    expect(steps.map((s) => s.getAttribute("data-state"))).toEqual(["done", "current", "todo"]);
    expect(within(steps[1]).getByText("Researching")).toBeTruthy();
    expect(within(steps[1]).getByText("Working on it")).toBeTruthy();
    expect(screen.getByText("Dossier for AI Summit Nairobi. Checked once so far.")).toBeTruthy();
  });

  it("asks after the job again while it runs", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderPage();
    await screen.findByRole("list", { name: "Working on it" });
    expect(api.getAnchor).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(5_000);
    await waitFor(() => expect(api.getAnchor).toHaveBeenCalledTimes(2));
  });

  it("opens a deliverable that is ready, and shows what is new", async () => {
    const { opened } = renderPage();
    const row = await screen.findByRole("button", { name: /Dossier for AI Summit Nairobi/ });
    expect(within(row).getByText("New")).toBeTruthy();
    fireEvent.click(row);
    expect(opened).toEqual(["d-1"]);
  });

  it("asks its open questions inline", async () => {
    api.answerQuestion.mockResolvedValue(undefined);
    renderPage();
    fireEvent.click(await screen.findByRole("radio", { name: "Matatu" }));
    fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await waitFor(() => expect(api.answerQuestion).toHaveBeenCalledWith("q-1", { choices: ["Matatu"] }));
  });
});

describe("the stop sheet", () => {
  it("sends the chosen reasons, the member's words and never-again, then leaves the page", async () => {
    api.stopAnchor.mockResolvedValue(undefined);
    const { said, back } = renderPage();

    fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
    const sheet = await screen.findByRole("dialog", { name: "Stop AI Summit Nairobi" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Too generic" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Bad timing" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Too generic" }));
    fireEvent.change(within(sheet).getByRole("textbox", { name: "What was wrong?" }), { target: { value: " I'm not going after all " } });
    fireEvent.click(within(sheet).getByRole("switch", { name: "Never suggest this again" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Stop it" }));

    await waitFor(() =>
      expect(api.stopAnchor).toHaveBeenCalledWith("a-summit", { reasons: ["bad_timing"], text: "I'm not going after all", never: true }),
    );
    expect(said).toEqual(["Stopped: Goose makes nothing more for AI Summit Nairobi."]);
    expect(back).toHaveBeenCalled();
  });

  it("takes a reason chip's tap anywhere in its 44px band", async () => {
    api.stopAnchor.mockResolvedValue(undefined);
    renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
    const sheet = await screen.findByRole("dialog", { name: "Stop AI Summit Nairobi" });
    const chip = within(sheet).getByRole("button", { name: "Wrong facts" });
    fireEvent.click(chip.closest(".mg-reasons__reach") as Element);
    expect(chip.getAttribute("aria-pressed")).toBe("true");
  });

  it("stays open with the pond's reason when the stop is refused", async () => {
    api.stopAnchor.mockRejectedValue(new ApiError(403, "this is shared with you: only the member who owns it can change it"));
    const { back } = renderPage();
    fireEvent.click(await screen.findByRole("button", { name: "Stop" }));
    const sheet = await screen.findByRole("dialog", { name: "Stop AI Summit Nairobi" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Stop it" }));

    expect((await within(sheet).findByRole("alert")).textContent).toBe(
      "This is shared with you: only the member who owns it can change it.",
    );
    expect(back).not.toHaveBeenCalled();
  });
});

describe("an anchor shared with the caller", () => {
  it("is read-only, says who shared it, and lets them answer but not skip", async () => {
    api.getAnchor.mockResolvedValue({ ...RUNNING, mine: false, shared_by: "Amina" });
    renderPage();

    expect(await screen.findByText("Shared with you by Amina. You can read it and answer its questions.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Share" })).toBeNull();
    expect(screen.getByRole("button", { name: "Send answer" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
  });
});
