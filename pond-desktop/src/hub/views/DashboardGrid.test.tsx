// The screen renders, at all of it.
//
// This file exists because it should have existed sooner. `DashboardGrid` was
// covered only by tests of the store beneath it, so a reference to `home` from
// inside a child component — where it was never in scope — typechecked, passed
// 603 unit tests, and put "home is not defined" on the panel. A render test is
// the only thing that catches that class of mistake, and every card has to be
// rendered for it to count.
//
// Three describes went away with the features they pinned, and are named here
// rather than silently dropped:
//
//   SEARCH (3 tests)  Search and the room filter are gone from Home. They
//                     existed to reach a device kept off Home on purpose, and
//                     that reach is now the Devices screen the empty state
//                     already points at.
//   GROWING CARDS (2) `SPARSE_LIMIT` and the `dash__cell--wide` ternaries are
//                     gone. A card's width is the household's now, held in the
//                     layout store as a size, and covered by that store's own
//                     tests rather than inferred from a class name here.

import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { DashboardGrid } from "./DashboardGrid";
import {
  __resetLayoutCache,
  CARDS,
  type CardId,
  type CardSize,
  type PlacedCard,
} from "../state/dashboardLayout";

function renderGrid() {
  return render(<DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />);
}

/** Seed the household's arrangement, one array per page. */
function storePages(pages: { id: CardId; size: CardSize }[][]): void {
  localStorage.setItem(
    "giap-dashboard-layout",
    JSON.stringify({ version: 2, pages, hidden: [] as CardId[] } satisfies {
      version: 2;
      pages: PlacedCard[][];
      hidden: CardId[];
    }),
  );
  __resetLayoutCache();
}

/** Put every card on one page at one size, the way the catalogue test needs it. */
function storeEveryCardAt(size: CardSize): void {
  localStorage.setItem(
    "giap-dashboard-layout",
    JSON.stringify({
      version: 2,
      pages: [CARDS.map((c) => ({ id: c.id, size }))],
      hidden: [],
    }),
  );
  __resetLayoutCache();
}

beforeEach(() => {
  // This repo does not configure Testing Library's automatic cleanup, so
  // renders otherwise accumulate in the document and a second `getByRole`
  // finds the first test's markup as well as this one's.
  cleanup();
  localStorage.clear();
  __resetLayoutCache();
});

