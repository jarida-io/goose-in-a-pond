// Desktop's "You could ask", right under the composer (plan §5) -- a sibling to
// SuggestionQueue, not a mode of it. SuggestionQueue's offers and proposals share
// one component because they share a cursor/fetch lifecycle; proposals have no
// business living under the composer, so splitting this out was cheaper than
// threading a render-location flag through all of that shared state.
//
// This duplicates SuggestionQueue's suggestions fetch and `take()` -- both are a
// few lines, and importing SuggestionQueue's internals would couple two
// independently-positioned pieces of UI for the sake of not repeating a fetch.

import { useCallback, useEffect, useState, type ReactElement } from "react";
import { api } from "../../api/PondApiClient";
import type { Suggestion } from "../../api/types";
import "./suggestion-queue.css";

export interface SuggestionChipsProps {
  /** Null before a chat session exists; suggestions don't need one. */
  sessionId: string | null;
  /** Sends a prompt to chat. Without it there is nothing for a chip to do, so nothing renders. */
  onAsk?: (prompt: string) => void;
}

/** Chips beside the lead before the row wraps or "+N more" takes over. Wider
    berth than the panel column's 2 (SuggestionQueue.tsx) -- a flex-wrap row
    under a 900px composer has the room a narrow column doesn't. */
const CHIP_ROWS = 3;

export function SuggestionChips({ sessionId, onAsk }: SuggestionChipsProps): ReactElement | null {
  const [suggestions, setSuggestions] = useState<Suggestion[]>([]);

  useEffect(() => {
    let cancelled = false;

    async function load(): Promise<void> {
      try {
        const list = await api.listSuggestions(sessionId);
        if (!cancelled) setSuggestions(list.suggestions ?? []);
      } catch {
        // No banner: Home stays quiet rather than reporting its own plumbing.
        if (!cancelled) setSuggestions([]);
      }
    }

    void load();
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  const take = useCallback(
    async (offer: Suggestion) => {
      onAsk?.(offer.prompt);
      if (!offer.composed) return;
      await api.markSuggestionTaken(offer.id).catch(() => {});
    },
    [onAsk],
  );

  if (!onAsk || suggestions.length === 0) return null;

  const [lead, ...rest] = suggestions;
  const rows = rest.slice(0, CHIP_ROWS);
  const unshown = rest.length - rows.length;

  return (
    <div className="sq__offer-chips" data-hook="suggestion-chips">
      <button type="button" className="sq__chip reach" title={lead.because} onClick={() => void take(lead)}>
        Try: {lead.prompt}
      </button>
      {rows.map((s) => (
        <button
          key={s.id}
          type="button"
          className="sq__chip reach"
          title={s.because}
          onClick={() => void take(s)}
        >
          Try: {s.prompt}
        </button>
      ))}
      {unshown > 0 && <span className="sq__chip sq__chip--more">+{unshown} more</span>}
    </div>
  );
}
