// One idea card: collapsed it is readable in five seconds; opened it shows Goose's notes and sources.

import { useId, useState } from "react";
import { ChevronDown } from "lucide-react";
import { InkButton, InkCard, InkChip, InkFormField, InkLink, InkTextArea } from "@jarida/ink/react";
import type { IdeaCard, IdeaCardDismissal } from "../api/types";
import {
  CAVEATS_HEADING,
  DETAILS,
  DO_IT,
  FOUND_HEADING,
  GO_DEEPER,
  NEXT_HEADING,
  NOT_THIS,
  SAVE_CARD,
  SOURCES_HEADING,
  UNSAVE_CARD,
  cardFields,
  cardKindWords,
  sourceWords,
} from "./cardWords";
import { OTHER, OTHER_ANSWER, RECOMMENDED, buttonSize, type Variant } from "./myGooseWords";
import { shownOptions } from "./questionShape";
import { problemWords } from "./problems";
import { ActionProblem } from "./MyGooseParts";
import { RejectSheet } from "./TurnDownSheets";

type Busy = "do" | "deeper" | "save" | "answer" | null;

export interface IdeaCardViewProps {
  card: IdeaCard;
  variant: Variant;
  onDo: (card: IdeaCard) => Promise<void>;
  onGoDeeper: (card: IdeaCard) => Promise<void>;
  onSave: (card: IdeaCard, save: boolean) => Promise<void>;
  onDismiss: (card: IdeaCard, dismissal: IdeaCardDismissal) => Promise<void>;
  /** An ask card's option, or the member's own words, sent to Goose as their message. */
  onAnswer?: (card: IdeaCard, words: string) => Promise<void>;
}

