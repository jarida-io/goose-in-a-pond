// What the web search setting says to a person: the wording of small-model-tools.md sections 8.5 and 8.10.5.

import { ApiError, type WebSearchState, type WebSearchStatus } from "../api/types";

/** What the pond reports about nearby places: the switch, and the town a search would send. */
export type LocalResults = NonNullable<WebSearchState["local_results"]>;

export const BRAVE_DASHBOARD_HOST = "api-dashboard.search.brave.com";
export const BRAVE_DASHBOARD_URL = `https://${BRAVE_DASHBOARD_HOST}`;

export const PRIVACY_LINE =
  "When the pond searches the web, your question is sent to Brave, and the top pages are read from their sites. Pictures in the results load from Brave's image server.";

export type StatusTone = "ok" | "warn" | "muted";

export const STATUS_LINES: Record<WebSearchStatus, { label: string; tone: StatusTone }> = {
  ok: { label: "On, searching with Brave", tone: "ok" },
  not_configured: { label: "Off", tone: "muted" },
  rejected: { label: "Brave did not accept the key", tone: "warn" },
  quota: { label: "This month's free searches are used up", tone: "warn" },
  unverified: { label: "Saved, not yet checked", tone: "muted" },
};

export const NEARBY_LABEL = "Nearby places";

export const NEARBY_OLD_POND =
  "This pond cannot look for nearby places yet. Update the pond, then open this screen again.";

export const NEARBY_FAILED = "Could not save this setting. Try again in a moment.";

export const NEARBY_SET_LOCATION = "Set your location";

/** What leaves the house when the switch is on, said before it is turned: the town, and no more. */
export function nearbyDescription(town: string | null, enabled: boolean): string {
  if (town) {
    return `Sends your town (${town}) to Brave with each search, so it can find places near you. Your exact location is never sent.`;
  }
  return enabled
    ? "Your location is not set, so no town is sent. Set it in Settings, under Account and Home, then Location."
    : "Needs your location first. Set it in Settings, under Account and Home, then Location, and come back to turn this on.";
}

/** The privacy line's second sentence, only while a search would carry the town. */
export function townSentence(local: LocalResults | null): string | null {
  if (!local?.enabled || !local.town) return null;
  return `Each web search sends ${local.town} to Brave, so the results can include places near you.`;
}

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

/** The nearby-places report, or undefined from a pond that predates it. A blank town is no town. */
function readLocalResults(raw: unknown): LocalResults | undefined {
  if (typeof raw !== "object" || raw === null) return undefined;
  const { enabled, town } = raw as { enabled?: unknown; town?: unknown };
  if (typeof enabled !== "boolean") return undefined;
  const name = typeof town === "string" ? town.trim() : "";
  return { enabled, town: name === "" ? null : name };
}

/**
 * The setting from a reply, or null when it is not one (`request` returns undefined for an empty
 * body and for an older pond's web page). An unknown status is read from `configured`.
 */
export function readWebSearchState(raw: unknown): WebSearchState | null {
  if (typeof raw !== "object" || raw === null) return null;
  const { configured, status, local_results } = raw as {
    configured?: unknown;
    status?: unknown;
    local_results?: unknown;
  };
  if (typeof configured !== "boolean") return null;
  const local = readLocalResults(local_results);
  const withLocal = local ? { local_results: local } : {};
  if (!configured) return { provider: "brave", configured, status: "not_configured", ...withLocal };
  const known = isWebSearchStatus(status) && status !== "not_configured" ? status : "unverified";
  return { provider: "brave", configured, status: known, ...withLocal };
}

/** What went wrong and what to do about it, never a server code. */
export function describeProblem(error: unknown, action: WebSearchAction): string {
  if (error instanceof ApiError) {
    if (action === "save" && error.status === 422) return KEY_REFUSED;
    if (error.status === 404 || error.status === 405) return OLD_POND;
  }
  return FALLBACK[action];
}
