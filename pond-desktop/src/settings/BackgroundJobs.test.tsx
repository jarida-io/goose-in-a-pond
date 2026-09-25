import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  BackgroundJobs,
  describeElapsed,
  describeHistory,
  describeLastRun,
  describeWait,
} from "./BackgroundJobs";
import { api } from "../api/PondApiClient";
import type { LaneJobStatus, LaneStatus } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: { laneStatus: vi.fn(), runLaneJob: vi.fn() },
}));

function job(over: Partial<LaneJobStatus> = {}): LaneJobStatus {
  return {
    job: "memory_extraction",
    title: "Read conversations for memories",
    present: true,
    registered: true,
    enabled: true,
    since_last_run_secs: null,
    interval_floor_secs: 60,
    idle_threshold_secs: 900,
    blocked_by: null,
    would_run_next: false,
    ...over,
  };
}

function lane(jobs: LaneJobStatus[], over: Partial<LaneStatus> = {}): LaneStatus {
  return { lane: true, jobs, slot_busy: false, ...over };
}

afterEach(cleanup);
beforeEach(() => vi.clearAllMocks());

describe("what a job is waiting for", () => {
  /// The sentence is the whole screen. Each reason names something the
  /// household can act on rather than repeating the gate's own vocabulary.
  it("says each wait in words somebody can act on", () => {
    expect(describeWait(job({ blocked_by: "still_active" })))
      .toBe("Waiting for the house to be quiet");
    expect(describeWait(job({ blocked_by: "no_activity_since_start" })))
      .toBe("Waiting for you to say something first");
    expect(describeWait(job({ blocked_by: "interval_floor" })))
      .toBe("Ran recently, waiting its turn again");
    expect(describeWait(job({ blocked_by: "disabled" })))
      .toBe("Turned off in settings");
    expect(describeWait(job({ blocked_by: null }))).toBe("Ready");
  });

  /// `present` and `registered` outrank the reason, because a job with no loop
  /// in this process has no reason -- reporting "Ready" for one would be the
  /// exact confusion this screen was built to end.
  it("puts having no loop here above any other explanation", () => {
    expect(describeWait(job({ present: false, blocked_by: "still_active" })))
      .toBe("Not running on this pond");
    expect(describeWait(job({ registered: false }))).toBe("Starting up");
    expect(describeWait(job({ would_run_next: true }))).toBe("Next to run");
  });

  /// A gate added on the server must not render as "Ready" on an older app.
  /// That direction of failure is the one that misleads the person debugging.
  it("passes an unfamiliar reason through rather than calling it ready", () => {
    const said = describeWait(job({ blocked_by: "some_new_gate" }));
    expect(said).toContain("some_new_gate");
    expect(said).not.toBe("Ready");
  });

  /// "Never" is the most important value on the screen, not a missing one: a
  /// job that has not run on a pond that has been up for hours IS the symptom.
  it("says plainly when a job has never run", () => {
    expect(describeLastRun(null)).toBe("Hasn't run yet");
    expect(describeLastRun(30)).toBe("Ran just now");
    expect(describeLastRun(600)).toBe("Ran 10 minutes ago");
    expect(describeLastRun(7200)).toBe("Ran 2 hours ago");
  });
});

describe("the watcher", () => {
  /// `slot_busy` could only ever say something was running. The point of the
  /// watcher is which one, because "the pond is busy" and "the memory engine
  /// is reading your conversations" tell a household different things.
  it("names the job holding the slot, not just that one is", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(
      lane([job()], {
        slot_busy: true,
        running: "memory_extraction",
        running_title: "Read conversations for memories",
        running_for_secs: 95,
      }),
    );
    render(<BackgroundJobs />);

    const now = await screen.findByText(/Running now/);
    expect(now.textContent).toContain("Read conversations for memories");
    expect(now.textContent).toContain("2 minutes");
  });

  it("says plainly when nothing holds the slot", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(lane([job()], { slot_busy: false, running: null }));
    render(<BackgroundJobs />);
    await screen.findByText(/Nothing is running/);
  });

  /// A pass four minutes in and one that started two seconds ago read
  /// identically without this, and on a small board that difference is whether
  /// something is stuck. Under a few seconds it says nothing rather than
  /// "for 0 seconds" on a line that reprints every five.
  it("says how long, once that is worth saying", () => {
    expect(describeElapsed(null)).toBe("");
    expect(describeElapsed(0)).toBe("");
    expect(describeElapsed(2)).toBe("");
    expect(describeElapsed(20)).toBe(", for 20 seconds");
    expect(describeElapsed(95)).toBe(", for 2 minutes");
    expect(describeElapsed(60)).toBe(", for 1 minute");
  });

  /// A tick that fails must not blank the panel or raise an error over the last
  /// good answer. A watcher that flickers to "could not read" whenever one
  /// request misses is worse than one showing a five-second-old truth.
  it("keeps the last good answer when a poll misses", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(api.laneStatus).mockResolvedValueOnce(
        lane([job({ title: "Name conversations" })], { running: null }),
      );
      render(<BackgroundJobs />);
      await vi.waitFor(() => expect(screen.getByText("Name conversations")).toBeTruthy());

      vi.mocked(api.laneStatus).mockRejectedValue(new Error("offline"));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(6000);
      });

      expect(screen.getByText("Name conversations")).toBeTruthy();
      expect(screen.queryByText(/Could not read/)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  /// `request<T>` can hand back `undefined` or a parsed `index.html` while the
  /// server is starting. A one-shot call meets that rarely; a polled one meets
  /// it every time the pond restarts under a panel left open.
  it("ignores a tick that is not a lane answer", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(api.laneStatus).mockResolvedValueOnce(
        lane([job({ title: "Name conversations" })], { running: null }),
      );
      render(<BackgroundJobs />);
      await vi.waitFor(() => expect(screen.getByText("Name conversations")).toBeTruthy());

      vi.mocked(api.laneStatus).mockResolvedValue(undefined as never);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(6000);
      });

      expect(screen.getByText("Name conversations")).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  /// The panel is left open on a wall for days. An interval that survived the
  /// unmount would keep asking a question nobody is looking at the answer to.
  it("stops asking once it is off the screen", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(api.laneStatus).mockResolvedValue(lane([job()], { running: null }));
      const { unmount } = render(<BackgroundJobs />);
      await vi.waitFor(() => expect(api.laneStatus).toHaveBeenCalled());

      unmount();
      const after = vi.mocked(api.laneStatus).mock.calls.length;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
      });
      expect(vi.mocked(api.laneStatus).mock.calls.length).toBe(after);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("the history beside the instant", () => {
  /// The blind spot this closes. A job that is eligible and losing every
  /// tie-break has `blocked_by: null` — identical on screen to one about to
  /// run — and on a real pond that is the difference between a defect and a
  /// setting.
  it("says a job has been waiting behind another, which the instant cannot", () => {
    const losing = job({
      blocked_by: null,
      granted: 0,
      lost_to_total: 1340,
      lost_to_most: { job: "index_maintenance", times: 1340 },
    });
    expect(describeWait(losing)).toBe("Ready");
    expect(describeHistory(losing)).toContain("waited behind index maintenance 1340×");
  });

  it("says how often it has actually run", () => {
    expect(describeHistory(job({ granted: 7 }))).toContain("ran 7×");
    expect(describeHistory(job({ slot_busy: 3 }))).toContain("found the pond busy 3×");
  });

  /// A row that always carries a clause trains people to stop reading it.
  it("says nothing about a job with no history", () => {
    expect(describeHistory(job())).toBe("");
    expect(describeHistory(job({ granted: 0, lost_to_total: 0, slot_busy: 0 }))).toBe("");
  });
});

