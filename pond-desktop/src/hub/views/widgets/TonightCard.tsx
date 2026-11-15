// What is left to run before midnight, on the night sky it runs under: the next routine and how
// soon, and the rest of the evening as marks on a line from now to midnight. Same rule for "due
// tonight" as HomeAlerts.tsx's routine banner (state/routines.ts), so the two never disagree.

import { useEffect, useState, type ReactElement } from "react";
import { api } from "../../../api/PondApiClient";
import { dueTonight } from "../../state/routines";
import { placeOnEvening, untilText } from "../../state/evening";
import { useNow } from "../../state/useNow";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
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

function Shell({ state, children }: { state: string; children: ReactElement | ReactElement[] }): ReactElement {
  return (
    <div className="tonight" data-hook="tonight" data-state={state}>
      <span className="tonight__stars" aria-hidden="true" />
      <span className="tonight__eyebrow">
        <HubIco d={HP_PATHS.moon} size={14} color="currentColor" sw={2} />
        Tonight
      </span>
      {children}
    </div>
  );
}

export function TonightCard({ onNavigate }: TonightCardProps): ReactElement {
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);
  // Ticks, so the countdown and the line move on a dashboard nobody reloads.
  const now = useNow().getTime();

  useEffect(() => {
    let cancelled = false;
    api
      .listSchedules()
      .then((list) => {
        if (!cancelled) setSchedules(list);
      })
      .catch(() => {
        // A widget that failed to load is not a claim that nothing is due, so it keeps the
        // loading copy rather than a false "nothing left".
        if (!cancelled) setSchedules(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const link = (
    <button type="button" className="tonight__link" onClick={() => onNavigate("schedules")}>
      View all routines
    </button>
  );

  if (schedules === null) {
    return (
      <Shell state="loading">
        <span className="tonight__quiet">Checking what's due tonight</span>
      </Shell>
    );
  }

  const due = dueTonight(schedules, now);

  if (due.length === 0) {
    return (
      <Shell state="empty">
        <span className="tonight__headline">A quiet night</span>
        <span className="tonight__quiet">Nothing left to run before midnight.</span>
        <div className="tonight__line" aria-hidden="true">
          <span className="tonight__track" />
        </div>
        {link}
      </Shell>
    );
  }

  const [next, ...rest] = due;
  const nextAt = new Date(next.next_run as string).getTime();

  return (
    <Shell state="ready">
      <div className="tonight__next">
        <span className="tonight__until">{untilText(nextAt - now)}</span>
        <span className="tonight__next-name">{next.label ?? next.name}</span>
        <span className="tonight__next-time">{formatTime(next.next_run as string)}</span>
      </div>

      <div
        className="tonight__line"
        role="img"
        aria-label={
          due.length === 1
            ? "1 routine before midnight"
            : `${due.length} routines before midnight, the next ${untilText(nextAt - now)}`
        }
      >
        <span className="tonight__track" />
        {due.map((s, i) => (
          <span
            key={s.id}
            className="tonight__mark"
            data-next={i === 0 || undefined}
            style={{ left: `${placeOnEvening(new Date(s.next_run as string).getTime(), now)}%` }}
          />
        ))}
        <span className="tonight__ends">
          <span>Now</span>
          <span>Midnight</span>
        </span>
      </div>

      <p className="tonight__more">
        {rest.length === 0
          ? "The last routine before midnight."
          : rest.length === 1
            ? "1 more after that."
            : `${rest.length} more after that.`}
      </p>

      {link}
    </Shell>
  );
}
