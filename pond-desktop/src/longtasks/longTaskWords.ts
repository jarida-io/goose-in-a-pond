// A long task in the words a household would use, and how the chat finds one in what the prime said.
import type { LongTask, LongTaskAhead, LongTaskArtifact, LongTaskState, LongTaskStep, LongTaskStepState, SessionMessage } from "../api/types";
import { agentLabel } from "../agents/prefillWords";

const STARTED = /Started task ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/i;

/** A result from any other tool can quote the phrase, so a named tool must be `task_start`. */
function isStartTool(tool: unknown): boolean {
  return typeof tool !== "string" || tool === "" || /(^|__)task_start$/.test(tool);
}

/** The id in the text `task_start` answers with ("Started task <uuid>. ..."), or `null` for any other text. */
export function startedTaskId(text: string | null | undefined, tool?: string | null): string | null {
  if (!text || !isStartTool(tool)) return null;
  const match = STARTED.exec(text);
  return match ? match[1].toLowerCase() : null;
}

/** A stored tool row keeps its result as JSON inside `content`; fall back to the text itself. */
export function startedTaskIdOfRow(row: Pick<SessionMessage, "role" | "content">): string | null {
  if (row.role !== "tool") return null;
  try {
    const parsed: unknown = JSON.parse(row.content);
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
      const fields = parsed as Record<string, unknown>;
      if (typeof fields.content === "string") return startedTaskId(fields.content, typeof fields.tool === "string" ? fields.tool : null);
    }
  } catch {
    // Not JSON: the content is the result itself.
  }
  return startedTaskId(row.content);
}

/** A task that does not say what it is for (an older pond) is a general one. */
export function isResearch(task: Pick<LongTask, "kind">): boolean {
  return task.kind === "research";
}

/** What the card calls itself for assistive tech: a research task by its name, any other by what it is. */
export function taskKindLabel(task: Pick<LongTask, "kind">): string {
  return isResearch(task) ? "Deep research" : "Long task";
}

/** The report's own `# Title` line, taken off when the reading view already shows that title above the text. */
export function withoutTitle(markdown: string, title: string): string {
  const heading = /^\s*#\s+([^\n]*)\n+/.exec(markdown);
  return heading && heading[1].trim() === title.trim() ? markdown.slice(heading[0].length) : markdown;
}

export function isEnded(state: LongTaskState): boolean {
  return state === "completed" || state === "failed" || state === "cancelled";
}

/** Planning and running move on their own, so they are asked after often; the rest wait on a person or the pond. */
export function isMoving(state: LongTaskState): boolean {
  return state === "planning" || state === "running";
}

/** Which step the task is on, counting from 1; the one under way, else the first not yet finished. */
export function currentStep(steps: readonly LongTaskStep[]): number {
  if (steps.length === 0) return 0;
  const at = steps.findIndex((s) => s.state === "running" || s.state === "awaiting_approval");
  if (at >= 0) return at + 1;
  const finished = steps.filter((s) => s.state === "completed" || s.state === "skipped" || s.state === "failed").length;
  return Math.min(finished + 1, steps.length);
}

/** "1st", "2nd", "3rd", "11th", "22nd". */
export function ordinal(n: number): string {
  const tens = n % 100;
  if (tens >= 11 && tens <= 13) return `${n}th`;
  switch (n % 10) {
    case 1:
      return `${n}st`;
    case 2:
      return `${n}nd`;
    case 3:
      return `${n}rd`;
    default:
      return `${n}th`;
  }
}

/** An open task with another ahead of it in the one line the pond works through. */
export function isWaiting(task: Pick<LongTask, "state" | "queue">): boolean {
  return !isEnded(task.state) && (task.queue?.position ?? 1) > 1;
}

/** Where a task stands in the line: the one under way, or its place. */
export function placeWords(position: number): string {
  return position <= 1 ? "Working now" : `${ordinal(position)} in line`;
}

export function isOwnAhead(ahead: LongTaskAhead): ahead is Extract<LongTaskAhead, { id: string }> {
  return "id" in ahead;
}

/** Who is ahead, without naming another member's task: "Ahead of this one: 2 of your tasks and 1 for another member." */
export function aheadWords(ahead: readonly LongTaskAhead[]): string {
  const yours = ahead.filter(isOwnAhead).length;
  const others = ahead.length - yours;
  const parts: string[] = [];
  if (yours > 0) parts.push(yours === 1 ? "1 of your tasks" : `${yours} of your tasks`);
  if (others > 0) parts.push(others === 1 ? "1 for another member" : `${others} for other members`);
  return `Ahead of this one: ${parts.join(" and ")}.`;
}

export function taskWords(task: Pick<LongTask, "state" | "steps"> & Partial<Pick<LongTask, "queue">>): string {
  if (isWaiting({ state: task.state, queue: task.queue })) return `Waiting its turn: ${placeWords(task.queue?.position ?? 1)}`;
  switch (task.state) {
    case "planning":
      return "Planning";
    case "running":
      return task.steps.length > 0 ? `Working: step ${currentStep(task.steps)} of ${task.steps.length}` : "Working";
    case "awaiting_approval":
      return "Waiting for your approval";
    case "paused":
      return "Paused";
    case "completed":
      return "Done";
    case "failed":
      return "Couldn't finish";
    case "cancelled":
      return "Stopped";
  }
}

const STEP_WORDS: Record<LongTaskStepState, string> = {
  pending: "Waiting",
  running: "Working",
  completed: "Done",
  failed: "Couldn't finish",
  skipped: "Skipped",
  awaiting_approval: "Waiting for approval",
};

export function stepWords(state: LongTaskStepState): string {
  return STEP_WORDS[state] ?? state;
}

/** The agent on a step: a built-in by its job, anything else by its recipe name. */
export function stepAgent(step: Pick<LongTaskStep, "role">): string {
  return agentLabel(step.role);
}

/** What the task made as its result, or `null` when it has made none (yet). */
export function resultOf(task: Pick<LongTask, "artifacts" | "result_artifact_id">): LongTaskArtifact | null {
  if (!task.result_artifact_id) return null;
  return task.artifacts.find((a) => a.id === task.result_artifact_id) ?? null;
}

/** A finished short answer's text, which the card shows whole; `null` for a report or an older pond. */
export function shortAnswerOf(task: Pick<LongTask, "answer" | "answer_text">): string | null {
  if (task.answer !== "short") return null;
  const text = task.answer_text?.trim();
  return text ? text : null;
}

/** The one line a finished task shows: the result's summary, else its title. */
export function resultLine(artifact: LongTaskArtifact): string {
  return artifact.summary.trim() || artifact.title.trim();
}

/** Tasks the conversation started that no message in it points at, newest first. */
export function strayTasks(tasks: Readonly<Record<string, LongTask>>, anchored: ReadonlySet<string>): LongTask[] {
  return Object.values(tasks)
    .filter((task) => !anchored.has(task.id))
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
}
