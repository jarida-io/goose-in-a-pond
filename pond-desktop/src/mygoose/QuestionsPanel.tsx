// Every open question batch as a card. On the kiosk one batch at a time: one decision per screen.

import { useState } from "react";
import { MessageCircleQuestion } from "lucide-react";
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
  moreWords,
  type Variant,
} from "./myGooseWords";
import { useLoaded } from "./useLoaded";
import { LoadProblem, Loading } from "./MyGooseParts";
import { QuestionCard } from "./QuestionCard";

interface Props {
  variant: Variant;
  say: (text: string) => void;
  onSeePlans: () => void;
}

export function QuestionsPanel({ variant, say, onSeePlans }: Props) {
  const batches = useLoaded(async () => {
    const [found, anchors] = await Promise.all([api.listQuestionBatches(), api.listAnchors()]);
    return { found, shared: new Set(anchors.filter((a) => !a.mine).map((a) => a.id)) };
  });
  const [done, setDone] = useState<ReadonlySet<string>>(new Set());

  function finished(batchId: string) {
    setDone((all) => new Set(all).add(batchId));
    batches.reload();
  }

  const open = batches.state.status === "ready" ? batches.state.value.found.filter((b) => !done.has(b.id)) : [];
  const shared = batches.state.status === "ready" ? batches.state.value.shared : new Set<string>();
  const shown = variant === "hub" ? open.slice(0, 1) : open;
  const rest = open.length - shown.length;

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
