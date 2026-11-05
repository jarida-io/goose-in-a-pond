// The feed holds only real schedule runs: no fixtures, so its count matches the bell's badge.

import { render, screen, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import type { ScheduleRunNotification } from "../../api/types";

const appState = {
  serverOnline: true,
  scheduleRuns: [] as ScheduleRunNotification[],
};

vi.mock("../../state/useAppContext", () => ({
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

  it("invents no security, camera or battery events", () => {
    render(<NotificationsView />);
    expect(screen.queryByText("Front door unlocked")).toBeNull();
    expect(screen.queryByText("Garage door left open")).toBeNull();
    expect(screen.queryByText("Driveway — motion detected")).toBeNull();
    expect(screen.queryByText("Bedroom sensor — 12%")).toBeNull();
    expect(screen.queryByText("Earlier")).toBeNull();
  });

  /** The bell's badge is `unreadRunCount` over `scheduleRuns`; the header must count the same. */
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

  it("previews a run that wrote markdown as plain text", () => {
    appState.scheduleRuns = [run({ result: "### Briefing\n\n*   **Exchange Rate:** 129 KES" })];
    render(<NotificationsView />);
    expect(screen.getByText("Briefing Exchange Rate: 129 KES")).toBeTruthy();
  });

  it("stays empty when the server is unreachable", () => {
    appState.serverOnline = false;
    render(<NotificationsView />);
    expect(screen.getByText("Nothing here yet")).toBeTruthy();
    expect(
      screen.getByText("Connect to Goose server to see schedule and routine history"),
    ).toBeTruthy();
  });
});
