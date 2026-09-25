// Onboarding wizard context: the draft and a patch helper for every step.

import React, { createContext, useContext, useState, useCallback, useEffect } from "react";
import type { OnboardingDraft } from "./onboarding.types";
import { DEFAULT_DRAFT } from "./onboarding.constants";
import { api } from "../../api/PondApiClient";

interface OnboardingContextValue {
  draft: OnboardingDraft;
  patch: (p: Partial<OnboardingDraft>) => void;
  /** True while loading saved settings to pre-populate the draft. */
  loading: boolean;
}

const Ctx = createContext<OnboardingContextValue | null>(null);

export function OnboardingProvider({ children }: { children: React.ReactNode }) {
  const [draft, setDraft] = useState<OnboardingDraft>(DEFAULT_DRAFT);
  const [loading, setLoading] = useState(true);

  const patch = useCallback((p: Partial<OnboardingDraft>) => {
    setDraft((d) => ({ ...d, ...p }));
  }, []);

  // Resume support: pre-populate draft from saved settings + localStorage
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const settings = await api.getSettings();
        // Load user profile from localStorage (fields not in Settings)
        const stored = localStorage.getItem("giap-user-profile");
        const profile = stored ? JSON.parse(stored) : {};

        if (cancelled) return;
        patch({
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
        });
      } catch {
        // Settings not available yet — use defaults
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [patch]);

  return (
    <Ctx.Provider value={{ draft, patch, loading }}>
      {children}
    </Ctx.Provider>
  );
}

export function useOnboarding(): OnboardingContextValue {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useOnboarding must be used within OnboardingProvider");
  return ctx;
}
