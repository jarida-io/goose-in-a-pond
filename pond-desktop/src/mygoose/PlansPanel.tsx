// The anchors home: what Goose thinks is happening first, each with why and a yes or a not now;
// then what it is working on. On the kiosk one candidate at a time: one decision per screen.

import { useState } from "react";
import { CalendarHeart, Plus } from "lucide-react";
import { InkButton, InkEmptyState } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import type { Anchor } from "../api/types";
import {
  ACTIVE_HEADING,
  ADD_PLAN,
  ANSWER_THEM,
  CANDIDATES_HEADING,
  LOADING_PLANS,
  NO_PLANS_BODY,
  NO_PLANS_TITLE,
  PLANS_HEADING,
  PLANS_INTRO,
  addedWords,
  buttonSize,
  confirmedWords,
  moreWords,
  notNowWords,
  questionCountWords,
  type Variant,
} from "./myGooseWords";
import { pollFor, useLoaded } from "./useLoaded";
import { LoadProblem, Loading } from "./MyGooseParts";
import { AnchorCard, CandidateCard } from "./AnchorCards";
import { AddAnchorForm } from "./AddAnchorForm";

interface Props {
  variant: Variant;
  say: (text: string) => void;
  onOpen: (anchor: Anchor) => void;
  onAnswerQuestions: () => void;
}

export function PlansPanel({ variant, say, onOpen, onAnswerQuestions }: Props) {
  const anchors = useLoaded(() => api.listAnchors(), (all) => pollFor(all.map((a) => a.job)));
  const [adding, setAdding] = useState(false);

  async function confirm(anchor: Anchor) {
    await api.confirmAnchor(anchor.id);
    say(confirmedWords(anchor.title));
    anchors.reload();
  }

  async function dismiss(anchor: Anchor) {
    await api.dismissAnchor(anchor.id);
    say(notNowWords(anchor.title));
    anchors.reload();
  }

  function added(anchor: Anchor) {
    setAdding(false);
    say(addedWords(anchor.title));
    anchors.reload();
  }

  const all = anchors.state.status === "ready" ? anchors.state.value : [];
  const candidates = all.filter((a) => a.state === "candidate");
  const active = all.filter((a) => a.state === "active");
  const shownCandidates = variant === "hub" ? candidates.slice(0, 1) : candidates;
  const moreCandidates = candidates.length - shownCandidates.length;
  const questions = active.reduce((sum, a) => sum + a.open_questions, 0);
  const empty = anchors.state.status === "ready" && candidates.length === 0 && active.length === 0;

  const form = adding ? <AddAnchorForm variant={variant} onAdded={added} onCancel={() => setAdding(false)} /> : null;

  return (
    <section className="mg-panel" aria-label={PLANS_HEADING}>
      <p className="mg__intro">{PLANS_INTRO}</p>
      {anchors.state.status === "loading" ? <Loading>{LOADING_PLANS}</Loading> : null}
      {anchors.state.status === "failed" ? <LoadProblem problem={anchors.state.problem} onRetry={anchors.retry} variant={variant} /> : null}

      {empty && !adding ? (
        <InkEmptyState
          icon={<CalendarHeart size={32} aria-hidden="true" />}
          title={NO_PLANS_TITLE}
          action={
            <InkButton variant="primary" size={buttonSize(variant)} icon={<Plus size={18} aria-hidden="true" />} onPress={() => setAdding(true)}>
              {ADD_PLAN}
            </InkButton>
          }
        >
          {NO_PLANS_BODY}
        </InkEmptyState>
      ) : null}
      {empty ? form : null}

      {shownCandidates.length > 0 ? (
        <div className="mg-sec">
          <h2 className="mg__h2">{CANDIDATES_HEADING}</h2>
          <div className="mg-list">
            {shownCandidates.map((anchor) => (
              <CandidateCard key={anchor.id} anchor={anchor} variant={variant} onConfirm={confirm} onDismiss={dismiss} />
            ))}
          </div>
          {moreCandidates > 0 ? <p className="mg__note">{moreWords(moreCandidates, "after this one", "after this one")}</p> : null}
        </div>
      ) : null}

      {questions > 0 ? (
        <div className="mg-asks">
          <span>{questionCountWords(questions)}</span>
          <InkButton size={buttonSize(variant)} onPress={onAnswerQuestions}>
            {ANSWER_THEM}
          </InkButton>
        </div>
      ) : null}

      {active.length > 0 ? (
        <div className="mg-sec">
          <h2 className="mg__h2">{ACTIVE_HEADING}</h2>
          <ul className="mg-list" aria-label={ACTIVE_HEADING}>
            {active.map((anchor) => (
              <li key={anchor.id}>
                <AnchorCard anchor={anchor} onOpen={onOpen} />
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {!empty && anchors.state.status === "ready" ? (
        adding ? (
          form
        ) : (
          <div className="mg__actions">
            <InkButton size={buttonSize(variant)} icon={<Plus size={18} aria-hidden="true" />} onPress={() => setAdding(true)}>
              {ADD_PLAN}
            </InkButton>
          </div>
        )
      ) : null}
    </section>
  );
}
