// Shapes the long-task routes answer with, for tests: a task, its steps and its result.
import type { LongTask, LongTaskArtifact, LongTaskStep } from "../api/types";
import type { LongTasksState } from "./useLongTasks";

export const TASK_ID = "0b6f1c1e-52a1-4c55-9a53-3d3a8a1f7a10";
export const OTHER_ID = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
export const SESSION = "sess-1";

export function stepOf(key: string, over: Partial<LongTaskStep> = {}): LongTaskStep {
  return { key, role: "long-task-worker", state: "pending", attempts: 0, summary: null, failure: null, ...over };
}

export function artifactOf(over: Partial<LongTaskArtifact> = {}): LongTaskArtifact {
  return {
    id: "art-1",
    task_id: TASK_ID,
    step_key: "write",
    kind: "result",
    title: "Trip plan",
    summary: "Three days in Lamu, with ferry times.",
    tokens: 420,
    created_at: "2026-10-28T09:10:00Z",
    ...over,
  };
}

export function taskOf(over: Partial<LongTask> = {}): LongTask {
  return {
    id: TASK_ID,
    goal: "Plan a trip to Lamu",
    state: "running",
    origin_session_id: SESSION,
    created_at: "2026-10-28T09:00:00Z",
    updated_at: "2026-10-28T09:01:00Z",
    finished_at: null,
    steps: [
      stepOf("plan", { role: "long-task-planner", state: "completed", attempts: 1, summary: "Split it into three parts." }),
      stepOf("research", { state: "running", attempts: 1 }),
      stepOf("write", { state: "pending" }),
      stepOf("check", { role: "long-task-verifier", state: "pending" }),
    ],
    artifacts: [],
    result_artifact_id: null,
    ...over,
  };
}

export function doneTask(over: Partial<LongTask> = {}): LongTask {
  return taskOf({
    state: "completed",
    finished_at: "2026-10-28T09:10:00Z",
    steps: [
      stepOf("plan", { role: "long-task-planner", state: "completed", attempts: 1, summary: "Split it into three parts." }),
      stepOf("write", { state: "completed", attempts: 1, summary: "Wrote the plan." }),
    ],
    artifacts: [artifactOf()],
    result_artifact_id: "art-1",
    ...over,
  });
}

export const STARTED_TEXT = `Started task ${TASK_ID}. It runs in the background and will report here when it is done.`;

export function fakeLongTasks(over: Partial<LongTasksState> = {}): LongTasksState {
  return {
    tasks: {},
    supported: true,
    gone: {},
    unreadable: {},
    stopping: {},
    stopErrors: {},
    stop: async () => {},
    retry: () => {},
    ...over,
  };
}
