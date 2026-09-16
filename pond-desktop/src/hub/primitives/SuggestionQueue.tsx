// ────────────────────────────────────────────────────────────
// SuggestionQueue — the left column of Home, and the only thing on the screen
// that asks rather than reports.
//
// One suggestion is open at a time. The rest are a two-line peek and a count,
// because a household glancing at a panel needs to know something is waiting,
// not be handed a queue to work through. The wheel moves the cursor through
// the queue; on the touch panel the peek rows and the "Waiting on you" overlay
// do the same job with a finger.
//
// Two things the design asked for are deliberately not built, because the pond
// cannot back them:
//
//   * A per-suggestion verb ("Turn them off", "Close it"). The only action
//     field on the wire is `proposed_action`, a TaskKind tagged union typed as
//     `string` — never bind it, it renders [object Object]. A button labelled
//     with a verb it cannot perform is exactly what DESIGN.md section 3
//     forbids, so both buttons are generic: Approve, and Not now.
//
//   * The doing/done toast pair ("Turning off the patio lights" then "Patio
//     lights off"). Approving moves the row to `approved` and executes
//     nothing — the handler in routes.rs returns "executed": false and says so
//     in its own docstring. So there is one toast, and it claims only what
//     happened: the answer was recorded. The card leaves the column because it
//     was answered, not because anything changed in the house.
//
// Backed by GET /api/v1/proposals and POST /api/v1/proposals/:id/decide.
// ────────────────────────────────────────────────────────────

