import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ApiError, type Anchor } from "../api/types";
import { anchor } from "./myGooseFixtures";

const api = vi.hoisted(() => ({
  listAnchors: vi.fn(),
  confirmAnchor: vi.fn(),
  dismissAnchor: vi.fn(),
  addAnchor: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { PlansPanel } from "./PlansPanel";

const CANDIDATE: Anchor = anchor({
  id: "a-summit",
  kind: "event",
  title: "AI Summit Nairobi",
  state: "candidate",
  why: "You said you are going on 3 December.",
  starts_at: "2026-12-03T06:00:00Z",
});

const TRIP: Anchor = anchor({
  id: "a-diani",
  title: "Diani trip",
  place: "Diani",
  deliverables: [
    { id: "d1", kind: "plan", slot: "plan", title: "Plan for Diani trip", state: "ready", hook: "", version: 1, changed: "", unseen: true },
    { id: "d2", kind: "shortlist", slot: "stays", title: "Stays in Diani", state: "making", hook: "", version: 0, changed: "", unseen: false },
  ],
  open_questions: 2,
  job: { kind: "deliverable", subject: "Stays in Diani", state: "running", rounds: 1 },
});

function renderPanel(variant: "classic" | "hub" = "classic") {
  const said: string[] = [];
  const opened: string[] = [];
  const answer = vi.fn();
  render(
    <PlansPanel variant={variant} say={(t) => said.push(t)} onOpen={(a) => opened.push(a.id)} onAnswerQuestions={answer} />,
  );
  return { said, opened, answer };
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.listAnchors.mockResolvedValue([CANDIDATE, TRIP]);
});

describe("a candidate", () => {
  it("says why Goose thinks so, and Confirm tells the pond and says what happened", async () => {
    api.confirmAnchor.mockResolvedValue({ ...CANDIDATE, state: "active" });
    const { said } = renderPanel();

    expect(await screen.findByText("You said you are going on 3 December.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    await waitFor(() => expect(api.confirmAnchor).toHaveBeenCalledWith("a-summit"));
    expect(said).toEqual(["Confirmed: AI Summit Nairobi. Goose is starting on it."]);
    await waitFor(() => expect(api.listAnchors).toHaveBeenCalledTimes(2));
  });

  it("sets it aside with Not now", async () => {
    api.dismissAnchor.mockResolvedValue(undefined);
    const { said } = renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "Not now" }));

    await waitFor(() => expect(api.dismissAnchor).toHaveBeenCalledWith("a-summit"));
    expect(said).toEqual(["Not now: AI Summit Nairobi is set aside."]);
  });

  it("keeps the card and shows the pond's own reason when the answer is refused", async () => {
    api.confirmAnchor.mockRejectedValue(new ApiError(409, "this is over: add it again if you still want it"));
    const { said } = renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "Confirm" }));

    expect((await screen.findByRole("alert")).textContent).toBe("This is over: add it again if you still want it.");
    expect(screen.getByText("AI Summit Nairobi")).toBeTruthy();
    expect(said).toEqual([]);
  });

  it("asks about one candidate at a time on the kiosk", async () => {
    api.listAnchors.mockResolvedValue([CANDIDATE, { ...CANDIDATE, id: "a-2", title: "Car hunt" }, TRIP]);
    renderPanel("hub");

    expect(await screen.findByText("AI Summit Nairobi")).toBeTruthy();
    expect(screen.queryByText("Car hunt")).toBeNull();
    expect(screen.getByText("1 more after this one")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Confirm" })).toHaveLength(1);
  });
});

describe("an active anchor", () => {
  it("shows its deliverables' states, the running job and what is new, and opens on a press", async () => {
    const { opened } = renderPanel();
    const list = await screen.findByRole("list", { name: "What Goose is working on" });
    const card = within(list).getByRole("button");

    expect(within(card).getByText("Diani trip")).toBeTruthy();
    expect(within(card).getByText("Trip · Diani")).toBeTruthy();
    expect(within(card).getByText("Ready")).toBeTruthy();
    expect(within(card).getByText("Being made")).toBeTruthy();
    expect(within(card).getByText("Working on it now: Stays in Diani")).toBeTruthy();
    expect(within(card).getByText("1 new")).toBeTruthy();
    expect(within(card).getByText("2 questions")).toBeTruthy();

    fireEvent.click(card);
    expect(opened).toEqual(["a-diani"]);
  });

  it("points at the questions waiting on the member", async () => {
    const { answer } = renderPanel();
    expect(await screen.findByText("Goose has 2 questions about your plans.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Answer them" }));
    expect(answer).toHaveBeenCalled();
  });
});

describe("with nothing planned", () => {
  it("says what an anchor is and how to start one, and adds it from the form", async () => {
    api.listAnchors.mockResolvedValue([]);
    api.addAnchor.mockResolvedValue(anchor({ id: "a-new", title: "Lamu trip" }));
    const { said } = renderPanel();

    expect(await screen.findByText("Nothing planned yet")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add something to plan" }));
    fireEvent.click(screen.getByRole("button", { name: "Add it" }));
    expect(await screen.findByText("Say what it is in a few words.")).toBeTruthy();
    expect(api.addAnchor).not.toHaveBeenCalled();

    fireEvent.change(screen.getByRole("textbox", { name: "What is it?" }), { target: { value: "  Lamu trip " } });
    fireEvent.change(screen.getByRole("textbox", { name: "Where" }), { target: { value: "Lamu" } });
    fireEvent.click(screen.getByRole("button", { name: "Add it" }));

    await waitFor(() => expect(api.addAnchor).toHaveBeenCalledWith({ kind: "trip", title: "Lamu trip", place: "Lamu" }));
    expect(said).toEqual(["Added: Lamu trip. Goose is starting on it."]);
  });
});
