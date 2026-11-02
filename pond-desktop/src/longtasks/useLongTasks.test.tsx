import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { doneTask, OTHER_ID, taskOf, TASK_ID } from "./longTaskFixtures";
import { LONG_TASK_POLL_MS, LONG_TASK_SLOW_POLL_MS, useLongTasks } from "./useLongTasks";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listLongTasks: vi.fn(),
    getLongTask: vi.fn(),
    cancelLongTask: vi.fn(),
  },
}));

async function pass(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

/** A read, the render it causes and the read that render starts each need their own turn. */
async function settle() {
  for (let i = 0; i < 4; i++) await pass(0);
}

const reads = () => vi.mocked(api.getLongTask).mock.calls.length;

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("a conversation's long tasks", () => {
  it("lists the tasks the conversation started, by its session", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf(), doneTask({ id: OTHER_ID })]);
    const { result } = renderHook(() => useLongTasks("sess-1", []));
    await settle();
    expect(api.listLongTasks).toHaveBeenCalledWith("sess-1");
    expect(Object.keys(result.current.tasks).sort()).toEqual([OTHER_ID, TASK_ID].sort());
    expect(result.current.supported).toBe(true);
  });

  it("reads a task a message points at, once the list has been read", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([]);
    vi.mocked(api.getLongTask).mockResolvedValue(doneTask());
    const { result } = renderHook(() => useLongTasks("sess-1", [TASK_ID]));
    await settle();
    expect(api.getLongTask).toHaveBeenCalledWith(TASK_ID);
    expect(result.current.tasks[TASK_ID].state).toBe("completed");
  });

  it("does not read a task the list already gave", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([doneTask()]);
    renderHook(() => useLongTasks("sess-1", [TASK_ID]));
    await settle();
    expect(reads()).toBe(0);
  });

  it("reads a task a live turn just started, before the conversation has an id", async () => {
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
    const { result } = renderHook(() => useLongTasks(null, [TASK_ID]));
    await settle();
    expect(api.listLongTasks).not.toHaveBeenCalled();
    expect(result.current.tasks[TASK_ID].goal).toBe("Plan a trip to Lamu");
  });

  it("goes quiet, and asks nothing more, on a pond with no long tasks", async () => {
    vi.mocked(api.listLongTasks).mockRejectedValue(new ApiError(404, "Not Found"));
    const { result } = renderHook(() => useLongTasks("sess-1", [TASK_ID]));
    await settle();
    expect(result.current.supported).toBe(false);
    await pass(10 * LONG_TASK_SLOW_POLL_MS);
    expect(api.listLongTasks).toHaveBeenCalledTimes(1);
    expect(reads()).toBe(0);
  });

  it("still reads the tasks messages point at when the list itself fails", async () => {
    vi.mocked(api.listLongTasks).mockRejectedValue(new ApiError(500, "boom"));
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
    const { result } = renderHook(() => useLongTasks("sess-1", [TASK_ID]));
    await settle();
    expect(result.current.supported).toBe(true);
    expect(result.current.tasks[TASK_ID]).toBeDefined();
  });
});

describe("asking after a task that is under way", () => {
  it("asks every 5 seconds while it is running", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
    renderHook(() => useLongTasks("sess-1", []));
    await settle();
    expect(reads()).toBe(0);
    await pass(LONG_TASK_POLL_MS - 1);
    expect(reads()).toBe(0);
    await pass(1);
    expect(reads()).toBe(1);
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(2);
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(3);
  });

  it("asks every 5 seconds while it is planning", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf({ state: "planning", steps: [] })]);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ state: "planning", steps: [] }));
    renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(1);
  });

  it("shows what a poll finds", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ updated_at: "2026-10-28T09:05:00Z", state: "planning", steps: [] }));
    const { result } = renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(LONG_TASK_POLL_MS);
    expect(result.current.tasks[TASK_ID].state).toBe("planning");
  });

  it("stops asking once the task has ended", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    vi.mocked(api.getLongTask).mockResolvedValue(doneTask());
    const { result } = renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(1);
    expect(result.current.tasks[TASK_ID].state).toBe("completed");
    await pass(20 * LONG_TASK_SLOW_POLL_MS);
    expect(reads()).toBe(1);
  });

  it.each(["completed", "failed", "cancelled"] as const)("never asks after a task listed as %s", async (state) => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf({ state })]);
    renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(20 * LONG_TASK_SLOW_POLL_MS);
    expect(reads()).toBe(0);
  });

  it.each(["paused", "awaiting_approval"] as const)("asks only every 30 seconds after one that is %s", async (state) => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf({ state })]);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ state }));
    renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(LONG_TASK_POLL_MS * 5);
    expect(reads()).toBe(0);
    await pass(LONG_TASK_SLOW_POLL_MS - LONG_TASK_POLL_MS * 5);
    expect(reads()).toBe(1);
  });

  it("speeds up again when a paused task is resumed", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf({ state: "paused" })]);
    vi.mocked(api.getLongTask).mockResolvedValueOnce(taskOf({ state: "running" })).mockResolvedValue(taskOf({ state: "running" }));
    renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(LONG_TASK_SLOW_POLL_MS);
    expect(reads()).toBe(1);
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(2);
  });

  it("keeps the last answer through a failed poll, and asks again", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    vi.mocked(api.getLongTask).mockRejectedValueOnce(new Error("offline")).mockResolvedValue(taskOf());
    const { result } = renderHook(() => useLongTasks("sess-1", []));
    await settle();
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(1);
    expect(result.current.tasks[TASK_ID]).toBeDefined();
    await pass(LONG_TASK_POLL_MS);
    expect(reads()).toBe(2);
    expect(result.current.unreadable[TASK_ID]).toBeUndefined();
  });

  it("stops the clock when the chat goes away", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
    const { unmount } = renderHook(() => useLongTasks("sess-1", []));
    await settle();
    unmount();
    await pass(10 * LONG_TASK_POLL_MS);
    expect(reads()).toBe(0);
  });
});

