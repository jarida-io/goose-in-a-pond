// Sections from Home resolve like the drawer's, through `navigate`. DashboardGrid is stubbed:
// the subject is the shell, not the screen.

import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";

const dispatch = vi.fn();

vi.mock("../state/useAppContext", () => ({
  useAppState: () => ({ sessionId: "s1", unreadRunCount: 0, serverOnline: true, scheduleRuns: [], householdNotices: [] }),
  useAppDispatch: () => dispatch,
}));

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
  it("hands a section with no hub screen to the classic shell", () => {
    render(<Hub />);
    fireEvent.click(screen.getByText("Add your first device"));

    expect(dispatch).toHaveBeenCalledWith({ type: "SET_SECTION", payload: "devices" });
  });

  it("does not swallow the tap and redraw Home", () => {
    render(<Hub />);
    fireEvent.click(screen.getByText("Add your first device"));

    expect(localStorage.getItem("goosehub_route")).not.toBe("devices");
  });

  it("keeps a section with a hub screen inside the hub", () => {
    render(<Hub />);
    fireEvent.click(screen.getByText("Open Settings"));

    expect(dispatch).not.toHaveBeenCalledWith({ type: "SET_SECTION", payload: "settings" });
    expect(localStorage.getItem("goosehub_route")).toBe("settings");
  });
});
