import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { deliverable } from "./myGooseFixtures";

const api = vi.hoisted(() => ({
  getDeliverable: vi.fn(),
  markDeliverableSeen: vi.fn(),
  refreshDeliverable: vi.fn(),
  rejectDeliverable: vi.fn(),
  getDeliverableFile: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { DeliverableView } from "./DeliverableView";

function renderView() {
  const said: string[] = [];
  const back = vi.fn();
  render(<DeliverableView id="d-1" anchorTitle="Diani trip" variant="classic" say={(t) => said.push(t)} onBack={back} />);
  return { said, back };
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.getDeliverable.mockResolvedValue(deliverable());
  api.markDeliverableSeen.mockResolvedValue(undefined);
  api.rejectDeliverable.mockResolvedValue(undefined);
});

describe("reading a deliverable", () => {
  it("draws the body as a document with its facts and sources, and goes back to its anchor", async () => {
    const { back } = renderView();
    expect(await screen.findByRole("heading", { level: 1, name: "Plan for Diani trip" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Day one" })).toBeTruthy();
    expect(screen.getByText("KSh 80,000")).toBeTruthy();
    expect(screen.getByRole("region", { name: "Sources" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Back to Diani trip" }));
    expect(back).toHaveBeenCalled();
  });

  it("marks it seen when it was new, and only then", async () => {
    api.getDeliverable.mockResolvedValue(deliverable({ unseen: true }));
    renderView();
    await waitFor(() => expect(api.markDeliverableSeen).toHaveBeenCalledWith("d-1"));
    cleanup();

    vi.clearAllMocks();
    api.getDeliverable.mockResolvedValue(deliverable({ unseen: false }));
    renderView();
    await screen.findByRole("heading", { level: 1, name: "Plan for Diani trip" });
    expect(api.markDeliverableSeen).not.toHaveBeenCalled();
  });

  it("shows the critic's objections plainly above the body when Goose is unsure", async () => {
    api.getDeliverable.mockResolvedValue(
      deliverable({
        state: "unsure",
        objections: [{ kind: "grounding", about: "The ferry runs hourly", fix: "Cite the timetable" }],
      }),
    );
    renderView();
    const unsure = await screen.findByRole("region", { name: /isn't sure/ });
    expect(within(unsure).getByText("Not backed by a source")).toBeTruthy();
    expect(within(unsure).getByText("The ferry runs hourly")).toBeTruthy();
    expect(within(unsure).getByText("What would fix it: Cite the timetable")).toBeTruthy();
    const order = unsure.compareDocumentPosition(screen.getByRole("heading", { name: "Day one" }));
    expect(order & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("says what a refresh changed", async () => {
    api.getDeliverable.mockResolvedValue(deliverable({ changed: "The SGR fare went up to KSh 1,500." }));
    renderView();
    const changed = await screen.findByRole("region", { name: "What changed" });
    expect(within(changed).getByText("The SGR fare went up to KSh 1,500.")).toBeTruthy();
  });
});

describe("turning it down", () => {
  it("Not this one sends the reasons and words, says so, and leaves", async () => {
    const { said, back } = renderView();
    fireEvent.click(await screen.findByRole("button", { name: "Not this one" }));
    const sheet = await screen.findByRole("dialog", { name: "Not this one" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Not relevant to me" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Not this one" }));

    await waitFor(() => expect(api.rejectDeliverable).toHaveBeenCalledWith("d-1", { verb: "not_this", reasons: ["not_relevant"] }));
    expect(said).toEqual(["Not this one: Plan for Diani trip is turned down."]);
    expect(back).toHaveBeenCalled();
  });

  it("Never this carries the member's own words, from which the pond writes a rule", async () => {
    const { said } = renderView();
    fireEvent.click(await screen.findByRole("button", { name: "Never this" }));
    const sheet = await screen.findByRole("dialog", { name: "Never this" });
    fireEvent.change(within(sheet).getByRole("textbox", { name: "What was wrong?" }), { target: { value: "No beach holidays" } });
    fireEvent.click(within(sheet).getByRole("button", { name: "Too generic" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Never this" }));

    await waitFor(() =>
      expect(api.rejectDeliverable).toHaveBeenCalledWith("d-1", { verb: "never", reasons: ["too_generic"], text: "No beach holidays" }),
    );
    expect(said).toEqual(["Never this: Goose won't make anything like Plan for Diani trip again."]);
  });

  it("can be called off, sending nothing", async () => {
    renderView();
    fireEvent.click(await screen.findByRole("button", { name: "Not this one" }));
    const sheet = await screen.findByRole("dialog", { name: "Not this one" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog", { name: "Not this one" })).toBeNull();
    expect(api.rejectDeliverable).not.toHaveBeenCalled();
  });

  it("is not offered to a member it is only shared with", async () => {
    api.getDeliverable.mockResolvedValue(deliverable({ mine: false }));
    renderView();
    await screen.findByRole("heading", { level: 1, name: "Plan for Diani trip" });
    expect(screen.queryByRole("button", { name: "Not this one" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Refresh" })).toBeNull();
    expect(screen.getByRole("button", { name: "Download" })).toBeTruthy();
  });
});

describe("refresh and download", () => {
  it("asks for a refresh and says where it stands", async () => {
    api.refreshDeliverable.mockResolvedValue({ state: "queued" });
    const { said } = renderView();
    fireEvent.click(await screen.findByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(said).toEqual(["Refresh asked for. It's next in line."]));
  });

  it("downloads the pond's file under the pond's name", async () => {
    const blob = new Blob(["# Plan"], { type: "text/markdown" });
    api.getDeliverableFile.mockResolvedValue({ blob, filename: "plan-for-diani-trip.md" });
    const created: string[] = [];
    const objectUrl = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:plan");
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      created.push(this.download);
    });

    renderView();
    fireEvent.click(await screen.findByRole("button", { name: "Download" }));
    await waitFor(() => expect(created).toEqual(["plan-for-diani-trip.md"]));
    click.mockRestore();
    objectUrl.mockRestore();
  });
});
