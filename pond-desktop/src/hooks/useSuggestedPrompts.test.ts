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
      answered_by: "giap-memory",
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

  // Chips can be personal, and a shared panel's next person may be someone else.
  it("drops the last audience's prompts when the next fetch comes back empty", async () => {
    answers(["When is my appointment at the clinic?"]);
    const { result, rerender } = renderHook(({ sid }) => useSuggestedPrompts(sid), {
      initialProps: { sid: "s-liz" as string | null },
    });
    // Control: without it the assertion below passes on the initial fallback.
    await waitFor(() => expect(result.current).toEqual(["When is my appointment at the clinic?"]));

    answers([]);
    rerender({ sid: null });
    await waitFor(() => expect(vi.mocked(api.listSuggestions)).toHaveBeenLastCalledWith(null));
    await waitFor(() =>
      expect(result.current).not.toContain("When is my appointment at the clinic?"),
    );
    expect(result.current.length).toBeGreaterThan(0);
  });

  it("drops the last audience's prompts when the next fetch fails", async () => {
    answers(["When is my appointment at the clinic?"]);
    const { result, rerender } = renderHook(({ sid }) => useSuggestedPrompts(sid), {
      initialProps: { sid: "s-liz" as string | null },
    });
    await waitFor(() => expect(result.current).toEqual(["When is my appointment at the clinic?"]));

    vi.mocked(api.listSuggestions).mockRejectedValue(new Error("offline"));
    rerender({ sid: null });
    await waitFor(() =>
      expect(result.current).not.toContain("When is my appointment at the clinic?"),
    );
  });
});
