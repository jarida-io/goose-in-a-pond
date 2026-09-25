import { beforeEach, describe, expect, it } from "vitest";
import {
  CARDS,
  DEFAULT_LAYOUT,
  __resetLayoutCache,
  getDashboardLayout,
  hideCard,
  moveCard,
  resetLayout,
  showCard,
  type CardId,
} from "./dashboardLayout";

const KEY = "giap-dashboard-layout";

function store(layout: unknown): void {
  localStorage.setItem(KEY, JSON.stringify(layout));
  __resetLayoutCache();
}

beforeEach(() => {
  localStorage.clear();
  __resetLayoutCache();
});

describe("the default", () => {
  it("is the pared-back Home and nothing more", () => {
    expect(getDashboardLayout().order).toEqual([
      "suggestion",
      "devices",
      "weather",
      "nowPlaying",
    ]);
  });

  it("offers everything else rather than discarding it", () => {
    const l = getDashboardLayout();
    const known = CARDS.map((c) => c.id).sort();
    expect([...l.order, ...l.hidden].sort()).toEqual(known);
  });
});

describe("a layout written by an older release", () => {
  it("drops cards this release no longer has", () => {
    store({ order: ["devices", "stockTicker"], hidden: [] });
    expect(getDashboardLayout().order).toEqual(["devices"]);
  });

  it("offers a card added since, without placing it on Home", () => {
    store({ order: ["devices"], hidden: [] });
    const l = getDashboardLayout();
    expect(l.order).toEqual(["devices"]);
    expect(l.hidden).toContain("weather");
    expect(l.hidden).toContain("scenes");
  });

  it("survives a corrupt payload rather than rendering nothing", () => {
    localStorage.setItem(KEY, "{not json");
    __resetLayoutCache();
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });

  it("ignores a card claimed as both shown and hidden", () => {
    store({ order: ["devices", "weather"], hidden: ["weather"] });
    const l = getDashboardLayout();
    expect(l.order).toEqual(["devices", "weather"]);
    expect(l.hidden).not.toContain("weather");
  });

  it("does not repeat a card listed twice", () => {
    store({ order: ["devices", "devices", "weather"], hidden: [] });
    expect(getDashboardLayout().order).toEqual(["devices", "weather"]);
  });
});

describe("an empty Home", () => {
  it("is not a preference the store will hold", () => {
    store({ order: [], hidden: CARDS.map((c) => c.id) });
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });

  it("cannot be reached by hiding the last card", () => {
    store({ order: ["devices"], hidden: [] });
    hideCard("devices");
    expect(getDashboardLayout().order).toEqual(["devices"]);
  });
});

describe("arranging", () => {
  it("adds a card at the end, where it can be seen to have arrived", () => {
    showCard("scenes");
    const l = getDashboardLayout();
    expect(l.order[l.order.length - 1]).toBe("scenes");
    expect(l.hidden).not.toContain("scenes");
  });

  it("returns a hidden card to the sheet rather than forgetting it", () => {
    hideCard("weather");
    const l = getDashboardLayout();
    expect(l.order).not.toContain("weather");
    expect(l.hidden).toContain("weather");
  });

  it("moves a card one place", () => {
    moveCard("devices", -1);
    expect(getDashboardLayout().order.slice(0, 2)).toEqual(["devices", "suggestion"]);
  });

  /** Moving past either end is a no-op, not a wrap. */
  it("will not move a card off either end", () => {
    const before = [...getDashboardLayout().order];
    moveCard(before[0] as CardId, -1);
    moveCard(before[before.length - 1] as CardId, 1);
    expect(getDashboardLayout().order).toEqual(before);
  });

  it("resets to the shipped Home", () => {
    showCard("cameras");
    hideCard("weather");
    resetLayout();
    expect(getDashboardLayout()).toEqual(DEFAULT_LAYOUT);
  });
});

describe("persistence", () => {
  it("survives a reload", () => {
    showCard("routines");
    __resetLayoutCache();
    expect(getDashboardLayout().order).toContain("routines");
  });

  it("still applies when storage refuses the write", () => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = () => {
      throw new Error("quota");
    };
    try {
      showCard("todos");
      expect(getDashboardLayout().order).toContain("todos");
    } finally {
      Storage.prototype.setItem = original;
    }
  });
});
