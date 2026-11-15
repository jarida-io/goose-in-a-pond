import { describe, expect, it } from "vitest";
import type { ReliabilityReport } from "../api/types";
import { reliabilityLines, share } from "./reliabilityView";

const report: ReliabilityReport = {
  days: 7,
  notices: { queued: 20, woken: 12, fetched: 18, fetched_within_a_minute: 9, fetched_after_an_hour: 2, shown: 16, opened: 8, spoken: 1 },
  turns: { asked: 30, answered: 29 },
  phones: { paired: 3, test: 1, unseen: 1, with_push_token: 2 },
  stops: 4,
  android_push_key: true,
  apple_push_key: false,
};

const line = (label: string) => reliabilityLines(report).find((l) => l.label === label)!;

describe("reliabilityLines", () => {
  it("says it in plain words with the share", () => {
    expect(line("Fetched within a minute").value).toBe("9 of 20 (45%)");
    expect(line("Left without a reply").value).toBe("1");
    expect(line("Apple push key").value).toBe("missing");
    expect(line("Android push key").value).toBe("present");
  });
  it("draws failures heavier and the rest plain", () => {
    expect(line("Apple push key").problem).toBe(true);
    expect(line("Android push key").problem).toBe(false);
    expect(line("Left without a reply").problem).toBe(true);
  });
  it("never goes below zero unanswered", () => {
    const odd = { ...report, turns: { asked: 1, answered: 2 } };
    expect(reliabilityLines(odd).find((l) => l.label === "Left without a reply")!.value).toBe("0");
  });
});

describe("share", () => {
  it("shows the bare count when there is nothing to divide by", () => {
    expect(share(0, 0)).toBe("0");
    expect(share(1, 3)).toBe("1 of 3 (33%)");
  });
});
