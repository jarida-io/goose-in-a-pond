// Suggestion: the one thing on Home that asks; answerable in place, or every answer is "later".

import { useCallback, useEffect, useState } from "react";
import { api } from "../../api/PondApiClient";
import type { Proposal, ProposalDecision } from "../../api/types";

interface Props {
  /** Who is asking; suggestions are per-person, so without it nothing is safe to show. */
  sessionId: string | null;
  /** Shown when there is nothing to suggest; defaults to a plain reassurance. */
  quiet?: React.ReactNode;
}

export function Suggestion({ sessionId, quiet }: Props) {
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [expanded, setExpanded] = useState(false);
  const [deciding, setDeciding] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!sessionId) {
      setProposals([]);
      return;
    }
    try {
      const list = await api.listProposals(sessionId);
      setProposals(list.proposals);
      setError(null);
    } catch {
      // A failed fetch just means nothing to show; a banner would be the loudest thing on a quiet screen.
      setProposals([]);
    }
  }, [sessionId]);

  useEffect(() => {
    void load();
  }, [load]);

  async function decide(answered: Proposal, decision: ProposalDecision) {
    if (!sessionId || deciding) return;
    setDeciding(true);
    // Optimistic: a spinner on a decision this small reads as doubt that the tap registered.
    const before = proposals;
    setProposals((all) => all.filter((p) => p.id !== answered.id));
    try {
      await api.decideProposal(answered.id, sessionId, decision);
      void load();
    } catch {
      // Restore it: a silently failed send would look answered and never happen.
      setProposals(before);
      setError("That didn't send. Try again.");
    } finally {
      setDeciding(false);
    }
  }

  if (proposals.length === 0) {
    return (
      <section className="sugg sugg--quiet" aria-live="polite">
        {/* Not an invitation to act: nothing needing you is the good outcome
            here, so it reads as reassurance rather than an empty inbox.

            A caller can supply something better. "Nothing needs you right now"
            is true, and true of any house on any day, which makes it wallpaper
            on a screen that is glanced at. Home passes a line about THIS house
            — which lights are on, whether the doors are locked — and the
            default stays for callers with no such context to offer. */}
        {quiet ?? <p className="sugg__quiet-text">Nothing needs you right now.</p>}
      </section>
    );
  }

  const [first, ...rest] = proposals;
  // Only the first is open; the rest are one line each, and answering the top one promotes the next.
  const shown = expanded ? rest : [];

  return (
    <div className="sugg-stack" aria-live="polite">
      <section className="sugg">
        <p className="sugg__summary">{first.summary}</p>
        {first.rationale && <p className="sugg__why">{first.rationale}</p>}
        {error && <p className="sugg__error">{error}</p>}
        <div className="sugg__actions">
          <button
            type="button"
            className="sugg__go"
            disabled={deciding}
            onClick={() => void decide(first, "approve")}
          >
            Approve
          </button>
          <button
            type="button"
            className="sugg__no"
            disabled={deciding}
            onClick={() => void decide(first, "reject")}
          >
            Not now
          </button>
        </div>
      </section>

      {rest.length > 0 && (
        <button
          type="button"
          className="sugg__more"
          aria-expanded={expanded}
          onClick={() => setExpanded((v) => !v)}
        >
          {expanded
            ? "Hide the rest"
            : `${rest.length} more suggestion${rest.length === 1 ? "" : "s"}`}
        </button>
      )}

      {shown.map((p) => (
        <section key={p.id} className="sugg sugg--stacked">
          <p className="sugg__summary sugg__summary--small">{p.summary}</p>
          <div className="sugg__actions">
            <button
              type="button"
              className="sugg__go"
              disabled={deciding}
              onClick={() => void decide(p, "approve")}
            >
              Approve
            </button>
            <button
              type="button"
              className="sugg__no"
              disabled={deciding}
              onClick={() => void decide(p, "reject")}
            >
              Not now
            </button>
          </div>
        </section>
      ))}
    </div>
  );
}
