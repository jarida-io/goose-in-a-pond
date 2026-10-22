// Plan §7's optional widget, built on request: a glance at what's left to run before midnight,
// same data HomeAlerts.tsx's routine banner reads (state/routines.ts), one rule for "due tonight"
// shared between the two rather than each guessing at its own definition.

import { useEffect, useState, type ReactElement } from "react";
import { api } from "../../../api/PondApiClient";
import { dueTonight } from "../../state/routines";
import type { Schedule } from "../../../api/types";
import type { GuiSection } from "../../../desktopState";
import "./tonight-card.css";

export interface TonightCardProps {
  onNavigate: (section: GuiSection) => void;
}

/** "10:08 PM", matching HomeStatusBar's own clock format. */
function formatTime(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function TonightCard({ onNavigate }: TonightCardProps): ReactElement {
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .listSchedules()
      .then((list) => {
        if (!cancelled) setSchedules(list);
      })
      .catch(() => {
        // Quiet like HomeAlerts.tsx's own fetch: a widget that failed to load is not a claim
        // that nothing is due, so it shows the loading copy rather than a false "nothing left".
        if (!cancelled) setSchedules(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (schedules === null) {
    return (
      <div className="tonight" data-hook="tonight" data-state="loading">
        <span className="tonight__eyebrow">Tonight</span>
        <span className="tonight__quiet">Checking what's due tonight</span>
      </div>
    );
  }

  const due = dueTonight(schedules);

  if (due.length === 0) {
    return (
      <div className="tonight" data-hook="tonight" data-state="empty">
        <span className="tonight__eyebrow">Tonight</span>
        <span className="tonight__quiet">Nothing left to run tonight</span>
        <button type="button" className="tonight__link" onClick={() => onNavigate("schedules")}>
          View all routines
        </button>
      </div>
    );
  }

  const [next, ...rest] = due;

  return (
    <div className="tonight" data-hook="tonight" data-state="ready">
      <span className="tonight__eyebrow">Tonight</span>
      <div className="tonight__count">
        <span className="tonight__number">{due.length}</span>
        <span className="tonight__count-label">
          {due.length === 1 ? "routine before midnight" : "routines before midnight"}
        </span>
      </div>

      <div className="tonight__next">
        <span className="tonight__next-kind">Up next</span>
        <span className="tonight__next-name">{next.label ?? next.name}</span>
        <span className="tonight__next-time">{formatTime(next.next_run as string)}</span>
      </div>

      {rest.length > 0 && (
        <p className="tonight__more">
          {rest.length === 1 ? "1 more after that." : `${rest.length} more after that.`}
        </p>
      )}

      <button type="button" className="tonight__link" onClick={() => onNavigate("schedules")}>
        View all routines
      </button>
    </div>
  );
}
