import { useMemo } from "react";
import { Switch } from "@heroui/react";
import { AlertTriangle, Bot, CalendarClock, Info, Link2, ListChecks, MessageSquare, Pencil, Trash2, Zap } from "lucide-react";
import "./agents.css";
import type { AgentRecipe } from "../api/types";
import { AgentPrefill } from "./AgentPrefill";
import { draftFromYaml, type ActivationDraft } from "./agentModel";
import { describeActivation, describeAfter, describeBefore, purposeLine, readOwner, type Names } from "./agentWords";
import { gateOpen, type ActivationGate, type AgentEnvironment } from "./useAgentEnvironment";

const SHOWN_ACTIVATIONS = 3;

function ActivationIcon({ kind }: { kind: ActivationDraft["kind"] }) {
  const props = { size: 14, "aria-hidden": true as const };
  switch (kind) {
    case "schedule":
      return <CalendarClock {...props} />;
    case "event":
      return <Zap {...props} />;
    case "delegated":
      return <MessageSquare {...props} />;
    case "task_step":
      return <ListChecks {...props} />;
    case "agent":
      return <Link2 {...props} />;
    default:
      return <Info {...props} />;
  }
}

function gateReason(gate: ActivationGate): string {
  if (!gate.delegation) return "Delegation is off in Settings.";
  if (!gate.activations) return "Starting agents by themselves is switched off.";
  return "Long tasks are switched off.";
}

interface Props {
  recipe: AgentRecipe;
  variant: "classic" | "hub";
  env: AgentEnvironment;
  onEdit: (recipe: AgentRecipe) => void;
  onDelete: (recipe: AgentRecipe) => void;
  onToggle: (recipe: AgentRecipe) => void;
}

export function AgentCard({ recipe, variant, env, onEdit, onDelete, onToggle }: Props) {
  const read = useMemo(() => draftFromYaml(recipe.yaml, recipe.description ?? ""), [recipe.yaml, recipe.description]);
  const draft = read.ok ? read.draft : null;
  const title = draft?.title.trim() || recipe.title || recipe.name;
  const names = useMemo<Names>(
    () => ({
      member: (id) => env.members?.find((m) => m.id === id)?.name,
      device: (id) => env.devices?.find((d) => d.id === id)?.name,
    }),
    [env.members, env.devices],
  );
  const purpose = purposeLine(draft ?? { description: "", instructions: "" }, recipe.description);
  const paused = recipe.active === false;
  const owner = readOwner({
    saved: true,
    ownerId: recipe.owner_profile_id,
    members: env.members,
    activations: draft?.activations ?? [],
    after: draft?.after ?? [],
  });
  const startsByItself = (draft?.activations ?? []).some((a) => a.kind === "schedule" || a.kind === "event");
  const activations = draft?.activations ?? [];
  const before = draft ? describeBefore(draft.before) : null;
  const after = draft ? describeAfter(draft.after) : null;
  const warning =
    startsByItself && env.gate !== null && !gateOpen(env.gate)
      ? `It will not start by itself. ${gateReason(env.gate)}`
      : startsByItself
        ? owner.runs
        : null;

  return (
    <article className={`ink-edge ink-card ag-card ag-card--${variant}${paused ? " ag-card--paused" : ""}`} aria-label={title}>
      <div className="ag-card__top">
        <span className="ag-card__icon">
          <Bot size={16} aria-hidden="true" />
        </span>
        <h3 className="ag-card__title">{title}</h3>
        <Switch size="sm" isSelected={!paused} onChange={() => onToggle(recipe)} aria-label={`Enable ${title}`}>
          <Switch.Content>
            <Switch.Control>
              <Switch.Thumb />
            </Switch.Control>
          </Switch.Content>
        </Switch>
      </div>

      <p className="ag-card__purpose">{purpose !== "" ? purpose : "No description yet."}</p>

      {draft === null ? (
        <p className="ag-hint">The form cannot show how this one runs. Open it to see its YAML.</p>
      ) : (
        <>
          <p className="ag-card__label">Runs</p>
          <ul className="ag-facts">
            {activations.length === 0 ? (
              <li className="ag-fact">Nothing starts it yet.</li>
            ) : (
              activations.slice(0, SHOWN_ACTIVATIONS).map((a, i) => (
                <li key={i} className="ag-fact">
                  <ActivationIcon kind={a.kind} />
                  <span>{describeActivation(a, names)}</span>
                </li>
              ))
            )}
            {activations.length > SHOWN_ACTIVATIONS ? (
              <li className="ag-fact ag-fact--more">and {activations.length - SHOWN_ACTIVATIONS} more</li>
            ) : null}
          </ul>
          {before || after ? (
            <>
              <p className="ag-card__label">Around each run</p>
              <ul className="ag-facts">
                {before ? <li className="ag-fact">{before}</li> : null}
                {after ? <li className="ag-fact">{after}</li> : null}
              </ul>
            </>
          ) : null}
        </>
      )}

      <AgentPrefill agent={recipe.name} prefills={env.prefills} variant="card" />

      <p className="ag-hint">{owner.line}</p>
      {warning ? (
        <p className="ag-card__warn">
          <AlertTriangle size={14} aria-hidden="true" />
          <span>{warning}</span>
        </p>
      ) : null}

      <div className="ag-card__actions">
        <button type="button" className="ink-btn ink-btn--quiet ag-btn" onClick={() => onEdit(recipe)} aria-label={`Edit ${title}`}>
          <Pencil size={14} aria-hidden="true" /> Edit
        </button>
        <button type="button" className="ag-iconbtn ag-iconbtn--danger" onClick={() => onDelete(recipe)} aria-label={`Delete ${title}`}>
          <Trash2 size={15} aria-hidden="true" />
        </button>
      </div>
    </article>
  );
}
