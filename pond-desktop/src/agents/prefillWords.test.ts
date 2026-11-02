import { describe, expect, it } from "vitest";
import type { PrefillAgent } from "../api/types";
import {
  agentLabel,
  agoWords,
  buildLine,
  daysSince,
  isWorking,
  prefillSummary,
  prefillTimes,
  prefillWords,
} from "./prefillWords";

const NOW = new Date("2026-10-28T12:00:00Z");
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const before = (ms: number) => new Date(NOW.getTime() - ms).toISOString();

function row(over: Partial<PrefillAgent> = {}): PrefillAgent {
  return { agent: "study-planner", kind: "member", status: "ready", compiled_at: before(2 * DAY), last_used_at: before(3 * HOUR), ...over };
}

describe("naming an agent", () => {
  it("names the built-ins by their job, and anything else by its own name", () => {
    expect(agentLabel("long-task-planner")).toBe("Point Gander");
    expect(agentLabel("long-task-worker")).toBe("Forager Gosling");
    expect(agentLabel("long-task-verifier")).toBe("Sentinel Goose");
    expect(agentLabel("study-planner")).toBe("study-planner");
  });
});

describe("how long ago", () => {
  it("says it in the largest unit that fits", () => {
    expect(agoWords(before(20_000), NOW)).toBe("just now");
    expect(agoWords(before(MIN), NOW)).toBe("1 minute ago");
    expect(agoWords(before(5 * MIN), NOW)).toBe("5 minutes ago");
    expect(agoWords(before(HOUR), NOW)).toBe("1 hour ago");
    expect(agoWords(before(3 * HOUR), NOW)).toBe("3 hours ago");
    expect(agoWords(before(DAY), NOW)).toBe("1 day ago");
    expect(agoWords(before(20 * DAY), NOW)).toBe("20 days ago");
    expect(agoWords(before(45 * DAY), NOW)).toBe("1 month ago");
    expect(agoWords(before(70 * DAY), NOW)).toBe("2 months ago");
  });

  it("says just now for a time ahead of the clock, and nothing for no time or a bad one", () => {
    expect(agoWords(new Date(NOW.getTime() + HOUR).toISOString(), NOW)).toBe("just now");
    expect(agoWords(null, NOW)).toBeNull();
    expect(agoWords("not a time", NOW)).toBeNull();
  });

  it("counts whole days", () => {
    expect(daysSince(before(20 * DAY + 5 * HOUR), NOW)).toBe(20);
    expect(daysSince(before(HOUR), NOW)).toBe(0);
    expect(daysSince(null, NOW)).toBeNull();
    expect(daysSince("nonsense", NOW)).toBeNull();
  });
});

describe("a start-up copy in plain words", () => {
  it("says each state the way the household would", () => {
    expect(prefillWords(row({ status: "ready" }), NOW)).toBe("Ready");
    expect(prefillWords(row({ status: "building" }), NOW)).toBe("Getting ready");
    expect(prefillWords(row({ status: "queued" }), NOW)).toBe("Waiting to be rebuilt");
    expect(prefillWords(row({ status: "not_built", compiled_at: null, last_used_at: null }), NOW)).toBe("Not built yet");
    expect(prefillWords(row({ status: "failed" }), NOW)).toBe("Couldn't build");
  });

  it("says how long a resting copy has gone unused, and that its next run rebuilds it", () => {
    expect(prefillWords(row({ status: "idle", last_used_at: before(20 * DAY) }), NOW)).toBe(
      "Resting: unused for 20 days, rebuilt on its next run",
    );
    expect(prefillWords(row({ status: "idle", last_used_at: before(DAY) }), NOW)).toBe(
      "Resting: unused for 1 day, rebuilt on its next run",
    );
  });

  it("counts a copy that was never used from when it was built", () => {
    expect(prefillWords(row({ status: "idle", last_used_at: null, compiled_at: before(30 * DAY) }), NOW)).toBe(
      "Resting: unused for 30 days, rebuilt on its next run",
    );
    expect(prefillWords(row({ status: "idle", last_used_at: null, compiled_at: null }), NOW)).toBe(
      "Resting: unused for a while, rebuilt on its next run",
    );
  });

  it("says when it was built and last used", () => {
    expect(prefillTimes(row(), NOW)).toBe("Built 2 days ago. Last used 3 hours ago.");
    expect(prefillTimes(row({ last_used_at: null }), NOW)).toBe("Built 2 days ago. Not used yet.");
    expect(prefillTimes(row({ compiled_at: null }), NOW)).toBe("Last used 3 hours ago.");
    expect(prefillTimes(row({ compiled_at: null, last_used_at: null }), NOW)).toBeNull();
  });

  it("knows when the pond is already on it", () => {
    expect(isWorking("building")).toBe(true);
    expect(isWorking("queued")).toBe(true);
    for (const status of ["ready", "idle", "not_built", "failed"] as const) expect(isWorking(status)).toBe(false);
  });
});

describe("Home's line while the pond builds", () => {
  it("says how far along it is and what it is on", () => {
    expect(buildLine({ active: true, current: "study-planner", done: 2, total: 5 })).toBe(
      "Getting agents ready (2 of 5): study-planner",
    );
    expect(buildLine({ active: true, current: "long-task-planner", done: 0, total: 4 })).toBe(
      "Getting agents ready (0 of 4): Point Gander",
    );
  });

  it("says less when it does not know more", () => {
    expect(buildLine({ active: true, current: null, done: 1, total: 3 })).toBe("Getting agents ready (1 of 3)");
    expect(buildLine({ active: true, current: "x", done: 0, total: 0 })).toBe("Getting agents ready: x");
    expect(buildLine({ active: true, current: null, done: 0, total: 0 })).toBe("Getting agents ready");
  });

  it("says nothing when it is not building", () => {
    expect(buildLine({ active: false, current: "x", done: 2, total: 5 })).toBeNull();
  });
});

describe("one sentence over every agent", () => {
  const statuses = (...list: PrefillAgent["status"][]) => list.map((status, i) => row({ agent: `a${i}`, status }));

  it("says all are ready, or that the one is", () => {
    expect(prefillSummary(statuses("ready", "ready", "ready"))).toBe("All 3 agents are ready.");
    expect(prefillSummary(statuses("ready"))).toBe("The agent is ready.");
  });

  it("counts what is not ready, in words", () => {
    expect(prefillSummary(statuses("ready", "ready", "ready", "building", "queued"))).toBe(
      "3 of 5 ready. 1 getting ready, 1 waiting to be rebuilt.",
    );
    expect(prefillSummary(statuses("ready", "not_built", "failed", "idle", "idle"))).toBe(
      "1 of 5 ready. 1 not built yet, 1 couldn't be built, 2 resting.",
    );
  });

  it("says nothing when there are no agents", () => {
    expect(prefillSummary([])).toBeNull();
  });
});

describe("an agent a plan made for one step", () => {
  it("shows by its own name, marked as made for the task", () => {
    expect(agentLabel("new:Fare Checker")).toBe("Fare Checker (made for this task)");
    expect(agentLabel("new-ish")).toBe("new-ish");
  });
});

