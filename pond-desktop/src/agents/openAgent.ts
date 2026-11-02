import type { AgentRecipe } from "../api/types";

let pending: AgentRecipe | null = null;

/** Home turns a suggestion on, then hands the new copy to the Recipes screen, which opens the builder on it. */
export function requestOpenAgent(recipe: AgentRecipe): void {
  pending = recipe;
}

/** Read without taking, so a double-invoked initializer sees the same answer; `clearOpenAgent` takes it. */
export function peekOpenAgent(): AgentRecipe | null {
  return pending;
}

export function clearOpenAgent(): void {
  pending = null;
}
