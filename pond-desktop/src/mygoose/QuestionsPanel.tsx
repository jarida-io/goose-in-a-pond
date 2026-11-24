// Every open and waiting question batch as a card, newest first. On the kiosk, and on Home, one
// batch at a time: one decision per screen. On a short screen Home gives it one line that leads to
// My Goose, as it does the idea cards, so the mic stays on the panel.

import { useState } from "react";
import { ChevronRight, MessageCircleQuestion } from "lucide-react";
import { InkButton, InkEmptyState } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import {
  LOADING_QUESTIONS,
  NO_QUESTIONS_BODY,
  NO_QUESTIONS_TITLE,
  QUESTIONS_HEADING,
  QUESTIONS_INTRO,
  SEE_PLANS,
  buttonSize,
  glanceQuestionCount,
  moreWords,
  type Variant,
} from "./myGooseWords";
import { useLoaded } from "./useLoaded";
import { LoadProblem, Loading } from "./MyGooseParts";
import { QuestionCard } from "./QuestionCard";
import { useShortScreen } from "./useShortScreen";
import "./card-deck.css";

interface Props {
  variant: Variant;
  say: (text: string) => void;
  onSeePlans: () => void;
  /** Home shows the newest batch alone and keeps quiet while there is none. */
  place?: "home" | "mygoose";
}

export function QuestionsPanel({ variant, say, onSeePlans, place = "mygoose" }: Props) {
  const batches = useLoaded(async () => {
    const [found, anchors] = await Promise.all([api.listQuestionBatches(), api.listAnchors()]);
    return { found, shared: new Set(anchors.filter((a) => !a.mine).map((a) => a.id)) };
  });
  const [done, setDone] = useState<ReadonlySet<string>>(new Set());
  const short = useShortScreen();

  function finished(batchId: string) {
    setDone((all) => new Set(all).add(batchId));
    batches.reload();
  }

  const open = batches.state.status === "ready" ? batches.state.value.found.filter((b) => !done.has(b.id)) : [];
  const shared = batches.state.status === "ready" ? batches.state.value.shared : new Set<string>();
  const shown = variant === "hub" || place === "home" ? open.slice(0, 1) : open;
  const rest = open.length - shown.length;

  if (place === "home") {
    if (shown.length === 0) return null;
    if (short) {
      const batch = shown[0];
      return (
        <button type="button" className="cd-glance" onClick={onSeePlans}>
          <span className="cd-glance__label">{QUESTIONS_HEADING}</span>
          <span className="cd-glance__title">{batch.title}</span>
          <span className="cd-glance__count">{glanceQuestionCount(batch.questions.length)}</span>
          <ChevronRight size={18} aria-hidden="true" />
        </button>
      );
    }
    return (
      <section className="mg-panel mg-panel--home" aria-label={QUESTIONS_HEADING}>
        {shown.map((batch) => (
          <QuestionCard
            key={batch.id}
            batch={batch}
            variant={variant}
            canSkip={batch.anchor_id === null || !shared.has(batch.anchor_id)}
            say={say}
            onDone={() => finished(batch.id)}
          />
        ))}
      </section>
    );
  }

  return (
    <section className="mg-panel" aria-label={QUESTIONS_HEADING}>
      <p className="mg__intro">{QUESTIONS_INTRO}</p>
      {batches.state.status === "loading" ? <Loading>{LOADING_QUESTIONS}</Loading> : null}
      {batches.state.status === "failed" ? <LoadProblem problem={batches.state.problem} onRetry={batches.retry} variant={variant} /> : null}
      {batches.state.status === "ready" && open.length === 0 ? (
        <InkEmptyState
          icon={<MessageCircleQuestion size={32} aria-hidden="true" />}
          title={NO_QUESTIONS_TITLE}
          action={
            <InkButton size={buttonSize(variant)} onPress={onSeePlans}>
              {SEE_PLANS}
            </InkButton>
          }
        >
          {NO_QUESTIONS_BODY}
        </InkEmptyState>
      ) : null}
      <div className="mg-list">
        {shown.map((batch) => (
          <QuestionCard
            key={batch.id}
            batch={batch}
            variant={variant}
            canSkip={batch.anchor_id === null || !shared.has(batch.anchor_id)}
            say={say}
            onDone={() => finished(batch.id)}
          />
        ))}
      </div>
      {rest > 0 ? <p className="mg__note">{moreWords(rest, "set of questions after this", "sets of questions after this")}</p> : null}
    </section>
  );
}
