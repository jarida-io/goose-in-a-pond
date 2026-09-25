import { Goose } from "../../../components/Goose";
import { workingQuip } from "../../../components/quips";

interface TypingIndicatorProps {
  /** Stable for a turn, so the quip doesn't reshuffle mid-run. */
  seed?: number;
  /** The backend's `status` frame; preferred over the local quip, since the server knows which tool runs. */
  status?: string;
}

/** Shown above the composer while a turn runs: app state rather than a message, and always in view. */
export function TypingIndicator({ seed = 0, status }: TypingIndicatorProps) {
  return (
    <div className="ch-working" role="status" aria-live="polite">
      <Goose state="working" size={40} water={false} />
      {/* The backend's own account first; the local quip only covers the gap
          before the first status frame arrives. */}
      <span className="ch-working__quip">{status?.trim() || workingQuip(seed)}</span>
      <span className="ch-working__dots" aria-hidden="true">
        <span />
        <span />
        <span />
      </span>
    </div>
  );
}
