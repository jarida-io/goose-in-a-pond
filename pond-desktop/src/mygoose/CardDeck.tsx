// The member's idea cards, the same deck on My Goose and Home. The hub and Home show one card at a
// time, so a panel asks one decision per screen; the classic My Goose lists them all.

import { useId, useState } from "react";
import { ChevronLeft, ChevronRight, RefreshCw } from "lucide-react";
import { InkButton } from "@jarida/ink/react";
import "./mygoose.css";
import "./card-deck.css";
import { api } from "../api/PondApiClient";
import type { IdeaCard, IdeaCardDismissal, IdeaCardSet } from "../api/types";
import {
  CARDS_LABEL,
  DECK_TITLE,
  LOADING_CARDS,
  MADE_FRESH,
  MAKING_CARDS,
  NEXT,
  NO_CARDS,
  PREVIOUS,
  UNSAVED,
  countWords,
  dismissedWords,
  doneWords,
  isMaking,
  positionWords,
  savedWords,
  setStateWords,
} from "./cardWords";
import { REFRESH, buttonSize, type Variant } from "./myGooseWords";
import { POLL_QUEUED_MS, POLL_RUNNING_MS, RefusalContext, useLoaded, useSaid } from "./useLoaded";
import { isNotServed, isUnknownCaller, problemWords } from "./problems";
import { ActionProblem, LoadProblem, Loading, Said } from "./MyGooseParts";
import { IdeaCardView } from "./IdeaCardView";
import { useShortScreen } from "./useShortScreen";

export interface CardDeckProps {
  variant: Variant;
  /** Home keeps quiet when the cards cannot load; My Goose says what went wrong. */
  place: "home" | "mygoose";
  /** Sends a prompt as the member's message; a chat card needs it. */
  onAsk?: (prompt: string) => void;
  /** Where Home's one-line glance leads on a short screen: My Goose, which has room for the card. */
  onOpenAll?: () => void;
}

function pollFor(deck: IdeaCardSet): number | null {
  if (deck.set.state === "making") return POLL_RUNNING_MS;
  if (deck.set.state === "queued") return POLL_QUEUED_MS;
  return null;
}

// A pond without card routes must not close the rest of My Goose, so the deck refuses nothing.
const REFUSE_NOTHING = () => {};

export function CardDeck(props: CardDeckProps) {
  return (
    <RefusalContext.Provider value={REFUSE_NOTHING}>
      <Deck {...props} />
    </RefusalContext.Provider>
  );
}

