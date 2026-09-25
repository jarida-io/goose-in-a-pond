import { useState } from "react";
import { Gauge } from "lucide-react";
import { api } from "../api/PondApiClient";
import { TURNS_REMAINING_UNKNOWN } from "../api/types";
import type { CompactionReport, ContextWarning } from "../api/types";

/** Shows `context_warning` with a compact button. Not in `TurnStatsFooter` (hidden by default);
 *  refusals are a polite note, never `role="alert"`; `turns_remaining` is clamped. */

interface ContextPressureNoteProps {
  warning: ContextWarning;
  /** Read at click time: the frame has no session id, and a first turn's id comes with `done`. */
  sessionId: string | null;
  onCompacted?: (report: CompactionReport) => void;
}

/** Server `reason` → plain text; keys stay verbatim so a new reason misses, not mislabels. */
const REASON_TEXT: Record<string, string> = {
  cooling_down: "A compaction ran recently — try again in a few turns.",
  not_under_pressure: "There is still room in this window.",
  already_running: "Already compacting.",
  nothing_to_summarise: "Nothing new to summarise.",
  preempted_by_turn: "Stopped because you started typing.",
  no_summariser: "No model is loaded to write the summary.",
  monitor_disabled: "Context monitoring is switched off in Settings.",
  compaction_disabled: "Compaction is switched off in Settings.",
  failed: "The summariser did not finish. Nothing was changed.",
};

function reportText(report: CompactionReport): string {
  if (report.status === "compacted") {
    return "Compacted — the window has room again.";
  }
  if (report.reason && REASON_TEXT[report.reason]) {
    return REASON_TEXT[report.reason];
  }
  return "Nothing was compacted.";
}

function pressureLine(warning: ContextWarning): string {
  // `warning` is only set above 60%, but the frame also fires below that when few turns remain.
  const base =
    warning.warning ??
    `This conversation is using ${Math.round(warning.utilization_pct)}% of the context window.`;

  if (warning.turns_remaining === TURNS_REMAINING_UNKNOWN) {
    return base;
  }
  const turns = warning.turns_remaining;
  return `${base} About ${turns} ${turns === 1 ? "turn" : "turns"} left at this rate.`;
}

export function ContextPressureNote({
  warning,
  sessionId,
  onCompacted,
}: ContextPressureNoteProps) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  async function compactNow() {
    if (!sessionId || busy) return;
    setBusy(true);
    setNote(null);
    try {
      const report = await api.compactSession(sessionId);
      setNote(reportText(report));
      onCompacted?.(report);
    } catch {
      // A real fault (server gone, a 500); still neutral, since nothing was lost or changed.
      setNote("Could not reach the pond just now. Nothing was changed.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="ctx-pressure">
      <span className="ctx-pressure__note">
        <Gauge size={11} aria-hidden /> {pressureLine(warning)}
      </span>
      <button
        className="ctx-pressure__btn"
        onClick={compactNow}
        disabled={busy || !sessionId}
      >
        {busy ? "Compacting…" : "Compact now"}
      </button>
      {note && (
        <span className="ctx-pressure__result" aria-live="polite">
          {note}
        </span>
      )}
    </div>
  );
}
