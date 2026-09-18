// ────────────────────────────────────────────────────────────
// Background jobs — what the pond does when nobody is talking to it, and a way
// to stop waiting for it.
//
// Rendered by BOTH settings surfaces, from one file, for the same reason Home
// is: two copies would drift into two answers about what the pond is doing.
//
// WHY THIS SCREEN EXISTS. Six jobs share one inference slot, and the lane that
// hands it out could not explain itself: every refusal logs at `trace!` and
// only a success logs at `debug!`, so from outside the process a job that was
// eligible and losing a tie-break looked exactly like one that was switched
// off. On a real pond that produced a memory engine which had never completed a
// single pass -- 958 conversations read, zero cursors, zero attempts -- with
// nothing anywhere saying why.
//
// The cause was a queue nobody could see. `select_next` is least-recently-run
// with declaration order as the tie-break, and a job that has never run skips
// the interval-floor check entirely, so several never-run jobs drain only as
// fast as each one's OWN poll: 60s, then five minutes, then fifteen. The
// last-run map is per-process, so the queue restarts from the top on every
// boot. A pond restarted more often than the queue drains never reaches the
// end, and the job declared last is the one that never runs.
//
// So the screen does two things: it says what each job is waiting for, and it
// lets a person put one at the front.
//
// RUN NOW WAKES; IT DOES NOT RUN. The request returns as soon as the job's loop
// has been asked to take its next tick. The work happens in that loop, under
// the same single slot every scheduled pass takes -- pressing this while
// another job holds the machine queues behind it rather than decoding beside
// it. That is why the button's own copy says "Asked" and not "Done", and why
// the row is what reports the outcome afterwards.
// ────────────────────────────────────────────────────────────

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import type { LaneJobStatus, LaneStatus } from "../api/types";
import "./background-jobs.css";

/**
 * How often the watcher asks what is running.
 *
 * Five seconds, matching `hub/views/settings/Logs.tsx`, which is the existing
 * precedent for a detail panel that refreshes while it is on screen. Anything
 * under two buys nothing: the lane's own gates move on 60s, 5-minute and
 * 15-minute cadences, so a faster tick reports the same row again. The read is
 * cheap -- two in-memory locks and no database -- but this runs on a panel that
 * may be left open for days on a six-core board, which is why it stops when
 * nobody is looking.
 */
const POLL_MS = 5_000;

/**
 * How long each job waited, in the household's words.
 *
 * `null` is "never", and it is the most important value on this screen rather
 * than a missing one -- a job reading "hasn't run yet" on a pond that has been
 * up for hours is the whole symptom.
 */
export function describeLastRun(secs: number | null): string {
  if (secs === null) return "Hasn't run yet";
  if (secs < 90) return "Ran just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `Ran ${mins} minutes ago`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `Ran ${hours} ${hours === 1 ? "hour" : "hours"} ago`;
  return `Ran ${Math.round(hours / 24)} days ago`;
}

/**
 * What a job is waiting for, said the way a person would ask it.
 *
 * Every branch names a condition the household can act on -- talk to it, leave
 * it alone, turn it on -- rather than repeating the gate's own vocabulary.
 * `still_active` in particular: "the house is busy" is a fact about them, not
 * about a scheduler, and it is the one that most often explains a quiet pond.
 *
 * An unrecognised reason is passed through rather than swallowed. A new gate
 * added on the server would otherwise render as "Ready" on every older app,
 * which is the failure this screen exists to end.
 */
export function describeWait(job: LaneJobStatus): string {
  if (!job.present) return "Not running on this pond";
  if (!job.registered) return "Starting up";
  if (job.would_run_next) return "Next to run";
  switch (job.blocked_by) {
    case null:
      return "Ready";
    case "disabled":
      return "Turned off in settings";
    case "no_activity_since_start":
      return "Waiting for you to say something first";
    case "still_active":
      return "Waiting for the house to be quiet";
    case "interval_floor":
      return "Ran recently, waiting its turn again";
    default:
      return `Waiting: ${job.blocked_by}`;
  }
}

