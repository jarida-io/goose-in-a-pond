// ────────────────────────────────────────────────────────────
// SuggestionQueue — the left column of Home, and the only thing on the screen
// that asks rather than reports.
//
// THE HEAD. One sentence about this house, above everything, in the display
// italic DESIGN.md section 2 calls "the single most recognisable thing about
// our typography". It was previously drawn only when the column had nothing
// else to say, so the screen lost its own voice the moment the pond had
// something to offer -- and Home, the front door, was the one surface in the
// product carrying none of the product's typography.
//
// ONE THING IS LOUD. Whatever the column is carrying, exactly one item wears
// the 2px ink edge and the hard offset: the open proposal, or the lead offer.
// Everything under it is a hairline row. DESIGN.md section 3 budgets the offset
// at about twice per screen and Home spends it here and on the mic; four
// equally-weighted cards would have spent it four times and meant it none.
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
//   * A per-suggestion verb ("Turn them off", "Close it"). There is no verb on
//     the wire at all: `go`/`doing`/`done` exist only in the design's mock
//     array, and `ProposalPayload` carries `{trigger, proposed_action,
//     confidence}` — adding one is a schema change, not a prompt change. The
//     nearest field, `proposed_action`, is a TaskKind tagged union; it is now
//     TYPED as one (it used to be declared `string`, so this comment was the
//     only thing stopping anyone binding it and shipping [object Object]).
//     Even typed, a button labelled with a verb the pond cannot perform is what
//     DESIGN.md section 3 forbids, so both buttons stay generic: Approve, and
//     Not now.
//
//   * The doing/done toast pair ("Turning off the patio lights" then "Patio
//     lights off"). Approving moves the row to `approved` and executes
//     nothing — the handler in routes.rs returns "executed": false and says so
//     in its own docstring. So there is one toast, and it claims only what
//     happened: the answer was recorded. The card leaves the column because it
//     was answered, not because anything changed in the house.
//
// Backed by GET /api/v1/proposals and POST /api/v1/proposals/:id/decide.
//
// ── The other half: suggestions ─────────────────────────────────────────────
//
// When nothing is waiting on you, the column offers things you might want to
// ASK instead, from GET /api/v1/suggestions. The two are different in kind and
// the screen says so:
//
//   a proposal   is addressed to you, expires, and wants an answer  -> Approve
//   a suggestion is addressed to nobody, keeps, and is an offer     -> Ask
//
// Proposals win the column whenever there are any, because only they are
// waiting on somebody. Suggestions fill the quiet, which until now was a single
// sentence about the weather.
//
// "Ask" rather than "Approve" is the point. `decide_proposal` returns
// `"executed": false` and there is no consumer of an approved proposal anywhere
// in the tree, so Approve records an answer and changes nothing. Asking is a
// verb the pond demonstrably performs: the prompt goes to chat through the same
// path the composer uses, and the household watches it answer. That is the
// difference between a button that does what it says and the one DESIGN.md
// section 3 forbids.
//
// The suggestion fetch deliberately does NOT wait for a session. `sessionId` is
// null on a cold launch and never persisted, so requiring one would blank the
// column on exactly the launch this fills. Tapping a suggestion mints the
// session that the proposals half has never had -- so the offer lane bootstraps
// the asking lane.
// ────────────────────────────────────────────────────────────

