import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ApiError, type DontRule } from "../api/types";
import { ConfirmProvider } from "../components/shared";
import { factLine } from "./myGooseFixtures";

const api = vi.hoisted(() => ({
  listFacts: vi.fn(),
  changeFact: vi.fn(),
  removeFact: vi.fn(),
  addFact: vi.fn(),
  rebuildFacts: vi.fn(),
  listDontRules: vi.fn(),
  addDontRule: vi.fn(),
  removeDontRule: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { AboutYouPanel } from "./AboutYouPanel";

const VEG = factLine({ id: "f-veg", fact: "Vegetarian", tools: ["giap-places__find_restaurants"] });
const HOME = factLine({ id: "f-home", category: "home", fact: "Lives in Kilimani", confidence: "sure", source: { kind: "you", ref: null }, pinned: true });

const RULE: DontRule = {
  id: "r-1",
  rule: "Avoid: Recipes built on a colour",
  strength: "avoid",
  reasons: ["not_relevant"],
  said: null,
  origin: "reject",
  origin_ref: "d-9",
  created_at: "2026-11-10T08:00:00Z",
};

function renderPanel() {
  const said: string[] = [];
  render(
    <ConfirmProvider>
      <AboutYouPanel variant="classic" say={(t) => said.push(t)} />
    </ConfirmProvider>,
  );
  return { said };
}

function lineOf(fact: string): HTMLElement {
  const row = screen.getByText(fact).closest(".mg-line");
  if (!row) throw new Error(`no line for ${fact}`);
  return row as HTMLElement;
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.listFacts.mockResolvedValue({ facts: [VEG, HOME], limit: 100 });
  api.listDontRules.mockResolvedValue([RULE]);
});

describe("the fact sheet", () => {
  it("groups lines by category with their confidence, source and tool hints, and counts them against the limit", async () => {
    renderPanel();
    expect(await screen.findByText("2 of 100 lines")).toBeTruthy();
    expect(within(screen.getByRole("list", { name: "Food" })).getByText("Vegetarian")).toBeTruthy();
    expect(within(screen.getByRole("list", { name: "Home" })).getByText("Lives in Kilimani")).toBeTruthy();
    expect(screen.getByText("Likely, from a memory. Helps with: find restaurants")).toBeTruthy();
    expect(screen.getByText("Sure, you wrote it")).toBeTruthy();
    expect(within(lineOf("Lives in Kilimani")).getByRole("button", { name: "Unpin" })).toBeTruthy();
  });

  it("rewords a line in place and shows the pond's answer", async () => {
    api.changeFact.mockResolvedValue({ ...VEG, fact: "Vegetarian, eats fish", confidence: "sure", source: { kind: "you", ref: null } });
    const { said } = renderPanel();
    await screen.findByText("Vegetarian");

    fireEvent.click(within(lineOf("Vegetarian")).getByRole("button", { name: "Edit" }));
    const form = screen.getByRole("form", { name: "The line" });
    fireEvent.change(within(form).getByRole("textbox", { name: "The line" }), { target: { value: " Vegetarian, eats fish " } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.changeFact).toHaveBeenCalledWith("f-veg", { fact: "Vegetarian, eats fish" }));
    expect(await screen.findByText("Vegetarian, eats fish")).toBeTruthy();
    expect(screen.getByText("Sure, you wrote it. Helps with: find restaurants")).toBeTruthy();
    expect(said).toEqual(["Saved."]);
  });

  it("keeps the edit open with the pond's reason when another line already says it", async () => {
    api.changeFact.mockRejectedValue(new ApiError(409, "another line of your fact sheet already says that"));
    renderPanel();
    await screen.findByText("Vegetarian");
    fireEvent.click(within(lineOf("Vegetarian")).getByRole("button", { name: "Edit" }));
    const form = screen.getByRole("form", { name: "The line" });
    fireEvent.change(within(form).getByRole("textbox", { name: "The line" }), { target: { value: "Lives in Kilimani" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));

    expect((await within(form).findByRole("alert")).textContent).toBe("Another line of your fact sheet already says that.");
  });

  it("sends nothing for an edit that changed nothing", async () => {
    renderPanel();
    await screen.findByText("Vegetarian");
    fireEvent.click(within(lineOf("Vegetarian")).getByRole("button", { name: "Edit" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(api.changeFact).not.toHaveBeenCalled();
    expect(screen.getByText("Vegetarian")).toBeTruthy();
  });

  it("pins a line", async () => {
    api.changeFact.mockResolvedValue({ ...VEG, pinned: true });
    renderPanel();
    await screen.findByText("Vegetarian");
    fireEvent.click(within(lineOf("Vegetarian")).getByRole("button", { name: "Pin" }));
    await waitFor(() => expect(api.changeFact).toHaveBeenCalledWith("f-veg", { pinned: true }));
    expect(await within(lineOf("Vegetarian")).findByRole("button", { name: "Unpin" })).toBeTruthy();
  });

  it("asks before removing, saying it will not be learned again", async () => {
    api.removeFact.mockResolvedValue(undefined);
    const { said } = renderPanel();
    await screen.findByText("Vegetarian");
    fireEvent.click(within(lineOf("Vegetarian")).getByRole("button", { name: "Remove" }));

    const dialog = await screen.findByRole("dialog", { name: "Remove this line" });
    expect(dialog.textContent).toContain("Goose won't learn it again");
    expect(api.removeFact).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove" }));

    await waitFor(() => expect(api.removeFact).toHaveBeenCalledWith("f-veg"));
    await waitFor(() => expect(screen.queryByText("Vegetarian")).toBeNull());
    expect(said).toEqual(["Removed."]);
  });

  it("adds a line in the member's words", async () => {
    api.addFact.mockResolvedValue({ facts: [VEG, HOME, factLine({ id: "f-new", category: "other", fact: "Has two cats" })], limit: 100 });
    renderPanel();
    await screen.findByText("Vegetarian");
    fireEvent.change(screen.getByRole("textbox", { name: "What should Goose know?" }), { target: { value: "Has two cats" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));
    await waitFor(() => expect(api.addFact).toHaveBeenCalledWith("other", "Has two cats"));
    expect(await screen.findByText("3 of 100 lines")).toBeTruthy();
  });

  it("asks for a rebuild", async () => {
    api.rebuildFacts.mockResolvedValue({ state: "queued" });
    const { said } = renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Rebuild" }));
    await waitFor(() => expect(said[0]).toMatch(/^Rebuild asked for/));
  });
});

describe("the don't-list", () => {
  it("shows each rule's strength and where it came from, deletes and adds", async () => {
    api.removeDontRule.mockResolvedValue([]);
    api.addDontRule.mockResolvedValue([{ ...RULE, id: "r-2", rule: "Never: beach holidays", strength: "never", origin: "you", reasons: [] }]);
    renderPanel();
    const list = await screen.findByRole("list", { name: "Things Goose won't offer" });
    expect(within(list).getByText("Avoid")).toBeTruthy();
    expect(within(list).getByText("You turned something down. Not relevant to me")).toBeTruthy();

    fireEvent.click(within(list).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(api.removeDontRule).toHaveBeenCalledWith("r-1"));
    expect(await screen.findByText("No rules yet. Turning something down with a reason adds one.")).toBeTruthy();

    fireEvent.change(screen.getByRole("textbox", { name: "A new rule" }), { target: { value: "Beach holidays" } });
    fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await waitFor(() => expect(api.addDontRule).toHaveBeenCalledWith("Beach holidays", "never"));
    expect(await screen.findByText("Never: beach holidays")).toBeTruthy();
  });
});
