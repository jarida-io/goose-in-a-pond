import { useId } from "react";
import { AlertTriangle, RotateCcw } from "lucide-react";
import "./agents.css";
import { useConfirm } from "../components/shared";
import { PrefillStatusLine, RebuildAgent } from "./AgentPrefill";
import { agentLabel, buildLine, isWorking, prefillSummary } from "./prefillWords";
import { ALL_AGENTS, type PrefillsState } from "./useAgentPrefills";

interface GroupProps {
  prefills: PrefillsState;
  variant: "classic" | "hub";
}

/** The pond's own agents, read-only: their start-up copies can be rebuilt, but they cannot be edited or deleted. */
export function BuiltInAgents({ prefills, variant }: GroupProps) {
  const headingId = useId();
  const rows = prefills.supported ? (prefills.report?.agents.filter((a) => a.kind === "default") ?? []) : [];
  if (rows.length === 0) return null;
  const now = new Date();
  return (
    <section className={`ag-list ag-list--${variant}`} aria-labelledby={headingId}>
      <div className="ag-list__head">
        <h2 className="ag-list__title" id={headingId}>
          Built-in agents
        </h2>
        <p className="ag-hint">
          The pond's own helpers for long tasks. You cannot edit or delete them, but you can ask for them to be rebuilt.
        </p>
      </div>
      <ul className="ag-builtins">
        {rows.map((row) => (
          <li key={row.agent} className="ag-builtin" aria-label={agentLabel(row.agent)}>
            <div className="ag-builtin__main">
              <p className="ag-builtin__name">{agentLabel(row.agent)}</p>
              <PrefillStatusLine row={row} now={now} />
            </div>
            <div className="ag-builtin__actions">
              <RebuildAgent row={row} prefills={prefills} />
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** How every agent's start-up copy stands, the build in progress, and Rebuild all behind a confirmation. */
export function PrefillBar({ prefills }: { prefills: PrefillsState }) {
  const confirm = useConfirm();
  const report = prefills.supported ? prefills.report : null;
  if (!report || report.agents.length === 0) return null;

  const summary = prefillSummary(report.agents);
  const building = buildLine(report.building);
  const asking = prefills.busy[ALL_AGENTS] === true;
  const error = prefills.errors[ALL_AGENTS];
  const allWorking = report.agents.every((a) => isWorking(a.status));

  async function rebuildAll() {
    const ok = await confirm(
      "Rebuild every agent's start-up copy? Agents will be slower until the pond is quiet again and has rebuilt them.",
      { title: "Rebuild all", confirmLabel: "Rebuild all" },
    );
    if (ok) await prefills.rebuildAll();
  }

  return (
    <div className="ag-prefillbar">
      <p className="ag-hint" role="status">
        {[summary, building].filter(Boolean).join(" ")}
      </p>
      <button
        type="button"
        className="ink-btn ink-btn--quiet ag-btn"
        disabled={asking || allWorking}
        onClick={() => void rebuildAll()}
      >
        <RotateCcw size={14} aria-hidden="true" /> Rebuild all
      </button>
      {error ? (
        <p className="ag-errors__item" role="alert">
          <AlertTriangle size={14} aria-hidden="true" />
          <span>{error}</span>
        </p>
      ) : null}
    </div>
  );
}
