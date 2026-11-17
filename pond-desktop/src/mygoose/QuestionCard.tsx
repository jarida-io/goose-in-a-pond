// A batch of up to four structured questions, asked one at a time with a pager: the header chip,
// the options with what each means and Goose's pick first, the preview of the chosen option, an
// "Other" that opens a text field, Skip, and Done. Each answer is sent on its own, so a member
// can stop halfway.

import { useId, useState, type FocusEvent } from "react";
import {
  InkBadge,
  InkButton,
  InkCard,
  InkCheckbox,
  InkChip,
  InkFormField,
  InkPagination,
  InkRadioGroup,
  InkTextArea,
} from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import { ApiError, type QuestionAnswer, type QuestionBatch, type QuestionView } from "../api/types";
import {
  ANSWER_SENT,
  DONE_ASKING,
  OTHER,
  OTHER_ANSWER,
  QUESTIONS_PAGER,
  RECOMMENDED,
  SEND_ANSWER,
  SKIP,
  SKIPPED,
  WAITING_FOR_YOU,
  WENT_AHEAD,
  buttonSize,
  questionStep,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { ActionProblem } from "./MyGooseParts";
import { QuestionPreview } from "./QuestionPreview";
import { shownOptions } from "./questionShape";

/** Never one of a question's options: the pond keeps "Other" out of them. */
const OTHER_VALUE = "\u0000other";

interface Props {
  batch: QuestionBatch;
  variant: Variant;
  /** A member an anchor is shared with may answer its questions, not skip them or say Done. */
  canSkip: boolean;
  /** Whether Done is offered; off where the card gathers questions from several batches. */
  canEnd?: boolean;
  say: (text: string) => void;
  /** Every question of the batch has been answered or skipped, or the member said Done. */
  onDone: () => void;
}

export function QuestionCard({ batch, variant, canSkip, canEnd = canSkip, say, onDone }: Props) {
  // By id, not position: a fresh read of the batch leaves out what was answered meanwhile.
  const [settled, setSettled] = useState<string[]>([]);
  const [at, setAt] = useState(0);
  const [ending, setEnding] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const remaining = batch.questions.filter((q) => !settled.includes(q.id));
  const shown = Math.min(at, Math.max(0, remaining.length - 1));
  const question = remaining[shown];
  const total = settled.length + remaining.length;
  const titleId = useId();

  function next(id: string) {
    setSettled((all) => [...all, id]);
    if (remaining.length <= 1) onDone();
  }

  async function enough() {
    setEnding(true);
    setProblem(null);
    try {
      await api.questionsEnough(batch.id);
      say(WENT_AHEAD);
      onDone();
    } catch (e) {
      setProblem(problemWords(e));
      setEnding(false);
    }
  }

  if (!question) return null;
  return (
    <InkCard className="mg-q">
      <section aria-labelledby={titleId}>
        <h3 id={titleId} className="mg-q__batch">
          {batch.title}
        </h3>
        {batch.state === "waiting" ? <p className="mg-q__waiting">{WAITING_FOR_YOU}</p> : null}
        {total > 1 ? <p className="mg-q__step">{questionStep(settled.length + 1, total)}</p> : null}
        {remaining.length > 1 ? (
          <InkPagination
            className="mg-q__pager"
            label={QUESTIONS_PAGER}
            page={shown + 1}
            count={remaining.length}
            onChange={(page) => setAt(page - 1)}
          />
        ) : null}
        <QuestionItem
          key={question.id}
          question={question}
          variant={variant}
          canSkip={canSkip}
          say={say}
          onSettled={() => next(question.id)}
        />
        {canEnd ? (
          <div className="mg-q__done">
            <InkButton size={buttonSize(variant)} disabled={ending} loading={ending} onPress={() => void enough()}>
              {DONE_ASKING}
            </InkButton>
          </div>
        ) : null}
        <ActionProblem text={problem} />
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

function OptionWords({ description, recommended }: { description: string; recommended: boolean }) {
  if (!description && !recommended) return null;
  return (
    <span className="mg-q__means">
      {recommended ? <InkBadge tone="brand">{RECOMMENDED}</InkBadge> : null}
      {description ? <span>{description}</span> : null}
    </span>
  );
}

function QuestionItem({ question, variant, canSkip, say, onSettled }: ItemProps) {
  const ids = useId();
  const options = shownOptions(question);
  const [choice, setChoice] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  // The option whose preview is shown: the last one chosen.
  const [focused, setFocused] = useState<string | null>(null);
  const [otherOn, setOtherOn] = useState(false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState<"send" | "skip" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  const showOther = question.multi ? otherOn : choice === OTHER_VALUE;
  const otherWords = text.trim();
  const ready = question.multi
    ? (chosen.length > 0 || otherOn) && (!otherOn || otherWords !== "")
    : choice !== "" && (choice !== OTHER_VALUE || otherWords !== "");
  const previewed = options.find((o) => o.label === focused && o.preview);

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

  function pick(value: string) {
    setChoice(value);
    setFocused(value === OTHER_VALUE ? null : value);
  }

  function toggle(option: string, on: boolean) {
    setChosen((all) => (on ? [...all.filter((o) => o !== option), option] : all.filter((o) => o !== option)));
    setFocused(on ? option : null);
  }

  return (
    <div className="mg-q__item">
      {question.header ? (
        <div className="mg-q__chip">
          <InkChip tone="brand">{question.header}</InkChip>
        </div>
      ) : null}
      <p className="mg-q__text" id={`${ids}-text`}>
        {question.text}
      </p>
      {question.multi ? (
        <div className="mg-q__checks" role="group" aria-labelledby={`${ids}-text`}>
          {options.map((option) => (
            <InkCheckbox
              key={option.label}
              label={option.label}
              checked={chosen.includes(option.label)}
              onChange={(on) => toggle(option.label, on)}
            >
              <span className="mg-q__option">
                <span className="mg-q__label">{option.label}</span>
                <OptionWords description={option.description} recommended={option.recommended} />
              </span>
            </InkCheckbox>
          ))}
          <InkCheckbox label={OTHER} checked={otherOn} onChange={setOtherOn}>
            {OTHER}
          </InkCheckbox>
        </div>
      ) : (
        <div className="mg-q__radios" tabIndex={choice === "" ? 0 : -1} onFocus={forwardFocus}>
          <InkRadioGroup
            label={question.text}
            value={choice}
            options={[
              ...options.map((o) => ({
                value: o.label,
                label: o.label,
                description: <OptionWords description={o.description} recommended={o.recommended} />,
              })),
              { value: OTHER_VALUE, label: OTHER },
            ]}
            onChange={pick}
          />
        </div>
      )}
      {previewed?.preview ? <QuestionPreview preview={previewed.preview} label={previewed.label} /> : null}
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
