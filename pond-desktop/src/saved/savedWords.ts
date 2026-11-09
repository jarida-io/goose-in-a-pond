// What the saved screens say, in the words a household would use.

import type { SavedItem, SavedKind } from "../api/types";

export const SAVED_TITLE = "Saved";
export const SAVED_INTRO = "Recipes, listings, places and reports you kept, sorted into your own collections.";
export const NO_SAVED_TITLE = "Nothing saved yet";
export const NO_SAVED_BODY =
  "Ask Goose to save a recipe or a place from a chat, or tap Save on an option in a report. Saved cards land here.";
export const START_IN_CHAT = "Open a chat";
export const LOADING_SAVED = "Loading what you saved...";
export const RETRY = "Try again";
export const ALL_SAVED = "All saved";
export const COLLECTIONS_LABEL = "Collections";
export const ALL = "All";
export const LOOSE = "Not in a collection";

export const KIND_LABEL: Record<SavedKind, string> = {
  recipe: "Recipe",
  listing: "Listing",
  place: "Place",
  report: "Report",
  note: "Note",
};

export const INGREDIENTS = "Ingredients";
export const STEPS = "Steps";
export const NOTES = "Notes";
export const SERVES = "Serves";
export const TIME = "Time";
export const YEAR = "Year";
export const MILEAGE = "Mileage";
export const LOCATION = "Location";
export const ADDRESS = "Address";
export const HOURS = "Hours";
export const SOURCES = "Sources";
export const OPEN_LISTING = "Open the listing";
export const OPEN_PLACE = "Open the page";
export const WATCH_PRICE = "Watch price";
export const WATCHING_PRICE = "Starting the watch...";
export const PRICE_WATCHED = "Watching the price. Find it under Watches.";

export const MANAGE_CARD = "Manage this card";
export const RENAME = "Rename";
export const NAME_LABEL = "Name";
export const SAVE_NAME = "Save name";
export const CANCEL = "Cancel";
export const MOVE_LABEL = "Collection";
export const NEW_COLLECTION = "New collection...";
export const NEW_COLLECTION_NAME = "New collection name";
export const MOVE = "Move";
export const SHARE_LABEL = "Share with the household";
export const SHARE_HINT = "Everyone in the household can see it. Only you can change or delete it.";
export const SHARED = "Shared";
export const NOT_YOURS = "Shared with the household by another member. Only they can change it.";
export const DELETE = "Delete";
export const DELETE_TITLE = "Delete saved card";

export function deleteQuestion(title: string): string {
  return `Delete "${title}"? It leaves every collection and cannot be undone.`;
}

export function countOf(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The line under a card's title: what a glance needs for its kind. */
export function cardFacts(item: SavedItem): string {
  switch (item.kind) {
    case "recipe": {
      const parts = [item.servings ? `Serves ${item.servings}` : null, item.time ?? null];
      const ingredients = item.ingredients?.length ?? 0;
      const steps = item.steps?.length ?? 0;
      if (ingredients > 0) parts.push(countOf(ingredients, "ingredient", "ingredients"));
      if (steps > 0) parts.push(countOf(steps, "step", "steps"));
      return parts.filter(Boolean).join(" · ");
    }
    case "listing":
      return [item.year, item.mileage, item.location].filter(Boolean).join(" · ");
    case "place":
      return [item.address, item.hours].filter(Boolean).join(" · ");
    default:
      return "";
  }
}

export function priceOf(item: Extract<SavedItem, { kind: "listing" }>): string | null {
  if (!item.price) return null;
  return item.currency && !item.price.includes(item.currency) ? `${item.currency} ${item.price}` : item.price;
}

/** The first lines of a card's Markdown, as plain words for a card face. */
export function excerpt(markdown: string, max = 160): string {
  const plain = markdown
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^#+\s+/gm, "")
    .replace(/^\s*(?:[-*+]|\d+\.|>)\s+/gm, "")
    .replace(/[*_`|]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return plain.length > max ? `${plain.slice(0, max - 1).trimEnd()}...` : plain;
}

/** Collections join by name without case, as the pond does: "cars" is in "Cars". */
export function sameCollection(a: string | undefined, b: string): boolean {
  return (a ?? "").trim().toLowerCase() === b.trim().toLowerCase();
}