export function IdeaCardView({ card, variant, onDo, onGoDeeper, onSave, onDismiss, onAnswer }: IdeaCardViewProps) {
  const ids = useId();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<Busy>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [turningDown, setTurningDown] = useState(false);
  const size = buttonSize(variant);
  const fields = cardFields(card);
  const saved = card.state === "saved";
  const question = card.kind === "ask" && onAnswer ? card.action?.question ?? null : null;
  const [otherOn, setOtherOn] = useState(false);
  const [otherText, setOtherText] = useState("");

  async function act(which: Exclude<Busy, null>, work: () => Promise<void>) {
    setBusy(which);
    setProblem(null);
    try {
      await work();
    } catch (e) {
      setProblem(problemWords(e));
    }
    setBusy(null);
  }

  const notes = [
    { heading: FOUND_HEADING, text: card.notes.found },
    { heading: NEXT_HEADING, text: card.notes.next },
    { heading: CAVEATS_HEADING, text: card.notes.caveats },
  ].filter((n) => n.text.trim() !== "");

  return (
    <InkCard className={`cd-card cd-card--${variant}`}>
      <article className="cd-card__body" aria-labelledby={`${ids}-title`}>
        <p className="cd-card__kind">{cardKindWords(card.kind)}</p>
        <h3 id={`${ids}-title`} className="cd-card__title">
          {card.title}
        </h3>
        <p className="cd-card__why">
          {card.why.text} <span className="cd-card__source">{sourceWords(card.why.source)}</span>
        </p>
        {fields.length > 0 ? (
          <dl className="cd-card__fields">
            {fields.map((f) => (
              <div key={f.label} className="cd-card__field">
                <dt>{f.label}</dt>
                <dd>{f.value}</dd>
              </div>
            ))}
          </dl>
        ) : null}
        {question && onAnswer ? (
          <div className="cd-card__ask" role="group" aria-labelledby={`${ids}-ask`}>
            {question.header ? <InkChip tone="brand">{question.header}</InkChip> : null}
            <p id={`${ids}-ask`} className="cd-card__ask-text">
              {question.text}
            </p>
            <div className="cd-card__ask-options">
              {shownOptions(question).map((option) => (
                <InkButton
                  key={option.label}
                  variant={option.recommended ? "primary" : undefined}
                  size={size}
                  disabled={busy !== null}
                  loading={busy === "answer"}
                  onPress={() => void act("answer", () => onAnswer(card, option.label))}
                >
                  {option.label}
                </InkButton>
              ))}
              <InkButton size={size} disabled={busy !== null} onPress={() => setOtherOn((on) => !on)}>
                {OTHER}
              </InkButton>
            </div>
            <ul className="cd-card__ask-means">
              {shownOptions(question)
                .filter((o) => o.description || o.recommended)
                .map((o) => (
                  <li key={o.label}>
                    <strong>{o.label}</strong>
                    {o.recommended ? ` (${RECOMMENDED})` : ""}
                    {o.description ? `: ${o.description}` : ""}
                  </li>
                ))}
            </ul>
            {otherOn ? (
              <div className="cd-card__ask-other">
                <InkFormField id={`${ids}-other`} label={OTHER_ANSWER}>
                  <InkTextArea
                    label={OTHER_ANSWER}
                    value={otherText}
                    rows={2}
                    inputProps={{ id: `${ids}-other-control` }}
                    onChange={setOtherText}
                  />
                </InkFormField>
                <InkButton
                  variant="primary"
                  size={size}
                  disabled={busy !== null || otherText.trim() === ""}
                  onPress={() => void act("answer", () => onAnswer(card, otherText.trim()))}
                >
                  {card.action?.label.trim() || DO_IT}
                </InkButton>
              </div>
            ) : null}
          </div>
        ) : null}
        <div className="cd-card__actions">
          {card.action && card.action.kind !== "go_deeper" && !question ? (
            <InkButton
              variant="primary"
              size={size}
              loading={busy === "do"}
              disabled={busy !== null}
              onPress={() => void act("do", () => onDo(card))}
            >
              {card.action.label.trim() || DO_IT}
            </InkButton>
          ) : null}
          <InkButton
            size={size}
            loading={busy === "save"}
            disabled={busy !== null}
            onPress={() => void act("save", () => onSave(card, !saved))}
          >
            {saved ? UNSAVE_CARD : SAVE_CARD}
          </InkButton>
          <InkButton size={size} disabled={busy !== null} onPress={() => setTurningDown(true)}>
            {NOT_THIS}
          </InkButton>
          <InkButton
            variant={card.action?.kind === "go_deeper" ? "primary" : undefined}
            size={size}
            loading={busy === "deeper"}
            disabled={busy !== null}
            onPress={() => void act("deeper", () => onGoDeeper(card))}
          >
            {GO_DEEPER}
          </InkButton>
          <button
            type="button"
            className="cd-card__more reach"
            aria-expanded={open}
            aria-controls={`${ids}-notes`}
            onClick={() => setOpen((o) => !o)}
          >
            {DETAILS}
            <ChevronDown size={16} aria-hidden="true" className="cd-card__chevron" data-open={open || undefined} />
          </button>
        </div>
        <ActionProblem text={problem} />
        {open ? (
          <div id={`${ids}-notes`} className="cd-card__notes">
            {notes.map((n) => (
              <section key={n.heading} className="cd-card__note">
                <h4>{n.heading}</h4>
                <p>{n.text}</p>
              </section>
            ))}
            {card.sources.length > 0 ? (
              <section className="cd-card__note">
                <h4>{SOURCES_HEADING}</h4>
                <ul className="cd-card__sources">
                  {card.sources.map((s, i) => (
                    <li key={`${i}-${s.title}`}>
                      {s.url ? (
                        <InkLink href={s.url} external>
                          {s.title}
                        </InkLink>
                      ) : (
                        s.title
                      )}
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}
          </div>
        ) : null}
      </article>
      {turningDown ? (
        <RejectSheet
          verb="not_this"
          variant={variant}
          onClose={() => setTurningDown(false)}
          onReject={({ reasons, text }) => onDismiss(card, { reasons, text })}
        />
      ) : null}
    </InkCard>
  );
}
