// useOnboardingPersist: saves each wizard step to the backend.

import { useState, useCallback } from "react";
import { api } from "../../../api/PondApiClient";
import type { OnboardingDraft } from "../onboarding.types";
import type { Settings } from "../../../api/types";
import { FE_STEP_TO_BE } from "../onboarding.constants";

/** Persists a step: Settings fields via PUT /api/v1/settings, member prefs (name, birthday,
 *  accessibility) via PATCH /api/v1/profiles/{id}, creating a primary profile if needed. */
export function useOnboardingPersist() {
  const [isPersisting, setIsPersisting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const persist = useCallback(async (stepId: string, draft: OnboardingDraft): Promise<boolean> => {
    setError(null);
    setIsPersisting(true);

    try {
      const settingsPatch = buildSettingsPatch(stepId, draft);
      const profilePatch = buildProfilePatch(stepId, draft);

      if (settingsPatch && Object.keys(settingsPatch).length > 0) {
        await api.updateSettings(settingsPatch);
      }

      if (profilePatch && Object.keys(profilePatch).length > 0) {
        const profileId = await ensurePrimaryProfile(draft.userName);
        if (profileId) {
          await api.updateProfilePrefs(profileId, profilePatch);
        }
      }

      // Record progress so a quit resumes here; non-fatal, it must never block advancing.
      const beStep = FE_STEP_TO_BE[stepId];
      if (beStep) {
        try { await api.recordOnboardingStep(beStep); }
        catch (err) { console.warn("onboard step tracking failed (non-fatal):", err); }
      }

      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to save settings");
      return false;
    } finally {
      setIsPersisting(false);
    }
  }, []);

  const clearError = useCallback(() => setError(null), []);

  return { persist, isPersisting, error, clearError };
}

function buildSettingsPatch(stepId: string, draft: OnboardingDraft): Partial<Settings> | null {
  switch (stepId) {
    case "about-you":
      return {
        user_name: draft.userName.trim(),
      };

    case "locale":
      return {
        timezone: draft.timezone,
        weather_location_name: draft.locationName,
        weather_enabled: draft.enableWeather,
        // 0/0 means unset: sending it would overwrite coordinates the server already geocoded.
        ...(draft.latitude !== 0 || draft.longitude !== 0
          ? { weather_latitude: draft.latitude, weather_longitude: draft.longitude }
          : {}),
      };

    case "personality":
      return {
        prompt_style: draft.promptStyle,
        assistant_personality: draft.personality,
        assistant_name: draft.assistantName.trim(),
        voice_tts_voice: draft.ttsVoice,
        voice_tts_speed: draft.ttsRate / 100,
      };

    case "wake-word":
      return {
        voice_wake_word: draft.wakeWord === "custom"
          ? draft.wakeWordCustom.trim()
          : draft.wakeWord,
      };

    case "complete":
      return {
        agent_memory_inject: draft.enableMcpMemory,
      };

    default:
      return null;
  }
}

/** The `profiles.preferences` keys `routes.rs :: particulars_for` reads; exported for tests. */
export const PROFILE_PREF_KEYS = {
  preferredName: "preferred_name",
  birthday: "birthday",
  language: "language",
  atypicalSpeech: "accessibility_atypical_speech",
  slowSpeech: "accessibility_slow_speech",
  highContrast: "accessibility_high_contrast",
  reduceMotion: "accessibility_reduce_motion",
  avatar: "avatar",
} as const;

/** String values (`profiles.preferences` is HashMap<String, String>; flags compare to "true").
 *  Empty values are omitted: the prompt builder would render them blank. */
export function buildProfilePatch(
  stepId: string,
  draft: OnboardingDraft,
): Record<string, string> | null {
  if (stepId !== "about-you") return null;

  const raw: Record<string, string> = {
    [PROFILE_PREF_KEYS.preferredName]: (draft.preferredName ?? "").trim(),
    [PROFILE_PREF_KEYS.birthday]: (draft.birthday ?? "").trim(),
    [PROFILE_PREF_KEYS.avatar]: draft.avatar ?? "",
    [PROFILE_PREF_KEYS.atypicalSpeech]: draft.atypicalSpeech ? "true" : "false",
    [PROFILE_PREF_KEYS.slowSpeech]: draft.slowSpeech ? "true" : "false",
    [PROFILE_PREF_KEYS.highContrast]: draft.highContrast ? "true" : "false",
    [PROFILE_PREF_KEYS.reduceMotion]: draft.reduceMotion ? "true" : "false",
  };
  return Object.fromEntries(Object.entries(raw).filter(([, v]) => v !== ""));
}

/** Primary profile id, created if the pond has none; null on failure so onboarding never blocks. */
async function ensurePrimaryProfile(userName: string): Promise<string | null> {
  try {
    const settings = await api.getSettings();
    const existing = settings.primary_profile_id;
    if (existing) return existing;

    const name = (userName ?? "").trim() || "Me";
    const created = await api.createProfile(name);
    await api.updateSettings({ primary_profile_id: created.id });
    return created.id;
  } catch (err) {
    console.warn("could not resolve a primary profile for preferences:", err);
    return null;
  }
}
