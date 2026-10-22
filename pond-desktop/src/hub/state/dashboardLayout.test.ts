// Home's layout store. The v1 cases matter most: a missed migration resets every Home silently.

import { beforeEach, describe, expect, it } from "vitest";
import {
  CARDS,
  DEFAULT_LAYOUT,
  MAX_PAGES,
  __resetLayoutCache,
  getDashboardLayout,
  hideCard,
  moveCard,
  moveCardToPage,
  placedCards,
  resetLayout,
  setCardSize,
  showCard,
} from "./dashboardLayout";

const KEY = "giap-dashboard-layout";

function store(layout: unknown): void {
  localStorage.setItem(KEY, JSON.stringify(layout));
  __resetLayoutCache();
}

/** Every placed card's id, page by page. The shape most assertions want. */
function ids(): string[][] {
  return getDashboardLayout().pages.map((p) => p.map((c) => c.id));
}

beforeEach(() => {
  localStorage.clear();
  __resetLayoutCache();
});

describe("the default", () => {
  it("is two pages and nothing more", () => {
    expect(ids()).toEqual([["weather", "devices"], ["nowPlaying", "tonight"]]);
    // remoteAccess ships available but unplaced (dashboardLayout.ts's own comment on DEFAULT_LAYOUT).
    expect(getDashboardLayout().hidden).toEqual(["remoteAccess"]);
  });

  it("accounts for every card in the catalogue exactly once", () => {
    const l = getDashboardLayout();
    const known = CARDS.map((c) => c.id).sort();
    expect([...l.pages.flat().map((c) => c.id), ...l.hidden].sort()).toEqual(known);
  });
});

describe("a layout written by an older release", () => {
  it("keeps the arrangement a v1 release wrote", () => {
    store({ order: ["devices", "weather"], hidden: ["nowPlaying"] });
    const l = getDashboardLayout();
    expect(l.pages).toEqual([
      [
        { id: "devices", size: "l" },
        { id: "weather", size: "m" },
      ],
    ]);
    // migrateV1 recomputes hidden from the catalogue rather than trusting the v1 payload's own
    // "hidden" field, so every other card lands here, in CARDS order.
    expect(l.hidden).toEqual(["nowPlaying", "tonight", "remoteAccess"]);
    expect(l).not.toEqual(DEFAULT_LAYOUT);
  });

  it("drops cards this release no longer has", () => {
    store({
      version: 2,
      pages: [
        [
          { id: "cameras", size: "m" },
          { id: "devices", size: "l" },
          { id: "scenes", size: "s" },
        ],
        [
          { id: "todos", size: "s" },
          { id: "routines", size: "m" },
          { id: "suggestion", size: "l" },
        ],
      ],
      hidden: [],
    });
    expect(ids()).toEqual([["devices"]]);
  });

  it("offers a card added since, without placing it on Home", () => {
    store({ version: 2, pages: [[{ id: "devices", size: "l" }]], hidden: [] });
    const l = getDashboardLayout();
    expect(ids()).toEqual([["devices"]]);
    expect(l.hidden).toContain("weather");
    expect(l.hidden).toContain("nowPlaying");
  });

  it("survives a corrupt payload rather than rendering nothing", () => {
    localStorage.setItem(KEY, "{not json");
    __resetLayoutCache();
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });

  it("ignores a card claimed as both placed and hidden", () => {
    store({ version: 2, pages: [[{ id: "weather", size: "m" }]], hidden: ["weather"] });
    const l = getDashboardLayout();
    expect(ids()).toEqual([["weather"]]);
    expect(l.hidden).not.toContain("weather");
  });

  it("does not repeat a card listed twice on the same page", () => {
    store({
      version: 2,
      pages: [[{ id: "devices", size: "l" }, { id: "devices", size: "s" }, { id: "weather", size: "m" }]],
      hidden: [],
    });
    expect(ids()).toEqual([["devices", "weather"]]);
  });

  it("does not repeat a card listed on two different pages", () => {
    store({
      version: 2,
      pages: [[{ id: "devices", size: "l" }], [{ id: "devices", size: "s" }, { id: "weather", size: "m" }]],
      hidden: [],
    });
    expect(ids()).toEqual([["devices"], ["weather"]]);
  });

  it("falls back to the card's default size when the stored one is unknown", () => {
    store({ version: 2, pages: [[{ id: "weather", size: "enormous" }]], hidden: [] });
    expect(getDashboardLayout().pages[0][0].size).toBe("m");
  });

  it("drops an empty page rather than paging onto nothing", () => {
    store({ version: 2, pages: [[], [{ id: "devices", size: "l" }], []], hidden: [] });
    expect(ids()).toEqual([["devices"]]);
  });

  /** Losing a card the household placed is worse than a crowded last page. */
  it("merges pages past the limit into the last one it keeps", () => {
    store({
      version: 2,
      pages: [
        [{ id: "devices", size: "l" }],
        [{ id: "weather", size: "m" }],
        [{ id: "nowPlaying", size: "s" }],
        [],
      ],
      hidden: [],
    });
    const l = getDashboardLayout();
    expect(l.pages.length).toBeLessThanOrEqual(MAX_PAGES);
    expect(placedCards(l).map((c) => c.id).sort()).toEqual(
      ["devices", "nowPlaying", "weather"].sort(),
    );
  });
});

