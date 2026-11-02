import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { AlertTriangle, Check, ChevronLeft, Info, Trash2 } from "lucide-react";
import "./agents.css";
import { api } from "../api/PondApiClient";
import type { AgentRecipe, RecipeFieldError } from "../api/types";
import { useConfirm } from "../components/shared";
import { paths, sentence } from "./agentErrors";
import { agentNames } from "./agentModel";
import { readOwner, type Names } from "./agentWords";
import { Field, FormSection, Messages } from "./AgentField";
import { ActivationsEditor } from "./ActivationsEditor";
import { AgentPrefill } from "./AgentPrefill";
import { ExamplesEditor } from "./ExamplesEditor";
import { HooksEditor } from "./HooksEditor";
import { StartCost } from "./StartCost";
import { TestRunPanel } from "./TestRunPanel";
import { ToolsEditor } from "./ToolsEditor";
import type { AgentEnvironment } from "./useAgentEnvironment";
import { useAgentBuilder } from "./useAgentBuilder";
import { useTestRun } from "./useTestRun";

export interface AgentBuilderProps {
  /** Classic is dense for a mouse; hub takes the touch sizes. Same parts, same words. */
  variant: "classic" | "hub";
  /** The agent being edited, or `null` for a new one. */
  recipe: AgentRecipe | null;
  /** Every saved recipe: names to avoid, and the agents this one may hand work to. */
  recipes: readonly AgentRecipe[];
  env: AgentEnvironment;
  onBack: () => void;
  onSaved: (recipe: AgentRecipe) => void;
  onDeleted: () => void;
}

function ProblemList({ problems, checking }: { problems: readonly RecipeFieldError[]; checking: boolean }) {
  if (problems.length === 0) {
    return checking ? (
      <p className="ag-hint">Checking with the pond...</p>
    ) : (
      <p className="ag-ok">
        <Check size={14} aria-hidden="true" /> The pond found no problems.
      </p>
    );
  }
  return (
    <ul className="ag-problems">
      {problems.map((problem, i) => (
        <li key={i} className="ag-problems__item">
          <code className="ag-mono">{problem.path === "" ? "the whole file" : problem.path}</code>
          <span>{sentence(problem.message)}</span>
        </li>
      ))}
    </ul>
  );
}

