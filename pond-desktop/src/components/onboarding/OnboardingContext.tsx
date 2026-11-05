// Onboarding wizard provider: holds the draft every step reads and patches.

import React, { useState, useCallback, useEffect } from "react";
import type { OnboardingDraft } from "./onboarding.types";
import { DEFAULT_DRAFT } from "./onboarding.constants";
import { OnboardingContext } from "./onboarding.context";
import { api } from "../../api/PondApiClient";

export function OnboardingProvider({ children }: { children: React.ReactNode }) {
  const [draft, setDraft] = useState<OnboardingDraft>(DEFAULT_DRAFT);
  const [loading, setLoading] = useState(true);
  const [touched, setTouched] = useState<Set<keyof OnboardingDraft>>(new Set());

  // The only way a step changes the draft; marks every key it sets as touched, so
  // `buildSettingsPatch` later knows the user actually set it, not just saw it pre-filled.
  const patch = useCallback((p: Partial<OnboardingDraft>) => {
    setDraft((d) => ({ ...d, ...p }));
    setTouched((t) => {
      const next = new Set(t);
      for (const key of Object.keys(p)) next.add(key as keyof OnboardingDraft);
      return next;
    });
  }, []);

  // Resume support: pre-populate draft from saved settings + localStorage. Goes through `setDraft`
  // directly, not `patch` — these are the server's/localStorage's own values, not something the
  // user just chose, so they must not count as touched.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const settings = await api.getSettings();
        // Load user profile from localStorage (fields not in Settings)
        const stored = localStorage.getItem("giap-user-profile");
        const profile = stored ? JSON.parse(stored) : {};

        if (cancelled) return;
        setDraft((d) => ({
          ...d,
          userName: settings.user_name || DEFAULT_DRAFT.userName,
          preferredName: profile.preferredName || DEFAULT_DRAFT.preferredName,
          birthday: profile.birthday || DEFAULT_DRAFT.birthday,
          avatar: profile.avatar || DEFAULT_DRAFT.avatar,
          atypicalSpeech: profile.atypicalSpeech ?? DEFAULT_DRAFT.atypicalSpeech,
          slowSpeech: profile.slowSpeech ?? DEFAULT_DRAFT.slowSpeech,
          highContrast: profile.highContrast ?? DEFAULT_DRAFT.highContrast,
          reduceMotion: profile.reduceMotion ?? DEFAULT_DRAFT.reduceMotion,
          timezone: settings.timezone || DEFAULT_DRAFT.timezone,
          locationName: settings.weather_location_name || DEFAULT_DRAFT.locationName,
          enableWeather: settings.weather_enabled ?? DEFAULT_DRAFT.enableWeather,
          promptStyle: settings.prompt_style || DEFAULT_DRAFT.promptStyle,
          personality: settings.assistant_personality || DEFAULT_DRAFT.personality,
          assistantName: settings.assistant_name || DEFAULT_DRAFT.assistantName,
          ttsVoice: settings.voice_tts_voice || DEFAULT_DRAFT.ttsVoice,
          wakeWord: settings.voice_wake_word || DEFAULT_DRAFT.wakeWord,
          enableMcpMemory: settings.agent_memory_inject ?? DEFAULT_DRAFT.enableMcpMemory,
        }));
      } catch {
        // Settings not available yet — use defaults
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  return (
    <OnboardingContext.Provider value={{ draft, patch, loading, touched }}>
      {children}
    </OnboardingContext.Provider>
  );
}
