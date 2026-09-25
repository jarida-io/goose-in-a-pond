// ────────────────────────────────────────────────────────────
// useVisionStatus — live view of picture support for the active chat model.
//
// Modelled on `useWarmupStatus`: try/catch around the call (a missing mock in
// a test, or an older server without this route, must degrade to "unknown"
// rather than throw), a cancelled flag, and the timer cleared on unmount. It
// differs in three ways that useWarmupStatus does not need. It never settles
// into "stop polling" — an absent encoder can start downloading at any time
// from outside this tab (another window, the boot prewarm), so it polls for
// the life of the component, fast while something is actually changing and
// slow otherwise. It exposes `refresh()`, because a model switch or a
// restored 409 both need an answer sooner than the next tick. And it only
// polls while the document is visible, since a backgrounded tab gains nothing
// from tracking a multi-hundred-megabyte download tick by tick.
// ────────────────────────────────────────────────────────────

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./PondApiClient";
import type { VisionStatus } from "./types";

const FAST_POLL_MS = 2_000;
const SLOW_POLL_MS = 30_000;
/** Everything this client understands. A reply naming a kind outside this
 *  set is treated as unknown (fail open — no line, attach stays reachable),
 *  which is what keeps a server ahead of this client from breaking attach. */
const KNOWN_KINDS = new Set([
  "unknown",
  "not_declared",
  "not_on_this_device",
  "absent",
  "verifying",
  "downloading",
  "ready",
  "failed",
  "blocked",
]);
/** Poll fast while something is actually in motion; slow otherwise. */
const FAST_KINDS = new Set(["absent", "downloading", "verifying"]);

export interface UseVisionStatus {
  status: VisionStatus | null;
  /** Ask again right now, outside the normal cadence — after a model switch
   *  (including to/from mesh) or a restored 409. */
  refresh: () => void;
}

export function useVisionStatus(): UseVisionStatus {
  const [status, setStatus] = useState<VisionStatus | null>(null);
  const cancelledRef = useRef(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const scheduleRef = useRef<(s: VisionStatus | null) => void>(() => {});
  const tickRef = useRef<() => void>(() => {});

  scheduleRef.current = (s: VisionStatus | null) => {
    if (cancelledRef.current) return;
    if (timerRef.current) clearTimeout(timerRef.current);
    const delay = s && FAST_KINDS.has(s.state.kind) ? FAST_POLL_MS : SLOW_POLL_MS;
    timerRef.current = setTimeout(() => {
      // A backgrounded tab gets no polling cost; re-check on the same cadence
      // rather than subscribing to visibilitychange, which would need its own
      // teardown and buys nothing a tab this idle needs sooner.
      if (typeof document !== "undefined" && document.visibilityState === "hidden") {
        scheduleRef.current(s);
        return;
      }
      tickRef.current();
    }, delay);
  };

  tickRef.current = () => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    // Partial api mocks (tests) and older clients may lack the method; a
    // synchronous throw here must degrade the same as a failed request.
    let call: Promise<VisionStatus>;
    try {
      call = api.getVisionStatus();
    } catch {
      if (!cancelledRef.current) setStatus(null);
      return;
    }
    call
      .then((s) => {
        if (cancelledRef.current) return;
        // request<T> can hand back undefined or an HTML shell on a broken
        // route, and the models/** E2E catch-all answers with {status:"ok"}
        // — guard the shape before trusting it.
        if (!s || typeof s !== "object" || !s.state || !KNOWN_KINDS.has(s.state.kind)) {
          setStatus(null);
          scheduleRef.current(null);
          return;
        }
        setStatus(s);
        scheduleRef.current(s);
      })
      .catch(() => {
        if (cancelledRef.current) return;
        setStatus(null);
        scheduleRef.current(null);
      });
  };

  const refresh = useCallback(() => {
    tickRef.current();
  }, []);

  useEffect(() => {
    cancelledRef.current = false;
    tickRef.current();
    return () => {
      cancelledRef.current = true;
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  return { status, refresh };
}
