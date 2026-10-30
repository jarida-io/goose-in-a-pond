import { describe, expect, it } from "vitest";
import type { HouseholdNotice, ScheduleRunNotification } from "../api/types";
import {
  NOTICE_CAP,
  centreUnreadCount,
  foldedIntoRun,
  historyId,
  isTransient,
  listedNotices,
  mergeNotices,
  openSection,
} from "./householdNotices";
import { reducer, buildInitialState } from "./reducer";

function notice(over: Partial<HouseholdNotice> = {}): HouseholdNotice {
  return {
    id: "n1",
    category: "info",
    title: "Model downloaded",
    body: "ready",
    timestamp: "2026-10-28T07:00:00Z",
    data: null,
    read: false,
    ...over,
  };
}

function run(id: string, read = false): ScheduleRunNotification {
  return {
    id,
    scheduleId: "s1",
    scheduleName: "Daily Brief",
    status: "completed",
    result: "ok",
    error: null,
    startedAt: "2026-10-28T07:00:00Z",
    finishedAt: "2026-10-28T07:00:01Z",
    durationMs: 1000,
    read,
    excerpt: "ok",
    recipe: null,
  };
}

describe("historyId", () => {
  it("maps this device's own copy back to the household id", () => {
    expect(historyId("proposal-7:pond-desktop-abc", "pond-desktop-abc")).toBe("proposal-7");
  });

  it("leaves a broadcast copy, and another device's suffix, alone", () => {
    expect(historyId("disk-1", "pond-desktop-abc")).toBe("disk-1");
    expect(historyId("proposal-7:phone-liz", "pond-desktop-abc")).toBe("proposal-7:phone-liz");
    expect(historyId("x:", "")).toBe("x:");
  });
});

describe("mergeNotices", () => {
  it("keeps one notice per id, newest first, with the incoming content", () => {
    const merged = mergeNotices(
      [notice({ id: "a", timestamp: "2026-10-28T06:00:00Z", body: "old" })],
      [
        notice({ id: "a", timestamp: "2026-10-28T06:00:00Z", body: "new" }),
        notice({ id: "b", timestamp: "2026-10-28T08:00:00Z" }),
      ],
    );
    expect(merged.map((n) => n.id)).toEqual(["b", "a"]);
    expect(merged[1].body).toBe("new");
  });

  it("a live copy of a notice read here, or read on the Pond, stays read", () => {
    const readHere = mergeNotices([notice({ read: true })], [notice({ read: false })]);
    expect(readHere[0].read).toBe(true);
    const readOnPond = mergeNotices([notice({ read: false })], [notice({ read: true })]);
    expect(readOnPond[0].read).toBe(true);
  });

  it("never keeps a transient status", () => {
    const warmup = notice({ id: "warmup-1", data: { kind: "pond_warmup", transient: true } });
    expect(isTransient(warmup)).toBe(true);
    expect(mergeNotices([], [warmup])).toEqual([]);
  });

  it("is capped", () => {
    const many = Array.from({ length: NOTICE_CAP + 5 }, (_, i) =>
      notice({ id: `n${i}`, timestamp: new Date(Date.UTC(2026, 9, 1, 0, i)).toISOString() }),
    );
    const merged = mergeNotices([], many);
    expect(merged).toHaveLength(NOTICE_CAP);
    expect(merged[0].id).toBe(`n${NOTICE_CAP + 4}`);
  });
});

describe("schedule notices and run cards", () => {
  const scheduleNotice = notice({
    id: "sched",
    data: { open: "schedule", schedule_id: "s1", run_id: "r1" },
  });

  it("folds a schedule notice into its run's card", () => {
    expect(foldedIntoRun(scheduleNotice, [run("r1")])).toBe(true);
    expect(listedNotices([scheduleNotice], [run("r1")])).toEqual([]);
  });

  it("lists it when its run has no card", () => {
    expect(foldedIntoRun(scheduleNotice, [run("r2")])).toBe(false);
    expect(listedNotices([scheduleNotice], [])).toEqual([scheduleNotice]);
  });

  it("counts unread runs and unread listed notices, once each", () => {
    expect(
      centreUnreadCount(
        [run("r1"), run("r2", true)],
        [scheduleNotice, notice({ id: "x" }), notice({ id: "y", read: true })],
      ),
    ).toBe(2);
  });
});

describe("openSection", () => {
  it.each([
    ["schedule", "schedules"],
    ["devices", "devices"],
    ["chat", "chat"],
    ["models", "models"],
    ["extensions", "extensions"],
    ["remote_access", "pairing"],
  ])("maps %s to %s", (hint, section) => {
    expect(openSection({ data: { open: hint } })).toBe(section);
  });

  it("opens nothing for no hint or an unknown one", () => {
    expect(openSection({ data: null })).toBeNull();
    expect(openSection({ data: { open: "garden" } })).toBeNull();
    expect(openSection({ data: { open: 3 } })).toBeNull();
  });
});

describe("reducer", () => {
  it("merges notices by id and marks them read", () => {
    let state = buildInitialState();
    state = reducer(state, {
      type: "MERGE_HOUSEHOLD_NOTICES",
      payload: [notice({ id: "a" }), notice({ id: "b" })],
    });
    state = reducer(state, { type: "MERGE_HOUSEHOLD_NOTICES", payload: [notice({ id: "a" })] });
    expect(state.householdNotices.map((n) => n.id).sort()).toEqual(["a", "b"]);

    state = reducer(state, { type: "MARK_NOTICES_READ", payload: ["a"] });
    expect(state.householdNotices.find((n) => n.id === "a")?.read).toBe(true);
    expect(state.householdNotices.find((n) => n.id === "b")?.read).toBe(false);
  });

  it("records whether the notice feed is failing", () => {
    let state = buildInitialState();
    expect(state.noticeFeedFailed).toBe(false);
    state = reducer(state, { type: "SET_NOTICE_FEED_FAILED", payload: true });
    expect(state.noticeFeedFailed).toBe(true);
    const same = reducer(state, { type: "SET_NOTICE_FEED_FAILED", payload: true });
    expect(same).toBe(state);
  });
});
