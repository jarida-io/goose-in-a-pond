// How a refused or failed `/me` call reads to a member.

import { ApiError } from "../api/types";

/** The pond's 409 when it cannot tell whose My Goose a device is asking for (`caller_member`). */
export function isUnknownCaller(e: unknown): boolean {
  return e instanceof ApiError && e.status === 409 && e.message.startsWith("who_are_you");
}

/** An older pond: the route is missing, so its 404 carries no sentence of the pond's own. */
export function isNotServed(e: unknown): boolean {
  if (!(e instanceof ApiError)) return false;
  if (e.status === 405 || e.status === 501) return true;
  const body = e.body as { error?: unknown } | undefined;
  return e.status === 404 && typeof body?.error !== "string";
}

function sentence(text: string): string {
  const trimmed = text.trim();
  if (trimmed === "") return trimmed;
  const capital = trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
  return /[.!?]$/.test(capital) ? capital : `${capital}.`;
}

/** The pond's own refusal where it gave one, else what to do next. */
export function problemWords(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.status === 408) return "The pond took too long to answer. Try again.";
    if (e.status >= 500) return "The pond couldn't do that just now. Try again in a moment.";
    // The pond's 404s name its own nouns ("no such anchor"), which a member never sees.
    if (e.status === 404) return "That is no longer on the pond. Go back to see what is.";
    if (e.message.trim() !== "") return sentence(e.message);
  }
  if (e instanceof TypeError) return "Goose couldn't reach the pond. Check that it's on, then try again.";
  return "That didn't go through. Try again.";
}
