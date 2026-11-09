// The drawer is live from first paint, so it's tested in the pre-load store state as well as loaded.

import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";

const home = {
  rooms: [] as { id: string; name: string; icon: string }[],
  devicesAreReal: false,
};
let routines: { id: string; name: string; iconPath: string }[] = [];

vi.mock("./state/hubDataStore", () => ({
  useHomeData: () => home,
  useRoutines: () => routines,
}));

import { HubDrawer } from "./HubDrawer";

function open() {
  return render(
    <HubDrawer open onClose={() => {}} active="dashboard" onNavigate={() => {}} />,
  );
}

const DEMO_ROOMS = [
  { id: "home",    name: "Home",        icon: "home" },
  { id: "living",  name: "Living Room", icon: "sofa" },
  { id: "kitchen", name: "Kitchen",     icon: "utensils" },
];

beforeEach(() => {
  cleanup();
  home.rooms = [];
  home.devicesAreReal = false;
  routines = [];
});

describe("rooms", () => {
  /** The seed is empty, but the drawer must still honour `devicesAreReal`, as DashboardGrid does. */
  it("lists no rooms before the load has landed", () => {
    home.rooms = DEMO_ROOMS;
    home.devicesAreReal = false;
    open();
    expect(screen.queryByText("Rooms")).toBeNull();
    expect(screen.queryByText("Living Room")).toBeNull();
    expect(screen.queryByText("Kitchen")).toBeNull();
  });

  it("lists the rooms once they are the household's own", () => {
    home.rooms = DEMO_ROOMS;
    home.devicesAreReal = true;
    open();
    expect(screen.getByText("Rooms")).toBeTruthy();
    expect(screen.getByText("Living Room")).toBeTruthy();
  });

  it("says nothing about rooms a pond with no devices does not have", () => {
    home.rooms = [];
    home.devicesAreReal = true;
    open();
    expect(screen.queryByText("Rooms")).toBeNull();
  });
});

describe("quick routines", () => {
  it("offers no routines on a pond with no recipes", () => {
    routines = [];
    open();
    expect(screen.queryByText("Quick routines")).toBeNull();
    expect(screen.queryByText("Good Morning")).toBeNull();
    expect(screen.queryByText("Movie Time")).toBeNull();
  });

  it("offers the household's own recipes", () => {
    routines = [{ id: "Sunset Bath", name: "Sunset Bath", iconPath: "M0 0" }];
    open();
    expect(screen.getByText("Quick routines")).toBeTruthy();
    expect(screen.getByText("Sunset Bath")).toBeTruthy();
  });
});

describe("the pond group", () => {
  it("reaches what the pond keeps for the member: saved cards and web watches", () => {
    const onNavigate = vi.fn();
    render(<HubDrawer open onClose={() => {}} active="watches" onNavigate={onNavigate} />);
    fireEvent.click(screen.getByRole("button", { name: "Saved" }));
    expect(onNavigate).toHaveBeenCalledWith({ kind: "section", section: "saved" });
    fireEvent.click(screen.getByRole("button", { name: "Watches" }));
    expect(onNavigate).toHaveBeenCalledWith({ kind: "section", section: "watches" });
  });
});
