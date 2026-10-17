// What the web search setting says to a person: the wording of small-model-tools.md section 8.5.

import { ApiError, type WebSearchState, type WebSearchStatus } from "../api/types";

export const BRAVE_DASHBOARD_HOST = "api-dashboard.search.brave.com";
export const BRAVE_DASHBOARD_URL = `https://${BRAVE_DASHBOARD_HOST}`;

export const PRIVACY_LINE =
  "When the pond searches the web, your question is sent to Brave, and the top pages are read from their sites.";

export type StatusTone = "ok" | "warn" | "muted";

export const STATUS_LINES: Record<WebSearchStatus, { label: string; tone: StatusTone }> = {
  ok: { label: "On, searching with Brave", tone: "ok" },
  not_configured: { label: "Off", tone: "muted" },
  rejected: { label: "Brave did not accept the key", tone: "warn" },
  quota: { label: "This month's free searches are used up", tone: "warn" },
  unverified: { label: "Saved, not yet checked", tone: "muted" },
};

export const KEY_REFUSED = "Brave did not accept this key.";

export const OLD_POND =
  "This pond does not know about web search yet. Update the pond, then open this screen again.";

const FALLBACK = {
  read: "Could not read the web search setting. Try again in a moment.",
  save: "Could not save the key. Try again in a moment.",
  remove: "Could not remove the key. Try again in a moment.",
} as const;

export type WebSearchAction = keyof typeof FALLBACK;

const STATUSES: readonly string[] = Object.keys(STATUS_LINES);

export function isWebSearchStatus(value: unknown): value is WebSearchStatus {
  return typeof value === "string" && STATUSES.includes(value);
}

/**
 * The setting from a reply, or null when it is not one (`request` returns undefined for an empty
 * body and for an older pond's web page). An unknown status is read from `configured`.
 */
export function readWebSearchState(raw: unknown): WebSearchState | null {
  if (typeof raw !== "object" || raw === null) return null;
  const { configured, status } = raw as { configured?: unknown; status?: unknown };
  if (typeof configured !== "boolean") return null;
  if (!configured) return { provider: "brave", configured, status: "not_configured" };
  const known = isWebSearchStatus(status) && status !== "not_configured" ? status : "unverified";
  return { provider: "brave", configured, status: known };
}

/** What went wrong and what to do about it, never a server code. */
export function describeProblem(error: unknown, action: WebSearchAction): string {
  if (error instanceof ApiError) {
    if (action === "save" && error.status === 422) return KEY_REFUSED;
    if (error.status === 404 || error.status === 405) return OLD_POND;
  }
  return FALLBACK[action];
}
