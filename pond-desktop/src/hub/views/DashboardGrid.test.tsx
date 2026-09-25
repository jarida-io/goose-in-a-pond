// Renders every card: only a render catches a child referencing something out of scope.

import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import { DashboardGrid } from "./DashboardGrid";
import { __resetLayoutCache, CARDS } from "../state/dashboardLayout";

function renderGrid() {
  return render(
    <DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />,
  );
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

  it("shows the greeting and the device section", () => {
    renderGrid();
    expect(screen.getByRole("heading", { level: 1 })).toBeTruthy();
    // "Devices" is also an arrange-sheet row and could be a room, so query the heading.
    expect(screen.getByRole("heading", { name: "Devices", level: 2 })).toBeTruthy();
  });

  it("says something about this house rather than nothing", () => {
    const { container } = renderGrid();
    expect(screen.queryByText("Nothing needs you right now.")).toBeNull();
    const line = container.querySelector(".dash__line");
    expect(line?.textContent?.trim().endsWith(".")).toBe(true);
  });
});

/** Hidden cards too: the default layout doesn't reach every branch. */
describe("every card in the catalogue", () => {
  it("mounts without throwing", () => {
    localStorage.setItem(
      "giap-dashboard-layout",
      JSON.stringify({ order: CARDS.map((c) => c.id), hidden: [] }),
    );
    __resetLayoutCache();
    expect(() => renderGrid()).not.toThrow();
  });
});

describe("the arrange button's name", () => {
  /** InkButton drops `aria-label`, so the text is the name: small-panel CSS must clip it, not hide it. */
  it("comes from text, since the aria-label is dropped", () => {
    const { container } = renderGrid();
    const btn = screen.getByRole("button", { name: /Arrange/ });
    expect(btn.getAttribute("aria-label")).toBeNull();
    expect(container.querySelector(".dash__btn-label")?.textContent).toBe("Arrange");
  });
});

describe("search", () => {
  it("narrows to what was typed and says so", () => {
    const { container } = renderGrid();
    const input = container.querySelector('input[type="search"]') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "kitchen" } });
    expect(screen.getByText(/Matching "kitchen"/i)).toBeTruthy();
  });

  it("puts the room filter away while searching", () => {
    const { container } = renderGrid();
    expect(container.querySelector(".ink-segmented")).toBeTruthy();
    const input = container.querySelector('input[type="search"]') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "lamp" } });
    expect(container.querySelector(".ink-segmented")).toBeNull();
  });

  it("says so plainly when nothing matches", () => {
    const { container } = renderGrid();
    const input = container.querySelector('input[type="search"]') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "zzzznope" } });
    expect(screen.getByText(/Nothing here matches that/i)).toBeTruthy();
  });
});

describe("arranging", () => {
  it("opens the sheet and lists what is on Home", () => {
    renderGrid();
    // The sheet shares the button's name, so ask for the button.
    fireEvent.click(screen.getByRole("button", { name: /Arrange/ }));
    expect(screen.getByRole("heading", { name: "On Home" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /Move .* up/ }).length).toBeGreaterThan(0);
  });

  it("disables the moves that would fall off an end", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: /Arrange/ }));
    const up = screen.getAllByRole("button", { name: /Move .* up/ })[0];
    expect((up as HTMLButtonElement).disabled).toBe(true);
  });
});


describe("cards that grow when they have something to say", () => {
  const base = {
    user: "Jerry",
    weather: {
      temp: 64, cond: "Partly cloudy", icon: "cloudSun", hi: 68, lo: 54,
      hum: 62, wind: 12, sunrise: "06:30", sunset: "19:10", forecast: [],
    },
    rooms: [], cameras: [], categories: [], scenes: [], todos: [],
    gooseSuggestions: [],
  };

  function mountWith(devices: unknown[], nowPlaying: Record<string, unknown>) {
    vi.resetModules();
    vi.doMock("../state/hubDataStore", async () => {
      const real = await vi.importActual<Record<string, unknown>>("../state/hubDataStore");
      return { ...real, useHomeData: () => ({ ...base, devices, nowPlaying }), useRoutines: () => [] };
    });
    return import("./DashboardGrid");
  }

  const silent = { track: "", artist: "", elapsed: 0, hue: 0, connected: false, playing: false };
  const lamp = { id: "l1", name: "Lamp", kind: "light", on: false, room: "Hall" };

  it("gives music the width only while something is playing", async () => {
    const { DashboardGrid: Grid } = await mountWith(
      [lamp, lamp, lamp, lamp, lamp, lamp],
      { ...silent, connected: true, playing: true, track: "Weightless" },
    );
    const { container } = render(<Grid sessionId="s" onNavigate={() => {}} onTalk={() => {}} />);
    expect(container.querySelector('[data-playing] ')).toBeTruthy();
    expect(container.querySelector('[data-playing]')?.className).toContain("dash__cell--wide");
    cleanup();
  });

  it("lets the weather spread when there is little else", async () => {
    const { DashboardGrid: Grid } = await mountWith([lamp], silent);
    const { container } = render(<Grid sessionId="s" onNavigate={() => {}} onTalk={() => {}} />);
    const wide = container.querySelectorAll(".dash__cell--wide");
    // Suggestion, devices AND weather — three, where a furnished house has two.
    expect(wide.length).toBeGreaterThanOrEqual(3);
    cleanup();
  });
});
