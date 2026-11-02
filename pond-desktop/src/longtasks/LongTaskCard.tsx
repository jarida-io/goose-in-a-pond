import { useState } from "react";
import { AlertCircle, AlertTriangle, Check, CircleSlash, Clock, Loader2, Pause, Telescope } from "lucide-react";
import "./longTasks.css";
import type { LongTask, LongTaskState } from "../api/types";
import { useConfirm } from "../components/shared";
import { isEnded, isOwnAhead, isResearch, isWaiting, resultLine, resultOf, shortAnswerOf, stepAgent, stepWords, taskKindLabel, taskWords } from "./longTaskWords";
import { MarkdownText } from "../components/MarkdownText";
import type { LongTasksState } from "./useLongTasks";
import { useLongTaskReport } from "./useLongTaskReport";
import { ReportExport } from "./ReportExport";
import { ReportSheet } from "./ReportSheet";
import { LongTaskQueue } from "./LongTaskQueue";

function StateIcon({ state }: { state: LongTaskState }) {
  const props = { size: 14, "aria-hidden": true as const };
  switch (state) {
    case "planning":
    case "running":
      return <Loader2 {...props} className="animate-spin" />;
    case "awaiting_approval":
      return <Clock {...props} />;
    case "paused":
      return <Pause {...props} />;
    case "completed":
      return <Check {...props} />;
    case "failed":
      return <AlertTriangle {...props} />;
    case "cancelled":
      return <CircleSlash {...props} />;
  }
}

interface Props {
  id: string;
  longTasks: LongTasksState;
  /** Classic is dense for a mouse; the hub takes the touch sizes. */
  variant: "classic" | "hub";
}

/** A working note on one long task: what it is for, how far it has got, who is on each step, and its result. */
export function LongTaskCard({ id, longTasks, variant }: Props) {
  const task: LongTask | undefined = longTasks.tasks[id];
  if (!task) return <LongTaskGap id={id} longTasks={longTasks} variant={variant} />;
  return <LongTaskBody task={task} longTasks={longTasks} variant={variant} />;
}

/** A card for a task the chat points at but the pond has not (yet) shown: checking, gone, or unreadable. */
function LongTaskGap({ id, longTasks, variant }: Props) {
  if (longTasks.gone[id]) {
    return (
      <div className={`lt lt--${variant} lt--quiet`}>
        <p className="lt__note">This task is no longer on the pond.</p>
      </div>
    );
  }
  const why = longTasks.unreadable[id];
  return (
    <div className={`lt lt--${variant} lt--quiet`} aria-busy={why === undefined}>
      <p className="lt__note">{why === undefined ? "Checking on this task..." : "Couldn't check on this task just now."}</p>
      {why !== undefined ? (
        <button type="button" className="ink-btn ink-btn--quiet lt__btn" onClick={() => longTasks.retry(id)}>
          Try again
        </button>
      ) : null}
    </div>
  );
}

function LongTaskBody({ task, longTasks, variant }: { task: LongTask; longTasks: LongTasksState; variant: "classic" | "hub" }) {
  const id = task.id;
  const confirm = useConfirm();
  const report = useLongTaskReport(task);
  const [opening, setOpening] = useState(false);
  const [openFailed, setOpenFailed] = useState(false);
  const [sheetOpen, setSheetOpen] = useState(false);

  const ended = isEnded(task.state);
  // The line's own choice covers this task, so the card does not offer a second Stop beside it.
  const choosingInLine = isWaiting(task) && (task.queue?.ahead ?? []).some(isOwnAhead);
  const stopping = longTasks.stopping[id] === true;
  const stopError = longTasks.stopErrors[id];
  const result = task.state === "completed" ? resultOf(task) : null;
  // A short answer is the answer itself, so the card shows it whole rather than a line about it.
  const shortAnswer = result ? shortAnswerOf(task) : null;

  async function onStop() {
    const ok = await confirm("Stop this task? A step that is already under way finishes first.", {
      title: "Stop task",
      confirmLabel: "Stop task",
      destructive: true,
    });
    if (ok) await longTasks.stop(id);
  }

  // The whole report opens on its own sheet; the card keeps only the short line about it.
  async function onRead() {
    setOpening(true);
    setOpenFailed(false);
    const loaded = await report.load();
    setOpening(false);
    if (loaded) setSheetOpen(true);
    else setOpenFailed(true);
  }

  return (
    // The thread is a polite live region; only the state line speaks, so step and summary churn stays quiet.
    <div
      className={`lt lt--${variant}`}
      role="group"
      aria-label={`${taskKindLabel(task)}: ${task.goal}`}
      aria-live="off"
      data-state={task.state}
      data-kind={isResearch(task) ? "research" : "general"}
    >
      {isResearch(task) ? (
        <p className="lt__kind" aria-hidden="true">
          <Telescope size={12} aria-hidden="true" />
          Deep research
        </p>
      ) : null}
      <div className="lt__head">
        <StateIcon state={task.state} />
        <p className="lt__goal">{task.goal}</p>
      </div>
      <p className="lt__state" aria-live="polite">
        {taskWords(task)}
      </p>
      <LongTaskQueue task={task} longTasks={longTasks} />

      {task.steps.length > 0 ? (
        <ol className="lt__steps">
          {task.steps.map((step) => (
            <li key={step.key} className="lt__step" data-state={step.state}>
              <span className="lt__agent">{stepAgent(step)}</span>
              <span className="lt__stepstate">
                {stepWords(step.state)}
                {step.attempts > 1 ? ` (attempt ${step.attempts})` : ""}
              </span>
              {step.failure ? <span className="lt__failure">{step.failure}</span> : null}
              {step.summary ? <span className="lt__summary">{step.summary}</span> : null}
            </li>
          ))}
        </ol>
      ) : null}

      {shortAnswer ? (
        <MarkdownText source={shortAnswer} className="lt__answer" />
      ) : result ? (
        <p className="lt__resultline">{resultLine(result)}</p>
      ) : null}

      <div className="lt__actions">
        {result && !shortAnswer ? (
          <>
            {/* Not disabled while opening: a disabled button drops focus, and the sheet would have nowhere to return it. */}
            <button
              type="button"
              className="ink-btn ink-btn--quiet lt__btn"
              aria-disabled={opening || undefined}
              onClick={() => {
                if (!opening) void onRead();
              }}
            >
              {opening ? "Opening..." : "Read the result"}
            </button>
            <ReportExport task={task} handle={report} variant={variant} placement="inline" />
          </>
        ) : null}
        {!ended && !choosingInLine ? (
          <button
            type="button"
            className="ink-btn ink-btn--quiet lt__btn"
            disabled={stopping}
            aria-label={`Stop the task: ${task.goal}`}
            onClick={() => void onStop()}
          >
            {stopping ? "Stopping..." : "Stop"}
          </button>
        ) : null}
      </div>

      {stopError ? (
        <p className="lt__error" role="alert">
          <AlertCircle size={14} aria-hidden="true" />
          <span>{stopError}</span>
        </p>
      ) : null}
      {openFailed && report.state.status === "failed" ? (
        <p className="lt__error" role="alert">
          <AlertCircle size={14} aria-hidden="true" />
          <span>Couldn't open the result. {report.state.message}</span>
        </p>
      ) : null}
      {sheetOpen && report.state.status === "ready" ? (
        <ReportSheet task={task} report={report.state.report} handle={report} variant={variant} onClose={() => setSheetOpen(false)} />
      ) : null}
    </div>
  );
}
