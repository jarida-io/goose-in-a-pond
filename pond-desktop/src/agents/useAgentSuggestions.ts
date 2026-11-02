import { useCallback, useEffect, useState } from "react";
import { api } from "../api/PondApiClient";
import { ApiError, type AgentRecipe, type AgentSuggestion } from "../api/types";

export type SuggestionWork = "enabling";

export interface AgentSuggestionsState {
  suggestions: AgentSuggestion[];
  work: Record<string, SuggestionWork>;
  /** What went wrong with one card, in words for the person who tapped it. */
  errors: Record<string, string>;
  /** Set when a card was already turned on elsewhere. */
  notice: string | null;
  /** Resolves with the new copy, or `null` when nothing was turned on. */
  enable: (template: string) => Promise<AgentRecipe | null>;
  dismiss: (template: string) => Promise<void>;
}

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** The pond's suggested agents. A failed list is silent: no banner over a screen that offers nothing urgent. */
export function useAgentSuggestions(): AgentSuggestionsState {
  const [suggestions, setSuggestions] = useState<AgentSuggestion[]>([]);
  const [work, setWork] = useState<Record<string, SuggestionWork>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    async function load(): Promise<void> {
      try {
        const list = await api.listAgentSuggestions();
        if (live) setSuggestions(Array.isArray(list) ? list : []);
      } catch {
        // An older pond has no such route; either way there is nothing to show.
      }
    }
    void load();
    return () => {
      live = false;
    };
  }, []);

  const clearError = useCallback((template: string) => {
    setErrors((all) => {
      if (!(template in all)) return all;
      const { [template]: _gone, ...rest } = all;
      return rest;
    });
  }, []);

  const enable = useCallback(
    async (template: string): Promise<AgentRecipe | null> => {
      const title = suggestions.find((s) => s.template === template)?.title ?? "That agent";
      setNotice(null);
      clearError(template);
      setWork((all) => ({ ...all, [template]: "enabling" }));
      try {
        const { recipe } = await api.enableAgentSuggestion(template);
        setSuggestions((all) => all.filter((s) => s.template !== template));
        return recipe;
      } catch (e) {
        if (e instanceof ApiError && e.status === 409) {
          setSuggestions((all) => all.filter((s) => s.template !== template));
          setNotice(`${title} is already turned on. Find it with your agents.`);
        } else {
          setErrors((all) => ({ ...all, [template]: `Could not turn it on. ${reasonOf(e)}` }));
        }
        return null;
      } finally {
        setWork((all) => {
          const { [template]: _done, ...rest } = all;
          return rest;
        });
      }
    },
    [suggestions, clearError],
  );

  const dismiss = useCallback(
    async (template: string): Promise<void> => {
      const at = suggestions.findIndex((s) => s.template === template);
      if (at < 0) return;
      const gone = suggestions[at];
      setNotice(null);
      clearError(template);
      // Optimistic: a card that lingers after the tap reads as if the tap did not land.
      setSuggestions((all) => all.filter((s) => s.template !== template));
      try {
        await api.dismissAgentSuggestion(template);
      } catch (e) {
        setSuggestions((all) => {
          const back = all.slice();
          back.splice(Math.min(at, back.length), 0, gone);
          return back;
        });
        setErrors((all) => ({ ...all, [template]: `Could not hide it. ${reasonOf(e)}` }));
      }
    },
    [suggestions, clearError],
  );

  return { suggestions, work, errors, notice, enable, dismiss };
}
