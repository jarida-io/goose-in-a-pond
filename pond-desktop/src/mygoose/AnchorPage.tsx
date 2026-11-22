// One anchor: what Goose made for it, the job running now as a live pipeline, its open questions,
// the facts it holds, and, for its owner, Share and Stop. A member it is shared with reads it.

import { useEffect, useId, useRef, useState, type RefObject } from "react";
import { ChevronRight, Share2, Square } from "lucide-react";
import { InkBadge, InkButton, InkCheckbox, InkSheet } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import type { Anchor, DeliverableSummary, HouseholdProfile } from "../api/types";
import {
  ALL_PLANS,
  CANCEL,
  CONFIRM,
  FACTS_HEADING,
  LOADING_PLAN,
  MADE_HEADING,
  NEW_BADGE,
  NOTHING_MADE,
  NOT_NOW,
  SAVE_SHARING,
  SHARE,
  SHARED_SAVED,
  SHARE_DESCRIPTION,
  SHARE_NOBODY,
  STOP,
  WHY_PREFIX,
  anchorMeta,
  buttonSize,
  canOpen,
  confirmedWords,
  deliverableKindWords,
  deliverableStateWords,
  notNowWords,
  sharedByWords,
  stoppedWords,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { pollFor, useLoaded } from "./useLoaded";
import { ActionProblem, BackButton, LoadProblem, Loading } from "./MyGooseParts";
import { JobPipeline } from "./JobPipeline";
import { QuestionCard } from "./QuestionCard";
import { StopSheet } from "./TurnDownSheets";

interface Props {
  id: string;
  variant: Variant;
  say: (text: string) => void;
  onBack: () => void;
  onOpenDeliverable: (deliverable: DeliverableSummary) => void;
}

export function AnchorPage({ id, variant, say, onBack, onOpenDeliverable }: Props) {
  const anchor = useLoaded(() => api.getAnchor(id), (a) => pollFor([a.job]));
  const titleRef = useRef<HTMLHeadingElement>(null);
  const focused = useRef(false);

  useEffect(() => {
    if (anchor.state.status !== "ready" || focused.current) return;
    focused.current = true;
    titleRef.current?.focus();
  }, [anchor.state.status]);

  return (
    <>
      <BackButton label={ALL_PLANS} onBack={onBack} />
      {anchor.state.status === "loading" ? <Loading>{LOADING_PLAN}</Loading> : null}
      {anchor.state.status === "failed" ? <LoadProblem problem={anchor.state.problem} onRetry={anchor.retry} variant={variant} /> : null}
      {anchor.state.status === "ready" ? (
        <AnchorBody
          anchor={anchor.state.value}
          variant={variant}
          titleRef={titleRef}
          say={say}
          onChanged={(next) => (next ? anchor.replace(next) : anchor.reload())}
          onGone={onBack}
          onOpenDeliverable={onOpenDeliverable}
        />
      ) : null}
    </>
  );
}

interface BodyProps {
  anchor: Anchor;
  variant: Variant;
  titleRef: RefObject<HTMLHeadingElement | null>;
  say: (text: string) => void;
  /** With the anchor a change answered, or nothing to ask the pond again. */
  onChanged: (next?: Anchor) => void;
  onGone: () => void;
  onOpenDeliverable: (deliverable: DeliverableSummary) => void;
}

function AnchorBody({ anchor, variant, titleRef, say, onChanged, onGone, onOpenDeliverable }: BodyProps) {
  const titleId = useId();
  const [sheet, setSheet] = useState<"stop" | "share" | null>(null);
  const [busy, setBusy] = useState<"confirm" | "dismiss" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const size = buttonSize(variant);
  const questions = anchor.questions ?? [];

  async function decide(which: "confirm" | "dismiss") {
    setBusy(which);
    setProblem(null);
    try {
      if (which === "confirm") {
        const next = await api.confirmAnchor(anchor.id);
        say(confirmedWords(anchor.title));
        onChanged(next);
      } else {
        await api.dismissAnchor(anchor.id);
        say(notNowWords(anchor.title));
        onGone();
      }
    } catch (e) {
      setProblem(problemWords(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <article className="mg-page" aria-labelledby={titleId}>
      <header className="mg__head">
        <h1 id={titleId} ref={titleRef} className="mg__title" tabIndex={-1}>
          {anchor.title}
        </h1>
        <p className="mg__intro">{anchorMeta(anchor)}</p>
        {anchor.mine ? null : <p className="mg__note">{sharedByWords(anchor.shared_by)}</p>}
        {anchor.state === "candidate" && anchor.why.trim() !== "" ? (
          <p className="mg-anchor__why">
            <span className="mg-anchor__why-label">{WHY_PREFIX}</span> {anchor.why}
          </p>
        ) : null}
        {anchor.mine ? (
          <div className="mg__actions">
            {anchor.state === "candidate" ? (
              <>
                <InkButton variant="primary" size={size} loading={busy === "confirm"} disabled={busy !== null} onPress={() => void decide("confirm")}>
                  {CONFIRM}
                </InkButton>
                <InkButton size={size} loading={busy === "dismiss"} disabled={busy !== null} onPress={() => void decide("dismiss")}>
                  {NOT_NOW}
                </InkButton>
              </>
            ) : null}
            {anchor.state === "active" ? (
              <>
                <InkButton size={size} icon={<Share2 size={16} aria-hidden="true" />} onPress={() => setSheet("share")}>
                  {SHARE}
                </InkButton>
                <InkButton variant="danger" size={size} icon={<Square size={16} aria-hidden="true" />} onPress={() => setSheet("stop")}>
                  {STOP}
                </InkButton>
              </>
            ) : null}
          </div>
        ) : null}
        <ActionProblem text={problem} />
      </header>

      {anchor.job ? <JobPipeline job={anchor.job} /> : null}

      {questions.length > 0 ? (
        <QuestionCard
          batch={{ id: `anchor-${anchor.id}`, purpose: "anchor", anchor_id: anchor.id, title: `Questions about ${anchor.title}`, questions }}
          variant={variant}
          canSkip={anchor.mine}
          canEnd={false}
          say={say}
          onDone={() => onChanged()}
        />
      ) : null}

      <MadeList anchor={anchor} onOpen={onOpenDeliverable} />

      {anchor.facts.length > 0 ? (
        <section className="mg-sec" aria-label={FACTS_HEADING}>
          <h2 className="mg__h2">{FACTS_HEADING}</h2>
          <dl className="mg-facts">
            {anchor.facts.map((fact) => (
              <div key={`${fact.label}-${fact.value}`} className="mg-facts__row">
                <dt>{fact.label}</dt>
                <dd>{fact.value}</dd>
              </div>
            ))}
          </dl>
        </section>
      ) : null}

      {sheet === "stop" ? (
        <StopSheet
          anchorTitle={anchor.title}
          variant={variant}
          onClose={() => setSheet(null)}
          onStop={async (stop) => {
            await api.stopAnchor(anchor.id, stop);
            setSheet(null);
            say(stoppedWords(anchor.title));
            onGone();
          }}
        />
      ) : null}
      {sheet === "share" ? (
        <ShareSheet
          anchor={anchor}
          variant={variant}
          onClose={() => setSheet(null)}
          onShared={(next) => {
            setSheet(null);
            say(SHARED_SAVED);
            onChanged(next);
          }}
        />
      ) : null}
    </article>
  );
}

function MadeList({ anchor, onOpen }: { anchor: Anchor; onOpen: (d: DeliverableSummary) => void }) {
  const headingId = useId();
  return (
    <section className="mg-sec" aria-labelledby={headingId}>
      <h2 id={headingId} className="mg__h2">
        {MADE_HEADING}
      </h2>
      {anchor.deliverables.length === 0 ? (
        <p className="mg__note">{NOTHING_MADE}</p>
      ) : (
        <ul className="mg-made">
          {anchor.deliverables.map((d) => {
            const body = (
              <>
                <span className="mg-made__main">
                  <span className="mg-made__title">{d.title}</span>
                  <span className="mg-made__meta">
                    {deliverableKindWords(d.kind)} · {deliverableStateWords(d)}
                  </span>
                  {d.hook.trim() !== "" ? <span className="mg-made__hook">{d.hook}</span> : null}
                </span>
                {d.unseen ? <InkBadge>{NEW_BADGE}</InkBadge> : null}
              </>
            );
            return (
              <li key={d.id}>
                {canOpen(d) ? (
                  <button type="button" className="mg-made__row mg-made__row--open" onClick={() => onOpen(d)}>
                    {body}
                    <ChevronRight size={18} aria-hidden="true" className="mg-made__go" />
                  </button>
                ) : (
                  <div className="mg-made__row">{body}</div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

interface ShareProps {
  anchor: Anchor;
  variant: Variant;
  onClose: () => void;
  onShared: (next: Anchor) => void;
}

function ShareSheet({ anchor, variant, onClose, onShared }: ShareProps) {
  const members = useLoaded(async () => (await api.listProfiles()).profiles ?? []);
  const [chosen, setChosen] = useState<string[]>(anchor.shares);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function save() {
    setBusy(true);
    setProblem(null);
    try {
      onShared(await api.shareAnchor(anchor.id, chosen));
    } catch (e) {
      setProblem(problemWords(e));
      setBusy(false);
    }
  }

  const list: HouseholdProfile[] =
    members.state.status === "ready"
      ? members.state.value.filter((p) => p.role !== "guest" && p.id !== anchor.owner_id)
      : [];

  return (
    <InkSheet open onClose={busy ? () => {} : onClose} title={`${SHARE} ${anchor.title}`} description={SHARE_DESCRIPTION}>
      {members.state.status === "failed" ? <LoadProblem problem={members.state.problem} onRetry={members.retry} variant={variant} /> : null}
      {members.state.status === "ready" && list.length < 1 ? <p className="mg__note">{SHARE_NOBODY}</p> : null}
      {list.length >= 1 ? (
        <div className="mg-share">
          {list.map((member) => (
            <InkCheckbox
              key={member.id}
              checked={chosen.includes(member.id)}
              onChange={(on) => setChosen((all) => (on ? [...all, member.id] : all.filter((x) => x !== member.id)))}
            >
              {member.display_name}
            </InkCheckbox>
          ))}
        </div>
      ) : null}
      <div className="mg__actions">
        {list.length >= 1 ? (
          <InkButton variant="primary" size={buttonSize(variant)} loading={busy} onPress={() => void save()}>
            {SAVE_SHARING}
          </InkButton>
        ) : null}
        <InkButton size={buttonSize(variant)} disabled={busy} onPress={onClose}>
          {CANCEL}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </InkSheet>
  );
}