describe("a task that cannot be read", () => {
  it("is gone when the pond has no such task, and is not asked after again", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([]);
    vi.mocked(api.getLongTask).mockRejectedValue(new ApiError(404, "Not Found"));
    const { result } = renderHook(() => useLongTasks("sess-1", [TASK_ID]));
    await settle();
    expect(result.current.gone[TASK_ID]).toBe(true);
    expect(result.current.tasks[TASK_ID]).toBeUndefined();
    await pass(10 * LONG_TASK_SLOW_POLL_MS);
    expect(reads()).toBe(1);
  });

  it("is unreadable on any other failure, and Try again reads it once more", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([]);
    vi.mocked(api.getLongTask).mockRejectedValueOnce(new Error("offline")).mockResolvedValue(doneTask());
    const { result } = renderHook(() => useLongTasks("sess-1", [TASK_ID]));
    await settle();
    expect(result.current.unreadable[TASK_ID]).toBe("offline");
    expect(reads()).toBe(1);
    await pass(10 * LONG_TASK_SLOW_POLL_MS);
    expect(reads()).toBe(1);
    act(() => result.current.retry(TASK_ID));
    await settle();
    expect(reads()).toBe(2);
    expect(result.current.unreadable[TASK_ID]).toBeUndefined();
    expect(result.current.tasks[TASK_ID].state).toBe("completed");
  });
});

describe("stopping a task", () => {
  async function running() {
    vi.mocked(api.listLongTasks).mockResolvedValue([taskOf()]);
    const hook = renderHook(() => useLongTasks("sess-1", []));
    await settle();
    return hook;
  }

  it("asks the pond to cancel, then reads the task to show how it ended", async () => {
    vi.mocked(api.cancelLongTask).mockResolvedValue(undefined as never);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ state: "cancelled" }));
    const { result } = await running();
    await act(async () => {
      await result.current.stop(TASK_ID);
    });
    expect(api.cancelLongTask).toHaveBeenCalledWith(TASK_ID);
    expect(result.current.tasks[TASK_ID].state).toBe("cancelled");
    expect(result.current.stopping).toEqual({});
    expect(result.current.stopErrors).toEqual({});
  });

  it("treats a task that had already ended as no error, and shows how it ended", async () => {
    vi.mocked(api.cancelLongTask).mockRejectedValue(new ApiError(409, "already ended"));
    vi.mocked(api.getLongTask).mockResolvedValue(doneTask());
    const { result } = await running();
    await act(async () => {
      await result.current.stop(TASK_ID);
    });
    expect(result.current.tasks[TASK_ID].state).toBe("completed");
    expect(result.current.stopErrors).toEqual({});
  });

  it("says so when the pond could not stop it, and leaves the task as it was", async () => {
    vi.mocked(api.cancelLongTask).mockRejectedValue(new ApiError(500, "boom"));
    const { result } = await running();
    await act(async () => {
      await result.current.stop(TASK_ID);
    });
    expect(result.current.stopErrors[TASK_ID]).toBe("Could not stop it. boom");
    expect(result.current.tasks[TASK_ID].state).toBe("running");
    expect(result.current.stopping).toEqual({});
    expect(reads()).toBe(0);
  });

  it("marks the task as stopping while the request is out", async () => {
    let release: () => void = () => {};
    vi.mocked(api.cancelLongTask).mockReturnValue(new Promise<never>((resolve) => (release = () => resolve(undefined as never))));
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf({ state: "cancelled" }));
    const { result } = await running();
    let done: Promise<void> = Promise.resolve();
    act(() => {
      done = result.current.stop(TASK_ID);
    });
    expect(result.current.stopping[TASK_ID]).toBe(true);
    await act(async () => {
      release();
      await done;
    });
    expect(result.current.stopping[TASK_ID]).toBeUndefined();
  });
});

describe("moving between conversations", () => {
  it("starts clean in another conversation", async () => {
    vi.mocked(api.listLongTasks).mockImplementation(async (id) => (id === "a" ? [taskOf()] : []));
    const { result, rerender } = renderHook(({ id }) => useLongTasks(id, []), { initialProps: { id: "a" as string | null } });
    await settle();
    expect(result.current.tasks[TASK_ID]).toBeDefined();
    rerender({ id: "b" });
    await settle();
    expect(api.listLongTasks).toHaveBeenLastCalledWith("b");
    expect(result.current.tasks).toEqual({});
  });

  it("keeps what a live turn found when the conversation gets its first id", async () => {
    vi.mocked(api.listLongTasks).mockResolvedValue([]);
    vi.mocked(api.getLongTask).mockResolvedValue(taskOf());
    const { result, rerender } = renderHook(({ id }) => useLongTasks(id, [TASK_ID]), { initialProps: { id: null as string | null } });
    await settle();
    expect(result.current.tasks[TASK_ID]).toBeDefined();
    rerender({ id: "sess-1" });
    await settle();
    expect(api.listLongTasks).toHaveBeenCalledWith("sess-1");
    expect(result.current.tasks[TASK_ID]).toBeDefined();
    expect(reads()).toBe(1);
  });
});
