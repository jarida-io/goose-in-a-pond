import { describe, expect, it, vi } from "vitest";
import type { SseFrame } from "../player/sse";
import type { NotificationHistoryResponse } from "../api/types";
import type { AppAction } from "./reducer";
import {
  RECONNECT_PAUSES_MS,
  followHouseholdNotices,
  followScheduleRuns,
  noticeFromFrame,
  runFromFrame,
  type FeedApi,
} from "./liveFeeds";

const DEVICE = "pond-desktop-abc";

function frame(data: unknown): SseFrame {
  return { event: "message", data: JSON.stringify(data) };
}

function history(ids: string[]): NotificationHistoryResponse {
  return {
    device_id: DEVICE,
    notifications: ids.map((id) => ({
      id,
      category: "info",
      title: id,
      body: "",
      timestamp: "2026-10-28T07:00:00Z",
      data: null,
      read: true,
    })),
  };
}

async function* frames(list: SseFrame[]): AsyncGenerator<SseFrame> {
  for (const f of list) yield f;
}

describe("noticeFromFrame", () => {
  it("maps this device's own copy to the household id, unread", () => {
    const n = noticeFromFrame(
      frame({
        id: `proposal-1:${DEVICE}`,
        target: DEVICE,
        category: "action_required",
        title: "A suggestion",
        body: "Water the plants?",
        timestamp: "2026-10-28T07:00:00Z",
        data: { open: "chat", proposal_id: "1" },
      }),
      DEVICE,
    );
    expect(n).toEqual({
      id: "proposal-1",
      category: "action_required",
      title: "A suggestion",
      body: "Water the plants?",
      timestamp: "2026-10-28T07:00:00Z",
      data: { open: "chat", proposal_id: "1" },
      read: false,
    });
  });

  it("drops a transient status and a malformed frame", () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(
      noticeFromFrame(
        frame({ id: "warmup-1", title: "Pond is ready", data: { transient: true } }),
        DEVICE,
      ),
    ).toBeNull();
    expect(noticeFromFrame({ event: "message", data: "{nope" }, DEVICE)).toBeNull();
    expect(noticeFromFrame(frame({ title: "no id" }), DEVICE)).toBeNull();
  });
});

describe("followHouseholdNotices", () => {
  it("loads the history, streams for its device, and reloads the history on reconnect", async () => {
    const controller = new AbortController();
    const dispatched: AppAction[] = [];
    const streamedFor: string[] = [];
    let connects = 0;
    const api: FeedApi = {
      notificationHistory: vi.fn(async () => history(connects === 0 ? ["h1"] : ["h1", "h2"])),
      streamNotifications: (deviceId) => {
        streamedFor.push(deviceId);
        connects += 1;
        return frames([
          frame({ id: "disk-1", title: "Low disk", category: "alert", timestamp: "2026-10-28T08:00:00Z" }),
          frame({ id: "warmup-2", title: "Pond is ready", data: { transient: true } }),
        ]);
      },
      streamScheduleEvents: () => frames([]),
    };
    const pauses: number[] = [];
    const wait = async (ms: number) => {
      pauses.push(ms);
      if (pauses.length === 2) controller.abort();
    };

    await followHouseholdNotices(api, (a) => dispatched.push(a), controller.signal, wait);

    expect(api.notificationHistory).toHaveBeenCalledTimes(2);
    expect(streamedFor).toEqual([DEVICE, DEVICE]);
    const merged = dispatched
      .filter((a) => a.type === "MERGE_HOUSEHOLD_NOTICES")
      .map((a) => (a.type === "MERGE_HOUSEHOLD_NOTICES" ? a.payload.map((n) => n.id) : []));
    expect(merged).toEqual([["h1"], ["disk-1"], ["h1", "h2"], ["disk-1"]]);
    expect(pauses).toEqual([RECONNECT_PAUSES_MS[0], RECONNECT_PAUSES_MS[0]]);
  });

  it("flags a failing history and backs off further each time", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const controller = new AbortController();
    const dispatched: AppAction[] = [];
    const api: FeedApi = {
      notificationHistory: vi.fn(async () => {
        throw new Error("403");
      }),
      streamNotifications: () => frames([]),
      streamScheduleEvents: () => frames([]),
    };
    const pauses: number[] = [];
    const wait = async (ms: number) => {
      pauses.push(ms);
      if (pauses.length === 3) controller.abort();
    };

    await followHouseholdNotices(api, (a) => dispatched.push(a), controller.signal, wait);

    expect(dispatched).toContainEqual({ type: "SET_NOTICE_FEED_FAILED", payload: true });
    expect(pauses).toEqual(RECONNECT_PAUSES_MS.slice(1, 4));
  });
});

it("treats an answer that is not a history as a failure", async () => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const controller = new AbortController();
  const dispatched: AppAction[] = [];
  const streamNotifications = vi.fn(() => frames([]));
  const api: FeedApi = {
    notificationHistory: async () => ({}) as NotificationHistoryResponse,
    streamNotifications,
    streamScheduleEvents: () => frames([]),
  };
  await followHouseholdNotices(api, (a) => dispatched.push(a), controller.signal, async () =>
    controller.abort(),
  );
  expect(dispatched).toEqual([{ type: "SET_NOTICE_FEED_FAILED", payload: true }]);
  expect(streamNotifications).not.toHaveBeenCalled();
});

describe("schedule runs", () => {
  it("keys a run by its run id, as the recent-runs list does", () => {
    const parsed = runFromFrame(
      frame({
        schedule_id: "s1",
        schedule_label: "Daily Brief",
        run_id: "run-42",
        status: "completed",
        result: "Done",
      }),
    );
    expect(parsed?.run.id).toBe("run-42");
    expect(parsed?.toast.id).toBe("run-42");
    expect(parsed?.run.recipe).toBe(null);
    expect(runFromFrame(frame({ status: "queued" }))).toBeNull();
  });

  it("raises a toast and a card per frame", async () => {
    const controller = new AbortController();
    const dispatched: AppAction[] = [];
    const api: FeedApi = {
      notificationHistory: async () => history([]),
      streamNotifications: () => frames([]),
      streamScheduleEvents: () =>
        frames([frame({ schedule_id: "s1", schedule_label: "Daily Brief", run_id: "r1", status: "running" })]),
    };
    await followScheduleRuns(api, (a) => dispatched.push(a), controller.signal, async () =>
      controller.abort(),
    );
    expect(dispatched.map((a) => a.type)).toEqual(["SCHEDULE_RESULT", "ADD_SCHEDULE_RUN"]);
  });
});
