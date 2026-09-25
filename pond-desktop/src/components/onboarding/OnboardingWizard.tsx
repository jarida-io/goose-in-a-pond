// Onboarding wizard orchestrator: step index, persist-on-advance, validation, transitions.

import { useState, useCallback, useEffect } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { Logo } from "../Logo";
import { api } from "../../api/PondApiClient";

import { OnboardingProvider, useOnboarding } from "./OnboardingContext";
import { STEPS, beStepToFeIndex } from "./onboarding.constants";
import { useOnboardingPersist } from "./hooks/useOnboardingPersist";
import { StepHeader, Actions, ErrorBanner } from "./primitives/StepShell";

import { StepWelcome } from "./steps/StepWelcome";
import { StepAboutYou } from "./steps/StepAboutYou";
import { StepLocale } from "./steps/StepLocale";
import { StepPersonality } from "./steps/StepPersonality";
import { StepWakeWord } from "./steps/StepWakeWord";
import { StepComplete } from "./steps/StepComplete";

import "./onboarding.css";

// ── Side Rail ────────────────────────────────────────────────

function StepRail({ stepIndex, onJump }: { stepIndex: number; onJump: (i: number) => void }) {
  return (
    <aside className="ob-rail">
      <div className="ob-rail__brand">
        <Logo size={32} className="ob-rail__brand-logo" />
        <div>
          <div className="ob-rail__brand-name">Goose In A Pond</div>
          <div className="ob-rail__brand-sub">First-time setup</div>
        </div>
      </div>

      <ol className="ob-rail__steps">
        {STEPS.map((s, i) => {
          const done = i < stepIndex;
          const active = i === stepIndex;
          return (
            <li key={s.id}>
              <button
                type="button"
                onClick={() => done && onJump(i)}
                disabled={!done && !active}
                className={`ob-rail__step-btn ${active ? "ob-rail__step-btn--active" : ""} ${done ? "ob-rail__step-btn--done" : ""}`}
              >
                <span className={`ob-rail__dot ${active ? "ob-rail__dot--active" : ""} ${done ? "ob-rail__dot--done" : ""}`}>
                  {done ? (
                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3.5" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M5 12l5 5 9-11" />
                    </svg>
                  ) : (
                    i + 1
                  )}
                </span>
                <span className="ob-rail__step-text">
                  <span className={`ob-rail__step-label ${active ? "ob-rail__step-label--active" : ""} ${done ? "ob-rail__step-label--done" : ""}`}>
                    {s.label}
                  </span>
                  <span className="ob-rail__step-caption">{s.caption}</span>
                </span>
              </button>
            </li>
          );
        })}
      </ol>

      <div className="ob-rail__footer">
        <span className="ob-rail__status">
          <span className="ob-rail__status-dot" />
          Connected &middot; pond.local
        </span>
      </div>
    </aside>
  );
}

// ── Inner Wizard (needs OnboardingContext) ────────────────────

function WizardInner({ onComplete }: { onComplete: () => void }) {
  const { draft, loading } = useOnboarding();
  const { persist, isPersisting, error, clearError } = useOnboardingPersist();
  const [stepIndex, setStepIndex] = useState(0);

  // Resume at the furthest step the backend has recorded; on failure start at 0.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const status = await api.getOnboardingStatus();
        if (cancelled || status.onboarded) return;
        const resumeAt = beStepToFeIndex(status.current_step);
        if (resumeAt > 0) setStepIndex(resumeAt);
      } catch { /* not started / offline — start from the beginning */ }
    })();
    return () => { cancelled = true; };
  }, []);

  const isWelcome = stepIndex === 0;
  const isDone = stepIndex === STEPS.length - 1;
  const currentStep = STEPS[stepIndex];

  // ── Validation ──────────────────────────────────────────

  const canProceed = (() => {
    switch (currentStep.id) {
      case "about-you": return !!draft.userName.trim();
      case "personality": return !!draft.assistantName.trim();
      case "wake-word": return draft.wakeWord !== "custom" || !!draft.wakeWordCustom.trim();
      default: return true;
    }
  })();

  // ── Navigation ──────────────────────────────────────────

  const next = useCallback(async () => {
    clearError();
    const ok = await persist(currentStep.id, draft);
    if (ok) {
      setStepIndex((i) => Math.min(i + 1, STEPS.length - 1));
    }
  }, [currentStep.id, draft, persist, clearError]);

  const skip = useCallback(async () => {
    clearError();
    // Skip still persists defaults (non-required steps)
    await persist(currentStep.id, draft);
    setStepIndex((i) => Math.min(i + 1, STEPS.length - 1));
  }, [currentStep.id, draft, persist, clearError]);

  const back = useCallback(() => {
    clearError();
    setStepIndex((i) => Math.max(i - 1, 0));
  }, [clearError]);

  // ── Step content ────────────────────────────────────────

  let body: React.ReactNode;
  switch (currentStep.id) {
    case "welcome":     body = <StepWelcome onNext={() => setStepIndex(1)} />; break;
    case "about-you":   body = <StepAboutYou />; break;
    case "locale":      body = <StepLocale />; break;
    case "personality": body = <StepPersonality />; break;
    case "wake-word":   body = <StepWakeWord />; break;
    case "complete":    body = <StepComplete onFinish={onComplete} />; break;
    default:            body = null;
  }

  const motionDuration = draft.reduceMotion ? 0 : 0.2;

  if (loading) {
    return (
      <div className="ob-shell">
        <StepRail stepIndex={0} onJump={() => {}} />
        <main className="ob-main ob-main--centered">
          <div className="ob-main__inner ob-loading">
            <p className="ob-loading__text">Loading your settings...</p>
          </div>
        </main>
      </div>
    );
  }

  return (
    <div className="ob-shell">
      <StepRail stepIndex={stepIndex} onJump={setStepIndex} />

      <main className={`ob-main ${isWelcome || isDone ? "ob-main--centered" : ""}`}>
        <AnimatePresence mode="wait">
          <motion.div
            key={stepIndex}
            className="ob-main__inner"
            initial={{ opacity: 0, x: 20 }}
            animate={{ opacity: 1, x: 0 }}
            exit={{ opacity: 0, x: -20 }}
            transition={{ duration: motionDuration, ease: "easeOut" }}
          >
            <StepHeader stepIndex={stepIndex} />

            {error && <ErrorBanner message={error} onDismiss={clearError} />}

            {body}

            {!isWelcome && !isDone && (
              <Actions
                onBack={back}
                onNext={next}
                onSkip={currentStep.required ? undefined : skip}
                nextLabel={stepIndex === STEPS.length - 2 ? "Finish" : "Continue"}
                disabled={!canProceed}
                isPersisting={isPersisting}
              />
            )}
          </motion.div>
        </AnimatePresence>
      </main>
    </div>
  );
}

// ── Public export (wraps with OnboardingProvider) ─────────────

interface OnboardingWizardProps {
  onComplete: () => void;
}

export function OnboardingWizard({ onComplete }: OnboardingWizardProps) {
  return (
    <OnboardingProvider>
      <WizardInner onComplete={onComplete} />
    </OnboardingProvider>
  );
}
