// ────────────────────────────────────────────────────────────
// One way a GuiSection is resolved in this shell.
//
// The hub kept two paths for the same question. The drawer's went through
// `navigate`, which consults HUB_ROUTE_FOR and hands anything with no hub
// screen to the classic shell -- the documented contract at the top of Hub.tsx.
// Home's went straight to `go`, which speaks hub routes only, so "Add your
// first device" set the route to "devices", matched nothing, fell through
// renderView's fallback and re-rendered Home. The call to action re-drew the
// screen it was asking you to leave, marked Settings in the drawer, and
// persisted the dead route to localStorage for the next launch.
//
// DashboardGrid is stubbed here on purpose: the subject is what the shell does
// with a section, not what the screen looks like. The screen's own tests cover
// the button.
// ────────────────────────────────────────────────────────────

import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";

const dispatch = vi.fn();

vi.mock("../state/AppContext", () => ({
  useAppState: () => ({ sessionId: "s1", unreadRunCount: 0, serverOnline: true, scheduleRuns: [] }),
  useAppDispatch: () => dispatch,
}));

// The real grid pulls in every widget, the api client and the layout store. All
// this test needs from it is the one thing it hands upward.
vi.mock("./views/DashboardGrid", () => ({
  DashboardGrid: ({ onNavigate }: { onNavigate: (s: string) => void }) => (
    <div data-hook="grid-stub">
      <button type="button" onClick={() => onNavigate("devices")}>Add your first device</button>
      <button type="button" onClick={() => onNavigate("settings")}>Open Settings</button>
    </div>
  ),
}));

vi.mock("./overlays/HubOverlay", () => ({ HubOverlay: () => null }));

import { Hub } from "./Hub";

beforeEach(() => {
  cleanup();
  dispatch.mockClear();
  localStorage.setItem("goosehub_route", "home");
});

describe("in-content navigation out of Home", () => {
  /**
   * `devices` has no hub screen, which is not an error: it belongs to the
   * classic shell, and SET_SECTION is how the hub hands it over.
   */
  it("hands a section with no hub screen to the classic shell", () => {
    render(<Hub />);
    fireEvent.click(screen.getByText("Add your first device"));

    expect(dispatch).toHaveBeenCalledWith({ type: "SET_SECTION", payload: "devices" });
  });

  /** And it does not silently re-render Home, or persist a route nothing serves. */
  it("does not swallow the tap and redraw Home", () => {
    render(<Hub />);
    fireEvent.click(screen.getByText("Add your first device"));

    expect(localStorage.getItem("goosehub_route")).not.toBe("devices");
  });

  /**
   * A section that DOES have a hub screen still stays in the hub. `settings`
   * only ever worked because the section id and the route id are the same
   * string; now it works because the table says so.
   */
  it("keeps a section with a hub screen inside the hub", () => {
    render(<Hub />);
    fireEvent.click(screen.getByText("Open Settings"));

    expect(dispatch).not.toHaveBeenCalledWith({ type: "SET_SECTION", payload: "settings" });
    expect(localStorage.getItem("goosehub_route")).toBe("settings");
  });
});
