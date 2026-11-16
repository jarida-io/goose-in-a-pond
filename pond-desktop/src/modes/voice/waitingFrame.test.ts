import { describe, it, expect } from "vitest";
import { waitingLabel } from "./waitingFrame";

describe("waitingLabel", () => {
  it("shows the message of a waiting status frame", () => {
    expect(waitingLabel({ type: "status", content: "Your pond is busy.", waiting: "another_conversation" })).toBe("Your pond is busy.");
  });

  it("clears on a status without waiting, and on text, thinking, tool_call, error and done", () => {
    for (const ev of [
      { type: "status", content: "Agent working..." },
      { type: "text", content: "Hi" },
      { type: "thinking", content: "hm" },
      { type: "tool_call", tool: "t" },
      { type: "error", error: "x" },
      { type: "done", done: true },
    ]) {
      expect(waitingLabel(ev)).toBeNull();
    }
  });

  it("leaves the label alone for frames that are neither", () => {
    expect(waitingLabel({ type: "run_started" })).toBeUndefined();
    expect(waitingLabel({ type: "turn_stats" })).toBeUndefined();
  });

  it("ignores an unknown reason", () => {
    expect(waitingLabel({ type: "status", content: "x", waiting: "bogus" })).toBeNull();
  });
});
