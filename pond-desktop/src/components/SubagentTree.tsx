import { CornerDownRight, Wrench } from "lucide-react";
import type { ChatEvent, SubagentStatus } from "../api/types";

/** Draws `subagent_progress` frames as a tree. Folds only here (`applySubagentProgress`), groups
 *  by `task_id` (a role can repeat), and renders WHILE streaming, unlike other message notes. */

export interface SubagentRun {
  taskId: string;
  role: string;
  status: SubagentStatus;
  /** Tool names, in the order the child called them. */
  tools: string[];
  /** The pond's own reason, when a run failed. Never the child's text. */
  detail?: string;
}

/** Folds a `subagent_progress` frame into a turn's runs; other events return `runs` as-is. */
export function applySubagentProgress(
  runs: SubagentRun[],
  ev: ChatEvent,
): SubagentRun[] {
  if (ev.type !== "subagent_progress" || !ev.task_id || !ev.status) return runs;

  const taskId = ev.task_id;
  const at = runs.findIndex((run) => run.taskId === taskId);
  const existing: SubagentRun = runs[at] ?? {
    taskId,
    role: ev.role ?? "subagent",
    status: ev.status,
    tools: [],
  };

  // `tool` frames only append to `tools`. Other statuses overwrite `detail` (never merge): it is
  // why a run ended, and nothing else clears a stale one.
  const next: SubagentRun =
    ev.status === "tool"
      ? {
          ...existing,
          tools: ev.detail ? [...existing.tools, ev.detail] : existing.tools,
        }
      : { ...existing, status: ev.status, detail: ev.detail };

  if (at < 0) return [...runs, next];
  const out = [...runs];
  out[at] = next;
  return out;
}

/** Plain language for a status, and a miss shows up as the raw value. */
const STATUS_TEXT: Record<SubagentStatus, string> = {
  queued: "waiting its turn",
  running: "working",
  tool: "working",
  completed: "done",
  cancelled: "stopped",
  turn_budget_exhausted: "ran out of steps",
  failed: "could not finish",
};

/** The last segment of an `extension__tool` name, which is what a person reads. */
function bareToolName(name: string): string {
  const bare = name.includes("__") ? name.split("__").pop()! : name;
  return bare.replace(/_/g, " ");
}

export function SubagentTree({ runs }: { runs: SubagentRun[] }) {
  if (runs.length === 0) return null;
  return (
    <div className="subagent-tree" role="list" aria-label="Delegated tasks">
      {runs.map((run) => (
        <div className="subagent-tree__run" role="listitem" key={run.taskId}>
          <span className="subagent-tree__head">
            <CornerDownRight size={11} aria-hidden />
            <span className="subagent-tree__role">{run.role}</span>
            <span className="subagent-tree__status">
              {STATUS_TEXT[run.status] ?? run.status}
            </span>
          </span>
          {run.tools.length > 0 && (
            <span className="subagent-tree__tools">
              {run.tools.map((tool, i) => (
                <span className="subagent-tree__tool" key={`${tool}-${i}`}>
                  <Wrench size={10} aria-hidden />
                  {bareToolName(tool)}
                </span>
              ))}
            </span>
          )}
          {run.detail && (
            <span className="subagent-tree__detail">{run.detail}</span>
          )}
        </div>
      ))}
    </div>
  );
}
