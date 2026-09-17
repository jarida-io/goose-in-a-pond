import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor, cleanup } from "@testing-library/react";
import { useSuggestedPrompts } from "./useSuggestedPrompts";
import { api } from "../api/PondApiClient";

vi.mock("../api/PondApiClient", () => ({
  api: { listSuggestions: vi.fn() },
}));

afterEach(cleanup);
beforeEach(() => vi.clearAllMocks());

function answers(prompts: string[]) {
  vi.mocked(api.listSuggestions).mockResolvedValue({
    suggestions: prompts.map((p, i) => ({
      id: `s${i}`,
      prompt: p,
      because: "a measured fact.",
      answered_by: "giap-memory", composed: false,
    })),
    considered: [],
    audience: "personal",
  });
}

describe("useSuggestedPrompts", () => {
  it("uses the engine's prompts when it offers any", async () => {
    answers(["What's on my calendar today?", "Which of my devices are online?"]);
    const { result } = renderHook(() => useSuggestedPrompts("s-1"));

    await waitFor(() =>
      expect(result.current).toEqual([
        "What's on my calendar today?",
        "Which of my devices are online?",
      ]),
    );
  });

  /// The chips these replaced named hardware: "Lock everything", "Bedroom to
  /// 67", "Show the driveway". On a pond with no lock, no thermostat and no
  /// camera each one reached a model that could only say so. The fallback must
  /// therefore never claim anything about the house.
  it("falls back to questions about the assistant, never about the house", async () => {
    answers([]);
    const { result } = renderHook(() => useSuggestedPrompts(null));

    await waitFor(() => expect(result.current.length).toBeGreaterThan(0));
    for (const chip of result.current) {
      for (const hardware of ["lock", "thermostat", "driveway", "bedroom", "camera", "°"]) {
        expect(
          chip.toLowerCase().includes(hardware),
          `"${chip}" names hardware this pond may not own`,
        ).toBe(false);
      }
    }
  });

  it("keeps the fallback rather than emptying the row when the fetch fails", async () => {
    vi.mocked(api.listSuggestions).mockRejectedValue(new Error("offline"));
    const { result } = renderHook(() => useSuggestedPrompts("s-1"));

    // A composer with no chips reads as a loading state that never finishes.
    await waitFor(() => expect(result.current.length).toBeGreaterThan(0));
  });

  it("asks without a session, because the route does not need one", async () => {
    answers(["What can you help me with?"]);
    renderHook(() => useSuggestedPrompts(null));

    await waitFor(() =>
      expect(vi.mocked(api.listSuggestions)).toHaveBeenCalledWith(null),
    );
  });
});