/**
 * How long the running job has held the slot, as a clause or nothing at all.
 *
 * A pass four minutes in and one that started two seconds ago read identically
 * without it — and on a small board the difference is whether something is
 * stuck. Omitted below a few seconds rather than saying "for 0 seconds", which
 * is noise on a line that reprints every five.
 */
export function describeElapsed(secs: number | null | undefined): string {
  if (secs === null || secs === undefined || secs < 3) return "";
  if (secs < 60) return `, for ${secs} seconds`;
  const mins = Math.round(secs / 60);
  return `, for ${mins} ${mins === 1 ? "minute" : "minutes"}`;
}

/**
 * What has happened to a job since the pond started, in one clause.
 *
 * The row above it says what the job is waiting for RIGHT NOW, which cannot
 * distinguish a job that is eligible and losing every tie-break from one about
 * to run — both report nothing blocking them. This is the sentence that can.
 *
 * Returns "" when there is nothing to say: a job that has never been picked
 * and never lost has no history worth a line, and a row that always carries a
 * clause trains people to stop reading it.
 */
export function describeHistory(job: LaneJobStatus): string {
  const parts: string[] = [];
  if (job.granted) parts.push(`ran ${job.granted}×`);
  // The one number that names a defect rather than a state: it is waiting its
  // turn and never getting it.
  if (job.lost_to_total) {
    const most = job.lost_to_most;
    parts.push(
      most
        ? `waited behind ${most.job.replace(/_/g, " ")} ${job.lost_to_total}×`
        : `waited its turn ${job.lost_to_total}×`,
    );
  }
  if (job.slot_busy) parts.push(`found the pond busy ${job.slot_busy}×`);
  return parts.length ? ` Since starting: ${parts.join(", ")}.` : "";
}

/** Per-row transient state. One row can be asked while another is idle. */
type RowState = "idle" | "asking" | "asked" | "nothing" | "failed";

export interface BackgroundJobsProps {
  /** Renders as a plain block instead of a titled card. The hub panel has its own heading. */
  bare?: boolean;
}

