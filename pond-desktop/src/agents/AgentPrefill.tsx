import { AlertTriangle, Check, CircleSlash, Clock, Loader2, Moon, RotateCcw } from "lucide-react";
import "./agents.css";
import type { PrefillAgent, PrefillStatus } from "../api/types";
import { agentLabel, buildLine, isWorking, prefillTimes, prefillWords } from "./prefillWords";
import { useAgentPrefills, type PrefillsState } from "./useAgentPrefills";

function StatusIcon({ status }: { status: PrefillStatus }) {
  const props = { size: 14, "aria-hidden": true as const };
  switch (status) {
    case "ready":
      return <Check {...props} />;
    case "building":
      return <Loader2 {...props} className="animate-spin" />;
    case "queued":
      return <Clock {...props} />;
    case "idle":
      return <Moon {...props} />;
    case "not_built":
      return <CircleSlash {...props} />;
    case "failed":
      return <AlertTriangle {...props} />;
  }
}

/** The state in words, with an icon that repeats it and never replaces it, and when it was built and used. */
export function PrefillStatusLine({ row, now }: { row: PrefillAgent; now: Date }) {
  const times = prefillTimes(row, now);
  return (
    <div className="ag-prefill__status" data-state={row.status}>
      <p className="ag-prefill__state">
        <StatusIcon status={row.status} />
        <span>{prefillWords(row, now)}</span>
      </p>
      {times ? <p className="ag-hint">{times}</p> : null}
    </div>
  );
}

/** Asks the pond to throw the copy away and build it again when it is quiet; not offered while it is already on it. */
export function RebuildAgent({ row, prefills }: { row: PrefillAgent; prefills: PrefillsState }) {
  const asking = prefills.busy[row.agent] === true;
  const error = prefills.errors[row.agent];
  return (
    <>
      <button
        type="button"
        className="ink-btn ink-btn--quiet ag-btn"
        disabled={asking || isWorking(row.status)}
        aria-label={`Rebuild ${agentLabel(row.agent)}`}
        onClick={() => void prefills.rebuild(row.agent)}
      >
        <RotateCcw size={14} aria-hidden="true" /> Rebuild
      </button>
      {error ? (
        <p className="ag-errors__item" role="alert">
          <AlertTriangle size={14} aria-hidden="true" />
          <span>{error}</span>
        </p>
      ) : null}
    </>
  );
}

interface Props {
  agent: string;
  prefills: PrefillsState;
  variant: "card" | "builder";
}

/** One agent's start-up copy, for its card and for its builder. Nothing at all when the pond cannot say. */
export function AgentPrefill({ agent, prefills, variant }: Props) {
  const row = prefills.supported ? prefills.report?.agents.find((a) => a.agent === agent) : undefined;
  if (!row) return null;
  return (
    <div className={`ag-prefill ag-prefill--${variant}`}>
      <p className="ag-card__label">Start-up copy</p>
      <PrefillStatusLine row={row} now={new Date()} />
      <div className="ag-actions">
        <RebuildAgent row={row} prefills={prefills} />
      </div>
    </div>
  );
}

/** Home's quiet line while the pond builds agents; empty, and so hidden, the rest of the time. */
export function AgentPrefillLine() {
  const { report } = useAgentPrefills();
  const text = report ? buildLine(report.building) : null;
  return (
    <p className="ag-prefillline" role="status">
      {text ? <Loader2 size={12} className="animate-spin" aria-hidden="true" /> : null}
      {text}
    </p>
  );
}
