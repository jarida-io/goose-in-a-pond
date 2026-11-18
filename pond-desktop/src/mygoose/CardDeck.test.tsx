import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ApiError, type IdeaCardSet } from "../api/types";
import { ideaCard } from "./myGooseFixtures";

const api = vi.hoisted(() => ({
  listIdeaCards: vi.fn(),
  refreshIdeaCards: vi.fn(),
  doIdeaCard: vi.fn(),
  goDeeperOnIdeaCard: vi.fn(),
  saveIdeaCard: vi.fn(),
  unsaveIdeaCard: vi.fn(),
  dismissIdeaCard: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { CardDeck } from "./CardDeck";

const WALK = ideaCard();
const ASK = ideaCard({
  id: "c-ask",
  kind: "ask",
  title: "What should we pack for Diani?",
  action: { kind: "chat", label: "Ask Goose", prompt: "What should we pack for Diani?" },
});

function deck(over: Partial<IdeaCardSet> = {}): IdeaCardSet {
  return { cards: [WALK, ASK], set: { state: "ready", made_at: "2026-11-17T06:00:00Z" }, ...over };
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.listIdeaCards.mockResolvedValue(deck());
});

describe("CardDeck", () => {
  it("lists every card on the classic My Goose, with the set's state", async () => {
    render(<CardDeck variant="classic" place="mygoose" />);
    const list = await screen.findByRole("list", { name: "Your cards" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getByText("Ready")).toBeTruthy();
  });

  it("does a to-do card, drops it and says what happened", async () => {
    api.doIdeaCard.mockResolvedValue({ card: { ...WALK, state: "done" }, result: { kind: "todo" } });
    render(<CardDeck variant="classic" place="mygoose" />);
    fireEvent.click(await screen.findByRole("button", { name: "Add to-do" }));
    await waitFor(() => expect(api.doIdeaCard).toHaveBeenCalledWith("c-1"));
    await waitFor(() => expect(screen.queryByText(WALK.title)).toBeNull());
    expect(screen.getByText(`Added to your to-dos: ${WALK.title}.`)).toBeTruthy();
  });

  it("goes deeper on any card through its own route", async () => {
    api.goDeeperOnIdeaCard.mockResolvedValue({ card: { ...ASK, state: "done" }, anchor_id: "a-diani" });
    render(<CardDeck variant="classic" place="mygoose" />);
    const askCard = await screen.findByRole("article", { name: ASK.title });
    fireEvent.click(within(askCard).getByRole("button", { name: "Go deeper" }));
    await waitFor(() => expect(api.goDeeperOnIdeaCard).toHaveBeenCalledWith("c-ask"));
    await waitFor(() => expect(screen.queryByText(ASK.title)).toBeNull());
    expect(api.doIdeaCard).not.toHaveBeenCalled();
    expect(screen.getByText(/Going deeper on/)).toBeTruthy();
  });

  it("sends a chat card's prompt to chat", async () => {
    api.doIdeaCard.mockResolvedValue({ card: { ...ASK, state: "done" }, result: { kind: "chat", prompt: "Pack list for Diani, please" } });
    const onAsk = vi.fn();
    render(<CardDeck variant="classic" place="mygoose" onAsk={onAsk} />);
    fireEvent.click(await screen.findByRole("button", { name: "Ask Goose" }));
    await waitFor(() => expect(onAsk).toHaveBeenCalledWith("Pack list for Diani, please"));
  });

  it("saves a card in place and turns another down with reasons", async () => {
    api.saveIdeaCard.mockResolvedValue({ ...WALK, state: "saved" });
    api.dismissIdeaCard.mockResolvedValue(undefined);
    render(<CardDeck variant="classic" place="mygoose" />);
    const [first] = await screen.findAllByRole("button", { name: "Save" });
    fireEvent.click(first);
    await screen.findByRole("button", { name: "Unsave" });
    expect(api.saveIdeaCard).toHaveBeenCalledWith("c-1");

    const askCard = screen.getByRole("article", { name: ASK.title });
    fireEvent.click(within(askCard).getByRole("button", { name: "Not this" }));
    const sheet = await screen.findByRole("dialog", { name: "Not this one" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Not relevant to me" }));
    fireEvent.click(within(sheet).getByRole("button", { name: "Not this one" }));
    await waitFor(() => expect(api.dismissIdeaCard).toHaveBeenCalledWith("c-ask", { reasons: ["not_relevant"], text: undefined }));
    await waitFor(() => expect(screen.queryByText(ASK.title)).toBeNull());
  });

  it("refreshes an empty deck and shows it being made", async () => {
    api.listIdeaCards
      .mockResolvedValueOnce(deck({ cards: [], set: { state: "none", made_at: null } }))
      .mockResolvedValue(deck({ cards: [], set: { state: "making", made_at: null } }));
    api.refreshIdeaCards.mockResolvedValue({ state: "queued" });
    render(<CardDeck variant="classic" place="mygoose" />);
    expect(await screen.findByText("Nothing yet")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(api.refreshIdeaCards).toHaveBeenCalled());
    expect(await screen.findByText("Making your cards")).toBeTruthy();
    expect(screen.getByText(/Goose is making your cards/)).toBeTruthy();
  });

  // A refresh queued behind ten jobs showed only "Making your cards", and read as stuck.
  it("says how many jobs a queued set waits behind", async () => {
    api.listIdeaCards.mockResolvedValue(deck({ cards: [], set: { state: "queued", made_at: null, ahead: 10 } }));
    render(<CardDeck variant="classic" place="mygoose" />);
    expect(await screen.findByText("Goose is finishing 10 things first")).toBeTruthy();
  });

  it("shows one card at a time on the hub", async () => {
    render(<CardDeck variant="hub" place="mygoose" />);
    expect(await screen.findByText("Card 1 of 2")).toBeTruthy();
    expect(screen.queryByText(ASK.title)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(screen.getByText(ASK.title)).toBeTruthy();
    expect(screen.getByText("Card 2 of 2")).toBeTruthy();
  });

  it("draws nothing on a pond that has no cards, and nothing on Home when the cards fail", async () => {
    api.listIdeaCards.mockRejectedValue(new ApiError(404, "Not Found", undefined, undefined));
    const { container } = render(<CardDeck variant="classic" place="mygoose" />);
    await waitFor(() => expect(api.listIdeaCards).toHaveBeenCalled());
    await waitFor(() => expect(container.innerHTML).toBe(""));
    cleanup();
    api.listIdeaCards.mockRejectedValue(new ApiError(500, "boom"));
    const home = render(<CardDeck variant="hub" place="home" />);
    await waitFor(() => expect(api.listIdeaCards).toHaveBeenCalledTimes(2));
    expect(home.container.innerHTML).toBe("");
  });
});