describe("an empty Home", () => {
  it("is not a preference the store will hold", () => {
    store({ version: 2, pages: [], hidden: CARDS.map((c) => c.id) });
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });

  it("is not reachable by a payload of nothing but empty pages", () => {
    store({ version: 2, pages: [[], []], hidden: [] });
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });

  it("cannot be reached by hiding the last placed card", () => {
    store({ version: 2, pages: [[{ id: "devices", size: "l" }]], hidden: [] });
    hideCard("devices");
    expect(ids()).toEqual([["devices"]]);
  });
});

describe("arranging", () => {
  it("adds a card at the end of a page, where it can be seen to have arrived", () => {
    store({ version: 2, pages: [[{ id: "devices", size: "l" }]], hidden: [] });
    showCard("nowPlaying");
    expect(ids()).toEqual([["devices", "nowPlaying"]]);
    expect(getDashboardLayout().hidden).not.toContain("nowPlaying");
  });

  it("adds a card at its default size", () => {
    store({ version: 2, pages: [[{ id: "devices", size: "l" }]], hidden: [] });
    showCard("weather");
    expect(getDashboardLayout().pages[0][1]).toEqual({ id: "weather", size: "m" });
  });

  it("returns a hidden card to the sheet rather than forgetting it", () => {
    hideCard("weather");
    const l = getDashboardLayout();
    expect(placedCards(l).map((c) => c.id)).not.toContain("weather");
    expect(l.hidden).toContain("weather");
  });

  it("moves a card one place within its page", () => {
    moveCard("devices", -1);
    expect(ids()[0]).toEqual(["devices", "weather"]);
  });

  /** Moving past either end is a no-op, not a wrap and not a page change. */
  it("will not move a card off either end of its page", () => {
    const before = ids();
    moveCard("weather", -1);
    moveCard("devices", 1);
    expect(ids()).toEqual(before);
  });

  it("will not let a move cross a page", () => {
    // "nowPlaying" is first on page 2; moving it up must not put it on page 1.
    moveCard("nowPlaying", -1);
    expect(ids()).toEqual([["weather", "devices"], ["nowPlaying", "tonight"]]);
  });

  it("crosses a page only when asked to explicitly", () => {
    moveCardToPage("nowPlaying", 0);
    // "tonight" is left behind on page 2, so it stays rather than collapsing.
    expect(ids()).toEqual([["weather", "devices", "nowPlaying"], ["tonight"]]);
  });

  // Emptying the source page shifts later pages down, so a card sent to page 2 can land on page 1.
  it("reports the page a card landed on, not the page it was sent to", () => {
    store({
      version: 2,
      pages: [
        [{ id: "weather", size: "m" }],
        [{ id: "nowPlaying", size: "s" }],
        [{ id: "devices", size: "l" }],
      ],
      hidden: [],
    });
    const landedOn = moveCardToPage("weather", 1);
    expect(ids()).toEqual([["nowPlaying", "weather"], ["devices"]]);
    expect(landedOn).toBe(0);
  });

  it("reports the page asked for when no page collapsed under the move", () => {
    // Devices stays behind on page 1, so nothing is dropped and the two agree.
    expect(moveCardToPage("weather", 1)).toBe(1);
    expect(ids()).toEqual([["devices"], ["nowPlaying", "tonight", "weather"]]);
  });

  it("reports nothing at all when the move is refused", () => {
    // Already on page 1, so there is no move and no page to follow it to.
    expect(moveCardToPage("weather", 0)).toBeNull();
    expect(ids()).toEqual([["weather", "devices"], ["nowPlaying", "tonight"]]);
  });

  it("makes one new page beyond the last, up to the limit", () => {
    moveCardToPage("weather", 2);
    expect(ids()).toEqual([["devices"], ["nowPlaying", "tonight"], ["weather"]]);
    // A fourth page is past MAX_PAGES, so the move is refused.
    moveCardToPage("devices", 3);
    expect(ids()).toEqual([["devices"], ["nowPlaying", "tonight"], ["weather"]]);
  });

  it("resizes a card and leaves its place alone", () => {
    setCardSize("weather", "l");
    expect(getDashboardLayout().pages[0]).toEqual([
      { id: "weather", size: "l" },
      { id: "devices", size: "l" },
    ]);
  });

  it("resets to the shipped Home", () => {
    hideCard("weather");
    setCardSize("devices", "s");
    resetLayout();
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });
});

describe("persistence", () => {
  it("survives a reload", () => {
    setCardSize("nowPlaying", "l");
    __resetLayoutCache();
    expect(getDashboardLayout().pages[1][0].size).toBe("l");
  });

  it("still applies when storage refuses the write", () => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = () => {
      throw new Error("quota");
    };
    try {
      hideCard("weather");
      expect(placedCards(getDashboardLayout()).map((c) => c.id)).not.toContain("weather");
    } finally {
      Storage.prototype.setItem = original;
    }
  });
});
