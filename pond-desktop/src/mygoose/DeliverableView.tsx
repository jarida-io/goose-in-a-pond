// One deliverable read as a document. When Goose is unsure, the critic's objections come before
// the body, so the member weighs them first.

import { useCallback, useEffect, useId, useMemo, useRef, useState, type RefObject } from "react";
import { Ban, Download, RefreshCw, ThumbsDown } from "lucide-react";
import { InkButton, InkNotice } from "@jarida/ink/react";
import "../longtasks/report.css";
import { api } from "../api/PondApiClient";
import type { Deliverable, RejectDeliverable } from "../api/types";
import { MarkdownText } from "../components/MarkdownText";
import { fileNameFor, saveBlob } from "../lib/saveFile";
import { withoutTitle } from "../longtasks/longTaskWords";
import { SourceChip, SourcesRow } from "../longtasks/ReportSources";
import { numberedSources, withoutSourcesList, wordsOnlyPoint } from "../longtasks/sourceNumbers";
import {
  BACK_TO,
  CHANGED_HEADING,
  DOWNLOAD,
  GLANCE_HEADING,
  LOADING_DELIVERABLE,
  NEVER_THIS,
  NOT_THIS_ONE,
  REFRESH,
  REFRESHING_NOW,
  STILL_MAKING,
  TO_PASS,
  UNSURE_NOTICE,
  buttonSize,
  dayWords,
  deliverableKindWords,
  objectionWords,
  refreshWords,
  turnedDownWords,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { pollFor, useLoaded } from "./useLoaded";
import { ActionProblem, BackButton, LoadProblem, Loading } from "./MyGooseParts";
import { RejectSheet } from "./TurnDownSheets";

interface Props {
  id: string;
  /** Shown on the way back before the deliverable itself has loaded. */
  anchorTitle: string;
  variant: Variant;
  say: (text: string) => void;
  onBack: () => void;
}

export function DeliverableView({ id, anchorTitle, variant, say, onBack }: Props) {
  const deliverable = useLoaded(() => api.getDeliverable(id), (d) => pollFor([d.job]));
  const titleRef = useRef<HTMLHeadingElement>(null);
  const marked = useRef(false);

  useEffect(() => {
    if (deliverable.state.status !== "ready" || marked.current) return;
    marked.current = true;
    titleRef.current?.focus();
    // Best effort: a missed mark leaves it shown as new, which the next open retries.
    if (deliverable.state.value.unseen) void api.markDeliverableSeen(id).catch(() => {});
  }, [deliverable.state, id]);

  const back = deliverable.state.status === "ready" ? deliverable.state.value.anchor_title : anchorTitle;

  return (
    <>
      <BackButton label={BACK_TO(back)} onBack={onBack} />
      {deliverable.state.status === "loading" ? <Loading>{LOADING_DELIVERABLE}</Loading> : null}
      {deliverable.state.status === "failed" ? (
        <LoadProblem problem={deliverable.state.problem} onRetry={deliverable.retry} variant={variant} />
      ) : null}
      {deliverable.state.status === "ready" ? (
        <DeliverableBody
          deliverable={deliverable.state.value}
          variant={variant}
          titleRef={titleRef}
          say={say}
          onRefreshed={deliverable.reload}
          onTurnedDown={onBack}
        />
      ) : null}
    </>
  );
}

interface BodyProps {
  deliverable: Deliverable;
  variant: Variant;
  titleRef: RefObject<HTMLHeadingElement | null>;
  say: (text: string) => void;
  onRefreshed: () => void;
  onTurnedDown: () => void;
}

function DeliverableBody({ deliverable: d, variant, titleRef, say, onRefreshed, onTurnedDown }: BodyProps) {
  const titleId = useId();
  const prefix = useId();
  const [rejecting, setRejecting] = useState<RejectDeliverable["verb"] | null>(null);
  const [busy, setBusy] = useState<"refresh" | "download" | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const size = buttonSize(variant);

  const numbered = useMemo(() => numberedSources(d.sources), [d.sources]);
  const markdown = useMemo(() => {
    const text = withoutTitle(d.body, d.title);
    return numbered.sources.length > 0 ? withoutSourcesList(text) : text;
  }, [d.body, d.title, numbered]);

  // A citation keeps its words and gains its number, as a long report's does.
  const cite = useCallback(
    (href: string, label: string) => {
      const n = numbered.numberOf(href);
      if (n === null) return null;
      const chip = <SourceChip n={n} source={numbered.sources[n - 1]} />;
      return wordsOnlyPoint(label, href) ? (
        chip
      ) : (
        <>
          {label}
          {chip}
        </>
      );
    },
    [numbered],
  );

  async function refresh() {
    setBusy("refresh");
    setProblem(null);
    try {
      const answer = await api.refreshDeliverable(d.id);
      say(refreshWords(answer.state));
      onRefreshed();
    } catch (e) {
      setProblem(problemWords(e));
    } finally {
      setBusy(null);
    }
  }

  async function download() {
    setBusy("download");
    setProblem(null);
    try {
      const { blob, filename } = await api.getDeliverableFile(d.id);
      saveBlob(blob, filename ?? fileNameFor(d.title, "md"));
    } catch (e) {
      setProblem(problemWords(e));
    } finally {
      setBusy(null);
    }
  }

  async function reject(rejection: RejectDeliverable) {
    await api.rejectDeliverable(d.id, rejection);
    setRejecting(null);
    say(turnedDownWords(rejection.verb, d.title));
    onTurnedDown();
  }

  const written = d.body.trim() !== "";
  const canTurnDown = d.mine && d.state !== "rejected" && d.state !== "archived" && (written || d.version > 0);
  const updated = dayWords(d.updated_at);
  const meta = [deliverableKindWords(d.kind), d.version > 0 ? `Version ${d.version}` : null, updated ? `Updated ${updated}` : null]
    .filter(Boolean)
    .join(" · ");

  return (
    <article className="mg-page mg-doc" aria-labelledby={titleId}>
      <header className="mg__head">
        <h1 id={titleId} ref={titleRef} className="mg__title" tabIndex={-1}>
          {d.title}
        </h1>
        <p className="mg__intro">{meta}</p>
        {d.job ? <p className="mg__note">{REFRESHING_NOW}</p> : null}
        <div className="mg__actions">
          {d.mine && d.state !== "rejected" ? (
            <InkButton size={size} icon={<RefreshCw size={16} aria-hidden="true" />} loading={busy === "refresh"} disabled={busy !== null} onPress={() => void refresh()}>
              {REFRESH}
            </InkButton>
          ) : null}
          {written ? (
            <InkButton size={size} icon={<Download size={16} aria-hidden="true" />} loading={busy === "download"} disabled={busy !== null} onPress={() => void download()}>
              {DOWNLOAD}
            </InkButton>
          ) : null}
          {canTurnDown ? (
            <>
              <InkButton size={size} icon={<ThumbsDown size={16} aria-hidden="true" />} onPress={() => setRejecting("not_this")}>
                {NOT_THIS_ONE}
              </InkButton>
              <InkButton size={size} icon={<Ban size={16} aria-hidden="true" />} onPress={() => setRejecting("never")}>
                {NEVER_THIS}
              </InkButton>
            </>
          ) : null}
        </div>
        <ActionProblem text={problem} />
      </header>

      {d.state === "unsure" && d.objections.length > 0 ? (
        <section className="mg-unsure" aria-label={UNSURE_NOTICE}>
          <InkNotice tone="warning">{UNSURE_NOTICE}</InkNotice>
          <ul className="mg-unsure__list">
            {d.objections.map((o, i) => (
              <li key={`${o.kind}-${i}`} className="mg-unsure__item">
                <span className="mg-unsure__kind">{objectionWords(o.kind)}</span>
                {o.about.trim() !== "" ? <span className="mg-unsure__about">{o.about}</span> : null}
                {o.fix.trim() !== "" ? (
                  <span className="mg-unsure__fix">
                    {TO_PASS} {o.fix}
                  </span>
                ) : null}
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {d.changed.trim() !== "" ? (
        <section className="mg-sec mg-changed" aria-label={CHANGED_HEADING}>
          <h2 className="mg__h2">{CHANGED_HEADING}</h2>
          <p className="mg__prose">{d.changed}</p>
        </section>
      ) : null}

      {d.facts.length > 0 ? (
        <section className="mg-sec" aria-label={GLANCE_HEADING}>
          <h2 className="mg__h2">{GLANCE_HEADING}</h2>
          <dl className="mg-facts">
            {d.facts.map((fact) => (
              <div key={`${fact.label}-${fact.value}`} className="mg-facts__row">
                <dt>{fact.label}</dt>
                <dd>{fact.value}</dd>
              </div>
            ))}
          </dl>
        </section>
      ) : null}

      {written ? (
        <div className="mg-doc__body">
          <MarkdownText source={markdown} headings="report" idPrefix={prefix} cite={cite} />
          <SourcesRow sources={numbered.sources} />
        </div>
      ) : (
        <p className="mg__note">{STILL_MAKING}</p>
      )}

      {rejecting ? <RejectSheet verb={rejecting} variant={variant} onClose={() => setRejecting(null)} onReject={reject} /> : null}
    </article>
  );
}
