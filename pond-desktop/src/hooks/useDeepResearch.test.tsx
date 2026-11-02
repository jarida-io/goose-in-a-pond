import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { useDeepResearch } from "./useDeepResearch";
import type { DeepResearchAvailability } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: { getDeepResearchAvailability: vi.fn() },
}));

const ask = () => vi.mocked(api.getDeepResearchAvailability);

function answer(a: DeepResearchAvailability | null) {
  ask().mockResolvedValue(a);
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("whether the pond offers deep research", () => {
  it("is unknown until the pond has answered, and cannot be entered meanwhile", () => {
    ask().mockReturnValue(new Promise<DeepResearchAvailability | null>(() => {}));
    const { result } = renderHook(() => useDeepResearch(true));
    expect(result.current.access).toBe("unknown");
    act(() => result.current.enter());
    expect(result.current.on).toBe(false);
  });

  it("is available when the pond says so", async () => {
    answer({ available: true });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("available"));
  });

  it("is hidden when the household switched it off", async () => {
    answer({ available: false, reason: "off" });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("hidden"));
  });

  it("is unidentified for a caller the pond cannot place", async () => {
    answer({ available: false, reason: "unidentified" });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("unidentified"));
  });

  it("is hidden on an older pond, which answers null", async () => {
    answer(null);
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("hidden"));
  });

  it("is hidden when the question fails, rather than guessing the item is there", async () => {
    ask().mockRejectedValue(new Error("offline"));
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("hidden"));
  });

  it("survives an api that lacks the method, as a partial mock does", async () => {
    ask().mockImplementation(() => {
      throw new TypeError("api.getDeepResearchAvailability is not a function");
    });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("hidden"));
  });
});

describe("asking", () => {
  it("asks once, however often it renders", async () => {
    answer({ available: true });
    const { result, rerender } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("available"));
    rerender();
    rerender();
    expect(ask()).toHaveBeenCalledTimes(1);
  });

  it("does not ask while the pond is offline, and asks when it comes back", async () => {
    answer({ available: true });
    const { result, rerender } = renderHook(({ online }) => useDeepResearch(online), { initialProps: { online: false } });
    expect(ask()).not.toHaveBeenCalled();
    expect(result.current.access).toBe("unknown");

    rerender({ online: true });
    await waitFor(() => expect(result.current.access).toBe("available"));
    expect(ask()).toHaveBeenCalledTimes(1);
  });

  it("ignores an answer that arrives after the screen has gone", async () => {
    let arrive: (a: DeepResearchAvailability | null) => void = () => {};
    ask().mockReturnValue(new Promise<DeepResearchAvailability | null>((resolve) => (arrive = resolve)));
    const { result, unmount } = renderHook(() => useDeepResearch(true));
    unmount();
    await act(async () => arrive({ available: true }));
    expect(result.current.access).toBe("unknown");
  });
});

describe("the mode", () => {
  async function available() {
    answer({ available: true });
    const hook = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(hook.result.current.access).toBe("available"));
    return hook;
  }

  it("starts off, goes on when entered and off when left", async () => {
    const { result } = await available();
    expect(result.current.on).toBe(false);
    act(() => result.current.enter());
    expect(result.current.on).toBe(true);
    act(() => result.current.leave());
    expect(result.current.on).toBe(false);
  });

  it("cannot be entered when the pond does not offer it", async () => {
    answer({ available: false, reason: "off" });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("hidden"));
    act(() => result.current.enter());
    expect(result.current.on).toBe(false);
  });

  it("cannot be entered by a caller the pond cannot place", async () => {
    answer({ available: false, reason: "unidentified" });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("unidentified"));
    act(() => result.current.enter());
    expect(result.current.on).toBe(false);
  });

  it("follows the pond when it refuses a research turn: off hides the item", async () => {
    const { result } = await available();
    act(() => result.current.enter());
    act(() => result.current.refused("deep_research_off"));
    expect(result.current.access).toBe("hidden");
    expect(result.current.on).toBe(false);
  });

  it("follows the pond when it refuses a research turn: unidentified disables the item", async () => {
    const { result } = await available();
    act(() => result.current.refused("deep_research_unidentified"));
    expect(result.current.access).toBe("unidentified");
    act(() => result.current.enter());
    expect(result.current.on).toBe(false);
  });

  it("is a fresh, off mode for each mount", async () => {
    const first = await available();
    act(() => first.result.current.enter());
    first.unmount();
    const second = await available();
    expect(second.result.current.on).toBe(false);
  });
});

describe("how much the pond writes", () => {
  it("leaves it to the pond until the person picks, and keeps their pick across questions", async () => {
    answer({ available: true });
    const { result } = renderHook(() => useDeepResearch(true));
    await waitFor(() => expect(result.current.access).toBe("available"));
    expect(result.current.answer).toBe("auto");
    act(() => result.current.setAnswer("short"));
    act(() => result.current.enter());
    act(() => result.current.leave());
    act(() => result.current.enter());
    expect(result.current.answer).toBe("short");
  });
});
