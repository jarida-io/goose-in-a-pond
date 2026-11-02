import { useMemo } from "react";
import type { Extension } from "../api/types";
import { ErrorIndex, paths } from "./agentErrors";
import type { AgentDraft } from "./agentModel";
import { toolChoices, type ToolOption } from "./agentToolGroups";
import { FormSection, Messages } from "./AgentField";
import type { EditDraft } from "./useAgentBuilder";

interface Props {
  draft: AgentDraft;
  edit: EditDraft;
  errors: ErrorIndex;
  extensions: Extension[] | null;
}

function metaFor(option: ToolOption): string {
  switch (option.state) {
    case "off":
      return "Switched off in Settings, so an agent would get nothing from it";
    case "withheld":
      return "An agent is never given this one";
    case "unreported":
      return "The pond does not list this one";
    default:
      return option.tools > 0 ? `${option.tools} ${option.tools === 1 ? "tool" : "tools"}` : "";
  }
}

export function ToolsEditor({ draft, edit, errors, extensions }: Props) {
  const choices = useMemo(() => toolChoices(extensions, draft.toolGroups), [extensions, draft.toolGroups]);
  const sectionProblems = errors.inSection("tools").length;

  function toggle(name: string) {
    edit(
      (d) => ({
        ...d,
        toolGroups: d.toolGroups.includes(name) ? d.toolGroups.filter((g) => g !== name) : [...d.toolGroups, name],
      }),
      { section: "tools", structural: true },
    );
  }

  function row(option: ToolOption) {
    const checked = draft.toolGroups.includes(option.name);
    const at = draft.toolGroups.indexOf(option.name);
    const messages = at === -1 ? [] : errors.at(paths.toolGroup(at));
    const blocked = !checked && option.state === "off";
    return (
      <li key={option.name} className="ag-pick">
        <label className="ag-option">
          <input
            type="checkbox"
            checked={checked}
            disabled={blocked}
            aria-invalid={messages.length > 0 ? true : undefined}
            onChange={() => toggle(option.name)}
          />
          <span className="ag-option__text">
            <span className="ag-option__name">{option.label}</span>
            {option.detail ? <span className="ag-option__meta ag-clamp">{option.detail}</span> : null}
            {metaFor(option) ? <span className="ag-option__meta">{metaFor(option)}</span> : null}
          </span>
        </label>
        <Messages messages={messages} />
      </li>
    );
  }

  return (
    <FormSection
      id="ag-tools"
      title="Tools"
      problems={sectionProblems}
      lead="What the agent may use. Pick only what its job needs: every tool it can see is text it reads each time it starts."
    >
      <Messages messages={errors.at(paths.toolGroups)} />
      {extensions === null ? (
        <p className="ag-hint">The pond did not list its tools just now, so this is the usual set.</p>
      ) : null}
      {choices.offered.length === 0 ? (
        <p className="ag-hint">The pond lists no tools an agent could use. An agent with none can still think and answer.</p>
      ) : (
        <ul className="ag-picks">{choices.offered.map(row)}</ul>
      )}
      {choices.kept.length > 0 ? (
        <>
          <h3 className="ag-sub-title">Named in the YAML, not on offer</h3>
          <ul className="ag-picks">{choices.kept.map(row)}</ul>
        </>
      ) : null}
      <p className="ag-hint">
        An agent is never given tools that act outside the pond or reach further than its own job: controlling devices,
        notifications, schedules, cameras, rides, and starting other helpers. It has nobody to ask for approval.
      </p>
    </FormSection>
  );
}
