// The card a `giap-questions__ask` call draws in chat: the questions at once from the call, then,
// once the pond has stored them, the same card My Goose shows, and the answers once sent.

import { useEffect, useState } from "react";
import { InkCard, InkChip } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import type { QuestionBatch } from "../api/types";
import type { ContextCard } from "../state/reducer";
import { QuestionCard } from "../mygoose/QuestionCard";
import { GETTING_READY, YOUR_ANSWERS } from "../mygoose/myGooseWords";
import { askedQuestions, isTheBatchAsked, shownOptions } from "../mygoose/questionShape";
import { useSaid } from "../mygoose/useLoaded";
import { Said } from "../mygoose/MyGooseParts";
import "../mygoose/mygoose.css";

/** How long the card looks for the stored batch: the pond stores it before the tool waits. */
const FIND_TRIES = 20;
const FIND_EVERY_MS = 600;

interface Props {
  card: ContextCard;
  /** The chat's id while it is known; without it the card looks through every open batch. */
  sessionId: string | null;
}

export function AskQuestionsCard({ card, sessionId }: Props) {
  const asked = askedQuestions(card.input);
  const answers = typeof card.data?.result === "string" ? card.data.result : null;
  const [batch, setBatch] = useState<QuestionBatch | null>(null);
  const [sent, setSent] = useState(false);
  const { said, say } = useSaid();
  const askedKey = JSON.stringify(asked.map((q) => q.text));

  useEffect(() => {
    if (answers !== null || asked.length === 0) return;
    let stopped = false;
    let tries = 0;
    async function look() {
      while (!stopped && tries < FIND_TRIES) {
        tries += 1;
        try {
          const found = await api.listQuestionBatches(sessionId ?? undefined);
          const mine = found.find((b) => b.purpose === "chat" && isTheBatchAsked(asked, b.questions));
          if (mine) {
            if (!stopped) setBatch(mine);
            return;
          }
        } catch {
          // A failed read is tried again; the card stays as drawn from the call meanwhile.
        }
        await new Promise((resolve) => setTimeout(resolve, FIND_EVERY_MS));
      }
    }
    void look();
    return () => {
      stopped = true;
    };
    // `askedKey` stands for `asked`, which is rebuilt each render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [askedKey, sessionId, answers]);

  if (asked.length === 0) return null;
  if (answers !== null) {
    return (
      <InkCard className="mg-q mg-q--answered">
        <section aria-label={YOUR_ANSWERS}>
          <h3 className="mg-q__batch">{YOUR_ANSWERS}</h3>
          <p className="mg-q__answers">{answers}</p>
        </section>
      </InkCard>
    );
  }
  if (batch && !sent) {
    return (
      <div className="ask-card">
        <QuestionCard batch={batch} variant="classic" canSkip say={say} onDone={() => setSent(true)} />
        <Said text={said} />
      </div>
    );
  }
  const first = asked[0];
  return (
    <InkCard className="mg-q mg-q--pending">
      <section aria-busy={!sent}>
        {first.header ? <InkChip tone="brand">{first.header}</InkChip> : null}
        <p className="mg-q__text">{first.text}</p>
        <ul className="mg-q__pending-options">
          {shownOptions(first).map((o) => (
            <li key={o.label}>{o.label}</li>
          ))}
        </ul>
        <p className="mg-q__step">{sent ? said ?? "" : GETTING_READY}</p>
      </section>
    </InkCard>
  );
}
