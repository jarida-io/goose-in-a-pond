// What Goose means to do (the heartbeat's waiting jobs, each withdrawable) and what it did lately
// (the engine log), all in plain words.

import { useId, useState } from "react";
import { X } from "lucide-react";
import { InkButton } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import type { EngineLogJob, Intention } from "../api/types";
import {
  ACTIVITY_HEADING,
  LOADING_ACTIVITY,
  LOG_HEADING,
  NOTHING_DONE,
  NOTHING_WAITING,
  WAITING_HEADING,
  WAITING_INTRO,
  WITHDRAW,
  WITHDRAWN,
  actWords,
  buttonSize,
  dayWords,
  intentionNote,
  jobKindWords,
  jobStateWords,
  roundsWords,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { useLoaded } from "./useLoaded";
import { ActionProblem, LoadProblem, Loading } from "./MyGooseParts";

/** The log moves while a job runs; a minute is close enough for a list read at leisure. */
const LOG_POLL_MS = 60_000;

interface Props {
  variant: Variant;
  say: (text: string) => void;
}

export function ActivityPanel({ variant, say }: Props) {
  return (
    <section className="mg-panel" aria-label={ACTIVITY_HEADING}>
      <WaitingSection variant={variant} say={say} />
      <LogSection variant={variant} />
    </section>
  );
}

function WaitingSection({ variant, say }: Props) {
  const intentions = useLoaded(() => api.listIntentions("waiting"));
  const headingId = useId();
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState<{ id: string; text: string } | null>(null);

  async function withdraw(intention: Intention) {
    setBusy(intention.id);
    setProblem(null);
    try {
      await api.withdrawIntention(intention.id);
      say(WITHDRAWN);
      if (intentions.state.status === "ready") {
        intentions.replace(intentions.state.value.filter((i) => i.id !== intention.id));
      }
    } catch (e) {
      setProblem({ id: intention.id, text: problemWords(e) });
    } finally {
      setBusy(null);
    }
  }

  const list = intentions.state.status === "ready" ? intentions.state.value : [];

  return (
    <section className="mg-sec" aria-labelledby={headingId}>
      <h2 id={headingId} className="mg__h2">
        {WAITING_HEADING}
      </h2>
      <p className="mg__intro">{WAITING_INTRO}</p>
      {intentions.state.status === "loading" ? <Loading>{LOADING_ACTIVITY}</Loading> : null}
      {intentions.state.status === "failed" ? (
        <LoadProblem problem={intentions.state.problem} onRetry={intentions.retry} variant={variant} />
      ) : null}
      {intentions.state.status === "ready" && list.length === 0 ? <p className="mg__note">{NOTHING_WAITING}</p> : null}
      {list.length > 0 ? (
        <ul className="mg-lines" aria-label={WAITING_HEADING}>
          {list.map((intention) => (
            <li key={intention.id} className="mg-line">
              <div className="mg-line__text">
                <span className="mg-line__fact">
                  {actWords(intention.act)}: {intention.text}
                </span>
                <span className="mg-line__note">{intentionNote(intention)}</span>
                {intention.why.trim() !== "" ? <span className="mg-line__note">{intention.why}</span> : null}
              </div>
              <div className="mg-line__actions">
                <InkButton
                  size={buttonSize(variant)}
                  icon={<X size={16} aria-hidden="true" />}
                  loading={busy === intention.id}
                  disabled={busy !== null}
                  onPress={() => void withdraw(intention)}
                >
                  {WITHDRAW}
                </InkButton>
              </div>
              <ActionProblem text={problem?.id === intention.id ? problem.text : null} />
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}

function jobNote(job: EngineLogJob): string {
  const parts = [jobKindWords(job.kind), jobStateWords(job.state)];
  const rounds = roundsWords(job.rounds);
  if (rounds) parts.push(rounds);
  const when = dayWords(job.finished_at ?? job.started_at ?? job.created_at);
  if (when) parts.push(when);
  return parts.join(" · ");
}

function LogSection({ variant }: { variant: Variant }) {
  const jobs = useLoaded(
    () => api.listEngineJobs(30),
    (all) => (all.some((j) => j.state === "running" || j.state === "queued") ? LOG_POLL_MS : null),
  );
  const headingId = useId();
  const list = jobs.state.status === "ready" ? jobs.state.value : [];

  return (
    <section className="mg-sec" aria-labelledby={headingId}>
      <h2 id={headingId} className="mg__h2">
        {LOG_HEADING}
      </h2>
      {jobs.state.status === "loading" ? <Loading>{LOADING_ACTIVITY}</Loading> : null}
      {jobs.state.status === "failed" ? <LoadProblem problem={jobs.state.problem} onRetry={jobs.retry} variant={variant} /> : null}
      {jobs.state.status === "ready" && list.length === 0 ? <p className="mg__note">{NOTHING_DONE}</p> : null}
      {list.length > 0 ? (
        <ul className="mg-lines" aria-label={LOG_HEADING}>
          {list.map((job) => (
            <li key={job.id} className="mg-line mg-line--log">
              <div className="mg-line__text">
                <span className="mg-line__fact">{job.subject}</span>
                <span className="mg-line__note">{jobNote(job)}</span>
                {job.outcome ? <span className="mg-line__outcome">{job.outcome}</span> : null}
              </div>
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}
