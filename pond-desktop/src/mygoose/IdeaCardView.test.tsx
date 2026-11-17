import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ApiError, type IdeaCard } from "../api/types";
import { ideaCard } from "./myGooseFixtures";
import { IdeaCardView } from "./IdeaCardView";

function renderCard(card: IdeaCard = ideaCard()) {
  const handlers = {
    onDo: vi.fn().mockResolvedValue(undefined),
    onGoDeeper: vi.fn().mockResolvedValue(undefined),
    onSave: vi.fn().mockResolvedValue(undefined),
    onDismiss: vi.fn().mockResolvedValue(undefined),
  };
  render(<IdeaCardView card={card} variant="classic" {...handlers} />);
  return handlers;
}

afterEach(cleanup);

describe("IdeaCardView", () => {
  it("shows the kind, title, why with its source, and only the fields the card has", () => {
    renderCard();
    expect(screen.getByText("Something to do")).toBeTruthy();
    expect(screen.getByRole("heading", { level: 3, name: "Walk the Karura Forest waterfall trail" })).toBeTruthy();
    expect(screen.getByText(/You said you want more time outdoors/)).toBeTruthy();
    expect(screen.getByText("From your interest: Hiking")).toBeTruthy();
    expect(screen.getByText("Saturday 9 am")).toBeTruthy();
    expect(screen.getByText("Karura Forest")).toBeTruthy();
    expect(screen.getByText("KSh 600 entry")).toBeTruthy();
    expect(screen.queryByText("Effort")).toBeNull();
    for (const name of ["Add to-do", "Save", "Not this", "Go deeper"]) {
      expect(screen.getByRole("button", { name })).toBeTruthy();
    }
    expect(screen.queryByText("What Goose found")).toBeNull();
  });

  it("opens to its notes and sources", () => {
    renderCard();
    const details = screen.getByRole("button", { name: "Details" });
    fireEvent.click(details);
    expect(details.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("The waterfall trail is about 5 km.")).toBeTruthy();
    expect(screen.getByText("Check the weather on Friday.")).toBeTruthy();
    expect(screen.getByText("Entry prices may have changed.")).toBeTruthy();
    const link = screen.getByRole("link", { name: /Karura Forest: visiting/ });
    expect(link.getAttribute("href")).toBe("https://www.karurafriends.org/visit");
  });

  it("does, saves, unsaves and goes deeper through its handlers", async () => {
    const card = ideaCard();
    const h = renderCard(card);
    fireEvent.click(screen.getByRole("button", { name: "Add to-do" }));
    await waitFor(() => expect(h.onDo).toHaveBeenCalledWith(card));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(h.onSave).toHaveBeenCalledWith(card, true));
    fireEvent.click(screen.getByRole("button", { name: "Go deeper" }));
    await waitFor(() => expect(h.onGoDeeper).toHaveBeenCalledWith(card));

    cleanup();
    const saved = ideaCard({ state: "saved" });
    const again = renderCard(saved);
    fireEvent.click(screen.getByRole("button", { name: "Unsave" }));
    await waitFor(() => expect(again.onSave).toHaveBeenCalledWith(saved, false));
  });

  it("has one Go deeper button when that is its action, and no Do it without an action", async () => {
    const deeper = ideaCard({ action: { kind: "go_deeper", label: "Look into it" } });
    const h = renderCard(deeper);
    expect(screen.getAllByRole("button", { name: "Go deeper" })).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "Look into it" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Go deeper" }));
    await waitFor(() => expect(h.onGoDeeper).toHaveBeenCalledWith(deeper));
    expect(h.onDo).not.toHaveBeenCalled();
    cleanup();
    renderCard(ideaCard({ action: null }));
    expect(screen.queryByRole("button", { name: "Add to-do" })).toBeNull();
  });

  it("turns a card down with reason chips and the member's words", async () => {
    const card = ideaCard();
    const h = renderCard(card);
    fireEvent.click(screen.getByRole("button", { name: "Not this" }));
    const sheet = await screen.findByRole("dialog", { name: "Not this one" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Wrong facts" }));
    fireEvent.change(within(sheet).getByRole("textbox", { name: "What was wrong?" }), { target: { value: "It is closed on Saturdays" } });
    fireEvent.click(within(sheet).getByRole("button", { name: "Not this one" }));
    await waitFor(() =>
      expect(h.onDismiss).toHaveBeenCalledWith(card, { reasons: ["wrong_facts"], text: "It is closed on Saturdays" }),
    );
  });

  it("says what went wrong when an action fails", async () => {
    const h = renderCard();
    h.onDo.mockRejectedValueOnce(new ApiError(500, "boom"));
    fireEvent.click(screen.getByRole("button", { name: "Add to-do" }));
    expect((await screen.findByRole("alert")).textContent).toMatch(/couldn't do that just now/);
  });
});
