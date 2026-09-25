// The store behind Home's arrangement, on both of its axes.
//
// Rewritten from the flat-order version rather than dropped: every behaviour
// the old names encoded is still here, re-aimed at `pages`, plus one case per
// failure mode the second axis introduced. The single most important addition
// is the v1 migration — without it a silent reset of every household's Home
// ships undetected, because a v1 payload is a perfectly valid object that a v2
// reader would simply find no `pages` in.

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
  /**
   * Home was pared back deliberately, and this file is where that decision
   * still lives. A card arriving in the default is a product decision; a card
   * arriving on one household's Home is theirs.
   */
  it("is two pages and nothing more", () => {
    expect(ids()).toEqual([["weather", "devices"], ["nowPlaying"]]);
    expect(getDashboardLayout().hidden).toEqual([]);
  });

  it("accounts for every card in the catalogue exactly once", () => {
    const l = getDashboardLayout();
    const known = CARDS.map((c) => c.id).sort();
    expect([...l.pages.flat().map((c) => c.id), ...l.hidden].sort()).toEqual(known);
  });
});

describe("a layout written by an older release", () => {
  /**
   * The migration, and the reason it is a migration rather than a reconcile: a
   * v1 `{order, hidden}` is a valid object, so a v2 reader looking for `pages`
   * would fall through to the default and reset an arrangement the household
   * made, with nothing failing anywhere to say so.
   */
  it("keeps the arrangement a v1 release wrote", () => {
    store({ order: ["devices", "weather"], hidden: ["nowPlaying"] });
    const l = getDashboardLayout();
    expect(l.pages).toEqual([
      [
        { id: "devices", size: "l" },
        { id: "weather", size: "m" },
      ],
    ]);
    expect(l.hidden).toEqual(["nowPlaying"]);
    expect(l).not.toEqual(DEFAULT_LAYOUT);
  });

  /**
   * A stored layout outlives the release that wrote it. Five cards left the
   * catalogue in this change, and every one of them has to fall out of a stored
   * layout on read rather than needing a migration of its own.
   */
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

  /**
   * The other direction matters more. A card added since they last saved should
   * appear in the sheet as something they MAY turn on — arriving on their Home
   * unannounced is the behaviour that makes people stop trusting a layout they
   * arranged.
   */
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

  /** The per-list dedupe the flat model used would not have caught this one. */
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
  /**
   * The failure this guards is circular: a Home with no cards anywhere also has
   * no card to arrange, so the household is left with a blank track and nothing
   * to put back on it except by clearing storage.
   */
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
    // "nowPlaying" is alone on page 2; moving it up must not put it on page 1.
    moveCard("nowPlaying", -1);
    expect(ids()).toEqual([["weather", "devices"], ["nowPlaying"]]);
  });

  it("crosses a page only when asked to explicitly", () => {
    moveCardToPage("nowPlaying", 0);
    // Page 2 is left empty by the move, so it goes.
    expect(ids()).toEqual([["weather", "devices", "nowPlaying"]]);
  });

  /**
   * The page a card is SENT to and the page it LANDS on are two different
   * facts, and only the store holds the second one.
   *
   * Emptying the source page drops it, which shifts every page after it down
   * one — so a card sent to page 2 from a page 1 it was alone on ends up on
   * page 1. The caller follows the card with the return value; following its own
   * argument scrolls the track to a page the card is not on, which is the bug
   * this pair exists for.
   */
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
    expect(ids()).toEqual([["devices"], ["nowPlaying", "weather"]]);
  });

  it("reports nothing at all when the move is refused", () => {
    // Already on page 1, so there is no move and no page to follow it to.
    expect(moveCardToPage("weather", 0)).toBeNull();
    expect(ids()).toEqual([["weather", "devices"], ["nowPlaying"]]);
  });

  it("makes one new page beyond the last, up to the limit", () => {
    moveCardToPage("weather", 2);
    expect(ids()).toEqual([["devices"], ["nowPlaying"], ["weather"]]);
    // A fourth page is past MAX_PAGES, so the move is refused rather than
    // silently dropping the card.
    moveCardToPage("devices", 3);
    expect(ids()).toEqual([["devices"], ["nowPlaying"], ["weather"]]);
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

  /**
   * Storage that refuses to write should cost the preference, never the
   * interaction — a panel in a private window still rearranges, it just does
   * not remember.
   */
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