describe("the default screen", () => {
  it("renders without throwing", () => {
    expect(() => renderGrid()).not.toThrow();
  });

  /**
   * The devices card has no heading of its own — it carries a title span, not
   * an h2 — so the stable handle is the hook attribute the card puts on its
   * root. That is also what the E2E suite selects on.
   */
  it("puts the device card on the screen", () => {
    const { container } = renderGrid();
    expect(container.querySelector('[data-hook="home-controls"]')).toBeTruthy();
  });

  /**
   * The line that replaced "Nothing needs you right now". It is a sentence
   * about THIS house, so the only stable assertion is that it is a sentence
   * and that the old wallpaper is gone. It lives in the suggestion column's
   * quiet slot now, which is where a household with nothing waiting looks.
   */
  it("says something about this house rather than nothing", () => {
    const { container } = renderGrid();
    expect(screen.queryByText("Nothing needs you right now.")).toBeNull();
    const line = container.querySelector(".sq__quiet");
    expect(line?.textContent?.trim().endsWith(".")).toBe(true);
  });

  it("offers both ways of answering it", () => {
    renderGrid();
    expect(screen.getByRole("button", { name: "Talk to Goose" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Type to Goose" })).toBeTruthy();
  });
});

/**
 * Every card, mounted, at every size it can be given.
 *
 * The bug this file was written for lived in ONE branch of a switch. Rendering
 * the default layout would not have found it; rendering all of them does. Size
 * is now a second axis through the same switch, so it is walked too — a card
 * that only crashes at "l" is a card nobody would see crash until a household
 * resized it.
 */
describe("every card in the catalogue", () => {
  it.each(["s", "m", "l"] as const)("mounts at size %s without throwing", (size) => {
    storeEveryCardAt(size);
    expect(() => renderGrid()).not.toThrow();
  });
});

describe("the arrange control's name", () => {
  /**
   * The strip's pill is a plain button whose visible text IS its accessible
   * name — no `aria-label` anywhere, so nothing can drift between the two. The
   * narrow-panel rule may clip that text but must never remove it, or every
   * panel under 640px gets an unlabelled icon button.
   */
  it("comes from its own text", () => {
    renderGrid();
    const btn = screen.getByRole("button", { name: "Arrange" });
    expect(btn.getAttribute("aria-label")).toBeNull();
  });
});

describe("arranging", () => {
  it("opens the sheet and lists the pages", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    expect(screen.getByRole("heading", { name: "Page 1" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Page 2" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /Move .* up/ }).length).toBeGreaterThan(0);
  });

  /** Crossing a page is its own act, with its own name, on every row. */
  it("names the page a card would move to", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    expect(screen.getByRole("button", { name: "Move Weather to page 2" })).toBeTruthy();
  });

  /**
   * The first card on a page cannot move up, the last cannot move down.
   *
   * There are two of each control while arranging — the widget's own toolbar
   * and the sheet's row — and they share a name because they are the same act.
   * Both have to agree about the end of the page, so both are asserted.
   */
  it("disables the moves that would fall off an end", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    const up = screen.getAllByRole("button", { name: "Move Weather up" }) as HTMLButtonElement[];
    expect(up.length).toBe(2);
    expect(up.every((b) => b.disabled)).toBe(true);
  });

  /** The toolbars come up with the sheet, on every framed widget. */
  it("turns the frames' own controls on", () => {
    renderGrid();
    expect(screen.queryByRole("button", { name: "Remove Weather from Home" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    expect(screen.getAllByRole("button", { name: "Remove Weather from Home" }).length)
      .toBeGreaterThan(0);
  });

  /**
   * The same parity, on the one act it was missing.
   *
   * The store refuses to hide the last card left anywhere — an empty Home has
   * no card to arrange and so no route back to the sheet — and the sheet's own
   * remove was already disabled for it. The frame's x was not: it rendered lit,
   * 44px and focusable, called a function that returned without writing, and
   * told a screen reader the opposite of what the button beside it said.
   */
  it("disables both removes when one card is all that is left", () => {
    storePages([[{ id: "nowPlaying", size: "s" }]]);
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    const remove = screen.getAllByRole("button", {
      name: "Remove Music from Home",
    }) as HTMLButtonElement[];
    expect(remove.length).toBe(2);
    expect(remove.every((b) => b.disabled)).toBe(true);
  });

  /**
   * The track follows the card, not the button that was pressed.
   *
   * Moving the only card off page 1 empties it, and an empty page is dropped —
   * so the card sent to page 2 is on page 1 by the time the track is told where
   * to go. Handing the track the page that was ASKED for parks it on a page the
   * card is not on, and nothing resets that when the sheet closes.
   */
  it("shows the page a moved card landed on, not the one it was sent to", () => {
    storePages([
      [{ id: "weather", size: "m" }],
      [{ id: "nowPlaying", size: "s" }],
      [{ id: "devices", size: "l" }],
    ]);
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    fireEvent.click(screen.getByRole("button", { name: "Move Weather to page 2" }));

    // Weather is now first-page furniture: [[nowPlaying, weather], [devices]].
    const dots = screen.getAllByRole("button", { name: /^Page \d+ of \d+$/ });
    expect(dots.length).toBe(2);
    expect(dots[0].getAttribute("aria-current")).toBe("true");
  });
});

/**
 * Two rules that decide what the household sees, asserted as source text.
 *
 * Structural guards rather than rendered ones, the same shape as
 * `styles/focus-ring.test.ts`: jsdom applies no external stylesheet, so there is
 * no computed height and no computed border to measure here. Both defects below
 * were invisible to every test in this file because both are one declaration in
 * a file nothing reads.
 */
describe("the arrange chrome", () => {
  const HERE = dirname(fileURLToPath(import.meta.url));
  const FRAME_CSS = readFileSync(join(HERE, "widgets/widget-frame.css"), "utf8");
  const GRID_CSS = readFileSync(join(HERE, "dashboard-grid.css"), "utf8");

  /** Selector and body of every rule, comments dropped so none rides along. */
  function rules(css: string): { selector: string; body: string }[] {
    return [...css.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(
      (m) => ({ selector: m[1].trim(), body: m[2] }),
    );
  }

  function bodyOf(css: string, selector: string): string {
    const found = rules(css).find((r) => r.selector === selector);
    if (found === undefined) throw new Error(`${selector} is gone from the stylesheet`);
    return found.body;
  }

  /**
   * `.wframe` is border-box, so a min-height on the FRAME is a height the
   * toolbar lane spends. Entering arrange mode swaps 5px of padding for
   * `48px 6px 6px`, and every card shorter than its own size lost those 44px:
   * size l fell from 266px to 222px, size m from 182px to 138px, so the S/M/L
   * control previewed heights it was about to change. Measured on the body, the
   * lane is additive — which is what the design does, its frames carrying the
   * lane's padding and no height constraint at all.
   */
  it("adds the toolbar lane above the card rather than out of it", () => {
    expect(bodyOf(FRAME_CSS, ".wframe[data-arranging]")).toMatch(/padding:\s*48px/);

    const sized = rules(FRAME_CSS).filter((r) => /(^|;)\s*min-height\s*:/.test(r.body));
    expect(sized.length).toBeGreaterThan(0);
    for (const { selector } of sized) {
      // `.wframe-add` is a bare button the page column holds, not a framed
      // widget, so it carries no toolbar and owns its own height.
      expect(
        selector.includes(".wframe__body") || selector === ".wframe-add",
        `${selector} pins a height on the box the arrange padding is taken out of`,
      ).toBe(true);
    }

    // Still three sizes, and still these three heights: dropping the min-height
    // rather than moving it collapses size l to about 208px and makes it worse.
    for (const height of ["132px", "196px", "280px"]) {
      expect(FRAME_CSS).toMatch(new RegExp(`\\.wframe__body\\s*\\{[^}]*min-height:\\s*${height}`));
    }
  });

  /**
   * A 2px dashed rounded rect means exactly one thing in this build — a widget
   * frame in arrange mode — and the weather-off slot was drawn with the same
   * width, style and token. A pond with weather off therefore showed arrange
   * chrome on a screen that was not arranging, and with Arrange on the slot's
   * rect and the frame's rect around it were the same border 6px apart.
   */
  it("is not what a pond with weather off is wearing", () => {
    expect(bodyOf(FRAME_CSS, ".wframe")).toMatch(/border:\s*2px dashed/);
    expect(bodyOf(GRID_CSS, ".dash__gap")).not.toMatch(/dashed/);
  });
});

/**
 * Paging.
 *
 * jsdom has no layout: `clientWidth` is 0 and `scrollTo` is a stub, so
 * `scrollLeft` says nothing about which page is showing. The dot's
 * `aria-current` is the component's own report of the page it settled on, and
 * it is the only honest thing to assert here.
 */
describe("the pages", () => {
  it("has one dot per page, and moves the current one when tapped", () => {
    renderGrid();
    const dots = screen.getAllByRole("button", { name: /^Page \d+ of \d+$/ });
    expect(dots.length).toBe(2);
    expect(dots[0].getAttribute("aria-current")).toBe("true");

    fireEvent.click(dots[1]);
    expect(dots[1].getAttribute("aria-current")).toBe("true");
    expect(dots[0].getAttribute("aria-current")).toBeNull();
  });
});

/**
 * Asserting nothing the pond does not know (DESIGN.md §3).
 *
 * Both of these used to render a plausible lie: a mock 64° / Partly cloudy on
 * any pond with no location, and "Weightless / Marconi Union" with a working-
 * looking play button on any pond with no Spotify — which is the state every
 * fresh install is in.
 */
describe("what the screen will not claim", () => {
  const base = {
    user: "Jerry",
    weather: {
      temp: 0, cond: "", icon: "", hi: 0, lo: 0,
      hum: 0, wind: 0, sunrise: "", sunset: "", forecast: [],
    },
    rooms: [], devices: [], cameras: [], categories: [], scenes: [],
    gooseSuggestions: [],
    weatherEnabled: false,
    devicesAreReal: true,
  };

  const silent = {
    track: "", artist: "", elapsed: 0, hue: 0,
    connected: false, playing: false, progressMs: null, durationMs: null,
  };

  async function mountWith(home: Record<string, unknown>) {
    vi.resetModules();
    vi.doMock("../state/hubDataStore", async () => {
      const real = await vi.importActual<Record<string, unknown>>("../state/hubDataStore");
      return { ...real, useHomeData: () => home, useRoutines: () => [] };
    });
    return import("./DashboardGrid");
  }

  it("draws no weather, and no temperature, when there is no location", async () => {
    const { DashboardGrid: Grid } = await mountWith({ ...base, nowPlaying: silent });
    const { container } = render(<Grid sessionId="s" onNavigate={() => {}} onTalk={() => {}} />);
    // No sky card at all, and the strip's temperature slot is absent rather
    // than showing a zero.
    expect(container.querySelector(".wx")).toBeNull();
    expect(container.querySelector(".hbar__temp")).toBeNull();
    expect(screen.getByText("Set your location to see weather")).toBeTruthy();
    cleanup();
  });

  it("offers no transport for a music service that is not connected", async () => {
    const { DashboardGrid: Grid } = await mountWith({ ...base, nowPlaying: silent });
    render(<Grid sessionId="s" onNavigate={() => {}} onTalk={() => {}} />);
    expect(screen.queryByRole("button", { name: "Play" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Next" })).toBeNull();
    expect(screen.getByRole("button", { name: "Connect in Settings" })).toBeTruthy();
    expect(screen.queryByText("Weightless")).toBeNull();
    cleanup();
  });
});
