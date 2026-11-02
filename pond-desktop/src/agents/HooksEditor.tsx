import { ErrorIndex, paths } from "./agentErrors";
import { BEFORE_HOOKS, HAND_TO, SIMPLE_AFTER_HOOKS, type AgentDraft } from "./agentModel";
import type { OwnerReading } from "./agentWords";
import { Field, FormSection, Messages } from "./AgentField";
import type { EditDraft } from "./useAgentBuilder";

const BEFORE_COPY: Record<string, { name: string; meta: string }> = {
  today: { name: "Today's date and schedule", meta: "What is on the calendar today." },
  weather: { name: "The weather", meta: "The forecast for the pond's place." },
  presence: { name: "Who is home", meta: "Who the pond last saw arrive or leave." },
};

const AFTER_COPY: Record<string, { name: string; meta: string }> = {
  notify_owner: { name: "Tell its owner", meta: "On the owner's own devices only." },
  save_artifact: { name: "Keep the result", meta: "Saved with the run, so you or the next agent can read it." },
  start_task: { name: "Start a long task", meta: "Only when the result asks for one." },
};

interface Props {
  draft: AgentDraft;
  edit: EditDraft;
  errors: ErrorIndex;
  others: readonly string[];
  owner: OwnerReading;
}

export function HooksEditor({ draft, edit, errors, others, owner }: Props) {
  const knownBefore: readonly string[] = BEFORE_HOOKS;
  const knownAfter: readonly string[] = [...SIMPLE_AFTER_HOOKS, HAND_TO];
  const strayBefore = draft.before.filter((h) => !knownBefore.includes(h));
  const strayAfter = draft.after.filter((h) => !knownAfter.includes(h.hook));
  const handAt = draft.after.findIndex((h) => h.hook === HAND_TO);
  const handTo = handAt === -1 ? null : draft.after[handAt];

  function toggleBefore(hook: string) {
    edit(
      (d) => ({ ...d, before: d.before.includes(hook) ? d.before.filter((h) => h !== hook) : [...d.before, hook] }),
      { section: "hooks", structural: true },
    );
  }

  function toggleAfter(hook: string) {
    edit(
      (d) =>
        d.after.some((h) => h.hook === hook)
          ? { ...d, after: d.after.filter((h) => h.hook !== hook) }
          : { ...d, after: [...d.after, { hook, agent: hook === HAND_TO ? (others[0] ?? "") : "" }] },
      { section: "hooks", structural: true },
    );
  }

  function setHandTo(agent: string) {
    edit((d) => ({ ...d, after: d.after.map((h) => (h.hook === HAND_TO ? { ...h, agent } : h)) }), { section: "hooks" });
  }

  function beforeRow(hook: string, label: string, meta: string) {
    const checked = draft.before.includes(hook);
    const messages = errors.at(paths.beforeHook(draft.before.indexOf(hook)));
    const blocked = hook === "presence" && draft.personalData === "deny" && !checked;
    return (
      <li key={hook} className="ag-pick">
        <label className="ag-option">
          <input
            type="checkbox"
            checked={checked}
            disabled={blocked}
            aria-invalid={messages.length > 0 ? true : undefined}
            onChange={() => toggleBefore(hook)}
          />
          <span className="ag-option__text">
            <span className="ag-option__name">{label}</span>
            <span className="ag-option__meta">{blocked ? "Not available while personal data is denied." : meta}</span>
          </span>
        </label>
        <Messages messages={messages} />
      </li>
    );
  }

  function afterRow(hook: string, label: string, meta: string) {
    const at = draft.after.findIndex((h) => h.hook === hook);
    const checked = at !== -1;
    const messages = checked ? errors.at(paths.afterHook(at)) : [];
    return (
      <li key={hook} className="ag-pick">
        <label className="ag-option">
          <input
            type="checkbox"
            checked={checked}
            aria-invalid={messages.length > 0 ? true : undefined}
            onChange={() => toggleAfter(hook)}
          />
          <span className="ag-option__text">
            <span className="ag-option__name">{label}</span>
            <span className="ag-option__meta">{meta}</span>
          </span>
        </label>
        <Messages messages={messages} />
        {hook === "notify_owner" && checked && owner.notify ? <p className="ag-hint">{owner.notify}</p> : null}
      </li>
    );
  }

  const handNames = handTo && handTo.agent !== "" && !others.includes(handTo.agent) ? [...others, handTo.agent] : others;

  return (
    <FormSection
      id="ag-hooks"
      title="Before and after a run"
      problems={errors.inSection("hooks").length}
      lead="Small fixed steps around each run, chosen from a short list. None of them runs code."
    >
      <Messages messages={[...errors.at(paths.hooks), ...errors.at(paths.before), ...errors.at(paths.after)]} />
      <fieldset className="ag-group">
        <legend className="ag-label">Before it runs, give it</legend>
        <ul className="ag-picks">
          {BEFORE_HOOKS.map((hook) => beforeRow(hook, BEFORE_COPY[hook].name, BEFORE_COPY[hook].meta))}
          {strayBefore.map((hook) => beforeRow(hook, `As written: ${hook}`, "The pond does not know this one."))}
        </ul>
      </fieldset>
      <fieldset className="ag-group">
        <legend className="ag-label">After it runs</legend>
        <ul className="ag-picks">
          {SIMPLE_AFTER_HOOKS.map((hook) => afterRow(hook, AFTER_COPY[hook].name, AFTER_COPY[hook].meta))}
          {strayAfter.map((h) => afterRow(h.hook, `As written: ${h.hook}`, "The pond does not know this one."))}
          <li className="ag-pick">
            <label className="ag-option">
              <input
                type="checkbox"
                checked={handTo !== null}
                disabled={handTo === null && others.length === 0}
                aria-invalid={handAt !== -1 && errors.at(paths.afterHook(handAt)).length > 0 ? true : undefined}
                onChange={() => toggleAfter(HAND_TO)}
              />
              <span className="ag-option__text">
                <span className="ag-option__name">Hand the result to another agent</span>
                <span className="ag-option__meta">
                  {others.length === 0 && handTo === null
                    ? "No other agents yet."
                    : "That agent must list this one under the agents it takes work from. A chain stops after four hand-offs, and never loops."}
                </span>
              </span>
            </label>
            {handAt !== -1 ? <Messages messages={errors.at(paths.afterHook(handAt))} /> : null}
            {handTo !== null ? (
              <Field label="Hand it to" errors={errors.at(paths.handTo(handAt))}>
                {(control) => (
                  <select {...control} className="ag-select" value={handTo.agent} onChange={(e) => setHandTo(e.target.value)}>
                    {handTo.agent === "" ? <option value="">Choose an agent</option> : null}
                    {handNames.map((name) => (
                      <option key={name} value={name}>
                        {others.includes(name) ? name : `As written: ${name}`}
                      </option>
                    ))}
                  </select>
                )}
              </Field>
            ) : null}
          </li>
        </ul>
      </fieldset>
    </FormSection>
  );
}
