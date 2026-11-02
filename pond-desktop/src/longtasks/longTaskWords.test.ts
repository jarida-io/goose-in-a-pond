import { describe, it, expect } from "vitest";
import {
  aheadWords,
  currentStep,
  isWaiting,
  ordinal,
  placeWords,
  isEnded,
  isMoving,
  isResearch,
  resultLine,
  resultOf,
  startedTaskId,
  startedTaskIdOfRow,
  stepAgent,
  stepWords,
  strayTasks,
  taskKindLabel,
  taskWords,
} from "./longTaskWords";
import { artifactOf, doneTask, OTHER_ID, STARTED_TEXT, stepOf, TASK_ID, taskOf } from "./longTaskFixtures";

describe("finding the task a tool result started", () => {
  it("takes the id out of the text task_start answers with", () => {
    expect(startedTaskId(STARTED_TEXT)).toBe(TASK_ID);
  });

  it("lowercases the id so it matches the one the pond lists", () => {
    expect(startedTaskId(`Started task ${TASK_ID.toUpperCase()}.`)).toBe(TASK_ID);
  });

  it("finds nothing in other text, or none", () => {
    expect(startedTaskId("Task list is empty")).toBeNull();
    expect(startedTaskId("Started task soon")).toBeNull();
    expect(startedTaskId(undefined)).toBeNull();
    expect(startedTaskId("")).toBeNull();
  });

  it("ignores the phrase from a tool that is not task_start", () => {
    expect(startedTaskId(STARTED_TEXT, "giap-knowledge__search_web")).toBeNull();
    expect(startedTaskId(STARTED_TEXT, "giap-tasks__task_start")).toBe(TASK_ID);
    expect(startedTaskId(STARTED_TEXT, "task_start")).toBe(TASK_ID);
    expect(startedTaskId(STARTED_TEXT, "")).toBe(TASK_ID);
  });

  it("reads a stored tool row, which keeps its result as JSON inside content", () => {
    const row = {
      role: "tool" as const,
      content: JSON.stringify({ tool: "giap-tasks__task_start", content: STARTED_TEXT }),
    };
    expect(startedTaskIdOfRow(row)).toBe(TASK_ID);
  });

  it("reads a tool row whose content is the text itself", () => {
    expect(startedTaskIdOfRow({ role: "tool", content: STARTED_TEXT })).toBe(TASK_ID);
  });

  it("skips a stored row of another tool, and rows that are not tool rows", () => {
    const other = JSON.stringify({ tool: "giap-knowledge__search_web", content: STARTED_TEXT });
    expect(startedTaskIdOfRow({ role: "tool", content: other })).toBeNull();
    expect(startedTaskIdOfRow({ role: "assistant", content: STARTED_TEXT })).toBeNull();
  });
});

describe("finding the task the pond itself started for deep research", () => {
  const POND_TEXT = `Started task ${TASK_ID}. It runs in the background and will report here when it is done.`;
  const POND_TOOL = "giap-tasks__task_start";

  it("reads the text of a result the pond made, as it reads a model's", () => {
    expect(startedTaskId(POND_TEXT, POND_TOOL)).toBe(TASK_ID);
  });

  it("reads a stored row whose call id is the pond's own, not a uuid the task is called by", () => {
    const row = {
      role: "tool" as const,
      content: JSON.stringify({ tool_call_id: `deep-research-${OTHER_ID}`, tool: POND_TOOL, content: POND_TEXT }),
    };
    expect(startedTaskIdOfRow(row)).toBe(TASK_ID);
  });
});

describe("what a task is for", () => {
  it("calls a research task Deep research, for assistive tech", () => {
    expect(isResearch(taskOf({ kind: "research" }))).toBe(true);
    expect(taskKindLabel(taskOf({ kind: "research" }))).toBe("Deep research");
  });

  it("counts a general task, and one from a pond that does not say, as a long task", () => {
    expect(isResearch(taskOf({ kind: "general" }))).toBe(false);
    expect(isResearch(taskOf())).toBe(false);
    expect(taskKindLabel(taskOf({ kind: "general" }))).toBe("Long task");
    expect(taskKindLabel(taskOf())).toBe("Long task");
  });
});

