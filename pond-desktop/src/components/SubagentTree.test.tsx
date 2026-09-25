import { afterEach, describe, expect, it } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { SubagentTree, applySubagentProgress } from "./SubagentTree";
import type { SubagentRun } from "./SubagentTree";
import type { ChatEvent, SubagentStatus } from "../api/types";

afterEach(cleanup);

const SRC_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

/** A `subagent_progress` frame exactly as `routes.rs` serialises it (`TurnAccumulator::absorb`). */
function frame(
  status: SubagentStatus,
  extra: Partial<ChatEvent> = {},
): ChatEvent {
  return {
    type: "subagent_progress",
    task_id: "task-1",
    role: "researcher",
    status,
    ...extra,
  };
}

function fold(events: ChatEvent[]): SubagentRun[] {
  return events.reduce<SubagentRun[]>(applySubagentProgress, []);
}

describe("applySubagentProgress", () => {
  it("follows one run from queued to done", () => {
    const runs = fold([frame("queued"), frame("running"), frame("completed")]);
    expect(runs).toHaveLength(1);
    expect(runs[0].status).toBe("completed");
    expect(runs[0].role).toBe("researcher");
  });

  it("takes a tool frame as a tool, not as where the run has got to", () => {
    // A real delegation's frames, in order; `detail` is why a run ended, so tools must not set it.
    const runs = fold([
      frame("queued"),
      frame("running"),
      frame("tool", { detail: "giap-weather__get_forecast" }),
      frame("completed"),
    ]);
    expect(runs[0].status).toBe("completed");
    expect(runs[0].tools).toEqual(["giap-weather__get_forecast"]);
    expect(runs[0].detail).toBeUndefined();
  });

  it("groups by task id, so the same role delegated twice is two runs", () => {
    const runs = fold([
      frame("running"),
      frame("running", { task_id: "task-2" }),
      frame("tool", { task_id: "task-2", detail: "giap-memory__recall_memories" }),
    ]);
    expect(
      runs.map((run) => run.taskId),
      "one turn may delegate the same role twice; grouping by name merges two " +
        "runs into one node whose status flickers between them",
    ).toEqual(["task-1", "task-2"]);
    expect(runs[0].tools).toEqual([]);
    expect(runs[1].tools).toEqual(["giap-memory__recall_memories"]);
  });

  it("keeps every tool call, in order, including a repeat", () => {
    const runs = fold([
      frame("running"),
      frame("tool", { detail: "giap-weather__get_forecast" }),
      frame("tool", { detail: "giap-memory__recall_memories" }),
      frame("tool", { detail: "giap-weather__get_forecast" }),
    ]);
    expect(runs[0].tools).toEqual([
      "giap-weather__get_forecast",
      "giap-memory__recall_memories",
      "giap-weather__get_forecast",
    ]);
  });

  it("clears a run's reason when a later frame gives it none", () => {
    // Catches the `detail: ev.detail ?? existing.detail` mutation; the render has no second gate.
    const runs = fold([
      frame("running"),
      frame("failed", { detail: "subagent produced no answer" }),
      frame("completed"),
    ]);
    expect(
      runs[0].detail,
      "a completed run kept the sentence from an earlier failure, so the tree " +
        "captions a success with an error",
    ).toBeUndefined();
  });

  it("ignores an event that is not a progress frame", () => {
    const runs = fold([
      { type: "text", content: "hello" },
      { type: "subagent_progress" },
      frame("running"),
    ]);
    expect(
      runs,
      "every event on the stream is handed to this reducer, so anything " +
        "without a task id or a status must pass straight through",
    ).toHaveLength(1);
  });
});

describe("SubagentTree", () => {
  it("names the role, says where it has got to, and lists the tools", () => {
    const runs = fold([
      frame("running"),
      frame("tool", { detail: "giap-weather__get_forecast" }),
    ]);
    render(<SubagentTree runs={runs} />);
    const text = document.querySelector(".subagent-tree")!.textContent ?? "";
    expect(text).toContain("researcher");
    expect(text).toContain("working");
    // The extension prefix (`giap-weather__`) is stripped for display.
    expect(text).toContain("get forecast");
  });

  it("shows the pond's reason for a failure", () => {
    const failed = fold([
      frame("running"),
      frame("failed", { detail: "subagent produced no answer" }),
    ]);
    render(<SubagentTree runs={failed} />);
    expect(document.body.textContent).toContain("subagent produced no answer");
  });

  it("renders nothing at all when a turn delegated nothing", () => {
    const { container } = render(<SubagentTree runs={[]} />);
    expect(
      container.innerHTML,
      "an empty tree must not reserve space on every ordinary turn",
    ).toBe("");
  });
});

/** A cheap grep tripwire, not coverage: neither chat surface can mount a whole SSE stream. It
 *  catches gating the tree on `!streaming`, which would hide it during a `delegate` call. */
describe("subagent_progress has a consumer", () => {
  it("the shared turn driver folds the frame through the one reducer", () => {
    const src = readFileSync(join(SRC_DIR, "state/chatRunStore.ts"), "utf8");
    expect(
      src.includes('ev.type === "subagent_progress"'),
      "the frame has no consumer in the turn driver - a delegating turn is " +
        "then a spinner for the whole of its child's run, on every surface",
    ).toBe(true);
    expect(
      src.includes("applySubagentProgress("),
      "the driver folds progress frames some other way than through the one " +
        "shared reducer",
    ).toBe(true);
  });

  for (const surface of ["hub/views/ChatHub.tsx", "sections/Chat.tsx"]) {
    it(`${surface} renders the tree while the turn is still streaming`, () => {
      const src = readFileSync(join(SRC_DIR, surface), "utf8");
      const render = src.slice(src.indexOf("<SubagentTree"));
      expect(
        render.startsWith("<SubagentTree"),
        `${surface} is handed delegations on the message but renders nothing`,
      ).toBe(true);
      const guard = src.slice(
        src.lastIndexOf("{", src.indexOf("<SubagentTree")),
        src.indexOf("<SubagentTree"),
      );
      expect(
        /!\s*\w*\.?streaming/.test(guard),
        `${surface} gates the delegation tree on the turn having finished. The ` +
          "tree exists for the minutes a turn is blocked inside a delegate tool " +
          "call; hidden until the turn ends it is the spinner it replaces",
      ).toBe(false);
    });
  }

  it("the frame type and its statuses are in the typed union", () => {
    const src = readFileSync(join(SRC_DIR, "api/types.ts"), "utf8");
    const union = src.match(/export type ChatEventType =[^;]+;/)?.[0] ?? "";
    expect(
      union.includes('"subagent_progress"'),
      "subagent_progress is absent from ChatEventType, so every consumer is " +
        "reading an untyped frame",
    ).toBe(true);

    const statuses = src.match(/export type SubagentStatus =[^;]+;/)?.[0] ?? "";
    // pond-core's seven variants; one the client can't name renders raw in the tree.
    for (const status of [
      "queued",
      "running",
      "tool",
      "completed",
      "cancelled",
      "turn_budget_exhausted",
      "failed",
    ]) {
      expect(
        statuses.includes(`"${status}"`),
        `SubagentStatus is missing "${status}", which pond-core can send`,
      ).toBe(true);
    }
  });
});
