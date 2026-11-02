import { useId } from "react";
import { AlertCircle } from "lucide-react";
import "./agents.css";
import type { AgentRecipe, AgentSuggestion } from "../api/types";
import { useAgentSuggestions } from "./useAgentSuggestions";

interface CardProps {
  suggestion: AgentSuggestion;
  enabling: boolean;
  error: string | undefined;
  onEnable: () => void;
  onDismiss: () => void;
}

function SuggestionCard({ suggestion, enabling, error, onEnable, onDismiss }: CardProps) {
  return (
    <article className="ag-sug__card" aria-label={suggestion.title}>
      <h3 className="ag-sug__title">{suggestion.title}</h3>
      <p className="ag-sug__text">{suggestion.description}</p>
      <dl className="ag-sug__facts">
        <div className="ag-sug__fact">
          <dt>Why it fits this pond</dt>
          <dd>{suggestion.why}</dd>
        </div>
        <div className="ag-sug__fact">
          <dt>When it would run</dt>
          <dd>{suggestion.activations_summary}</dd>
        </div>
        {suggestion.needs.length > 0 ? (
          <div className="ag-sug__fact ag-sug__fact--optional">
            <dt>It uses</dt>
            <dd>{suggestion.needs.join(", ")}</dd>
          </div>
        ) : null}
      </dl>
      <p className="ag-sug__promise">Nothing runs until you turn it on. A schedule or an event starts only after you do.</p>
      {error ? (
        <p className="ag-errors__item" role="alert">
          <AlertCircle size={14} aria-hidden="true" />
          <span>{error}</span>
        </p>
      ) : null}
      <div className="ag-actions">
        <button
          type="button"
          className="ink-btn ink-btn--primary ag-btn ag-btn--ask"
          disabled={enabling}
          aria-label={`Turn on ${suggestion.title}`}
          onClick={onEnable}
        >
          {enabling ? "Turning on..." : "Turn on"}
        </button>
        <button
          type="button"
          className="ink-btn ink-btn--quiet ag-btn"
          disabled={enabling}
          aria-label={`Do not suggest ${suggestion.title} again`}
          onClick={onDismiss}
        >
          Don't suggest this
        </button>
      </div>
    </article>
  );
}

export interface AgentSuggestionsProps {
  /** Home offers the first; the Recipes screen offers them all. */
  variant: "home" | "recipes";
  /** Gets the new copy, so the caller can open the builder on it. */
  onEnabled: (recipe: AgentRecipe) => void;
  /** Home's way to the rest. */
  onSeeAll?: () => void;
}

/** Agent templates this pond can run. A card shows what it does, why it fits and when it would run; none runs until turned on. */
export function AgentSuggestions({ variant, onEnabled, onSeeAll }: AgentSuggestionsProps) {
  const headingId = useId();
  const { suggestions, work, errors, notice, enable, dismiss } = useAgentSuggestions();

  if (suggestions.length === 0 && !notice) return null;

  const shown = variant === "home" ? suggestions.slice(0, 1) : suggestions;
  const more = suggestions.length - shown.length;

  async function turnOn(template: string) {
    const recipe = await enable(template);
    if (recipe) onEnabled(recipe);
  }

  return (
    <section className={`ag-sug ag-sug--${variant}`} aria-labelledby={headingId}>
      <h2 className="ag-sug__eyebrow" id={headingId}>
        {variant === "home" ? "A helper you could turn on" : "Suggested for this pond"}
      </h2>
      {variant === "recipes" ? (
        <p className="ag-hint">Ready-made agents this pond is able to run. Each is yours to change once it is on.</p>
      ) : null}
      {notice ? (
        <p className="ag-hint" role="status">
          {notice}
        </p>
      ) : null}
      {shown.map((suggestion) => (
        <SuggestionCard
          key={suggestion.template}
          suggestion={suggestion}
          enabling={work[suggestion.template] === "enabling"}
          error={errors[suggestion.template]}
          onEnable={() => void turnOn(suggestion.template)}
          onDismiss={() => void dismiss(suggestion.template)}
        />
      ))}
      {more > 0 && onSeeAll ? (
        <button type="button" className="ag-linkbtn" onClick={onSeeAll}>
          {more === 1 ? "1 more in Recipes" : `${more} more in Recipes`}
        </button>
      ) : null}
    </section>
  );
}
