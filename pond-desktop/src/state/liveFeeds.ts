// The dashboard's two live feeds from the Pond, each a fetch-based stream (an `EventSource`
// cannot send the session token the Pond requires) that reconnects with a growing pause:
//  - household notices: the history on every (re)connect, then `/notifications/stream`;
//  - schedule and rule runs: `/schedules/events`.
// They live in AppContext, not a section, so the bell counts on every page.

import type { Dispatch } from "react";
import type { SseFrame } from "../player/sse";
import type {
  HouseholdNotice,
  NotificationHistoryResponse,
  ScheduleRunNotification,
} from "../api/types";
import type { AppAction, ScheduleToast } from "./reducer";
import { historyId, isTransient } from "./householdNotices";

/** The part of `PondApiClient` the feeds use, so tests can stand in for it. */
export interface FeedApi {
  notificationHistory(limit?: number): Promise<NotificationHistoryResponse>;
  streamNotifications(deviceId: string, signal: AbortSignal): AsyncIterable<SseFrame>;
  streamScheduleEvents(signal: AbortSignal): AsyncIterable<SseFrame>;
}

/** Pauses between reconnects, in ms; the last repeats. */
export const RECONNECT_PAUSES_MS = [2_000, 5_000, 15_000, 30_000, 60_000];

type Sleep = (ms: number, signal: AbortSignal) => Promise<void>;

const sleep: Sleep = (ms, signal) =>
  new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });

/** A live `Notification` frame as a household notice, or null when it is not one to list. */
export function noticeFromFrame(
  frame: SseFrame,
  deviceId: string,
): HouseholdNotice | null {
  let raw: Record<string, unknown>;
  try {
    raw = JSON.parse(frame.data) as Record<string, unknown>;
  } catch {
    console.warn("[notifications] a stream frame was not JSON; skipped");
    return null;
  }
  if (typeof raw.id !== "string" || typeof raw.title !== "string") return null;
  const data =
    raw.data && typeof raw.data === "object" && !Array.isArray(raw.data)
      ? (raw.data as Record<string, unknown>)
      : null;
  const notice: HouseholdNotice = {
    id: historyId(raw.id, deviceId),
    category: typeof raw.category === "string" ? raw.category : "info",
    title: raw.title,
    body: typeof raw.body === "string" ? raw.body : "",
    timestamp:
      typeof raw.timestamp === "string" ? raw.timestamp : new Date().toISOString(),
    data,
    read: false,
  };
  return isTransient(notice) ? null : notice;
}

/** Run `connect` until `signal` aborts, pausing longer after each failure in a row. */
async function keepConnected(
  name: string,
  connect: () => Promise<void>,
  signal: AbortSignal,
  wait: Sleep,
): Promise<void> {
  let failures = 0;
  while (!signal.aborted) {
    try {
      await connect();
      // The stream ended on its own (a server restart): reconnect after the shortest pause.
      failures = 0;
    } catch (err) {
      if (signal.aborted) return;
      console.warn(`[${name}] feed failed; reconnecting`, err);
      failures += 1;
    }
    if (signal.aborted) return;
    await wait(
      RECONNECT_PAUSES_MS[Math.min(failures, RECONNECT_PAUSES_MS.length - 1)],
      signal,
    );
  }
}

/**
 * Household notices: on every (re)connect the history (which carries this device's id and read
 * state), then the live stream, each notice under its household id.
 */
export function followHouseholdNotices(
  api: FeedApi,
  dispatch: Dispatch<AppAction>,
  signal: AbortSignal,
  wait: Sleep = sleep,
): Promise<void> {
  return keepConnected(
    "notifications",
    async () => {
      let history: NotificationHistoryResponse;
      try {
        history = await api.notificationHistory();
        // An older Pond, or anything else answering on the route, is not a history.
        if (typeof history?.device_id !== "string" || !Array.isArray(history.notifications)) {
          throw new Error("the notification history answer is not one");
        }
      } catch (err) {
        dispatch({ type: "SET_NOTICE_FEED_FAILED", payload: true });
        throw err;
      }
      dispatch({ type: "SET_NOTICE_FEED_FAILED", payload: false });
      dispatch({ type: "MERGE_HOUSEHOLD_NOTICES", payload: history.notifications });
      for await (const frame of api.streamNotifications(history.device_id, signal)) {
        const notice = noticeFromFrame(frame, history.device_id);
        if (notice) dispatch({ type: "MERGE_HOUSEHOLD_NOTICES", payload: [notice] });
      }
    },
    signal,
    wait,
  );
}

/** Infer a recipe identifier from the schedule name for debrief card routing. */
export function inferRecipe(name: string): string | null {
  const lower = name.toLowerCase();
  if (/morning|briefing|daily.*summ/.test(lower)) return "daily-summary";
  if (/weekly.*report|week.*summ/.test(lower)) return "weekly-report";
  if (/compact|consolidat|memory.*clean/.test(lower)) return "compact-memory";
  return null;
}

/** One `ScheduleResultEvent` frame as the toast and the run card it raises. */
export function runFromFrame(
  frame: SseFrame,
): { toast: ScheduleToast; run: ScheduleRunNotification } | null {
  let data: {
    run_id?: string;
    schedule_id?: string;
    schedule_label?: string;
    status?: string;
    result?: string;
    error?: string;
  };
  try {
    data = JSON.parse(frame.data);
  } catch {
    console.warn("[schedules] a stream frame was not JSON; skipped");
    return null;
  }
  const status = data.status;
  if (status !== "completed" && status !== "failed" && status !== "running") return null;
  // Keyed by the run, as `getAllRecentRuns` keys its cards, so a reload does not list it twice.
  const id = data.run_id || data.schedule_id || String(Date.now());
  const toast: ScheduleToast = {
    id,
    schedule_id: data.schedule_id || "",
    schedule_label: data.schedule_label || data.schedule_id || "Schedule",
    status,
    result: data.result,
    error: data.error,
    timestamp: Date.now(),
  };
  const now = new Date().toISOString();
  const run: ScheduleRunNotification = {
    id,
    scheduleId: toast.schedule_id,
    scheduleName: toast.schedule_label,
    status,
    result: toast.result ?? null,
    error: toast.error ?? null,
    startedAt: now,
    finishedAt: status !== "running" ? now : null,
    durationMs: null,
    read: false,
    excerpt: (toast.result ?? toast.error ?? "").slice(0, 80),
    recipe: inferRecipe(toast.schedule_label),
  };
  return { toast, run };
}

/** Schedule and rule runs as they start and finish: a toast, and the run's card. */
export function followScheduleRuns(
  api: FeedApi,
  dispatch: Dispatch<AppAction>,
  signal: AbortSignal,
  wait: Sleep = sleep,
): Promise<void> {
  return keepConnected(
    "schedules",
    async () => {
      for await (const frame of api.streamScheduleEvents(signal)) {
        const parsed = runFromFrame(frame);
        if (!parsed) continue;
        dispatch({ type: "SCHEDULE_RESULT", payload: parsed.toast });
        // ADD_SCHEDULE_RUN dedupes by id, so this also covers a missed "running" event.
        dispatch({ type: "ADD_SCHEDULE_RUN", payload: parsed.run });
      }
    },
    signal,
    wait,
  );
}