import React, { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../api/PondApiClient";
import type { Proposal, ProposalDecision, Suggestion } from "../../api/types";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import "./suggestion-queue.css";

export interface SuggestionQueueProps {
  /** Who is asking. Null before a chat session exists, and the column shows its quiet state. */
  sessionId: string | null;
  /**
   * One short sentence about this house, and the column's head.
   *
   * It used to be `quietLine` and appeared only when there was nothing waiting
   * AND nothing on offer -- so the moment the suggestion engine started
   * answering, the screen lost the only sentence on it that was about this
   * household, and Home had no voice at all. It is now the head, drawn above
   * whatever the column is carrying, because it is true in every state.
   *
   * Already one sentence, <= 72 chars.
   */
  houseLine: string;
  /**
   * Put a question to the pond.
   *
   * The caller sends the prompt and goes to chat. Optional: a surface that has
   * nowhere to send it passes nothing and the suggestions half stays folded,
   * rather than drawing a button that would do nothing when tapped.
   */
  onAsk?: (prompt: string) => void;
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

/**
 * How many offers stand as quiet rows under the one that carries the edge.
 *
 * Measured, not chosen. The dock sits under this column in flow, so every row
 * pushes it down, and the screen's own bottom edge is the budget:
 *
 *   1024x600   head + lead + 2 rows + the count   dock ends at 536 of 600
 *   800x480    head + lead + 0 rows + the count   dock ends at 423 of 480
 *   800x480    head + lead + 2 rows + the count   dock ends at 480 of 480, and
 *                                                 the offers run to 467 --
 *                                                 through the dock they sit on
 *
 * So the small panel takes none: one question, which is what DESIGN.md section
 * 4 asks of the hub anyway -- "one decision per screen". The server caps a set
 * at four, so either way an offer can go unshown, and that is why the remainder
 * is drawn as a count rather than dropped in silence.
 */
const OFFER_ROWS_TALL = 2;
const OFFER_ROWS_SHORT = 0;

/**
 * The height below which the column shows one row instead of two.
 *
 * HEIGHT, deliberately, where the widget track beside it keys off its own width
 * with a container query. That rule exists because the two Home surfaces give
 * the track containers 64px apart at the same viewport width, so a viewport
 * query sizes one of them wrong. Vertically they differ by nothing worth a
 * step, and the cost of being one step out is one row shown or not shown rather
 * than a clipped card. 560 sits between the two panels: 600 keeps both rows,
 * 480 takes one.
 */
const SHORT_PANEL = "(max-height: 560px)";

/**
 * True on a panel too short for the second row.
 *
 * Subscribed rather than read once: the hub panel never resizes, but the same
 * column renders on the desktop surface inside a window somebody can drag. A
 * value read at mount would leave a resized window one row wrong until the next
 * navigation. Guarded for the environments -- jsdom among them -- that do not
 * carry a real `matchMedia`, the same guard WidgetTrack uses for its
 * reduced-motion query. jsdom without it reports a tall panel, which is the
 * safe default: the component's own tests assert what the column offers, and a
 * short-panel default would have them asserting the truncated set.
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

export function SuggestionQueue({ sessionId, houseLine, onAsk }: SuggestionQueueProps): React.ReactElement {
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [suggestions, setSuggestions] = useState<Suggestion[]>([]);
  // The cursor names a suggestion, not a slot. A stored index silently changes
  // meaning the moment anything above it leaves the queue -- answering a row
  // from the panel would slide a different suggestion into the open card under
  // a household that is mid-read. Null means "the first one", which is what a
  // freshly loaded queue and an emptied one both want.
  const [activeId, setActiveId] = useState<string | null>(null);
  const [listOpen, setListOpen] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const shortPanel = useShortPanel();

  /**
   * Tap an offer: send it, and tell the pond it was taken.
   *
   * The ORDER is deliberate. `onAsk` goes first and is never awaited, because
   * it is what the household is waiting for -- the turn is already in flight by
   * the time the transcript mounts. Settling is bookkeeping and rides behind
   * it; if it fails, the household still got their answer and the worst case is
   * the same question being offered again tomorrow.
   *
   * Only a composed suggestion is settled. A template one is recomputed on
   * every read and its `id` is a suggestor name, not a row -- posting it would
   * be asking the pond to settle something that does not exist.
   */
  const take = useCallback(
    async (offer: Suggestion) => {
      onAsk?.(offer.prompt);
      if (!offer.composed) return;
      // Dropped rather than surfaced. The household has left this screen by
      // now, and a toast about bookkeeping would land on a conversation.
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

  // Suggestions, on their own effect and their own terms. Note the dependency
  // is still `sessionId` -- not because the fetch needs one, but because
  // acquiring one sharpens the audience from shared to personal and the column
  // should pick that up. The request itself is made with or without it.
  useEffect(() => {
    let cancelled = false;

    async function load(): Promise<void> {
      try {
        const list = await api.listSuggestions(sessionId);
        if (cancelled) return;
        setSuggestions(list.suggestions ?? []);
      } catch {
        // Same reasoning as the proposals load: a column whose whole job is
        // quiet does not get a banner. The server-side `considered` list is
        // where a silent engine explains itself; this is the client, and it has
        // nothing useful to say that the quiet line does not already say.
        if (cancelled) return;
        setSuggestions([]);
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

  // Offered only when the caller can actually send one. A surface with no
  // `onAsk` would otherwise draw buttons that do nothing -- the exact thing the
  // verb refusal at the top of this file is about.
  const askable = onAsk ? suggestions : [];
  const rest = proposals.filter((_, i) => i !== activeIndex);
  const more = rest.length;
  const peek = rest.slice(0, 2);

  // One offer carries the ink edge and the rest are hairline rows, which is the
  // shape the proposals half of this file already has -- one open, the others a
  // peek. Four equally-weighted cards was the other option and it is the one
  // DESIGN.md section 3 rules out: spend the offset about twice per screen and
  // mean it both times. Home spends it here and on the mic.
  const lead = askable[0];
  const rows = askable.slice(1, 1 + (shortPanel ? OFFER_ROWS_SHORT : OFFER_ROWS_TALL));
  const unshown = Math.max(0, askable.length - 1 - rows.length);

  return (
    <div className="sq" data-hook="suggestion-queue" onWheel={onWheel}>
      {/* The head. Above everything the column can be carrying, and drawn in
          every state except an open proposal -- a proposal is addressed to
          somebody and is allowed to take the column over. */}
      {activeProposal === null && <p className="sq__head">{houseLine}</p>}

      {proposals.length > 0 && <p className="sq__eyebrow">Goose asks. Scroll for the next.</p>}

      {activeProposal === null ? (
        askable.length > 0 ? (
          // Nothing is waiting on anybody, so the column offers instead of
          // reporting. Each row is a question the pond can currently answer,
          // with the fact that produced it underneath.
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
          // Nothing to offer. Note this is ALSO what a failed fetch looks like
          // -- both loads swallow their error into an empty list on purpose --
          // so this line must not claim the pond looked and found nothing. It
          // claims nothing at all: it is an invitation, which is what DESIGN.md
          // section 7 asks an empty screen to be. The sentence about this house
          // is already above it, and is true either way.
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
