// ────────────────────────────────────────────────────────────
// The notification feed, and what it is allowed to remember.
//
// Seven fixtures used to be concatenated onto the live list unconditionally --
// no environment gate, no offline branch -- so a household opening the bell saw
// a front door "unlocked remotely at 8:14 AM", a driveway camera and a garage
// door left open, presented as their own history under an "Earlier" heading.
// Three of them were unread, which put "3 unread" in the header one tap after a
// bell that showed no badge at all, because the badge counts real schedule
// runs. The "Nothing here yet" empty state was unreachable in every
// configuration this app could be in.
// ────────────────────────────────────────────────────────────

import { render, screen, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import type { ScheduleRunNotification } from "../../api/types";

const appState = {
  serverOnline: true,
  scheduleRuns: [] as ScheduleRunNotification[],
};

vi.mock("../../state/AppContext", () => ({
  useAppState: () => appState,
}));

import { NotificationsView } from "./Notifications";

function run(over: Partial<ScheduleRunNotification> = {}): ScheduleRunNotification {
  return {
    id: "r1",
    scheduleId: "s1",
    scheduleName: "Morning briefing",
    status: "completed",
    result: "Read the news",
    error: null,
    startedAt: new Date().toISOString(),
    finishedAt: new Date().toISOString(),
    read: false,
    ...over,
  } as ScheduleRunNotification;
}

beforeEach(() => {
  cleanup();
  appState.serverOnline = true;
  appState.scheduleRuns = [];
});

describe("what the feed will not remember", () => {
  it("shows the empty state on a pond that has raised nothing", () => {
    render(<NotificationsView />);
    expect(screen.getByText("Nothing here yet")).toBeTruthy();
    expect(screen.getByText("You're all caught up")).toBeTruthy();
  });

  /** The three that made the header disagree with the bell, by name. */
  it("invents no security, camera or battery events", () => {
    render(<NotificationsView />);
    expect(screen.queryByText("Front door unlocked")).toBeNull();
    expect(screen.queryByText("Garage door left open")).toBeNull();
    expect(screen.queryByText("Driveway — motion detected")).toBeNull();
    expect(screen.queryByText("Bedroom sensor — 12%")).toBeNull();
    expect(screen.queryByText("Earlier")).toBeNull();
  });

  /**
   * The header's count and the shell bar's badge read the same list now. The
   * badge is `unreadRunCount` over `scheduleRuns`; anything else in this feed
   * makes the two disagree in the same app at the same moment.
   */
  it("counts exactly the unread runs the bell counts", () => {
    appState.scheduleRuns = [
      run({ id: "r1", read: false }),
      run({ id: "r2", read: true }),
    ];
    render(<NotificationsView />);
    const unread = appState.scheduleRuns.filter((r) => !r.read).length;
    expect(screen.getByText(`${unread} unread`)).toBeTruthy();
  });

  it("still shows the runs the pond actually has", () => {
    appState.scheduleRuns = [run({ scheduleName: "Morning briefing" })];
    render(<NotificationsView />);
    expect(screen.getByText("Morning briefing triggered")).toBeTruthy();
    expect(screen.queryByText("Nothing here yet")).toBeNull();
  });

  /** Offline is empty, not furnished. */
  it("stays empty when the server is unreachable", () => {
    appState.serverOnline = false;
    render(<NotificationsView />);
    expect(screen.getByText("Nothing here yet")).toBeTruthy();
    expect(
      screen.getByText("Connect to Goose server to see schedule and routine history"),
    ).toBeTruthy();
  });
});
