// Whether the pond is set to reach nothing beyond the house. The app has no connectivity probe, only
// the `network_mode` setting, so that is what a download button can honestly check.

import { useEffect, useState } from "react";
import { api } from "../api/PondApiClient";

/** An unread or failed setting counts as online: a download is never blocked on a guess. */
export function useOffline(): boolean {
  const [offline, setOffline] = useState(false);

  useEffect(() => {
    let alive = true;
    // A partial `api` (a test double) throws synchronously, which reads like a failed request.
    Promise.resolve()
      .then(() => api.getSettings())
      .then((s) => {
        if (alive) setOffline(s?.network_mode === "offline");
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  return offline;
}
