// Home's left column: waiting proposals, or else suggestions to ask in chat.
// Only one item carries the ink edge and offset (DESIGN.md §3 allows about two per screen).
// Buttons stay generic (Approve / Not now): a proposal carries no verb, and approving executes
// nothing (`decide` returns "executed": false), so the toast claims only that it was recorded.

import React, { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../api/PondApiClient";
import type { Proposal, ProposalDecision, Suggestion } from "../../api/types";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import "./suggestion-queue.css";

export interface SuggestionQueueProps {
  /** Null before a chat session exists; proposals need one, suggestions don't. */
  sessionId: string | null;
  /** Sends a prompt to chat. Without it the suggestions half stays folded. */
  onAsk?: (prompt: string) => void;
}

/** One notch per wheel gesture: below this a trackpad flings through the queue. */
const WHEEL_THROTTLE_MS = 260;
/** Below this a resting trackpad drifts the cursor on its own. */
const WHEEL_DEADZONE_PX = 4;
const TOAST_MS = 2600;

/** Null for a malformed timestamp: falling back to now would date it to this render. */
function timeOf(iso: string): string | null {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Offer rows under the lead. Measured: 1024x600 fits two above the dock; at 800x480 two overrun it. */
const OFFER_ROWS_TALL = 2;
const OFFER_ROWS_SHORT = 0;

/** Between the 600px and 480px panel heights; the two Home surfaces differ only in width. */
const SHORT_PANEL = "(max-height: 560px)";

/**
 * True on a short panel. Subscribed because the desktop window can be resized; without
 * `matchMedia` (jsdom) it reports tall, which the component's tests rely on.
 */
function useShortPanel(): boolean {
  const [short, setShort] = useState(false);

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mq = window.matchMedia(SHORT_PANEL);
    setShort(mq.matches);
    const onChange = (e: MediaQueryListEvent): void => setShort(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  return short;
}

export function SuggestionQueue({ sessionId, onAsk }: SuggestionQueueProps): React.ReactElement {
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [suggestions, setSuggestions] = useState<Suggestion[]>([]);
  // By id, not index, so answering a row above can't swap the open card. Null means the first.
  const [activeId, setActiveId] = useState<string | null>(null);
  const [listOpen, setListOpen] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const shortPanel = useShortPanel();

  /**
   * Sends an offer, then marks it taken. `onAsk` goes first, unawaited: settling is bookkeeping.
   * Only composed suggestions are settled; a template one's `id` is a suggestor name, not a row.
   */
  const take = useCallback(
    async (offer: Suggestion) => {
      onAsk?.(offer.prompt);
      if (!offer.composed) return;
      // Swallowed: the household is in chat by now, where a toast would land on the conversation.
      await api.markSuggestionTaken(offer.id).catch(() => {});
    },
    [onAsk],
  );

  const lastWheel = useRef(0);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let cancelled = false;
    setListOpen(false);
    setError(null);

    async function load(): Promise<void> {
      if (!sessionId) {
        // Without a session the server can't tell whom a proposal is addressed to.
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
        // No banner: an error would be the loudest thing on a screen meant to be quiet.
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

  // Needs no session (null on a cold launch), but refetches when one narrows the audience to personal.
  useEffect(() => {
    let cancelled = false;

    async function load(): Promise<void> {
      try {
        const list = await api.listSuggestions(sessionId);
        if (cancelled) return;
        setSuggestions(list.suggestions ?? []);
      } catch {
        // No banner, as for proposals.
        if (cancelled) return;
        setSuggestions([]);
      }
    }

    void load();
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  // Cleared on unmount so a pending toast can't setState on a gone component.
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

  const foundIndex = activeId === null ? -1 : proposals.findIndex((p) => p.id === activeId);
  const activeIndex = foundIndex < 0 ? 0 : foundIndex;
  const activeProposal = proposals[activeIndex] ?? null;

  const cycle = useCallback(
    (delta: number) => {
      // Clamped, not wrapped: jumping from last to first reads as the list changing.
      const next = Math.max(0, Math.min(proposals.length - 1, activeIndex + delta));
      setActiveId(proposals[next]?.id ?? null);
    },
    [proposals, activeIndex],
  );

  const onWheel = useCallback(
    (e: React.WheelEvent<HTMLDivElement>) => {
      // No preventDefault: React registers wheel listeners as passive.
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

      // Optimistic: a spinner on a decision this small reads as doubt that the tap registered.
      const remaining = proposals.filter((x) => x.id !== p.id);
      setProposals(remaining);
      // Only answering the open card moves the cursor, onto the one that slid into its place.
      if (wasActive) setActiveId(remaining[Math.min(at, remaining.length - 1)]?.id ?? null);
      if (remaining.length === 0) setListOpen(false);
      setError(null);
      showToast(decision === "approve" ? "Approved — recorded" : "Dismissed");

      try {
        await api.decideProposal(p.id, sessionId, decision);
      } catch {
        // Restore it: a failed decision must not look answered.
        setProposals((all) => {
          const back = all.slice();
          back.splice(Math.min(at, back.length), 0, p);
          return back;
        });
        // The error draws on the open card, so the cursor returns to this one.
        if (wasActive) setActiveId(p.id);
        setError("That didn't send. Try again.");
      }
    },
    [proposals, activeIndex, sessionId, showToast],
  );

  const askable = onAsk ? suggestions : [];
  const rest = proposals.filter((_, i) => i !== activeIndex);
  const more = rest.length;
  const peek = rest.slice(0, 2);

  const lead = askable[0];
  const rows = askable.slice(1, 1 + (shortPanel ? OFFER_ROWS_SHORT : OFFER_ROWS_TALL));
  const unshown = Math.max(0, askable.length - 1 - rows.length);

  return (
    <div className="sq" data-hook="suggestion-queue" onWheel={onWheel}>
      {/* The head. Above everything the column can be carrying, and drawn in
          every state except an open proposal -- a proposal is addressed to
          somebody and is allowed to take the column over. */}
      {proposals.length > 0 && <p className="sq__eyebrow">Goose asks. Scroll for the next.</p>}

      {activeProposal === null ? (
        askable.length > 0 ? (
          // Hidden at >=1101px (dashboard-grid.css): desktop's offers live in
          // SuggestionChips, right under the composer, not down in this aside
          // column. Same underlying data, a different component (SuggestionChips.tsx)
          // -- this stays the panel's whole asking side.
          <div className="sq__offers">
            <p className="sq__eyebrow">You could ask</p>

            <button
              type="button"
              className="sq__offer sq__offer--lead"
              onClick={() => void take(lead)}
            >
              <span className="sq__offer-prompt">{lead.prompt}</span>
              <span className="sq__offer-why">{lead.because}</span>
            </button>

            {rows.map((s) => (
              <button
                key={s.id}
                type="button"
                className="sq__offer"
                onClick={() => void take(s)}
              >
                <span className="sq__offer-prompt">{s.prompt}</span>
                <span className="sq__offer-why">{s.because}</span>
              </button>
            ))}

            {/* Said rather than dropped. A column that quietly showed three of
                four would look like the engine found three.

                It used to read "N more waiting in chat", and that was a promise
                the interface does not keep. Measured: tapping an offer sends the
                turn and lands you in chat with a message already in it, and the
                classic composer only draws its chips while the transcript is
                empty (`sections/Chat.tsx`, `messages.length === 0`) -- so the
                count said four were there and chat showed zero. The hub composer
                draws them unconditionally, which made it true on one surface and
                false on the other, which is worse than either.

                So it names no destination. What it claims is only what `offered`
                already means: the engine believes the pond can answer these. */}
            {unshown > 0 && (
              <p className="sq__unshown">
                {unshown === 1
                  ? "1 more the pond can answer"
                  : `${unshown} more the pond can answer`}
              </p>
            )}
          </div>
        ) : (
          // Also what a failed fetch looks like, so it must not claim the pond looked and found nothing.
          <p className="sq__quiet" aria-live="polite">
            Ask about the house, or just talk.
          </p>
        )
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
