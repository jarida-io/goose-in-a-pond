import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { agentRow, reportOf } from "./prefillFixtures";
import { ALL_AGENTS, PREFILL_ACTIVE_POLL_MS, PREFILL_IDLE_POLL_MS, useAgentPrefills } from "./useAgentPrefills";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getAgentPrefills: vi.fn(),
    rebuildAgentPrefill: vi.fn(),
    rebuildAllPrefills: vi.fn(),
  },
}));

const BUILDING = reportOf([agentRow("study-planner", { status: "building" })], { active: true, current: "study-planner", done: 1, total: 3 });
const IDLE = reportOf([agentRow("study-planner")]);

async function pass(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

const asked = () => vi.mocked(api.getAgentPrefills).mock.calls.length;

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("asking the pond how its agents' start-up copies stand", () => {
  it("asks at once, and again every 5 seconds while the pond is building", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(BUILDING);
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    expect(asked()).toBe(1);
    expect(result.current.report).toEqual(BUILDING);
    await pass(PREFILL_ACTIVE_POLL_MS - 1);
    expect(asked()).toBe(1);
    await pass(1);
    expect(asked()).toBe(2);
    await pass(PREFILL_ACTIVE_POLL_MS);
    expect(asked()).toBe(3);
  });

  it("slows to every 60 seconds when the pond is not building", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(IDLE);
    renderHook(() => useAgentPrefills());
    await pass(0);
    await pass(PREFILL_IDLE_POLL_MS - 1);
    expect(asked()).toBe(1);
    await pass(1);
    expect(asked()).toBe(2);
  });

  it("goes from fast to slow when the build ends, and back when another starts", async () => {
    vi.mocked(api.getAgentPrefills)
      .mockResolvedValueOnce(BUILDING)
      .mockResolvedValueOnce(IDLE)
      .mockResolvedValueOnce(BUILDING)
      .mockResolvedValue(BUILDING);
    renderHook(() => useAgentPrefills());
    await pass(0);
    await pass(PREFILL_ACTIVE_POLL_MS);
    expect(asked()).toBe(2);
    await pass(PREFILL_IDLE_POLL_MS - 1);
    expect(asked()).toBe(2);
    await pass(1);
    expect(asked()).toBe(3);
    await pass(PREFILL_ACTIVE_POLL_MS);
    expect(asked()).toBe(4);
  });

  it("stops asking, and reports nothing, on a pond with no such route", async () => {
    vi.mocked(api.getAgentPrefills).mockRejectedValue(new ApiError(404, "Not Found"));
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    expect(result.current.supported).toBe(false);
    expect(result.current.report).toBeNull();
    await pass(10 * PREFILL_IDLE_POLL_MS);
    expect(asked()).toBe(1);
  });

  it("keeps the last answer through a failed ask, and tries again slowly", async () => {
    vi.mocked(api.getAgentPrefills)
      .mockResolvedValueOnce(IDLE)
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue(BUILDING);
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    await pass(PREFILL_IDLE_POLL_MS);
    expect(asked()).toBe(2);
    expect(result.current.supported).toBe(true);
    expect(result.current.report).toEqual(IDLE);
    await pass(PREFILL_IDLE_POLL_MS);
    expect(asked()).toBe(3);
    expect(result.current.report).toEqual(BUILDING);
  });

  it("stops asking when the screen goes away", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(BUILDING);
    const { unmount } = renderHook(() => useAgentPrefills());
    await pass(0);
    unmount();
    await pass(PREFILL_IDLE_POLL_MS * 10);
    expect(asked()).toBe(1);
  });

  it("asks again now when told to refresh", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(IDLE);
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    act(() => result.current.refresh());
    await pass(0);
    expect(asked()).toBe(2);
  });
});

describe("asking for a rebuild", () => {
  it("asks for one agent by name, then refreshes so the new state shows", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(IDLE);
    vi.mocked(api.rebuildAgentPrefill).mockResolvedValue(undefined);
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    let ok = false;
    await act(async () => {
      ok = await result.current.rebuild("study-planner");
    });
    expect(ok).toBe(true);
    expect(api.rebuildAgentPrefill).toHaveBeenCalledWith("study-planner");
    await pass(0);
    expect(asked()).toBe(2);
    expect(result.current.busy).toEqual({});
  });

  it("marks the agent busy while the pond is being asked", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(IDLE);
    let finish: () => void = () => {};
    vi.mocked(api.rebuildAgentPrefill).mockReturnValue(
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
    );
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    let pending: Promise<boolean> = Promise.resolve(false);
    act(() => {
      pending = result.current.rebuild("study-planner");
    });
    expect(result.current.busy).toEqual({ "study-planner": true });
    await act(async () => {
      finish();
      await pending;
    });
    expect(result.current.busy).toEqual({});
  });

  it("says what went wrong, and clears it on the next try", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(IDLE);
    vi.mocked(api.rebuildAgentPrefill).mockRejectedValueOnce(new ApiError(404, "there is no agent named gone"));
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    let ok = true;
    await act(async () => {
      ok = await result.current.rebuild("gone");
    });
    expect(ok).toBe(false);
    expect(result.current.errors).toEqual({ gone: "Could not ask for a rebuild. there is no agent named gone" });

    vi.mocked(api.rebuildAgentPrefill).mockResolvedValueOnce(undefined);
    await act(async () => {
      await result.current.rebuild("gone");
    });
    expect(result.current.errors).toEqual({});
  });

  it("asks for every agent at once under its own key", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(IDLE);
    vi.mocked(api.rebuildAllPrefills).mockRejectedValueOnce(new Error("offline"));
    const { result } = renderHook(() => useAgentPrefills());
    await pass(0);
    await act(async () => {
      await result.current.rebuildAll();
    });
    expect(api.rebuildAllPrefills).toHaveBeenCalledTimes(1);
    expect(result.current.errors[ALL_AGENTS]).toBe("Could not ask for a rebuild. offline");
  });
});
