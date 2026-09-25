// ─── Settings draft state ─────────────────────────────────────────────────
// Rules for holding an unsaved edit against a server that may answer at any moment.

import type { Settings as SettingsType } from "../api/types";

/** Deep equality, as the UI replaces arrays/maps wholesale. Key order is ignored (the server
 *  serialises a `HashMap`), array order is not; plain JSON, so no cycle handling. */
export function settingsValueEquals(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  const aIsArray = Array.isArray(a);
  if (aIsArray !== Array.isArray(b)) return false;
  if (aIsArray) {
    const av = a as unknown[];
    const bv = b as unknown[];
    return av.length === bv.length && av.every((v, i) => settingsValueEquals(v, bv[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const aKeys = Object.keys(ao);
  if (aKeys.length !== Object.keys(bo).length) return false;
  return aKeys.every(
    (k) => Object.prototype.hasOwnProperty.call(bo, k) && settingsValueEquals(ao[k], bo[k]),
  );
}

/** Keys of `current` that differ from `baseline` (the server's last word). Keys missing from
 *  `current` are not reported: the endpoint is a patch and cannot express a deletion. */
export function diffSettings(
  baseline: Partial<SettingsType>,
  current: Partial<SettingsType>,
): Partial<SettingsType> {
  const out: Record<string, unknown> = {};
  const base = baseline as Record<string, unknown>;
  for (const [key, value] of Object.entries(current)) {
    if (!settingsValueEquals(value, base[key])) out[key] = value;
  }
  return out as Partial<SettingsType>;
}

/** Merge a server snapshot without losing unsaved edits: keys still equal to `baseline`
 *  take the server's value, edited keys keep the user's. */
export function foldServerState(
  prev: Partial<SettingsType>,
  baseline: Partial<SettingsType>,
  server: Partial<SettingsType>,
): Partial<SettingsType> {
  const next = { ...prev } as Record<string, unknown>;
  const prevRec = prev as Record<string, unknown>;
  const baseRec = baseline as Record<string, unknown>;
  for (const [key, value] of Object.entries(server)) {
    if (settingsValueEquals(prevRec[key], baseRec[key])) next[key] = value;
  }
  return next as Partial<SettingsType>;
}
