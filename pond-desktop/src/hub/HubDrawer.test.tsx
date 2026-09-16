// ────────────────────────────────────────────────────────────
// What the drawer will say about a house it has not heard from.
//
// The drawer is live from first paint: both shells render it unconditionally
// and the hamburger works before a single request has settled. So everything it
// reads out of the store is read in the pre-load state as well as the loaded
// one, and this file pins both.
//
// Two blocks used to be fabricated. Rooms came from the store's seed, which was
// the demo house, so the drawer listed Living Room, Kitchen, Bedroom, Office
// and Outdoor to a household that owned none of them. Routines came from an
// empty-recipe-list fallback, so the same drawer offered Good Morning, Good
// Night, Movie Time, Away and Focus, each with an Open control that led to a
// screen offering to run them.
// ────────────────────────────────────────────────────────────

import { render, screen, cleanup } from "@testing-library/react";
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
  /**
   * The store's seed is empty now, but the flag is the belt to that braces:
   * `devicesAreReal` is the one answer to "did this come off the wire", and the
   * drawer is a consumer of it exactly as DashboardGrid is.
   */
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

  /** A pond with nothing paired has one room, and the block stays away. */
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
