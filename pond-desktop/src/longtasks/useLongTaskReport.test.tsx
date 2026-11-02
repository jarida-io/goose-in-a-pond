import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type { LongTaskReport } from "../api/types";
import { useLongTaskReport } from "./useLongTaskReport";
import { artifactOf, doneTask, TASK_ID } from "./longTaskFixtures";

vi.mock("../api/PondApiClient", () => ({
  api: { getLongTaskReport: vi.fn(), getLongTaskArtifact: vi.fn() },
}));

const REPORT: LongTaskReport = {
  id: TASK_ID,
  title: "Kenya tax rules",
  state: "completed",
  kind: "research",
  markdown: "# Kenya tax rules\n\n## One\n\nText.\n",
  sections: 3,
  sources: [{ title: "KRA", url: "https://www.kra.go.ke/" }],
  calendar_events: 2,
};

const report = () => vi.mocked(api.getLongTaskReport);
const artifact = () => vi.mocked(api.getLongTaskArtifact);

beforeEach(() => {
  vi.clearAllMocks();
});

describe("a task's report", () => {
  it("is not asked for until something wants it", () => {
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    expect(result.current.state).toEqual({ status: "idle" });
    expect(report()).not.toHaveBeenCalled();
  });

  it("is the pond's whole report, in the shape the screens use", async () => {
    report().mockResolvedValue(REPORT);
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    let loaded: unknown;
    await act(async () => {
      loaded = await result.current.load();
    });

    expect(report()).toHaveBeenCalledWith(TASK_ID);
    expect(artifact()).not.toHaveBeenCalled();
    const expected = {
      title: "Kenya tax rules",
      markdown: REPORT.markdown,
      sections: 3,
      sources: REPORT.sources,
      calendarEvents: 2,
      summaryOnly: false,
    };
    expect(loaded).toEqual(expected);
    expect(result.current.state).toEqual({ status: "ready", report: expected });
  });

  it("says it is loading while the pond answers", async () => {
    report().mockReturnValue(new Promise<LongTaskReport | null>(() => {}));
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    act(() => {
      void result.current.load();
    });
    expect(result.current.state).toEqual({ status: "loading" });
  });

  it("falls back to the Result on an older pond, which can only be read, not saved or printed", async () => {
    report().mockResolvedValue(null);
    artifact().mockResolvedValue({ id: "art-1", title: "Trip plan", kind: "result", summary: "", body: "- ferry at **7:00**" });
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    await act(async () => {
      await result.current.load();
    });

    expect(artifact()).toHaveBeenCalledWith(TASK_ID, "art-1");
    expect(result.current.state).toEqual({
      status: "ready",
      report: { title: "Trip plan", markdown: "- ferry at **7:00**", sections: 0, sources: [], calendarEvents: 0, summaryOnly: true },
    });
  });

  it("says so when an older pond's task made no result", async () => {
    report().mockResolvedValue(null);
    const { result } = renderHook(() => useLongTaskReport(doneTask({ artifacts: [], result_artifact_id: null })));
    await act(async () => {
      await result.current.load();
    });
    expect(result.current.state).toEqual({ status: "failed", message: "This task has no result to open." });
  });

  it("fails with the pond's own words, and does not fall back for a refusal", async () => {
    report().mockRejectedValue(new ApiError(403, "nobody identified"));
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    let loaded: unknown = "unset";
    await act(async () => {
      loaded = await result.current.load();
    });
    expect(loaded).toBeNull();
    expect(result.current.state).toEqual({ status: "failed", message: "nobody identified" });
    expect(artifact()).not.toHaveBeenCalled();
  });

  it("fails with the reason when the Result cannot be read either", async () => {
    report().mockResolvedValue(null);
    artifact().mockRejectedValue(new Error("offline"));
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    await act(async () => {
      await result.current.load();
    });
    expect(result.current.state).toEqual({ status: "failed", message: "offline" });
  });

  it("survives an api that lacks the report call, as a partial mock does", async () => {
    report().mockImplementation(() => {
      throw new TypeError("api.getLongTaskReport is not a function");
    });
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    await act(async () => {
      await result.current.load();
    });
    expect(result.current.state).toMatchObject({ status: "failed" });
  });
});

describe("asking again", () => {
  it("is answered from what was fetched, not fetched again", async () => {
    report().mockResolvedValue(REPORT);
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    await act(async () => {
      await result.current.load();
      await result.current.load();
    });
    expect(report()).toHaveBeenCalledTimes(1);
  });

  it("shares one request between two askers at once", async () => {
    let arrive: (r: LongTaskReport) => void = () => {};
    report().mockReturnValue(new Promise<LongTaskReport>((resolve) => (arrive = resolve)));
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    let both: unknown[] = [];
    act(() => {
      both = [result.current.load(), result.current.load()];
    });
    await act(async () => {
      arrive(REPORT);
      await Promise.all(both);
    });
    expect(report()).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(result.current.state.status).toBe("ready"));
  });

  it("tries again after a failure", async () => {
    report().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(REPORT);
    const { result } = renderHook(() => useLongTaskReport(doneTask()));
    await act(async () => {
      await result.current.load();
    });
    expect(result.current.state.status).toBe("failed");
    await act(async () => {
      await result.current.load();
    });
    expect(result.current.state.status).toBe("ready");
    expect(report()).toHaveBeenCalledTimes(2);
  });

  it("reads the Result of the task as it is when the older-pond fallback runs", async () => {
    report().mockResolvedValue(null);
    artifact().mockResolvedValue({ id: "art-2", title: "T", kind: "result", summary: "", body: "B" });
    const first = doneTask({ result_artifact_id: null, artifacts: [] });
    const { result, rerender } = renderHook(({ task }) => useLongTaskReport(task), { initialProps: { task: first } });
    rerender({ task: doneTask({ artifacts: [artifactOf({ id: "art-2" })], result_artifact_id: "art-2" }) });
    await act(async () => {
      await result.current.load();
    });
    expect(artifact()).toHaveBeenCalledWith(TASK_ID, "art-2");
  });
});
