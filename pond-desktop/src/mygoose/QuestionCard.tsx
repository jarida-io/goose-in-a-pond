// A batch of up to four questions, asked one at a time: the options, always an "Other" that
// opens a text field, and Skip. Each answer is sent on its own, so a member can stop halfway.

import { useId, useState, type FocusEvent } from "react";
import { InkButton, InkCard, InkChoice, InkFormField, InkRadioGroup, InkTextArea } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import { ApiError, type QuestionAnswer, type QuestionBatch, type QuestionView } from "../api/types";
import { ANSWER_SENT, OTHER, OTHER_ANSWER, SEND_ANSWER, SKIP, SKIPPED, buttonSize, questionStep, type Variant } from "./myGooseWords";
import { problemWords } from "./problems";
import { ActionProblem } from "./MyGooseParts";

/** Never one of a question's options: the pond keeps "Other" out of them. */
const OTHER_VALUE = "\u0000other";

interface Props {
  batch: QuestionBatch;
  variant: Variant;
  /** A member an anchor is shared with may answer its questions, not skip them. */
  canSkip: boolean;
  say: (text: string) => void;
  /** Every question of the batch has been answered or skipped. */
  onDone: () => void;
}

export function QuestionCard({ batch, variant, canSkip, say, onDone }: Props) {
  // By id, not position: a fresh read of the batch leaves out what was answered meanwhile.
  const [settled, setSettled] = useState<string[]>([]);
  const remaining = batch.questions.filter((q) => !settled.includes(q.id));
  const question = remaining[0];
  const total = settled.length + remaining.length;
  const titleId = useId();

  function next(id: string) {
    setSettled((all) => [...all, id]);
    if (remaining.length <= 1) onDone();
  }

  if (!question) return null;
  return (
    <InkCard className="mg-q">
      <section aria-labelledby={titleId}>
        <h3 id={titleId} className="mg-q__batch">
          {batch.title}
        </h3>
        {total > 1 ? <p className="mg-q__step">{questionStep(settled.length + 1, total)}</p> : null}
        <QuestionItem
          key={question.id}
          question={question}
          variant={variant}
          canSkip={canSkip}
          say={say}
          onSettled={() => next(question.id)}
        />
      </section>
    </InkCard>
  );
}

interface ItemProps {
  question: QuestionView;
  variant: Variant;
  canSkip: boolean;
  say: (text: string) => void;
  onSettled: () => void;
}

function QuestionItem({ question, variant, canSkip, say, onSettled }: ItemProps) {
  const ids = useId();
  const [choice, setChoice] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [otherOn, setOtherOn] = useState(false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState<"send" | "skip" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  const showOther = question.multi ? otherOn : choice === OTHER_VALUE;
  const otherWords = text.trim();
  const ready = question.multi
    ? (chosen.length > 0 || otherOn) && (!otherOn || otherWords !== "")
    : choice !== "" && (choice !== OTHER_VALUE || otherWords !== "");

  function answer(): QuestionAnswer {
    if (question.multi) return otherOn ? { choices: chosen, text: otherWords } : { choices: chosen };
    return choice === OTHER_VALUE ? { choices: [], text: otherWords } : { choices: [choice] };
  }

  async function settle(which: "send" | "skip") {
    setBusy(which);
    setProblem(null);
    try {
      if (which === "send") await api.answerQuestion(question.id, answer());
      else await api.skipQuestion(question.id);
      say(which === "send" ? ANSWER_SENT : SKIPPED);
      onSettled();
    } catch (e) {
      // Answered elsewhere, from the phone say: there is nothing left to do here.
      if (e instanceof ApiError && e.status === 409) {
        say(problemWords(e));
        onSettled();
        return;
      }
      setProblem(problemWords(e));
      setBusy(null);
    }
  }

  // Ink's radios are all out of the tab order until one is chosen; this stop lets Tab reach them.
  function forwardFocus(e: FocusEvent<HTMLDivElement>) {
    if (e.target !== e.currentTarget || e.currentTarget.contains(e.relatedTarget as Node | null)) return;
    e.currentTarget.querySelector<HTMLElement>("[role='radio']")?.focus();
  }

  function toggle(option: string) {
    setChosen((all) => (all.includes(option) ? all.filter((o) => o !== option) : [...all, option]));
  }

  return (
    <div className="mg-q__item">
      <p className="mg-q__text" id={`${ids}-text`}>
        {question.text}
      </p>
      {question.multi ? (
        <div className="mg-q__pills" role="group" aria-labelledby={`${ids}-text`}>
          {question.options.map((option) => (
            <InkChoice key={option} shape="pill" selected={chosen.includes(option)} onSelect={() => toggle(option)}>
              {option}
            </InkChoice>
          ))}
          <InkChoice shape="pill" selected={otherOn} onSelect={() => setOtherOn((v) => !v)}>
            {OTHER}
          </InkChoice>
        </div>
      ) : (
        <div className="mg-q__radios" tabIndex={choice === "" ? 0 : -1} onFocus={forwardFocus}>
          <InkRadioGroup
            label={question.text}
            value={choice}
            options={[...question.options.map((o) => ({ value: o, label: o })), { value: OTHER_VALUE, label: OTHER }]}
            onChange={setChoice}
          />
        </div>
      )}
      {showOther ? (
        <InkFormField id={`${ids}-other`} label={OTHER_ANSWER}>
          <InkTextArea
            label={OTHER_ANSWER}
            value={text}
            rows={2}
            inputProps={{ id: `${ids}-other-control` }}
            onChange={setText}
          />
        </InkFormField>
      ) : null}
      <div className="mg__actions">
        <InkButton
          variant="primary"
          size={buttonSize(variant)}
          disabled={!ready || busy !== null}
          loading={busy === "send"}
          onPress={() => void settle("send")}
        >
          {SEND_ANSWER}
        </InkButton>
        {canSkip ? (
          <InkButton size={buttonSize(variant)} disabled={busy !== null} loading={busy === "skip"} onPress={() => void settle("skip")}>
            {SKIP}
          </InkButton>
        ) : null}
      </div>
      <ActionProblem text={problem} />
    </div>
  );
}
