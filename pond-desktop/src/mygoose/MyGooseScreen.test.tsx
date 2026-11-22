import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ApiError } from "../api/types";
import { ConfirmProvider } from "../components/shared";
import { anchor, deliverable } from "./myGooseFixtures";

const api = vi.hoisted(() => ({
  listAnchors: vi.fn(),
  getAnchor: vi.fn(),
  getDeliverable: vi.fn(),
  markDeliverableSeen: vi.fn(),
  listQuestionBatches: vi.fn(),
  listFacts: vi.fn(),
  listDontRules: vi.fn(),
  listSpecialists: vi.fn(),
  switchSpecialist: vi.fn(),
  listIntentions: vi.fn(),
  withdrawIntention: vi.fn(),
  listEngineJobs: vi.fn(),
  listIdeaCards: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { MyGooseScreen } from "./MyGooseScreen";

const WHO = new ApiError(409, "who_are_you: this phone is not linked to a member yet", undefined, {
  error: "who_are_you: this phone is not linked to a member yet",
});

const TRIP = anchor({
  id: "a-diani",
  deliverables: [{ id: "d-1", kind: "plan", slot: "plan", title: "Plan for Diani trip", state: "ready", hook: "", version: 1, changed: "", unseen: false }],
});

function renderScreen(variant: "classic" | "hub" = "classic") {
  return render(
    <ConfirmProvider>
      <MyGooseScreen variant={variant} />
    </ConfirmProvider>,
  );
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.listAnchors.mockResolvedValue([TRIP]);
  api.listIdeaCards.mockResolvedValue({ cards: [], set: { state: "none", made_at: null } });
  api.getAnchor.mockResolvedValue({ ...TRIP, questions: [] });
  api.getDeliverable.mockResolvedValue(deliverable());
  api.listQuestionBatches.mockResolvedValue([]);
  api.listFacts.mockResolvedValue({ facts: [], limit: 100 });
  api.listDontRules.mockResolvedValue([]);
  api.listSpecialists.mockResolvedValue([
    { id: "trip", title: "Trip", about: "Plans trips and events.", state: "on", why: "" },
    { id: "budget", title: "Budget", about: "Keeps a record of what you spend.", state: "offered", why: "You have sent three pictures of receipts or payments" },
  ]);
  api.listIntentions.mockResolvedValue([
    {
      id: "i-1",
      act: "push",
      text: "Remind you to pack the swimsuits",
      why: "The trip is close.",
      when: "On 2 December at 8am",
      until: "2026-12-03T00:00:00Z",
      state: "waiting",
      outcome: null,
      anchor_id: "a-diani",
      created_at: "2026-11-15T08:00:00Z",
      settled_at: null,
    },
  ]);
  api.listEngineJobs.mockResolvedValue([
    {
      id: "j-1",
      kind: "deliverable",
      subject: "Plan for Diani trip",
      state: "passed",
      rounds: 2,
      objections: [],
      outcome: "Ready after one revision.",
      anchor_id: "a-diani",
      deliverable_id: "d-1",
      created_at: "2026-11-12T08:00:00Z",
      started_at: "2026-11-12T08:00:00Z",
      finished_at: "2026-11-12T08:40:00Z",
    },
  ]);
});

describe("a pond that cannot tell who is asking", () => {
  it("shows one plain notice pointing to the phone, not an error per panel", async () => {
    api.listAnchors.mockRejectedValue(WHO);
    renderScreen();
    expect(await screen.findByText("Open My Goose on your phone")).toBeTruthy();
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("an older pond", () => {
  it("says to update it rather than reporting a missing route", async () => {
    api.listAnchors.mockRejectedValue(new ApiError(404, "Not Found"));
    renderScreen();
    expect(await screen.findByText("This pond has no My Goose yet")).toBeTruthy();
  });
});

describe("moving through My Goose", () => {
  it("goes from a plan to what Goose made for it, and back again", async () => {
    renderScreen();
    fireEvent.click(await screen.findByRole("button", { name: /Diani trip/ }));
    fireEvent.click(await screen.findByRole("button", { name: /Plan for Diani trip/ }));
    expect(await screen.findByRole("heading", { level: 1, name: "Plan for Diani trip" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Back to Diani trip" }));
    fireEvent.click(await screen.findByRole("button", { name: "All plans" }));
    expect(await screen.findByRole("heading", { level: 1, name: "My Goose" })).toBeTruthy();
  });

  it("shows the specialists with their state, and why one is offered", async () => {
    api.switchSpecialist.mockResolvedValue([
      { id: "trip", title: "Trip", about: "Plans trips and events.", state: "on", why: "" },
      { id: "budget", title: "Budget", about: "Keeps a record of what you spend.", state: "on", why: "" },
    ]);
    renderScreen();
    fireEvent.click(await screen.findByRole("tab", { name: "Specialists" }));
    expect(await screen.findByText("Goose suggests turning this on. You have sent three pictures of receipts or payments.")).toBeTruthy();
    fireEvent.click(screen.getByRole("switch", { name: "Budget" }));
    await waitFor(() => expect(api.switchSpecialist).toHaveBeenCalledWith("budget", true));
    expect(await screen.findByText("Budget turned on.")).toBeTruthy();
  });

  it("lists what waits to happen, withdrawably, and what Goose did, in plain words", async () => {
    api.withdrawIntention.mockResolvedValue(undefined);
    renderScreen();
    fireEvent.click(await screen.findByRole("tab", { name: "Activity" }));
    expect(await screen.findByText("Tell you: Remind you to pack the swimsuits")).toBeTruthy();
    expect(await screen.findByText("Ready after one revision.")).toBeTruthy();
    expect(screen.getByText(/Something made for you · Done, and it passed the check · Checked twice/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Withdraw" }));
    await waitFor(() => expect(api.withdrawIntention).toHaveBeenCalledWith("i-1"));
    expect(await screen.findByText("Nothing waiting.")).toBeTruthy();
  });
});
