import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, within } from "@testing-library/react";
import {
  ChatHistory,
  cardWeight,
  relativeWhen,
  messageCountLabel,
  matchesQuery,
} from "./ChatHistory";
import type { SessionSummary } from "../api/types";

afterEach(cleanup);

function session(over: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: "s1",
    title: "Wake word fires twice",
    preview: "so i was wondering whether the jetson was dropping them",
    created_at: "2026-08-14T09:00:00Z",
    updated_at: "2026-08-14T09:41:00Z",
    message_count: 8,
    ...over,
  };
}

describe("cardWeight", () => {
  it("grows a card with the conversation it holds", () => {
    expect(cardWeight(session({ message_count: 2, preview: "hi" }))).toBe("tile");
    expect(cardWeight(session({ message_count: 8, preview: "hi" }))).toBe("card");
    expect(cardWeight(session({ message_count: 40, preview: "hi" }))).toBe("column");
  });

  it("counts a long opening line as substance even in a short exchange", () => {
    const wordy = "x".repeat(200);
    expect(cardWeight(session({ message_count: 1, preview: wordy }))).toBe("column");
  });

  it("treats a conversation with nothing recorded as the smallest", () => {
    expect(cardWeight(session({ message_count: undefined, preview: undefined }))).toBe("tile");
  });
});

describe("relativeWhen", () => {
  const now = new Date(2026, 7, 14, 15, 0, 0); // Fri 14 Aug 2026

  it("uses a clock time for today", () => {
    const out = relativeWhen(new Date(2026, 7, 14, 9, 41).toISOString(), now);
    expect(out).toMatch(/9[:.]41/);
  });

  it("names yesterday, then the weekday, then the date", () => {
    expect(relativeWhen(new Date(2026, 7, 13, 9, 0).toISOString(), now)).toBe("Yesterday");
    expect(relativeWhen(new Date(2026, 7, 10, 9, 0).toISOString(), now)).toBe("Monday");
    // Past a week a weekday name stops helping — you are scanning, not recalling.
    expect(relativeWhen(new Date(2026, 6, 30, 9, 0).toISOString(), now)).toMatch(/Jul/);
  });

  it("says nothing rather than 'Invalid Date' for a timestamp it cannot read", () => {
    expect(relativeWhen("not-a-date", now)).toBe("");
  });
});

describe("messageCountLabel", () => {
  it("counts, pluralises, and stays silent when there is nothing to count", () => {
    expect(messageCountLabel(1)).toBe("1 message");
    expect(messageCountLabel(12)).toBe("12 messages");
    expect(messageCountLabel(0)).toBe("");
    expect(messageCountLabel(undefined)).toBe("");
  });
});

describe("matchesQuery", () => {
  it("searches what a person would actually remember", () => {
    const s = session({ title: "Wake word fires twice", preview: "the jetson was dropping them" });
    expect(matchesQuery(s, "wake")).toBe(true);
    expect(matchesQuery(s, "JETSON")).toBe(true);
    expect(matchesQuery(s, "bitcoin")).toBe(false);
    expect(matchesQuery(s, "   ")).toBe(true);
  });
});

describe("the wall", () => {
  const noop = () => {};

  function renderWall(sessions: SessionSummary[], over: Partial<Parameters<typeof ChatHistory>[0]> = {}) {
    const props = {
      sessions,
      loading: false,
      onOpen: vi.fn(),
      onNewChat: vi.fn(),
      onDelete: vi.fn(),
      ...over,
    };
    render(<ChatHistory {...props} />);
    return props;
  }

  it("shows every conversation, sized by what it holds", () => {
    renderWall([
      session({ id: "a", title: "Short one", message_count: 2, preview: "hi" }),
      session({ id: "b", title: "Long one", message_count: 40, preview: "hi" }),
    ]);

    const cards = document.querySelectorAll(".chist__card");
    expect(cards.length).toBe(2);
    expect(cards[0].getAttribute("data-weight")).toBe("tile");
    expect(cards[1].getAttribute("data-weight")).toBe("column");
  });

  it("opens a conversation from the card, with the point it grew from", () => {
    const props = renderWall([session({ id: "a", title: "Wake word fires twice" })]);

    const card = screen.getByRole("button", { name: /Open conversation: Wake word fires twice/ });
    // happy-dom has no layout, so the geometry is stubbed.
    card.getBoundingClientRect = () => ({ left: 300, top: 200, width: 240, height: 160 }) as DOMRect;
    const pane = document.querySelector(".chist")!;
    pane.getBoundingClientRect = () => ({ left: 100, top: 50, width: 900, height: 700 }) as DOMRect;

    fireEvent.click(card);

    expect(props.onOpen).toHaveBeenCalledWith("a", { x: 320, y: 230 });
  });

  it("filters as you search, and says so when nothing matches", () => {
    renderWall([
      session({ id: "a", title: "Wake word fires twice", preview: "jetson" }),
      session({ id: "b", title: "Bitcoin conversion", preview: "shillings" }),
    ]);

    fireEvent.change(screen.getByLabelText("Search conversations"), { target: { value: "bitcoin" } });
    expect(document.querySelectorAll(".chist__card").length).toBe(1);
    expect(screen.getByText("Bitcoin conversion")).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Search conversations"), { target: { value: "zzz" } });
    expect(document.querySelectorAll(".chist__card").length).toBe(0);
    expect(screen.getByText(/Nothing matches/)).toBeTruthy();
  });

  it("invites you to start one when there is nothing here", () => {
    const props = renderWall([]);
    expect(screen.getByText("No conversations yet.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Start one/ }));
    expect(props.onNewChat).toHaveBeenCalled();
  });

  it("asks before deleting, and opening is not what a delete press does", () => {
    const props = renderWall([session({ id: "a", title: "Wake word fires twice" })]);

    fireEvent.click(screen.getAllByRole("button", { name: /Delete conversation/ })[0]);
    expect(props.onDelete).not.toHaveBeenCalled();
    expect(props.onOpen).not.toHaveBeenCalled();

    const card = document.querySelector(".chist__card")!;
    fireEvent.click(within(card as HTMLElement).getByRole("button", { name: /Delete conversation/ }));
    expect(props.onDelete).toHaveBeenCalledWith("a");
    expect(props.onOpen).not.toHaveBeenCalled();
  });

  it("can be walked and opened from the keyboard", () => {
    const props = renderWall([session({ id: "a", title: "Wake word fires twice" })]);
    const card = screen.getByRole("button", { name: /Open conversation/ });
    card.getBoundingClientRect = () => ({ left: 0, top: 0, width: 0, height: 0 }) as DOMRect;

    expect((card as HTMLElement).tabIndex).toBe(0);
    fireEvent.keyDown(card, { key: "Enter" });
    expect(props.onOpen).toHaveBeenCalledWith("a", expect.anything());
  });

  it("holds the wall's shape while it loads, rather than jumping", () => {
    render(<ChatHistory sessions={[]} loading onOpen={noop} onNewChat={noop} onDelete={noop} />);
    const ghosts = document.querySelectorAll(".chist__card--ghost");
    expect(ghosts.length).toBeGreaterThan(0);
    // Placeholders are not conversations, so nothing here is pressable.
    expect(screen.queryByRole("button", { name: /Open conversation/ })).toBeNull();
  });
});
