// The sign that the composer is in research mode, with the way out and how much the pond writes.
// Sits above the field.

import { Telescope, X } from "lucide-react";
import type { ResearchAnswer } from "../api/types";
import { DEEP_RESEARCH, LEAVE_RESEARCH, RESEARCH_ANSWERS, RESEARCH_ANSWER_LABEL, RESEARCH_WAITS } from "./deepResearchCopy";
import "./composerMenu.css";

interface Props {
  variant: "classic" | "hub";
  /** A reply is still streaming, so Send is held until it ends. */
  waiting: boolean;
  onLeave: () => void;
  answer: ResearchAnswer;
  onAnswer: (answer: ResearchAnswer) => void;
}

export function ResearchModeChip({ variant, waiting, onLeave, answer, onAnswer }: Props) {
  return (
    <div
      className={`rchip-row rchip-row--${variant}`}
      // Escape on the chip's own button is as good as the field's.
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          onLeave();
        }
      }}
    >
      <span className="rchip">
        <Telescope size={14} aria-hidden="true" />
        <span className="rchip__label">{DEEP_RESEARCH}</span>
        <button type="button" className="rchip__x" aria-label={LEAVE_RESEARCH} title={LEAVE_RESEARCH} onClick={onLeave}>
          <X size={14} aria-hidden="true" />
        </button>
      </span>
      <span className="rchip-len" role="group" aria-label={RESEARCH_ANSWER_LABEL}>
        {RESEARCH_ANSWERS.map((choice) => (
          <button
            key={choice.value}
            type="button"
            className="rchip-len__opt"
            aria-pressed={answer === choice.value}
            title={choice.hint}
            onClick={() => onAnswer(choice.value)}
          >
            {choice.label}
          </button>
        ))}
      </span>
      {waiting && (
        <span className="rchip__wait" role="status">
          {RESEARCH_WAITS}
        </span>
      )}
    </div>
  );
}