describe("the list", () => {
  it("shows every job, including ones with no loop here", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(
      lane([
        job({ job: "titling", title: "Name conversations" }),
        job({ job: "proactive_review", title: "Look for something to suggest", present: false }),
      ]),
    );
    render(<BackgroundJobs />);

    await screen.findByText("Name conversations");
    expect(screen.getByText("Look for something to suggest")).toBeTruthy();
  });

  /// Disabled rather than hidden. A household troubleshooting a pond with no
  /// embedder needs to see that the job exists and is not running here; a
  /// missing row says neither.
  it("offers no button for a job with nothing to wake", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(
      lane([job({ job: "memory_extraction", present: false })]),
    );
    render(<BackgroundJobs />);

    const button = await screen.findByRole("button", { name: "Run now" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it("asks the lane for the job whose button was pressed", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(
      lane([job({ job: "memory_extraction", title: "Read conversations for memories" })]),
    );
    vi.mocked(api.runLaneJob).mockResolvedValue({
      lane: true,
      job: "memory_extraction",
      woken: true,
    });
    render(<BackgroundJobs />);

    fireEvent.click(await screen.findByRole("button", { name: "Run now" }));
    await waitFor(() => expect(api.runLaneJob).toHaveBeenCalledWith("memory_extraction"));
    await screen.findByText("Asked — it runs at its next turn");
  });

  /// The button wakes; it does not run. Saying "Done" would be a claim about
  /// work that has not happened yet, and on a busy slot may not for a while.
  it("promises a turn, not a result", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(lane([job()]));
    vi.mocked(api.runLaneJob).mockResolvedValue({
      lane: true,
      job: "memory_extraction",
      woken: true,
    });
    render(<BackgroundJobs />);

    fireEvent.click(await screen.findByRole("button", { name: "Run now" }));
    const said = await screen.findByRole("status");
    expect(said.textContent).toContain("next turn");
    expect(said.textContent).not.toMatch(/done|finished|complete/i);
  });

  /// `woken: false` is not an error and must not read as one -- a pond with no
  /// embedder genuinely has no extraction loop.
  it("says so when there was nothing to wake", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue(lane([job()]));
    vi.mocked(api.runLaneJob).mockResolvedValue({
      lane: true,
      job: "memory_extraction",
      woken: false,
      reason: "this job has no loop in this process",
    });
    render(<BackgroundJobs />);

    fireEvent.click(await screen.findByRole("button", { name: "Run now" }));
    await screen.findByText("Nothing here to run");
  });

  /// A process with no lane and a lane with no jobs are different facts, and an
  /// empty panel would say the second while meaning the first.
  it("distinguishes a pond with no lane from a lane with nothing to do", async () => {
    vi.mocked(api.laneStatus).mockResolvedValue({ lane: false, jobs: [] });
    render(<BackgroundJobs />);

    await screen.findByText(/not running the background jobs/i);
  });

  /// The list IS the screen: a failed read must not render as "no background
  /// jobs", which is the one thing it must never say by accident.
  it("says the read failed rather than showing an empty list", async () => {
    vi.mocked(api.laneStatus).mockRejectedValue(new Error("offline"));
    render(<BackgroundJobs />);

    // `status` rather than `alert`: this panel shares a page with the settings
    // error banner, and only one thing on a screen gets to interrupt.
    const said = await screen.findByRole("status");
    expect(said.textContent).toContain("offline");
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
