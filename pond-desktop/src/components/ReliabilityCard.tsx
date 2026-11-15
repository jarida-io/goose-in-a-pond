import { useEffect, useState } from "react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type { ReliabilityReport } from "../api/types";
import { RELIABILITY_DAYS, reliabilityLines } from "./reliabilityView";
import "./reliability-card.css";

type Load = { kind: "loading" } | { kind: "ready"; report: ReliabilityReport } | { kind: "locked" } | { kind: "failed" };

/** Whether notices arrived and messages were answered. Needs My Pond unlocked on a member's device. */
export function ReliabilityCard() {
  const [days, setDays] = useState<(typeof RELIABILITY_DAYS)[number]>(7);
  const [load, setLoad] = useState<Load>({ kind: "loading" });

  useEffect(() => {
    let live = true;
    setLoad({ kind: "loading" });
    (async () => {
      try {
        const report = await api.getReliability(days);
        if (live) setLoad({ kind: "ready", report });
      } catch (e) {
        if (live) setLoad(e instanceof ApiError && e.status === 403 ? { kind: "locked" } : { kind: "failed" });
      }
    })();
    return () => { live = false; };
  }, [days]);

  return (
    <section className="rel" aria-labelledby="rel-title">
      <header className="rel__head">
        <h2 className="rel__title" id="rel-title">Reliability</h2>
        <div className="rel__switch" role="group" aria-label="Period">
          {RELIABILITY_DAYS.map((d) => (
            <button key={d} type="button" className="rel__day" aria-pressed={days === d} onClick={() => setDays(d)}>
              {d} days
            </button>
          ))}
        </div>
      </header>
      {load.kind === "loading" && <p className="rel__quiet" role="status">Reading the last {days} days.</p>}
      {load.kind === "locked" && (
        <p className="rel__quiet" role="status">
          This is shown to a member whose My Pond is unlocked. Unlock it on your phone, then look again.
        </p>
      )}
      {load.kind === "failed" && <p className="rel__quiet" role="status">The pond could not read its record just now.</p>}
      {load.kind === "ready" && (
        <dl className="rel__list">
          {reliabilityLines(load.report).map((l) => (
            <div key={l.label} className="rel__row" data-problem={l.problem || undefined}>
              <dt>{l.label}</dt>
              <dd>{l.value}</dd>
            </div>
          ))}
        </dl>
      )}
    </section>
  );
}
