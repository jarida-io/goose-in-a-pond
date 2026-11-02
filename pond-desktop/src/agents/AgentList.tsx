import type { ReactNode, RefObject } from "react";
import { Bot, Plus } from "lucide-react";
import "./agents.css";
import type { AgentRecipe } from "../api/types";
import { ErrorBanner, SkeletonList } from "../components/shared";
import { AgentCard } from "./AgentCard";
import type { AgentEnvironment } from "./useAgentEnvironment";

interface Props {
  agents: readonly AgentRecipe[];
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  env: AgentEnvironment;
  variant: "classic" | "hub";
  onNew: () => void;
  onEdit: (recipe: AgentRecipe) => void;
  onDelete: (recipe: AgentRecipe) => void;
  onToggle: (recipe: AgentRecipe) => void;
  /** Where focus returns when the builder closes. */
  headingRef?: RefObject<HTMLHeadingElement | null>;
  /** Sits under the heading, over the cards. */
  toolbar?: ReactNode;
}

export function AgentList({ agents, loading, error, onRetry, env, variant, onNew, onEdit, onDelete, onToggle, headingRef, toolbar }: Props) {
  return (
    <section className={`ag-list ag-list--${variant}`} aria-labelledby="ag-list-heading">
      <div className="ag-list__head">
        <h2 className="ag-list__title" id="ag-list-heading" tabIndex={-1} ref={headingRef}>
          Agents{!loading && agents.length > 0 ? <span className="ag-list__count"> {agents.length}</span> : null}
        </h2>
        <p className="ag-hint">
          Helpers with one job each. An agent gets only the tools its job needs, and it can start on a schedule or when something
          happens at home.
        </p>
      </div>

      {toolbar}

      {error ? <ErrorBanner error={error} onRetry={onRetry} /> : null}

      {loading ? (
        <SkeletonList rows={2} />
      ) : agents.length === 0 && !error ? (
        <div className="empty-state">
          <Bot size={32} aria-hidden="true" />
          <span>No agents yet. Make one to hand a job to, such as a morning brief or a visitor greeter.</span>
          <button type="button" className="empty-state__cta" onClick={onNew}>
            <Plus size={14} aria-hidden="true" /> New agent
          </button>
        </div>
      ) : (
        <div className="ag-grid">
          {agents.map((recipe) => (
            <AgentCard
              key={recipe.id ?? recipe.name}
              recipe={recipe}
              variant={variant}
              env={env}
              onEdit={onEdit}
              onDelete={onDelete}
              onToggle={onToggle}
            />
          ))}
        </div>
      )}
    </section>
  );
}