function Deck({ variant, place, onAsk, onOpenAll }: CardDeckProps) {
  const ids = useId();
  const { state, reload, retry, replace } = useLoaded(() => api.listIdeaCards(), pollFor);
  const { said, say } = useSaid();
  const [at, setAt] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const short = useShortScreen();
  const paged = variant === "hub" || place === "home";

  if (place === "home" && state.status !== "ready") return null;
  if (state.status === "failed" && (isNotServed(state.problem) || isUnknownCaller(state.problem))) return null;

  const deck = state.status === "ready" ? state.value : null;

  function without(card: IdeaCard): void {
    if (deck) replace({ ...deck, cards: deck.cards.filter((c) => c.id !== card.id) });
  }

  /** A card the pond still lists stays, as it now is; one it has done or turned down leaves. */
  function keepOrDrop(next: IdeaCard): void {
    if (!deck) return;
    if (next.state === "new" || next.state === "saved") {
      replace({ ...deck, cards: deck.cards.map((c) => (c.id === next.id ? next : c)) });
    } else {
      without(next);
    }
  }

  async function refresh() {
    setRefreshing(true);
    setProblem(null);
    try {
      const answer = await api.refreshIdeaCards();
      if (deck) replace({ ...deck, set: { ...deck.set, state: answer.state } });
      say(MADE_FRESH);
      reload();
    } catch (e) {
      setProblem(problemWords(e));
    }
    setRefreshing(false);
  }

  async function doIt(card: IdeaCard) {
    const done = await api.doIdeaCard(card.id);
    without(card);
    if (done.result.kind === "chat") {
      onAsk?.(done.result.prompt ?? card.action?.prompt ?? card.title);
      return;
    }
    say(doneWords(done.result.kind, card.title));
  }

  async function answer(card: IdeaCard, words: string) {
    await api.doIdeaCard(card.id);
    without(card);
    onAsk?.(words);
  }

  async function goDeeper(card: IdeaCard) {
    const answer = await api.goDeeperOnIdeaCard(card.id);
    keepOrDrop(answer.card);
    say(doneWords("go_deeper", card.title));
  }

  async function save(card: IdeaCard, keep: boolean) {
    const next = keep ? await api.saveIdeaCard(card.id) : await api.unsaveIdeaCard(card.id);
    if (deck) replace({ ...deck, cards: deck.cards.map((c) => (c.id === next.id ? next : c)) });
    say(keep ? savedWords(card.title) : UNSAVED);
  }

  async function dismiss(card: IdeaCard, dismissal: IdeaCardDismissal) {
    await api.dismissIdeaCard(card.id, dismissal);
    without(card);
    say(dismissedWords(card.title));
  }

  const cards = deck?.cards ?? [];
  const shown = Math.min(at, Math.max(0, cards.length - 1));
  const making = deck !== null && isMaking(deck.set.state);

  if (place === "home" && short && onOpenAll) {
    if (cards.length === 0) return null;
    return (
      <button type="button" className="cd-glance" onClick={onOpenAll}>
        <span className="cd-glance__label">{DECK_TITLE}</span>
        <span className="cd-glance__title">{cards[0].title}</span>
        <span className="cd-glance__count">{countWords(cards.length)}</span>
        <ChevronRight size={18} aria-hidden="true" />
      </button>
    );
  }

  function cardView(card: IdeaCard) {
    return (
      <IdeaCardView
        key={card.id}
        card={card}
        variant={variant}
        onDo={doIt}
        onGoDeeper={goDeeper}
        onSave={save}
        onDismiss={dismiss}
        onAnswer={onAsk ? answer : undefined}
      />
    );
  }

  return (
    <section className={`cd cd--${variant} cd--${place}`} aria-labelledby={`${ids}-title`}>
      <header className="cd__head">
        <h2 id={`${ids}-title`} className="cd__title">
          {DECK_TITLE}
        </h2>
        {deck ? (
          <span className="cd__state" role="status">
            {setStateWords(deck.set.state, deck.set.ahead)}
          </span>
        ) : null}
        <InkButton
          size={buttonSize(variant)}
          icon={<RefreshCw size={16} aria-hidden="true" />}
          loading={refreshing || making}
          disabled={state.status !== "ready"}
          onPress={() => void refresh()}
          className="cd__refresh"
        >
          {REFRESH}
        </InkButton>
      </header>
      <ActionProblem text={problem} />
      {state.status === "loading" ? <Loading>{LOADING_CARDS}</Loading> : null}
      {state.status === "failed" ? <LoadProblem problem={state.problem} onRetry={retry} variant={variant} /> : null}
      {deck && cards.length === 0 ? <p className="cd__empty">{making ? MAKING_CARDS : NO_CARDS}</p> : null}
      {cards.length > 0 && paged ? (
        <div className="cd__paged">
          {cardView(cards[shown])}
          {cards.length > 1 ? (
            <nav className="cd__pager" aria-label={CARDS_LABEL}>
              <InkButton
                size={buttonSize(variant)}
                icon={<ChevronLeft size={16} aria-hidden="true" />}
                disabled={shown === 0}
                onPress={() => setAt(shown - 1)}
              >
                {PREVIOUS}
              </InkButton>
              <span className="cd__position" aria-live="polite">
                {positionWords(shown + 1, cards.length)}
              </span>
              <InkButton
                size={buttonSize(variant)}
                icon={<ChevronRight size={16} aria-hidden="true" />}
                disabled={shown === cards.length - 1}
                onPress={() => setAt(shown + 1)}
              >
                {NEXT}
              </InkButton>
            </nav>
          ) : null}
        </div>
      ) : null}
      {cards.length > 0 && !paged ? (
        <ul className="cd__list" aria-label={CARDS_LABEL}>
          {cards.map((card) => (
            <li key={card.id}>{cardView(card)}</li>
          ))}
        </ul>
      ) : null}
      <Said text={said} />
    </section>
  );
}