export function BackgroundJobs({ bare = false }: BackgroundJobsProps) {
  const [status, setStatus] = useState<LaneStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<Record<string, RowState>>({});

  // One request at a time. Without this a stalled pond stacks a new tick every
  // five seconds until the client's own 30s abort fires, so a slow answer
  // becomes six outstanding requests.
  const inFlight = useRef(false);

  /**
   * Read the lane.
   *
   * `silent` is what makes polling bearable: a tick that fails must not blank
   * the panel or raise an error over the last good answer. A watcher that
   * flickers to "could not read" every time one request misses is worse than
   * one showing a five-second-old truth, and the household cannot tell the two
   * apart anyway. Only the first load and an explicit Run now speak up.
   */
  const load = useCallback(async (silent = false) => {
    if (inFlight.current) return;
    inFlight.current = true;
    try {
      const next = await api.laneStatus();
      // `request<T>` can hand back `undefined` or a parsed `index.html` when
      // the server is starting or a proxy is in the way. Guarded at the call
      // site because a polled call meets that state far more often than a
      // one-shot one does.
      if (!next || typeof next.lane !== "boolean") return;
      setStatus(next);
      setError(null);
    } catch (e) {
      if (silent) return;
      // The list is the screen. Without it there is nothing to draw and a
      // silent empty panel would read as "no background jobs", which is the
      // one thing it must never say by accident.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
    }
  }, []);

  useEffect(() => {
    void load();

    // Skipped while the panel is hidden, and caught up the moment it is not.
    // A kitchen panel is left open for days; ticking behind a locked screen
    // spends the board's cores on a question nobody is asking. Unmounting
    // already covers navigating away -- this covers the window being hidden
    // with the panel still mounted.
    const tick = () => {
      if (document.visibilityState !== "visible") return;
      void load(true);
    };
    const id = setInterval(tick, POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void load(true);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [load]);

  const run = useCallback(
    async (job: string) => {
      setRows((r) => ({ ...r, [job]: "asking" }));
      try {
        const result = await api.runLaneJob(job);
        setRows((r) => ({ ...r, [job]: result.woken ? "asked" : "nothing" }));
      } catch (e) {
        setRows((r) => ({ ...r, [job]: "failed" }));
        setError(e instanceof Error ? e.message : String(e));
        return;
      }
      // Re-read rather than patch the row from the response. The response says
      // the doorbell rang; only the lane can say what happened next, and by the
      // time a person looks it usually has.
      await load();
    },
    [load],
  );

  const body = (
    <div className="bgjobs">
      {/* `status`, not `alert`. A background-jobs read that failed is worth
          saying and is not worth interrupting for -- and this panel shares a
          page with the settings error banner, which IS an alert. Two alerts on
          one screen is how the urgent one stops being urgent. */}
      {error !== null && (
        <p className="bgjobs__error" role="status">
          Could not read the background jobs. {error}
        </p>
      )}

      {status !== null && !status.lane && (
        <p className="bgjobs__note">
          This pond is not running the background jobs — nothing here schedules them.
        </p>
      )}

      {status?.lane && (
        <>
          {/* The watcher. `slot_busy` alone could only ever say something was
              running; this says what, and for how long — which is the
              difference between "the pond is busy" and "the memory engine is
              reading your conversations", and the second is what tells somebody
              whether to wait or to go and turn something off.

              `aria-live="polite"` so a screen reader hears a job start and
              finish without being interrupted mid-sentence by a five-second
              tick. */}
          <p className="bgjobs__now" data-running={status.running ? "" : undefined} aria-live="polite">
            {status.running
              ? `Running now: ${status.running_title ?? status.running}${describeElapsed(status.running_for_secs)}`
              : "Nothing is running. Jobs take turns, one at a time."}
          </p>

          <ul className="bgjobs__list">
            {status.jobs.map((job) => (
              <li key={job.job} className="bgjobs__row" data-present={job.present || undefined}>
                <div className="bgjobs__text">
                  <span className="bgjobs__title">{job.title}</span>
                  {/* Two sentences, not a metadata strip joined by a middle
                      dot. They are independent facts -- what it is waiting for,
                      and when it last managed to run -- and the second is the
                      one that makes the first mean something: "waiting for the
                      house to be quiet" reads very differently under "ran 5
                      minutes ago" than under "hasn't run yet". */}
                  <span className="bgjobs__wait">
                    {describeWait(job)}. {describeLastRun(job.since_last_run_secs)}.
                    {describeHistory(job)}
                  </span>
                </div>
                <div className="bgjobs__act">
                  <button
                    type="button"
                    className="bgjobs__run"
                    // A job with no loop in this process has nothing to ring.
                    // Disabled rather than hidden: the row still says the job
                    // exists, which is what a household troubleshooting a pond
                    // with no embedder needs to see.
                    disabled={!job.present || rows[job.job] === "asking"}
                    onClick={() => void run(job.job)}
                  >
                    {rows[job.job] === "asking" ? "Asking…" : "Run now"}
                  </button>
                  {rows[job.job] === "asked" && (
                    <span className="bgjobs__said" role="status">
                      Asked — it runs at its next turn
                    </span>
                  )}
                  {rows[job.job] === "nothing" && (
                    <span className="bgjobs__said" role="status">
                      Nothing here to run
                    </span>
                  )}
                  {rows[job.job] === "failed" && (
                    <span className="bgjobs__said" role="status">
                      That didn't send
                    </span>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );

  if (bare) return body;
  return (
    <section className="bgjobs__card">
      <h3 className="bgjobs__heading">Background jobs</h3>
      <p className="bgjobs__sub">
        What the pond does while nobody is talking to it. They share one slot, so they
        take turns.
      </p>
      {body}
    </section>
  );
}
