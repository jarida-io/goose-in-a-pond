// Whether phones can reach this Pond away from home, drawn as the path between the two: lit when
// they can, broken when they cannot, and named by the way they come in (the household's ngrok
// tunnel or the built-in network). Also whether a device recovery request is waiting on a person.
// Own fetch, like TonightCard: a widget-sized slice of sections/RemoteProvider.tsx's state.

import { useEffect, useState, type ReactElement } from "react";
import { api } from "../../../api/PondApiClient";
import type { GuiSection } from "../../../desktopState";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import { readReach, type Reach, type RemoteRead } from "../../state/remoteReach";
import "./remote-access-card.css";

export interface RemoteAccessCardProps {
  onNavigate: (section: GuiSection) => void;
}

const HEADLINE: Record<Reach, string> = {
  on: "Reachable away from home",
  connecting: "Opening the way in",
  off: "Home only",
  problem: "The way in is down",
  unknown: "Can't tell from here",
};

function routeLine(read: RemoteRead): string {
  if (read.reach === "unknown") return "Open remote access to see how phones get in.";
  if (read.reach === "off") return "Phones reach this Pond on your home Wi-Fi only.";
  if (read.route === "ngrok") {
    return read.reach === "problem" ? "Your ngrok tunnel stopped. Open remote access to see why." : "Through your ngrok account";
  }
  return "Through the built-in remote network";
}

export function RemoteAccessCard({ onNavigate }: RemoteAccessCardProps): ReactElement {
  const [read, setRead] = useState<RemoteRead | null>(null);
  const [pending, setPending] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    void Promise.allSettled([api.ngrokTunnel(), api.remoteStatus()]).then(([ngrok, builtIn]) => {
      if (cancelled) return;
      setRead(
        readReach(
          ngrok.status === "fulfilled" ? ngrok.value : null,
          builtIn.status === "fulfilled" ? builtIn.value : null,
        ),
      );
    });
    api
      .remoteRecoveryRequests()
      .then((list) => {
        if (!cancelled) setPending(list.filter((r) => !r.approved).length);
      })
      .catch(() => {
        // A failed count isn't a claim that nothing is waiting.
        if (!cancelled) setPending(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const link = (
    <button type="button" className="rac__link" onClick={() => onNavigate("pairing")}>
      Manage access
    </button>
  );

  if (read === null) {
    return (
      <div className="rac" data-hook="remote-access" data-state="loading">
        <span className="rac__eyebrow">Remote access</span>
        <span className="rac__quiet">Checking remote access</span>
      </div>
    );
  }

  return (
    <div className="rac" data-hook="remote-access" data-state={read.reach}>
      <span className="rac__glow" aria-hidden="true" />
      <span className="rac__eyebrow">Remote access</span>

      <div className="rac__path" aria-hidden="true">
        <span className="rac__end">
          <HubIco d={HP_PATHS.home} size={20} color="currentColor" sw={1.9} />
        </span>
        <span className="rac__wire">
          <span className="rac__spark" />
        </span>
        <span className="rac__end">
          <HubIco d={HP_PATHS.phone} size={20} color="currentColor" sw={1.9} />
        </span>
      </div>

      <div className="rac__words">
        <span className="rac__headline" role="status">
          {HEADLINE[read.reach]}
        </span>
        <span className="rac__route">{routeLine(read)}</span>
        {read.address && <span className="rac__address">{read.address}</span>}
      </div>

      {pending !== null && pending > 0 && (
        <div className="rac__pending">
          <span className="rac__pending-kind">Needs review</span>
          <span className="rac__pending-count">
            {pending === 1 ? "1 recovery request waiting" : `${pending} recovery requests waiting`}
          </span>
        </div>
      )}

      {link}
    </div>
  );
}
