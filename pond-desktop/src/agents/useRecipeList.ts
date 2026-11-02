import { useCallback, useEffect, useState } from "react";
import { api } from "../api/PondApiClient";
import type { AgentRecipe } from "../api/types";

/** The household's recipes, with the loading and error state both screens draw the same way. */
export function useRecipeList() {
  const [recipes, setRecipes] = useState<AgentRecipe[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(() => {
    setLoading(true);
    setError(null);
    api
      .listRecipes()
      .then((res) => setRecipes(Array.isArray(res) ? res : []))
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  return { recipes, loading, error, setError, reload };
}
