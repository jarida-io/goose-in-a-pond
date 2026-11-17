// The job an anchor is running, as its steps: each one's plain label and where it is.

import { useId } from "react";
import { InkTimeline } from "@jarida/ink/react";
import type { EngineJobSummary } from "../api/types";
import { STARTING, WAITING_TURN, WORKING_HEADING, roundsWords, stepMarker, stepWords } from "./myGooseWords";

export function JobPipeline({ job }: { job: EngineJobSummary }) {
  const headingId = useId();
  const steps = job.steps ?? [];
  const rounds = roundsWords(job.rounds);
  return (
    <section className="mg-sec mg-job" aria-labelledby={headingId}>
      <h2 id={headingId} className="mg__h2">
        {WORKING_HEADING}
      </h2>
      <p className="mg__note">
        {job.subject}
        {rounds ? `. ${rounds} so far.` : null}
      </p>
      {steps.length === 0 ? (
        <p className="mg__note">{job.state === "queued" ? WAITING_TURN : STARTING}</p>
      ) : (
        <InkTimeline
          label={WORKING_HEADING}
          entries={steps.map((step) => ({
            key: step.key,
            title: step.label,
            description: stepWords(step.state),
            state: stepMarker(step.state),
          }))}
        />
      )}
    </section>
  );
}
