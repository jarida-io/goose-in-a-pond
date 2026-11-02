import { useId, useState } from "react";
import { AlertCircle } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError, type LongTask } from "../api/types";
import { aheadWords, isOwnAhead, isWaiting, placeWords } from "./longTaskWords";
import type { LongTasksState } from "./useLongTasks";

interface Row {
  id: string;
  goal: string;
  where: string;
  isThis: boolean;
}

interface Outcome {
  stopped: number;
  failed: string[];
}

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * The line a waiting task stands in, with a choice for each of the member's own tasks in it: keep it
 * going, or stop it. Another member's task is only a place in the line, with nothing to choose.
 */
export function LongTaskQueue({ task, longTasks }: { task: LongTask; longTasks: LongTasksState }) {
  const [stopPicked, setStopPicked] = useState<Readonly<Record<string, true>>>({});
  const [applying, setApplying] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const name = useId();
  const queue = task.queue;
  if (!queue || !isWaiting(task)) return null;

  const ahead: Row[] = queue.ahead.flatMap((entry, index) =>
    isOwnAhead(entry) ? [{ id: entry.id, goal: entry.goal, where: placeWords(index + 1), isThis: false }] : [],
  );
  const rows: Row[] = ahead.length > 0 ? [...ahead, { id: task.id, goal: task.goal, where: placeWords(queue.position), isThis: true }] : [];
  const picked = rows.filter((row) => stopPicked[row.id]);

  function choose(id: string, stop: boolean) {
    setOutcome(null);
    setStopPicked((all) => {
      if (stop) return { ...all, [id]: true };
      const { [id]: _kept, ...rest } = all;
      return rest;
    });
  }

  async function stopPickedTasks() {
    setApplying(true);
    setOutcome(null);
    const failed: string[] = [];
    for (const row of picked) {
      try {
        await api.cancelLongTask(row.id);
      } catch (e) {
        // A 409 means it had already ended, which is what was asked for.
        if (!(e instanceof ApiError && e.status === 409)) failed.push(`${row.goal}: ${reasonOf(e)}`);
      }
    }
    setStopPicked({});
    setApplying(false);
    setOutcome({ stopped: picked.length - failed.length, failed });
    // Read again only what this chat shows: a task stopped from the line may belong to another conversation.
    for (const id of [task.id, ...picked.map((row) => row.id)]) {
      if (longTasks.tasks[id]) longTasks.retry(id);
    }
  }

  return (
    <section className="lt__queue" aria-label="Tasks in line">
      <p className="lt__queuehead">{aheadWords(queue.ahead)}</p>
      {rows.length > 0 ? (
        <>
          <p className="lt__queuehint">The pond works through these one at a time. Choose which keep going; a task you stop finishes the step it is on.</p>
          <ul className="lt__queuelist">
            {rows.map((row) => {
              const stop = stopPicked[row.id] === true;
              const label = row.isThis ? `${row.goal} (this one)` : row.goal;
              return (
                <li key={row.id} className="lt__queuerow">
                  <fieldset className="lt__queuechoice" disabled={applying}>
                    <legend className="lt__queuegoal">
                      {label}
                      <span className="lt__queuewhere">{row.where}</span>
                    </legend>
                    <label className="lt__queueopt" data-on={!stop}>
                      <input type="radio" name={`${name}-${row.id}`} checked={!stop} onChange={() => choose(row.id, false)} />
                      Keep going
                    </label>
                    <label className="lt__queueopt" data-on={stop}>
                      <input type="radio" name={`${name}-${row.id}`} checked={stop} onChange={() => choose(row.id, true)} />
                      Stop
                    </label>
                  </fieldset>
                </li>
              );
            })}
          </ul>
          {picked.length > 0 ? (
            <button type="button" className="ink-btn ink-btn--danger lt__btn lt__queueapply" aria-disabled={applying || undefined} onClick={() => {
              if (!applying) void stopPickedTasks();
            }}>
              {applying ? "Stopping..." : picked.length === 1 ? "Stop 1 task" : `Stop ${picked.length} tasks`}
            </button>
          ) : outcome === null ? (
            <p className="lt__queuehint">Everything here keeps going.</p>
          ) : null}
        </>
      ) : null}
      {outcome && outcome.stopped > 0 ? (
        <p className="lt__queuedone" role="status">
          {outcome.stopped === 1 ? "Stopped 1 task." : `Stopped ${outcome.stopped} tasks.`}
        </p>
      ) : null}
      {outcome && outcome.failed.length > 0 ? (
        <p className="lt__error" role="alert">
          <AlertCircle size={14} aria-hidden="true" />
          <span>Could not stop {outcome.failed.join("; ")}. Try again.</span>
        </p>
      ) : null}
    </section>
  );
}
