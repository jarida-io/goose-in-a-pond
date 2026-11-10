// Onboarding wizard context: the draft and a patch helper for every step. `OnboardingProvider`
// in OnboardingContext.tsx supplies it.

import { createContext, useContext } from "react";
import type { OnboardingDraft } from "./onboarding.types";

interface OnboardingContextValue {
  draft: OnboardingDraft;
  patch: (p: Partial<OnboardingDraft>) => void;
  /** True while loading saved settings to pre-populate the draft. */
  loading: boolean;
  /** Draft keys a step has actually set via `patch`, not merely pre-filled from saved settings —
   *  what `buildSettingsPatch` sends on, so skipping a step sends (and marks user-set) nothing. */
  touched: ReadonlySet<keyof OnboardingDraft>;
}

export const OnboardingContext = createContext<OnboardingContextValue | null>(null);

export function useOnboarding(): OnboardingContextValue {
  const ctx = useContext(OnboardingContext);
  if (!ctx) throw new Error("useOnboarding must be used within OnboardingProvider");
  return ctx;
}
