// Step 4, Wake Word. Calibration lives in Settings → Voice so setup never waits on mic permission.

import { Mic, Settings2 } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { RadioCard } from "../primitives/RadioCard";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { WAKE_PRESETS } from "../onboarding.constants";

export function StepWakeWord() {
  const { draft, patch } = useOnboarding();

  const isCustom = draft.wakeWord === "custom";

  return (
    <div className="ob-step-content">
      <Lead>
        Say this phrase to activate Goose when it's listening in voice mode.
      </Lead>

      {/* Wake phrase selection */}
      <div className="ob-field">
        <FormLabel>Wake phrase</FormLabel>
        <div className="ob-card-grid">
          {WAKE_PRESETS.map((p) => (
            <RadioCard
              key={p.value}
              selected={draft.wakeWord === p.value}
              onClick={() => patch({ wakeWord: p.value })}
            >
              <div className="ob-radio-card__content">
                <strong className="ob-radio-card__label">{p.label}</strong>
                <span className="ob-radio-card__desc">{p.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      {/* Custom phrase input */}
      {isCustom && (
        <div className="ob-field">
          <FormLabel>Your custom phrase <span className="ob-required">*</span></FormLabel>
          <input
            className="ob-input"
            placeholder="e.g. hey duck, morning pond"
            value={draft.wakeWordCustom}
            onChange={(e) => patch({ wakeWordCustom: e.target.value })}
            required
          />
        </div>
      )}

      {/* Calibration is deferred — no mic permission prompt during onboarding. */}
      <div className="ob-calibration">
        <div className="ob-calibration__header">
          <div>
            <div className="ob-calibration__title">
              <Mic size={16} strokeWidth={1.8} className="ob-calibration__title-icon" />
              Calibrate later
            </div>
            <p className="ob-calibration__desc">
              Calibration teaches Goose how your voice sounds so it hears your
              wake word more reliably. It needs microphone access, so we've left
              it out of setup. You can calibrate any time in{" "}
              <strong>Settings &rarr; Voice</strong>.
            </p>
          </div>
          <div className="ob-calibration__later" aria-hidden="true">
            <Settings2 size={18} strokeWidth={1.8} />
          </div>
        </div>
      </div>
    </div>
  );
}