describe("a task in plain words", () => {
  it.each([
    ["planning", "Planning"],
    ["awaiting_approval", "Waiting for your approval"],
    ["paused", "Paused"],
    ["completed", "Done"],
    ["failed", "Couldn't finish"],
    ["cancelled", "Stopped"],
  ] as const)("says %s as %s", (state, words) => {
    expect(taskWords(taskOf({ state }))).toBe(words);
  });

  it("counts the step a running task is on", () => {
    expect(taskWords(taskOf())).toBe("Working: step 2 of 4");
  });

  it("says only Working for a running task with no steps yet", () => {
    expect(taskWords(taskOf({ steps: [] }))).toBe("Working");
  });

  it("moves on to the first step not finished when none is running", () => {
    const steps = [stepOf("a", { state: "completed" }), stepOf("b", { state: "pending" }), stepOf("c")];
    expect(currentStep(steps)).toBe(2);
  });

  it("never counts past the last step", () => {
    const steps = [stepOf("a", { state: "completed" }), stepOf("b", { state: "skipped" })];
    expect(currentStep(steps)).toBe(2);
    expect(currentStep([])).toBe(0);
  });

  it("names each step state", () => {
    expect(stepWords("pending")).toBe("Waiting");
    expect(stepWords("running")).toBe("Working");
    expect(stepWords("completed")).toBe("Done");
    expect(stepWords("failed")).toBe("Couldn't finish");
    expect(stepWords("skipped")).toBe("Skipped");
    expect(stepWords("awaiting_approval")).toBe("Waiting for approval");
  });

  it("names a built-in agent by its job and any other by the name it was saved under", () => {
    expect(stepAgent({ role: "long-task-planner" })).toBe("Point Gander");
    expect(stepAgent({ role: "study-planner" })).toBe("study-planner");
  });
});

describe("which states keep being asked after", () => {
  it("asks after planning and running often, and ends on the three terminal states", () => {
    expect(isMoving("planning")).toBe(true);
    expect(isMoving("running")).toBe(true);
    expect(isMoving("paused")).toBe(false);
    expect(isMoving("awaiting_approval")).toBe(false);
    for (const state of ["completed", "failed", "cancelled"] as const) expect(isEnded(state)).toBe(true);
    for (const state of ["planning", "running", "paused", "awaiting_approval"] as const) expect(isEnded(state)).toBe(false);
  });
});

describe("the result of a task", () => {
  it("is the artifact the task points at", () => {
    expect(resultOf(doneTask())?.id).toBe("art-1");
  });

  it("is none before the task has pointed at one, or when the artifact is not listed", () => {
    expect(resultOf(taskOf())).toBeNull();
    expect(resultOf(taskOf({ result_artifact_id: "gone", artifacts: [] }))).toBeNull();
  });

  it("shows the summary, else the title", () => {
    expect(resultLine(artifactOf())).toBe("Three days in Lamu, with ferry times.");
    expect(resultLine(artifactOf({ summary: "  " }))).toBe("Trip plan");
  });
});

describe("tasks no message points at", () => {
  it("lists them newest first and leaves out the anchored ones", () => {
    const tasks = {
      [TASK_ID]: taskOf({ created_at: "2026-10-28T09:00:00Z" }),
      [OTHER_ID]: taskOf({ id: OTHER_ID, created_at: "2026-10-28T10:00:00Z" }),
      third: taskOf({ id: "third", created_at: "2026-10-28T08:00:00Z" }),
    };
    expect(strayTasks(tasks, new Set(["third"])).map((t) => t.id)).toEqual([OTHER_ID, TASK_ID]);
  });
});

describe("a task waiting its turn", () => {
  const second = { position: 2, of: 3, ahead: [{ id: "a", goal: "Plan the trip", state: "running" as const }] };

  it("says where it is in line instead of saying it is planning", () => {
    expect(taskWords(taskOf({ state: "planning", steps: [], queue: second }))).toBe("Waiting its turn: 2nd in line");
    expect(isWaiting(taskOf({ state: "planning", queue: second }))).toBe(true);
  });

  it("keeps its own words at the front of the line, once ended, and on an older pond", () => {
    const front = { position: 1, of: 2, ahead: [] };
    expect(taskWords(taskOf({ state: "planning", steps: [], queue: front }))).toBe("Planning");
    expect(isWaiting(taskOf({ state: "cancelled", queue: second }))).toBe(false);
    expect(isWaiting(taskOf({ state: "planning", queue: undefined }))).toBe(false);
  });

  it("counts places as people say them", () => {
    expect([1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101].map(ordinal)).toEqual([
      "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "101st",
    ]);
    expect(placeWords(1)).toBe("Working now");
    expect(placeWords(3)).toBe("3rd in line");
  });

  it("says who is ahead without naming another member's task", () => {
    const mine = { id: "a", goal: "Plan the trip", state: "running" as const };
    expect(aheadWords([mine])).toBe("Ahead of this one: 1 of your tasks.");
    expect(aheadWords([mine, { another_member: true }, mine])).toBe("Ahead of this one: 2 of your tasks and 1 for another member.");
    expect(aheadWords([{ another_member: true }, { another_member: true }])).toBe("Ahead of this one: 2 for other members.");
  });
});
