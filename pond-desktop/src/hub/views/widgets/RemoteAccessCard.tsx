// A glance at whether away-from-home access is on, and whether a device recovery request is
// waiting on a person. Own fetch, like TonightCard: this is a widget-sized slice of
// sections/RemoteAccess.tsx's state, not a HomeData concern the rest of the app shares.

import { useEffect, useState, type ReactElement } from "react";
import { api } from "../../../api/PondApiClient";
import type { GuiSection } from "../../../desktopState";
import "./remote-access-card.css";

export interface RemoteAccessCardProps {
  onNavigate: (section: GuiSection) => void;
}

type Status = "on" | "off" | "unknown";

/** `remoteStatus().state` is the coordinator's own vocabulary ('Running', 'Stopped', or an
    admission refusal/'connecting'/'provision' string); the widget only has room for three. */
function statusFor(state: string): Status {
  if (state === "Running") return "on";
  if (state === "Stopped") return "off";
  return "unknown";
}

export function RemoteAccessCard({ onNavigate }: RemoteAccessCardProps): ReactElement {
  const [status, setStatus] = useState<Status | null>(null);
  const [pending, setPending] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .remoteStatus()
      .then((s) => {
        if (!cancelled) setStatus(statusFor(s.state));
      })
      .catch(() => {
        if (!cancelled) setStatus("unknown");
      });
    api
      .remoteRecoveryRequests()
      .then((list) => {
        if (!cancelled) setPending(list.filter((r) => !r.approved).length);
      })
      .catch(() => {
        // Quiet, like the status fetch above: a failed count isn't a claim that nothing is waiting.
        if (!cancelled) setPending(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (status === null) {
    return (
      <div className="rac" data-hook="remote-access" data-state="loading">
        <span className="rac__eyebrow">Remote Access</span>
        <span className="rac__quiet">Checking remote access</span>
      </div>
    );
  }

  return (
    <div className="rac" data-hook="remote-access" data-state={status}>
      <span className="rac__eyebrow">Remote Access</span>

      <div className="rac__status">
        <span className={`rac__dot rac__dot--${status}`} aria-hidden="true" />
        <span className="rac__status-label">
          {status === "on" ? "On" : status === "off" ? "Off" : "Status unknown"}
        </span>
      </div>

      {pending !== null && pending > 0 && (
        <div className="rac__pending">
          <span className="rac__pending-kind">Needs review</span>
          <span className="rac__pending-count">
            {pending === 1 ? "1 recovery request waiting" : `${pending} recovery requests waiting`}
          </span>
        </div>
      )}

      <button type="button" className="rac__link" onClick={() => onNavigate("pairing")}>
        Manage access
      </button>
    </div>
  );
}
