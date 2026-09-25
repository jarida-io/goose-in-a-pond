import { useState } from "react";
import { ChevronRight } from "lucide-react";

/** "a moment" / "8 seconds" / "1m 04s" — never a bare millisecond count. */
export function formatThinkingTime(ms: number | undefined): string {
  if (ms === undefined || ms < 0) return "a moment";
  const seconds = Math.round(ms / 1000);
  if (seconds < 1) return "a moment";
  if (seconds === 1) return "1 second";
  if (seconds < 60) return `${seconds} seconds`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

interface ThinkingDisclosureProps {
  blocks: string[];
  /** Still reasoning — shows the sweep and the present tense. */
  active: boolean;
  /** Wall time the reasoning spanned; undefined while it is still running. */
  ms?: number;
}

/** The model's reasoning as one openable line saying how long it took; past tense once it ends. */
export function ThinkingDisclosure({ blocks, active, ms }: ThinkingDisclosureProps) {
  const [open, setOpen] = useState(false);
  const label = active ? "Thinking" : `Thought for ${formatThinkingTime(ms)}`;

  return (
    <div className={`think${active ? " think--active" : ""}${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="think__toggle"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="think__label">{label}</span>
        <ChevronRight className="think__chev" size={14} aria-hidden="true" />
      </button>

      {open && (
        <div className="think__body">
          {blocks.map((block, i) => (
            <p key={i}>{block}</p>
          ))}
        </div>
      )}
    </div>
  );
}
