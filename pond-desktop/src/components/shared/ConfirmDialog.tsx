import { createContext, useContext, useState, useCallback, useRef, useId, type ReactNode } from "react";
import { AlertTriangle } from "lucide-react";
import { useDialogFocusTrap } from "./useDialogFocusTrap";

// ── Types ────────────────────────────────────────────────────────

interface ConfirmOptions {
  title?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  destructive?: boolean;
}

type ConfirmFn = (message: string, options?: ConfirmOptions) => Promise<boolean>;

// ── Context ──────────────────────────────────────────────────────

const ConfirmContext = createContext<ConfirmFn | null>(null);

export function useConfirm(): ConfirmFn {
  const fn = useContext(ConfirmContext);
  if (!fn) throw new Error("useConfirm must be used within ConfirmProvider");
  return fn;
}

// ── Provider ─────────────────────────────────────────────────────

interface PendingConfirm {
  message: string;
  options: ConfirmOptions;
  resolve: (value: boolean) => void;
}

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<PendingConfirm | null>(null);
  const confirmBtnRef = useRef<HTMLButtonElement>(null);
  const titleId = useId();

  const confirm = useCallback<ConfirmFn>((message, options = {}) => {
    return new Promise<boolean>((resolve) => {
      setPending({ message, options, resolve });
    });
  }, []);

  const handleConfirm = useCallback(() => {
    pending?.resolve(true);
    setPending(null);
  }, [pending]);

  const handleCancel = useCallback(() => {
    pending?.resolve(false);
    setPending(null);
  }, [pending]);

  // Focus Confirm, not the first focusable element, so Enter confirms.
  const dialogRef = useDialogFocusTrap<HTMLDivElement>(!!pending, handleCancel, confirmBtnRef);

  const { title, confirmLabel, cancelLabel, destructive } = pending?.options ?? {};

  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      {pending && (
        <div className="confirm-overlay" onClick={handleCancel}>
          <div
            ref={dialogRef}
            className="confirm-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
            tabIndex={-1}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="confirm-dialog__header">
              <div className={`confirm-dialog__icon ${destructive ? "confirm-dialog__icon--destructive" : ""}`}>
                <AlertTriangle size={18} />
              </div>
              <h3 id={titleId} className="confirm-dialog__title">{title ?? "Confirm"}</h3>
            </div>
            <p className="confirm-dialog__message">{pending.message}</p>
            <div className="confirm-dialog__actions">
              <button className="confirm-dialog__btn confirm-dialog__btn--cancel" onClick={handleCancel}>
                {cancelLabel ?? "Cancel"}
              </button>
              <button
                ref={confirmBtnRef}
                className={`confirm-dialog__btn ${destructive ? "confirm-dialog__btn--destructive" : "confirm-dialog__btn--confirm"}`}
                onClick={handleConfirm}
              >
                {confirmLabel ?? "Confirm"}
              </button>
            </div>
          </div>
        </div>
      )}
    </ConfirmContext.Provider>
  );
}
