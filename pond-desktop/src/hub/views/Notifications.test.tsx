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

    const alert = screen.getByRole("article", { name: "The Pond is running out of space, unread" });
    expect(alert.getAttribute("data-category")).toBe("alert");
    expect(alert.textContent).toContain("Alert");
    expect(alert.textContent).toContain("Only 1 GB of 64 GB is free on the Pond's disk.");
    expect(alert.textContent).toContain("just now");

    const action = screen.getByRole("article", { name: "Remote access needs signing in, unread" });
    expect(action.getAttribute("data-category")).toBe("action_required");
    expect(action.textContent).toContain("Action needed");

    const info = screen.getByRole("article", { name: "Model downloaded, unread" });
    expect(info.getAttribute("data-category")).toBe("info");
    expect(info.textContent).not.toContain("Info");
  });

  it("renders an unknown category as info", () => {
    appState.householdNotices = [notice({ category: "something-new", title: "Odd one" })];
    render(<NotificationsView />);
    expect(
      screen.getByRole("article", { name: "Odd one, unread" }).getAttribute("data-category"),
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

  it("asks for the conversation a task-done notice names, then opens chat", () => {
    const go = vi.fn();
    appState.householdNotices = [
      notice({
        id: "long-task-t1",
        title: "Task done: How Embassies work?",
        data: { open: "chat", long_task_id: "t1", session_id: "sess-embassies" },
      }),
    ];
    render(<NotificationsView go={go} />);
    fireEvent.click(screen.getByText("Open chat"));
    expect(dispatch).toHaveBeenCalledWith({ type: "REQUEST_CONVERSATION", payload: "sess-embassies" });
    expect(go).toHaveBeenCalledWith("chat", undefined);
  });

  it("asks for the watch a watch notice names, then opens the watches screen", () => {
    const go = vi.fn();
    appState.householdNotices = [
      notice({ id: "w", title: "Price down: Subaru Forester", data: { open: "watch", watch_id: "watch-7" } }),
    ];
    render(<NotificationsView go={go} />);
    fireEvent.click(screen.getByText("Open the watch"));
    expect(dispatch).toHaveBeenCalledWith({ type: "REQUEST_WATCH", payload: "watch-7" });
    expect(go).toHaveBeenCalledWith("watches", undefined);
  });

  it("opens chat alone for a malformed conversation id", () => {
    const go = vi.fn();
    appState.householdNotices = [
      notice({ id: "p", title: "A suggestion", data: { open: "chat", session_id: "../settings" } }),
    ];
    render(<NotificationsView go={go} />);
    fireEvent.click(screen.getByText("Open chat"));
    expect(dispatch).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "REQUEST_CONVERSATION" }),
    );
    expect(go).toHaveBeenCalledWith("chat", undefined);
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

  it("says unread by a dot and a heavier row, never a coloured edge, and says it to a screen reader", () => {
    appState.householdNotices = [
      notice({ id: "u", title: "Unread one" }),
      notice({ id: "r", title: "Read one", read: true, timestamp: new Date(Date.now() - 60_000).toISOString() }),
    ];
    render(<NotificationsView />);
    const unread = screen.getByRole("article", { name: "Unread one, unread" });
    const read = screen.getByRole("article", { name: "Read one" });
    expect(unread.getAttribute("data-read")).toBe("false");
    expect(read.getAttribute("data-read")).toBe("true");
    expect(unread.querySelector(".ncard__dot")).toBeTruthy();
    expect(unread.getAttribute("style") ?? "").not.toContain("border");
  });

  it("folds identical notices in a row into one, which reads them all when opened", () => {
    const at = (min: number) => new Date(Date.now() - min * 60_000).toISOString();
    const unlocked = { category: "alert", title: "My Pond unlocked", body: "Emmanuel's A57 unlocked it.", data: { open: "my_pond" } };
    appState.householdNotices = [
      notice({ id: "a1", ...unlocked, timestamp: at(6) }),
      notice({ id: "a2", ...unlocked, timestamp: at(60) }),
      notice({ id: "a3", ...unlocked, timestamp: at(180), read: true }),
      notice({ id: "m", timestamp: at(200) }),
      notice({ id: "a4", ...unlocked, timestamp: at(240) }),
    ];
    render(<NotificationsView />);
    const rows = screen.getAllByRole("article");
    // Three in a row fold into one; the one after the model notice starts its own row.
    expect(rows).toHaveLength(3);
    expect(rows[0].textContent).toContain("3 times");
    expect(rows[2].textContent).not.toContain("times");
    fireEvent.click(rows[0]);
    expect(markNotificationsRead).toHaveBeenCalledWith(["a1", "a2"]);
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
