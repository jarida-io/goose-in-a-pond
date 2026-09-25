// One-time lift of member preferences from localStorage to the server. Never overwrites a
// server value, keeps localStorage (a marker stops reruns), never throws.

import { api } from "./PondApiClient";

const LOCAL_KEY = "giap-user-profile";
const DONE_KEY = "giap-user-profile-migrated";

/** camelCase in the browser, snake_case on the server. */
const KEY_MAP: Record<string, string> = {
  preferredName: "preferred_name",
  birthday: "birthday",
  language: "language",
  avatar: "avatar",
  atypicalSpeech: "accessibility_atypical_speech",
  slowSpeech: "accessibility_slow_speech",
  highContrast: "accessibility_high_contrast",
  reduceMotion: "accessibility_reduce_motion",
};

/** `profiles.preferences` is a string map; the server compares `== "true"`. */
function asPrefString(value: unknown): string | null {
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed === "" ? null : trimmed;
  }
  if (typeof value === "number") return String(value);
  return null;
}

export async function migrateLocalProfileToServer(): Promise<void> {
  try {
    if (localStorage.getItem(DONE_KEY)) return;

    const raw = localStorage.getItem(LOCAL_KEY);
    if (!raw) {
      localStorage.setItem(DONE_KEY, "1");
      return;
    }

    let local: Record<string, unknown>;
    try {
      local = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      // Unparseable is not worth retrying every boot.
      localStorage.setItem(DONE_KEY, "1");
      return;
    }

    const settings = await api.getSettings();
    const profileId = settings.primary_profile_id;
    // No member yet: leave the marker unset so a later boot can still migrate.
    if (!profileId) return;

    const profiles = await api.listProfiles();
    if (!profiles.profiles.some((p) => p.id === profileId)) return;

    const existing = await api.getProfilePrefs(profileId);

    const patch: Record<string, string> = {};
    for (const [camel, snake] of Object.entries(KEY_MAP)) {
      if (existing[snake] !== undefined && existing[snake] !== "") continue;
      const value = asPrefString(local[camel]);
      if (value !== null) patch[snake] = value;
    }

    if (Object.keys(patch).length > 0) {
      await api.updateProfilePrefs(profileId, { ...existing, ...patch });
      console.info(
        `migrated ${Object.keys(patch).length} member preference(s) from this browser to the pond`,
      );
    }
    localStorage.setItem(DONE_KEY, "1");
  } catch (err) {
    // Retried next boot: the marker is only set on a path that reached the API.
    console.warn("could not migrate local member preferences (will retry):", err);
  }
}