export function AgentBuilder({ variant, recipe, recipes, env, onBack, onSaved, onDeleted }: AgentBuilderProps) {
  const confirm = useConfirm();
  const b = useAgentBuilder(recipe, recipes);
  const test = useTestRun(b.current?.name ?? "");
  const rootRef = useRef<HTMLDivElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const [focusTick, setFocusTick] = useState(0);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  useEffect(() => {
    headingRef.current?.focus();
  }, []);

  useEffect(() => {
    if (focusTick === 0) return;
    rootRef.current?.querySelector<HTMLElement>('[aria-invalid="true"]')?.focus();
  }, [focusTick]);

  const names = useMemo<Names>(
    () => ({
      member: (id) => env.members?.find((m) => m.id === id)?.name,
      device: (id) => env.devices?.find((d) => d.id === id)?.name,
    }),
    [env.members, env.devices],
  );
  const others = useMemo(() => agentNames(recipes, b.name), [recipes, b.name]);
  const draft = b.draft;
  const owner = readOwner({
    saved: b.current !== null,
    ownerId: b.current?.owner_profile_id,
    members: env.members,
    activations: draft?.activations ?? [],
    after: draft?.after ?? [],
  });

  const title = b.current ? draft?.title.trim() || b.current.name : "New agent";

  function statusText(): string {
    if (b.saving) return "Saving...";
    if (b.justSaved && !b.dirty) return "Saved.";
    if (!b.dirty) return b.current ? "No unsaved changes." : "Not saved yet.";
    if (b.checking) return "Checking with the pond...";
    return b.readiness.reason ?? "Unsaved changes.";
  }

  async function back() {
    if (b.dirty) {
      const leave = await confirm("Leave without saving? Your changes to this agent will be lost.", {
        title: "Unsaved changes",
        confirmLabel: "Leave",
        destructive: true,
      });
      if (!leave) return;
    }
    onBack();
  }

  async function remove() {
    const target = b.current;
    if (!target?.id) return;
    const ok = await confirm(`Delete "${title}"? Its schedules and events stop, and this cannot be undone.`, {
      title: "Delete agent",
      confirmLabel: "Delete",
      destructive: true,
    });
    if (!ok) return;
    try {
      await api.removeRecipe(target.id);
      onDeleted();
    } catch (e) {
      setDeleteError(e instanceof Error ? e.message : String(e));
    }
  }

  async function onSave() {
    const saved = await b.save();
    if (saved) {
      env.prefills.refresh();
      onSaved(saved);
    } else {
      setFocusTick((t) => t + 1);
    }
  }

  async function toggleExpose() {
    if (!draft) return;
    if (!draft.expose) {
      const ok = await confirm(
        "Offer this agent to devices you have paired and to other ponds in your mesh? They could ask it to work as its owner. This version of the pond only records the choice.",
        { title: "Share this agent", confirmLabel: "Turn on sharing" },
      );
      if (!ok) return;
    }
    b.edit((d) => ({ ...d, expose: !d.expose }), { section: "sharing" });
  }

  function onTabKey(e: KeyboardEvent) {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    e.preventDefault();
    const next = e.key === "Home" ? "form" : e.key === "End" ? "yaml" : b.view === "form" ? "yaml" : "form";
    b.setView(next);
    document.getElementById(`ag-tab-${next}`)?.focus();
  }

  const errors = b.errors;
  const nameShown = b.nameProblem !== null && (b.name !== "" || b.submitted);
  const unplaced = errors.unplaced();
  const read = b.read;

  const headingClass = variant === "hub" ? "view-title" : "ink-title ink-title--2";

  return (
    <div className={`ag ag--${variant}`} ref={rootRef}>
      <div className="ag-bar">
        <button type="button" className="ag-back" onClick={() => void back()}>
          <ChevronLeft size={16} aria-hidden="true" /> Agents
        </button>
        <p className="ag-bar__status" role="status" aria-live="polite">
          {statusText()}
        </p>
        <div className="ag-bar__actions">
          {b.current ? (
            <button type="button" className="ink-btn ink-btn--quiet ag-btn ag-btn--danger" onClick={() => void remove()}>
              <Trash2 size={14} aria-hidden="true" /> Delete agent
            </button>
          ) : null}
          <button
            type="button"
            className="ink-btn ink-btn--primary ag-btn ag-btn--ask"
            disabled={!b.dirty || b.saving || b.checking}
            onClick={() => void onSave()}
          >
            Save agent
          </button>
        </div>
      </div>

      <header className="ag-head">
        <h1 className={headingClass} tabIndex={-1} ref={headingRef}>
          {title}
        </h1>
        <p className="ag-hint">
          An agent is a recipe with a job, a set of tools and a way to start. The pond keeps it with your other recipes.
        </p>
      </header>

      {b.saveError ? (
        <div className="ag-note ag-note--error" role="alert">
          <AlertTriangle size={16} aria-hidden="true" />
          <div className="ag-note__body">
            <p>Could not save the agent. The pond said: {b.saveError}</p>
            <p>Your changes are still here, so try again.</p>
          </div>
        </div>
      ) : null}
      {deleteError ? (
        <div className="ag-note ag-note--error" role="alert">
          <AlertTriangle size={16} aria-hidden="true" />
          <p>Could not delete the agent. The pond said: {deleteError}</p>
        </div>
      ) : null}
      {b.unavailable ? (
        <div className="ag-note ag-note--warn" role="status">
          <AlertTriangle size={16} aria-hidden="true" />
          <p>The pond did not answer the check. Your edits are kept here. {b.unavailable}</p>
        </div>
      ) : null}
      {b.unchecked ? (
        <div className="ag-note ag-note--info" role="status">
          <Info size={16} aria-hidden="true" />
          <p>This pond cannot check agents yet, so what you save is stored as written.</p>
        </div>
      ) : null}

      <div className="ag-tabs" role="tablist" aria-label="Edit as" onKeyDown={onTabKey}>
        <button
          type="button"
          role="tab"
          id="ag-tab-form"
          className="ag-tab"
          aria-selected={b.view === "form"}
          aria-controls="ag-panel-form"
          tabIndex={b.view === "form" ? 0 : -1}
          onClick={() => b.setView("form")}
        >
          Form
        </button>
        <button
          type="button"
          role="tab"
          id="ag-tab-yaml"
          className="ag-tab"
          aria-selected={b.view === "yaml"}
          aria-controls="ag-panel-yaml"
          tabIndex={b.view === "yaml" ? 0 : -1}
          onClick={() => b.setView("yaml")}
        >
          YAML
        </button>
      </div>

      {b.view === "form" ? (
        <div role="tabpanel" id="ag-panel-form" aria-labelledby="ag-tab-form" className="ag-panel">
          {draft === null ? (
            <div className="ag-note ag-note--info" role="status">
              <Info size={16} aria-hidden="true" />
              <div className="ag-note__body">
                <p>
                  The form cannot show this agent yet: {read.ok ? "" : read.message}
                  {!read.ok && read.line ? ` (line ${read.line})` : ""}. Fix it on the YAML tab and the form comes back.
                </p>
                <button type="button" className="ink-btn ink-btn--quiet ag-btn" onClick={() => b.setView("yaml")}>
                  Open the YAML
                </button>
              </div>
            </div>
          ) : (
            <>
              {unplaced.length > 0 ? (
                <div className="ag-note ag-note--error" role="status">
                  <AlertTriangle size={16} aria-hidden="true" />
                  <div className="ag-note__body">
                    <p>The pond found problems the form has no field for. They are on the YAML tab too.</p>
                    <ProblemList problems={unplaced} checking={false} />
                  </div>
                </div>
              ) : null}

              <FormSection
                id="ag-purpose"
                title="What it is for"
                problems={errors.inSection("purpose").length}
                lead="Say what the agent is for, the way you would brief a helper."
              >
                <Field label="Name">
                  {(control) => (
                    <input
                      {...control}
                      className="ag-input"
                      type="text"
                      placeholder="Morning brief"
                      value={draft.title}
                      onChange={(e) => b.edit((d) => ({ ...d, title: e.target.value }), { section: "purpose" })}
                    />
                  )}
                </Field>
                {b.nameLocked ? (
                  <p className="ag-hint">
                    Saved as <code className="ag-mono">{b.name}</code>. Other agents use this name to hand work to it.
                  </p>
                ) : (
                  <Field
                    label="Saved as"
                    hint="Made from the name. Other agents use it to hand work to this one. Lowercase letters, numbers, - and _."
                    errors={nameShown && b.nameProblem ? [b.nameProblem] : []}
                  >
                    {(control) => (
                      <input
                        {...control}
                        className="ag-input ag-mono"
                        type="text"
                        spellCheck={false}
                        value={b.name}
                        onChange={(e) => b.setName(e.target.value)}
                      />
                    )}
                  </Field>
                )}
                <Field label="In one line" hint="Shown on the agent's card.">
                  {(control) => (
                    <input
                      {...control}
                      className="ag-input"
                      type="text"
                      value={draft.description}
                      onChange={(e) => b.edit((d) => ({ ...d, description: e.target.value }), { section: "purpose" })}
                    />
                  )}
                </Field>
                <Field
                  label="Instructions"
                  hint="What the agent is for, in plain words. Every run starts from this text."
                  errors={errors.at(paths.instructions)}
                >
                  {(control) => (
                    <textarea
                      {...control}
                      className="ag-textarea"
                      rows={6}
                      value={draft.instructions}
                      onChange={(e) => b.edit((d) => ({ ...d, instructions: e.target.value }), { section: "purpose" })}
                    />
                  )}
                </Field>
                <Field
                  label="Most steps per run"
                  hint="From 1 to 12. Leave it empty for the usual 6."
                  errors={errors.at(paths.maxTurns)}
                  className="ag-narrow"
                >
                  {(control) => (
                    <input
                      {...control}
                      className="ag-input"
                      type="text"
                      inputMode="numeric"
                      value={draft.maxTurns}
                      onChange={(e) => b.edit((d) => ({ ...d, maxTurns: e.target.value }), { section: "purpose" })}
                    />
                  )}
                </Field>
                <StartCost report={b.report} checking={b.checking} unchecked={b.unchecked} />
                {b.current ? <AgentPrefill agent={b.current.name} prefills={env.prefills} variant="builder" /> : null}
              </FormSection>

              <ToolsEditor draft={draft} edit={b.edit} errors={errors} extensions={env.extensions} />

              <FormSection
                id="ag-data"
                title="Personal data"
                problems={errors.inSection("data").length}
                lead="How much of the household's private information the agent may see."
              >
                <fieldset className="ag-group">
                  <legend className="ag-label">What it may see</legend>
                  <label className="ag-option">
                    <input
                      type="radio"
                      name="ag-personal-data"
                      checked={draft.personalData === "inherit"}
                      onChange={() => b.edit((d) => ({ ...d, personalData: "inherit" }), { section: "data" })}
                    />
                    <span className="ag-option__text">
                      <span className="ag-option__name">What the person it works for may see</span>
                      <span className="ag-option__meta">
                        Their memories, calendar and who is home, as far as they are allowed. For a scheduled run, that is its owner.
                      </span>
                    </span>
                  </label>
                  <label className="ag-option">
                    <input
                      type="radio"
                      name="ag-personal-data"
                      checked={draft.personalData === "deny"}
                      onChange={() => b.edit((d) => ({ ...d, personalData: "deny" }), { section: "data" })}
                    />
                    <span className="ag-option__text">
                      <span className="ag-option__name">No personal data</span>
                      <span className="ag-option__meta">
                        No memories, no who-is-home, no personal context. Best for an agent that only needs the weather or the web.
                      </span>
                    </span>
                  </label>
                  {draft.personalData !== "inherit" && draft.personalData !== "deny" ? (
                    <label className="ag-option">
                      <input type="radio" name="ag-personal-data" checked readOnly aria-invalid="true" />
                      <span className="ag-option__text">
                        <span className="ag-option__name">{`As written: ${draft.personalData}`}</span>
                      </span>
                    </label>
                  ) : null}
                  <Messages messages={errors.at(paths.personalData)} />
                </fieldset>
              </FormSection>

              <ExamplesEditor draft={draft} edit={b.edit} errors={errors} />
              <ActivationsEditor
                draft={draft}
                edit={b.edit}
                errors={errors}
                env={env}
                others={others}
                owner={owner}
                names={names}
              />
              <HooksEditor draft={draft} edit={b.edit} errors={errors} others={others} owner={owner} />

              <FormSection
                id="ag-share"
                title="Sharing"
                problems={errors.inSection("sharing").length}
                lead="Whether other devices and ponds may ask this agent to work. It is off unless you turn it on."
              >
                <div className="ag-switchrow">
                  <div className="ag-switchrow__text">
                    <p className="ag-option__name" id="ag-expose-label">
                      Offer this agent to other devices
                    </p>
                    <p className="ag-hint" id="ag-expose-hint">
                      When on, devices you have paired and ponds in your mesh could ask this agent to do work as its owner. This version
                      of the pond only records the choice: nothing is offered yet.
                    </p>
                  </div>
                  <button
                    type="button"
                    role="switch"
                    className="ink-switch"
                    aria-checked={draft.expose}
                    aria-labelledby="ag-expose-label"
                    aria-describedby="ag-expose-hint"
                    onClick={() => void toggleExpose()}
                  >
                    <span className="ink-switch__knob" />
                  </button>
                </div>
                <Messages messages={[...errors.at(paths.a2a), ...errors.at(paths.expose)]} />
              </FormSection>

              <TestRunPanel
                name={b.current?.name ?? null}
                dirty={b.dirty}
                prompt={draft.testPrompt}
                onPrompt={(testPrompt) => b.edit((d) => ({ ...d, testPrompt }))}
                test={test}
              />
            </>
          )}
        </div>
      ) : (
        <div role="tabpanel" id="ag-panel-yaml" aria-labelledby="ag-tab-yaml" className="ag-panel">
          {!read.ok ? (
            <div className="ag-note ag-note--info" role="status">
              <Info size={16} aria-hidden="true" />
              <p>
                The form cannot read this text: {read.message}
                {read.line ? ` (line ${read.line})` : ""}. The pond's checks below still apply.
              </p>
            </div>
          ) : null}
          <Field
            label="Agent YAML"
            hint="Changes in the form rewrite this text, and comments are not kept. Changes here appear in the form whenever it can read them."
          >
            {(control) => (
              <textarea
                {...control}
                className="ag-textarea ag-yaml"
                spellCheck={false}
                wrap="off"
                value={b.yaml}
                onChange={(e) => b.changeYaml(e.target.value)}
              />
            )}
          </Field>
          <h2 className="ag-sub-title">What the pond found</h2>
          <ProblemList problems={errors.all} checking={b.checking} />
        </div>
      )}
    </div>
  );
}
