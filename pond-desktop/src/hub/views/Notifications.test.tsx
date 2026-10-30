// The feed holds real schedule runs and the Pond's household notices: no fixtures, so its count
// matches the bell's badge.

import { render, screen, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import "../../i18n";
import type { HouseholdNotice, ScheduleRunNotification } from "../../api/types";
import { centreUnreadCount } from "../../state/householdNotices";

const appState = {
  serverOnline: true,
  scheduleRuns: [] as ScheduleRunNotification[],
  householdNotices: [] as HouseholdNotice[],
  noticeFeedFailed: false,
};
const dispatch = vi.fn();
const markNotificationsRead = vi.fn();

vi.mock("../../state/useAppContext", () => ({
  useAppState: () => appState,
  useAppDispatch: () => dispatch,
}));

vi.mock("../../api/PondApiClient", () => ({
  api: { markNotificationsRead: (ids: string[]) => markNotificationsRead(ids) },
}));

import { NotificationsView } from "./Notifications";

function notice(over: Partial<HouseholdNotice> = {}): HouseholdNotice {
  return {
    id: "n1",
    category: "info",
    title: "Model downloaded",
    body: '"qwen3" finished downloading and is ready to use.',
    timestamp: new Date().toISOString(),
    data: { open: "models", kind: "model_downloaded" },
    read: false,
    ...over,
  };
}

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
  appState.householdNotices = [];
  appState.noticeFeedFailed = false;
  dispatch.mockReset();
  markNotificationsRead.mockReset();
  markNotificationsRead.mockResolvedValue({ marked: 0 });
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

  /** The bell's badge is `centreUnreadCount`; the header must count the same. */
  it("counts exactly the unread runs and notices the bell counts", () => {
    appState.scheduleRuns = [
      run({ id: "r1", read: false }),
      run({ id: "r2", read: true }),
    ];
    appState.householdNotices = [
      notice({ id: "n1" }),
      notice({ id: "n2", read: true }),
      // Folded into run r1's card, so neither listed nor counted twice.
      notice({ id: "n3", data: { open: "schedule", schedule_id: "s1", run_id: "r1" } }),
    ];
    render(<NotificationsView />);
    const unread = centreUnreadCount(appState.scheduleRuns, appState.householdNotices);
    expect(unread).toBe(2);
    expect(screen.getByText(`${unread} unread`)).toBeTruthy();
  });

  it("still shows the runs the pond actually has", () => {
    appState.scheduleRuns = [run({ scheduleName: "Morning briefing" })];
    render(<NotificationsView />);
    expect(screen.getByText("Morning briefing triggered")).toBeTruthy();
    expect(screen.queryByText("Nothing here yet")).toBeNull();
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

describe("the Pond's household notices", () => {
  it("lists every category with its title, body, time and styling", () => {
    appState.householdNotices = [
      notice({ id: "i", category: "info", title: "Model downloaded" }),
      notice({
        id: "a",
        category: "alert",
        title: "The Pond is running out of space",
        body: "Only 1 GB of 64 GB is free on the Pond's disk.",
      }),
      notice({
        id: "r",
        category: "action_required",
        title: "Remote access needs signing in",
        data: { open: "remote_access", kind: "remote_access_sign_in" },
      }),
    ];
    render(<NotificationsView />);

    const alert = screen.getByRole("article", { name: "The Pond is running out of space" });
    expect(alert.getAttribute("data-category")).toBe("alert");
    expect(alert.textContent).toContain("Alert");
    expect(alert.textContent).toContain("Only 1 GB of 64 GB is free on the Pond's disk.");
    expect(alert.textContent).toContain("just now");

    const action = screen.getByRole("article", { name: "Remote access needs signing in" });
    expect(action.getAttribute("data-category")).toBe("action_required");
    expect(action.textContent).toContain("Action needed");

    const info = screen.getByRole("article", { name: "Model downloaded" });
    expect(info.getAttribute("data-category")).toBe("info");
    expect(info.textContent).not.toContain("Info");
  });

  it("renders an unknown category as info", () => {
    appState.householdNotices = [notice({ category: "something-new", title: "Odd one" })];
    render(<NotificationsView />);
    expect(
      screen.getByRole("article", { name: "Odd one" }).getAttribute("data-category"),
    ).toBe("info");
  });

  it.each([
    ["schedule", "schedules", "Open schedules"],
    ["devices", "devices", "Open devices"],
    ["chat", "chat", "Open chat"],
    ["models", "models", "Open models"],
    ["extensions", "extensions", "Open extensions"],
    ["remote_access", "pairing", "Open remote access"],
  ])("an %s hint opens the %s section", (hint, section, label) => {
    const go = vi.fn();
    appState.householdNotices = [notice({ id: "n9", title: "T", data: { open: hint } })];
    render(<NotificationsView go={go} />);
    fireEvent.click(screen.getByText(label));
    expect(go).toHaveBeenCalledWith(section, undefined);
    expect(dispatch).toHaveBeenCalledWith({ type: "MARK_NOTICES_READ", payload: ["n9"] });
    expect(markNotificationsRead).toHaveBeenCalledWith(["n9"]);
  });

  it("offers no action for a notice with no hint, or one it does not know", () => {
    appState.householdNotices = [
      notice({ id: "x", title: "A rule fired", data: null }),
      notice({ id: "y", title: "From the future", data: { open: "garden" } }),
    ];
    render(<NotificationsView go={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /^Open / })).toBeNull();
  });

  it("keeps a schedule run's card, with View on Canvas, instead of its notice", () => {
    appState.scheduleRuns = [run({ id: "r1", scheduleName: "Daily Brief" })];
    appState.householdNotices = [
      notice({
        id: "sched-notice",
        title: "Schedule: Daily Brief",
        data: { open: "schedule", schedule_id: "s1", run_id: "r1" },
      }),
    ];
    render(<NotificationsView />);
    expect(screen.getByText("Daily Brief triggered")).toBeTruthy();
    expect(screen.getByText("View on Canvas")).toBeTruthy();
    expect(screen.queryByText("Schedule: Daily Brief")).toBeNull();
  });

  it("marks every run and notice read, here and on the Pond", () => {
    appState.scheduleRuns = [run({ id: "r1" })];
    appState.householdNotices = [
      notice({ id: "n1" }),
      notice({ id: "n2", read: true }),
      notice({ id: "n3", data: { open: "schedule", run_id: "r1" } }),
    ];
    render(<NotificationsView />);
    fireEvent.click(screen.getByText("Mark all read"));
    expect(dispatch).toHaveBeenCalledWith({ type: "MARK_ALL_RUNS_READ" });
    expect(dispatch).toHaveBeenCalledWith({
      type: "MARK_NOTICES_READ",
      payload: ["n1", "n3"],
    });
    expect(markNotificationsRead).toHaveBeenCalledWith(["n1", "n3"]);
  });

  it("says so when the read state could not be saved", async () => {
    markNotificationsRead.mockRejectedValue(new Error("offline"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    appState.householdNotices = [notice({ id: "n1" })];
    render(<NotificationsView />);
    fireEvent.click(screen.getByText("Mark all read"));
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("Could not save"),
    );
  });

  it("says so while the notices cannot be loaded", () => {
    appState.noticeFeedFailed = true;
    render(<NotificationsView />);
    expect(screen.getByRole("status").textContent).toContain("could not be loaded");
  });
});
