import { useEffect, useRef, useState } from "react";
import { Plus } from "lucide-react";
import "../../agents/agents.css";
import { api } from "../../api/PondApiClient";
import type { AgentRecipe } from "../../api/types";
import { useConfirm } from "../../components/shared";
import { AgentBuilder } from "../../agents/AgentBuilder";
import { AgentList } from "../../agents/AgentList";
import { AgentSuggestions } from "../../agents/AgentSuggestions";
import { BuiltInAgents, PrefillBar } from "../../agents/BuiltInAgents";
import { isAgentYaml } from "../../agents/agentModel";
import { clearOpenAgent, peekOpenAgent } from "../../agents/openAgent";
import { useAgentEnvironment } from "../../agents/useAgentEnvironment";
import { useRecipeList } from "../../agents/useRecipeList";

/**
 * The hub's Recipes screen. Agents live here; plain recipes are the hub's one-tap routines, so
 * this screen points at them rather than listing them twice.
 */
export function AgentsView({ go }: { go: (route: string) => void }) {
  const confirm = useConfirm();
  const { recipes, loading, error, reload } = useRecipeList();
  const env = useAgentEnvironment();
  // `undefined` is the list; `null` is a new agent; a recipe is that agent being edited.
  // Home can turn a suggestion on and hand the new copy here, to be opened in the builder.
  const [builder, setBuilder] = useState<AgentRecipe | null | undefined>(() => peekOpenAgent() ?? undefined);
  const [actionError, setActionError] = useState<string | null>(null);
  const listHeading = useRef<HTMLHeadingElement>(null);
  const builderWasOpen = useRef(false);

  useEffect(() => {
    clearOpenAgent();
  }, []);

  useEffect(() => {
    if (builderWasOpen.current && builder === undefined) listHeading.current?.focus();
    builderWasOpen.current = builder !== undefined;
  }, [builder]);

  const agents = recipes.filter((r) => isAgentYaml(r.yaml));
  const otherCount = recipes.length - agents.length;

  async function toggle(recipe: AgentRecipe) {
    if (!recipe.id) return;
    setActionError(null);
    try {
      await api.updateRecipe(recipe.id, { active: recipe.active === false });
      reload();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  }

  async function remove(recipe: AgentRecipe) {
    if (!recipe.id) return;
    const name = recipe.title || recipe.name;
    const ok = await confirm(`Delete agent "${name}"? Its schedules and events stop, and this cannot be undone.`, {
      title: "Delete Agent",
      confirmLabel: "Delete",
      destructive: true,
    });
    if (!ok) return;
    setActionError(null);
    try {
      await api.removeRecipe(recipe.id);
      reload();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  }

  if (builder !== undefined) {
    return (
      <AgentBuilder
        variant="hub"
        recipe={builder}
        recipes={recipes}
        env={env}
        onBack={() => {
          setBuilder(undefined);
          reload();
        }}
        onSaved={() => reload()}
        onDeleted={() => {
          setBuilder(undefined);
          reload();
        }}
      />
    );
  }

  return (
    <div className="ag ag--hub">
      <header className="view-head">
        <div>
          <h1 className="view-title">Recipes</h1>
          <p className="view-sub">Agents are helpers with a job. Plain recipes are the one-tap routines.</p>
        </div>
        <button type="button" className="primary-btn" onClick={() => setBuilder(null)}>
          <Plus size={16} aria-hidden="true" /> New agent
        </button>
      </header>

      {actionError ? (
        <p className="text-error text-error--sm" role="alert">
          {actionError}
        </p>
      ) : null}

      <AgentSuggestions
        variant="recipes"
        onEnabled={(recipe) => {
          reload();
          setBuilder(recipe);
        }}
      />

      <AgentList
        agents={agents}
        loading={loading}
        error={error}
        onRetry={reload}
        env={env}
        variant="hub"
        onNew={() => setBuilder(null)}
        onEdit={(recipe) => setBuilder(recipe)}
        onDelete={remove}
        onToggle={toggle}
        headingRef={listHeading}
        toolbar={<PrefillBar prefills={env.prefills} />}
      />

      <BuiltInAgents prefills={env.prefills} variant="hub" />

      {!loading && otherCount > 0 ? (
        <section className="ag-list ag-list--hub" aria-labelledby="hub-other-recipes">
          <div className="ag-list__head">
            <h2 className="ag-list__title" id="hub-other-recipes">
              Other recipes
            </h2>
            <p className="ag-hint">
              {otherCount === 1 ? "One other recipe is" : `${otherCount} other recipes are`} on the Routines screen, one tap each.
            </p>
          </div>
          <div className="ag-actions">
            <button type="button" className="ink-btn ink-btn--quiet ag-btn" onClick={() => go("routines")}>
              Open Routines
            </button>
          </div>
        </section>
      ) : null}
    </div>
  );
}