import React, { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../api/PondApiClient";
import type { Proposal, ProposalDecision } from "../../api/types";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import "./suggestion-queue.css";

export interface SuggestionQueueProps {
  /** Who is asking. Null before a chat session exists, and the column shows its quiet state. */
  sessionId: string | null;
  /** One short sentence about this house, shown when nothing is waiting. Already one sentence, <= 72 chars. */
  quietLine: string;
}

/** One notch per wheel gesture: below this a trackpad flings through the queue. */
const WHEEL_THROTTLE_MS = 260;
/** Below this a resting trackpad drifts the cursor on its own. */
const WHEEL_DEADZONE_PX = 4;
const TOAST_MS = 2600;

/** The clock on the wire is a string; a malformed one omits the time rather
 *  than substituting now, which would date the suggestion to this render. */
function timeOf(iso: string): string | null {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function SuggestionQueue({ sessionId, quietLine }: SuggestionQueueProps): React.ReactElement {
  const [proposals, setProposals] = useState<Proposal[]>([]);
  // The cursor names a suggestion, not a slot. A stored index silently changes
  // meaning the moment anything above it leaves the queue -- answering a row
  // from the panel would slide a different suggestion into the open card under
  // a household that is mid-read. Null means "the first one", which is what a
  // freshly loaded queue and an emptied one both want.
  const [activeId, setActiveId] = useState<string | null>(null);
  const [listOpen, setListOpen] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const lastWheel = useRef(0);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let cancelled = false;
    setListOpen(false);
    setError(null);

    async function load(): Promise<void> {
      if (!sessionId) {
        // Without a session the server cannot tell who a suggestion is
        // addressed to, so there is nothing safe to ask for. Quiet state.
        setProposals([]);
        setActiveId(null);
        return;
      }
      try {
        const list = await api.listProposals(sessionId);
        if (cancelled) return;
        setProposals(list.proposals);
        setActiveId(null);
      } catch {
        // A queue that cannot be fetched is not worth a banner. Errors here
        // would be the loudest thing on a screen whose whole job is quiet.
        if (cancelled) return;
        setProposals([]);
        setActiveId(null);
      }
    }

    void load();
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  // One timer, cleared on unmount: a toast that outlives the column would
  // setState on a gone component, and two toasts would stack over each other.
  useEffect(() => {
    return () => {
      if (toastTimer.current !== null) clearTimeout(toastTimer.current);
    };
  }, []);

  useEffect(() => {
    if (!listOpen) return;
    function onKey(e: KeyboardEvent): void {
      if (e.key === "Escape") setListOpen(false);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [listOpen]);

  const showToast = useCallback((message: string) => {
    if (toastTimer.current !== null) clearTimeout(toastTimer.current);
    setToast(message);
    toastTimer.current = setTimeout(() => {
      setToast(null);
      toastTimer.current = null;
    }, TOAST_MS);
  }, []);

  // Derived every render, never stored. The one moment a position is
  // authoritative is picking the successor when the cursor's own card is the
  // one answered; everywhere else the id is the truth.
  const foundIndex = activeId === null ? -1 : proposals.findIndex((p) => p.id === activeId);
  const activeIndex = foundIndex < 0 ? 0 : foundIndex;
  const activeProposal = proposals[activeIndex] ?? null;

  const cycle = useCallback(
    (delta: number) => {
      // Clamped, never wrapped: a cursor that jumps from the last back to the
      // first reads as the list having changed under you.
      const next = Math.max(0, Math.min(proposals.length - 1, activeIndex + delta));
      setActiveId(proposals[next]?.id ?? null);
    },
    [proposals, activeIndex],
  );

  const onWheel = useCallback(
    (e: React.WheelEvent<HTMLDivElement>) => {
      // No preventDefault: the column does not scroll, and the handler is
      // passive under React's listener anyway.
      const now = Date.now();
      if (now - lastWheel.current < WHEEL_THROTTLE_MS) return;
      if (Math.abs(e.deltaY) < WHEEL_DEADZONE_PX) return;
      lastWheel.current = now;
      cycle(e.deltaY > 0 ? 1 : -1);
    },
    [cycle],
  );

  const answer = useCallback(
    async (p: Proposal, decision: ProposalDecision) => {
      if (!sessionId) return;
      const at = proposals.findIndex((x) => x.id === p.id);
      if (at < 0) return;
      const wasActive = at === activeIndex;

      // Optimistic: the card goes the moment it is answered and the next takes
      // its place. A spinner on a decision this small reads as doubt about
      // whether the tap registered.
      const remaining = proposals.filter((x) => x.id !== p.id);
      setProposals(remaining);
      // Only answering the open card moves the cursor, and then only onto the
      // one that slid up into its place. Answering any other row -- which the
      // panel offers on every suggestion, above the cursor included -- leaves
      // the household reading what they were reading.
      if (wasActive) setActiveId(remaining[Math.min(at, remaining.length - 1)]?.id ?? null);
      if (remaining.length === 0) setListOpen(false);
      setError(null);
      showToast(decision === "approve" ? "Approved — recorded" : "Dismissed");

      try {
        await api.decideProposal(p.id, sessionId, decision);
      } catch {
        // Put it back where it was rather than swallow it — a suggestion that
        // silently failed to send would look answered and never happen.
        setProposals((all) => {
          const back = all.slice();
          back.splice(Math.min(at, back.length), 0, p);
          return back;
        });
        // The error is drawn on the open card, so a cursor that moved off it
        // has to come back or the message lands on somebody else's suggestion.
        if (wasActive) setActiveId(p.id);
        setError("That didn't send. Try again.");
      }
    },
    [proposals, activeIndex, sessionId, showToast],
  );

  const rest = proposals.filter((_, i) => i !== activeIndex);
  const more = rest.length;
  const peek = rest.slice(0, 2);

  return (
    <div className="sq" data-hook="suggestion-queue" onWheel={onWheel}>
      {proposals.length > 0 && <p className="sq__eyebrow">Goose asks · scroll for the next</p>}

      {activeProposal === null ? (
        // Nothing waiting is the good outcome, so it reads as reassurance
        // rather than an empty inbox. The caller supplies a line about THIS
        // house; a generic one would be wallpaper on a glanced-at screen.
        <p className="sq__quiet" aria-live="polite">
          {quietLine}
        </p>
      ) : (
        <>
          <h2 className="sq__summary">{activeProposal.summary}</h2>
          {activeProposal.rationale && <p className="sq__why">{activeProposal.rationale}</p>}
          {error && (
            <p className="sq__error" role="alert">
              {error}
            </p>
          )}
          <div className="sq__actions">
            <button type="button" className="sq__go" onClick={() => void answer(activeProposal, "approve")}>
              Approve
            </button>
            <button type="button" className="sq__no" onClick={() => void answer(activeProposal, "reject")}>
              Not now
            </button>
          </div>

          {peek.length > 0 && (
            <div className="sq__peek">
              {peek.map((p) => {
                const t = timeOf(p.created_at);
                return (
                  <button type="button" key={p.id} className="sq__peek-row" onClick={() => setActiveId(p.id)}>
                    <span className="sq__peek-summary">{p.summary}</span>
                    {t && <span className="sq__peek-time">{t}</span>}
                  </button>
                );
              })}
            </div>
          )}

          {more > 0 && (
            <button
              type="button"
              className="sq__more"
              aria-expanded={listOpen}
              onClick={() => setListOpen(true)}
            >
              <span className="sq__more-count">{more}</span>
              <span className="sq__more-label">
                {more === 1 ? "more suggestion — see it" : "more suggestions — see them"}
              </span>
            </button>
          )}
        </>
      )}

      {listOpen && proposals.length > 0 && (
        <>
          <button
            type="button"
            className="sq__scrim"
            aria-label="Close the list"
            onClick={() => setListOpen(false)}
          />
          <div className="sq__panel" role="dialog" aria-label="Waiting on you">
            <div className="sq__panel-head">
              <h2 className="sq__panel-title">Waiting on you</h2>
              <button
                type="button"
                className="sq__panel-close"
                aria-label="Close the list"
                onClick={() => setListOpen(false)}
              >
                <HubIco d={HP_PATHS.x} size={16} color="var(--color-text)" sw={2.4} />
              </button>
            </div>
            <div className="sq__panel-body">
              {proposals.map((p, i) => {
                const t = timeOf(p.created_at);
                return (
                  <div key={p.id} className="sq__row" data-active={i === activeIndex ? "" : undefined}>
                    <div className="sq__row-head">
                      <button
                        type="button"
                        className="sq__row-title"
                        onClick={() => {
                          setActiveId(p.id);
                          setListOpen(false);
                        }}
                      >
                        {p.summary}
                      </button>
                      {t && <span className="sq__row-time">{t}</span>}
                    </div>
                    {p.rationale && <p className="sq__row-why">{p.rationale}</p>}
                    <div className="sq__row-actions">
                      {/* Answering from here leaves the list open: a household
                          working through four of these should not have to
                          reopen the panel between each one. */}
                      <button type="button" className="sq__go" onClick={() => void answer(p, "approve")}>
                        Approve
                      </button>
                      <button type="button" className="sq__no" onClick={() => void answer(p, "reject")}>
                        Not now
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        </>
      )}

      {toast !== null && (
        <div className="sq__toast" role="status">
          <HubIco d={HP_PATHS.check} size={16} color="var(--pp)" sw={2.4} />
          <span>{toast}</span>
        </div>
      )}
    </div>
  );
}
