import { useId, useState } from "react";
import { AlertTriangle, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { useDialogFocusTrap } from "../components/shared";

/** The copy a refused reset shows; anything else falls back to the raw server message. */
const REFUSAL_COPY: Record<string, string> = {
  pond_has_no_name: "This Pond has no name set, so there's nothing to type to confirm.",
  pond_name_mismatch: "That doesn't match this Pond's name. Nothing was touched.",
  payments_held:
    "This Pond is holding a mesh-credit balance with another Pond. Settle it before resetting.",
  factory_reset_unavailable: "Factory reset isn't available on this Pond yet.",
};

interface Props {
  /** `Settings.home_name`; typed input must match this exactly before the reset is armed. */
  pondName: string;
  onClose: () => void;
}

export function FactoryResetModal({ pondName, onClose }: Props) {
  const trimmedName = pondName.trim();
  // An unnamed Pond has nothing safe to type as confirmation, so it gets its own step instead
  // of a warning/confirm card with a blank name in it.
  const [step, setStep] = useState<"unnamed" | "warning" | "confirm">(
    trimmedName ? "warning" : "unnamed",
  );
  const [typed, setTyped] = useState("");
  const [resetting, setResetting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const titleId = useId();
  const dialogRef = useDialogFocusTrap<HTMLDivElement>(true, onClose);

  const matches = trimmedName !== "" && typed === pondName;

  async function handleReset() {
    setResetting(true);
    setError(null);
    try {
      await api.factoryReset(typed);
      // The server has already reopened onboarding; reload so the app picks that up fresh.
      window.location.reload();
    } catch (e) {
      setError(
        e instanceof ApiError ? (REFUSAL_COPY[e.message] ?? e.message) : String(e),
      );
      setResetting(false);
    }
  }

  return (
    <div className="sched-modal__overlay" onClick={onClose}>
      <div
        ref={dialogRef}
        className="sched-modal__dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="sched-modal__header">
          <h2 id={titleId} className="sched-modal__title danger-modal__title">
            <AlertTriangle size={16} aria-hidden="true" />
            {step === "unnamed" ? "Name this Pond first" : step === "warning" ? "Warning" : "Factory reset"}
          </h2>
          <button className="sched-modal__close" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        {step === "unnamed" && (
          <div className="sched-modal__body">
            <p>
              This Pond doesn't have a name yet, so there's nothing safe to type to confirm
              a reset.
            </p>
            <p>
              Set one in <strong>Account &amp; Home → Home name</strong>, then come back here.
            </p>
          </div>
        )}

        {step === "warning" && (
          <div className="sched-modal__body">
            <p>
              This erases every household member, conversation, memory, device, room,
              saved Wi-Fi network and secret on this Pond, and reopens setup.
            </p>
            <p>
              <strong>This cannot be undone.</strong> Are you sure you want to continue?
            </p>
          </div>
        )}

        {step === "confirm" && (
          <div className="sched-modal__body">
            <div className="sched-modal__field">
              <label className="sched-modal__label" htmlFor="factory-reset-name">
                Type this Pond's name below to erase it:
              </label>
              <div className="danger-modal__nameToType">{pondName}</div>
              <input
                id="factory-reset-name"
                className="sched-modal__input"
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                disabled={resetting}
                autoComplete="off"
                autoFocus
              />
            </div>
            {error && (
              <p className="text-error text-error--sm" role="alert">{error}</p>
            )}
          </div>
        )}

        <div className="sched-modal__footer">
          {step === "unnamed" ? (
            <button type="button" className="danger-modal__btn danger-modal__btn--safe" onClick={onClose}>
              Got it
            </button>
          ) : (
            <>
              <button
                type="button"
                className="danger-modal__btn danger-modal__btn--safe"
                onClick={onClose}
                disabled={resetting}
              >
                Cancel
              </button>
              {step === "warning" ? (
                <button
                  type="button"
                  className="danger-modal__btn danger-modal__btn--danger"
                  onClick={() => setStep("confirm")}
                >
                  Yes, I'm sure
                </button>
              ) : (
                <button
                  type="button"
                  className="danger-modal__btn danger-modal__btn--danger"
                  disabled={!matches || resetting}
                  onClick={handleReset}
                >
                  {resetting ? "Resetting…" : "Erase everything"}
                </button>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
