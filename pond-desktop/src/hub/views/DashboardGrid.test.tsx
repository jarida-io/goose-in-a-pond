// Renders every card: only a render catches a child referencing something out of scope.

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

/** Every card on one page at one size. */
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
  // Testing Library's automatic cleanup isn't configured here, so renders would accumulate.
  cleanup();
  localStorage.clear();
  __resetLayoutCache();
});

describe("the default screen", () => {
  it("renders without throwing", () => {
    expect(() => renderGrid()).not.toThrow();
  });

  /** Found by its hook attribute: the card has no heading, and the E2E suite selects on the same. */
  it("puts the device card on the screen", () => {
    const { container } = renderGrid();
    expect(container.querySelector('[data-hook="home-controls"]')).toBeTruthy();
  });

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

/** Hidden cards too: the default layout doesn't reach every branch. */
describe("every card in the catalogue", () => {
  it.each(["s", "m", "l"] as const)("mounts at size %s without throwing", (size) => {
    storeEveryCardAt(size);
    expect(() => renderGrid()).not.toThrow();
  });
});

describe("the arrange control's name", () => {
  /** The narrow-panel rule may clip the pill's text but not remove it: the text is its only name. */
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

  it("names the page a card would move to", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    expect(screen.getByRole("button", { name: "Move Weather to page 2" })).toBeTruthy();
  });

  /** The frame's toolbar and the sheet's row share each name, so both copies are asserted. */
  it("disables the moves that would fall off an end", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    const up = screen.getAllByRole("button", { name: "Move Weather up" }) as HTMLButtonElement[];
    expect(up.length).toBe(2);
    expect(up.every((b) => b.disabled)).toBe(true);
  });

  it("turns the frames' own controls on", () => {
    renderGrid();
    expect(screen.queryByRole("button", { name: "Remove Weather from Home" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Arrange" }));
    expect(screen.getAllByRole("button", { name: "Remove Weather from Home" }).length)
      .toBeGreaterThan(0);
  });

  /** The last card can't be hidden (an empty Home has no way back to the sheet); both removes must say so. */
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

  /** Moving the only card off page 1 drops that page, so "page 2" is page 1 by the time the track moves. */
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

/** Asserted as CSS source text: jsdom applies no external stylesheet, so nothing can be measured. */
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

  /** `.wframe` is border-box, so min-heights belong on the body or the arrange padding eats them. */
  it("adds the toolbar lane above the card rather than out of it", () => {
    // In flow, so a toolbar that wraps to two rows pushes the card down instead of covering it.
    expect(bodyOf(FRAME_CSS, ".wframe")).toMatch(/flex-direction:\s*column/);
    expect(bodyOf(FRAME_CSS, ".wframe__bar")).not.toMatch(/position:\s*absolute/);
    // Nothing lets a card shrink below its content while that content stays painted.
    expect(bodyOf(FRAME_CSS, ".wframe__body > *")).not.toMatch(/min-height:\s*0/);

    const sized = rules(FRAME_CSS).filter((r) => /(^|;)\s*min-height\s*:/.test(r.body));
    expect(sized.length).toBeGreaterThan(0);
    for (const { selector } of sized) {
      // `.wframe-add` is a bare button, not a framed widget: no toolbar, so it may own its height.
      expect(
        selector.includes(".wframe__body") || selector === ".wframe-add",
        `${selector} pins a height on the box the arrange padding is taken out of`,
      ).toBe(true);
    }

    // Moved, not dropped: without a min-height size l collapses to about 208px.
    for (const height of ["132px", "196px", "280px"]) {
      expect(FRAME_CSS).toMatch(new RegExp(`\\.wframe__body\\s*\\{[^}]*min-height:\\s*${height}`));
    }
  });

  /** A 2px dashed border means arrange mode, so the weather-off slot must not wear one. */
  it("is not what a pond with weather off is wearing", () => {
    expect(bodyOf(FRAME_CSS, ".wframe")).toMatch(/border:\s*2px dashed/);
    expect(bodyOf(GRID_CSS, ".dash__gap")).not.toMatch(/dashed/);
  });
});

/** jsdom has no layout, so the dots' `aria-current` is the only record of the page shown. */
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

/** Nothing the pond doesn't know: no mock weather or track on a fresh install (DESIGN.md §3). */
describe("what the screen will not claim", () => {
  const base = {
    user: "Jerry",
    weather: {
      temp: 0, cond: "", icon: "", hi: 0, lo: 0,
      hum: 0, wind: 0, sunrise: "", sunset: "", forecast: [],
    },
    rooms: [], devices: [], cameras: [], categories: [], scenes: [],
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
