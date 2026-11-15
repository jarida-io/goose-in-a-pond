// Whether phones can reach this Pond away from home, from the two ways in.

import type { NgrokTunnelState } from "../../api/types";

export type Reach = "on" | "connecting" | "off" | "problem" | "unknown";

export interface RemoteRead {
  reach: Reach;
  /** How phones come in, when they can or are about to. */
  route: "ngrok" | "built-in" | null;
  /** The tunnel's public address, `host:port`, while it runs. */
  address: string | null;
}

/**
 * One answer from the two ways in. A running or starting ngrok tunnel replaces the built-in
 * network, so it is asked first. Either read may fail (null): only both failing is unknown.
 */
export function readReach(ngrok: NgrokTunnelState | null, builtIn: { state: string } | null): RemoteRead {
  if (ngrok?.config.enabled) {
    const s = ngrok.status;
    if (s.state === "running") return { reach: "on", route: "ngrok", address: s.address };
    if (s.state === "starting") return { reach: "connecting", route: "ngrok", address: null };
    if (s.state === "failed") return { reach: "problem", route: "ngrok", address: null };
  }
  if (builtIn) {
    // The coordinator's own vocabulary: 'Running', 'Stopped', or a step on the way to running.
    if (builtIn.state === "Running") return { reach: "on", route: "built-in", address: null };
    if (builtIn.state === "Stopped") return { reach: "off", route: null, address: null };
    return { reach: "connecting", route: "built-in", address: null };
  }
  if (ngrok) return { reach: "off", route: null, address: null };
  return { reach: "unknown", route: null, address: null };
}
