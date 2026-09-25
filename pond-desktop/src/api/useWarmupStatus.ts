// useWarmupStatus: polls GET /api/v1/warmup while "warming"; null on any error, never throws.

import { useState, useEffect } from "react";
import { api } from "./PondApiClient";
import type { WarmupStatus } from "./types";

const POLL_MS = 700;

export function useWarmupStatus(): WarmupStatus | null {
  const [status, setStatus] = useState<WarmupStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const tick = () => {
      // Clients lacking the method (partial test mocks) throw synchronously; degrade the same way.
      let call: Promise<import("./types").WarmupStatus>;
      try {
        call = api.getWarmupStatus();
      } catch {
        setStatus(null);
        return;
      }
      call
        .then((s) => {
          if (cancelled) return;
          // request<T> may return undefined or an HTML shell on a broken route.
          if (!s || typeof s.state !== "string") {
            setStatus(null);
            return;
          }
          setStatus(s);
          if (s.state === "warming") timer = setTimeout(tick, POLL_MS);
        })
        .catch(() => {
          if (!cancelled) setStatus(null);
        });
    };
    tick();

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, []);

  return status;
}
