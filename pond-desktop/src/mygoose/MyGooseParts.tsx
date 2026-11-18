// Small pieces every My Goose panel draws the same way.

import type { ReactNode } from "react";
import { ArrowLeft, Check } from "lucide-react";
import { InkButton, InkChip, InkNotice } from "@jarida/ink/react";
import type { ReasonChip } from "../api/types";
import { REASONS, TRY_AGAIN, buttonSize, type Variant } from "./myGooseWords";
import { problemWords } from "./problems";

export function LoadProblem({ problem, onRetry, variant }: { problem: unknown; onRetry: () => void; variant: Variant }) {
  return (
    <InkNotice
      tone="danger"
      action={
        <InkButton size={buttonSize(variant)} onPress={onRetry}>
          {TRY_AGAIN}
        </InkButton>
      }
    >
      {problemWords(problem)}
    </InkNotice>
  );
}

export function Loading({ children }: { children: ReactNode }) {
  return (
    <p className="mg__note" role="status">
      {children}
    </p>
  );
}

/** What an action did, for a moment, where the eye is not. */
export function Said({ text }: { text: string | null }) {
  if (text === null) return null;
  return (
    <p className="mg-said" role="status">
      <Check size={16} aria-hidden="true" />
      <span>{text}</span>
    </p>
  );
}

export function BackButton({ label, onBack }: { label: string; onBack: () => void }) {
  return (
    <div className="mg__bar">
      <button type="button" className="mg__back" onClick={onBack}>
        <ArrowLeft size={16} aria-hidden="true" />
        {label}
      </button>
    </div>
  );
}

export function ActionProblem({ text }: { text: string | null }) {
  if (text === null) return null;
  return (
    <p className="mg__problem" role="alert">
      {text}
    </p>
  );
}

/** Each chip sits in a 44px band that takes the tap vertically too, so the dense chip keeps its look. */
export function ReasonChips({
  value,
  onChange,
  label,
}: {
  value: readonly ReasonChip[];
  onChange: (next: ReasonChip[]) => void;
  label: string;
}) {
  function toggle(reason: ReasonChip) {
    onChange(value.includes(reason) ? value.filter((r) => r !== reason) : [...value, reason]);
  }
  return (
    <div className="mg-reasons" role="group" aria-label={label}>
      {REASONS.map((reason) => (
        <span
          key={reason.value}
          className="mg-reasons__reach"
          onClick={(e) => {
            if (e.target === e.currentTarget) toggle(reason.value);
          }}
        >
          <InkChip selected={value.includes(reason.value)} onPress={() => toggle(reason.value)}>
            {reason.label}
          </InkChip>
        </span>
      ))}
    </div>
  );
}
