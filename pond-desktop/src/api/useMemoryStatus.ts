// useMemoryStatus: GET /api/v1/models/memory-status once; null on any error, never throws.

import { useState, useEffect } from "react";
import { api } from "./PondApiClient";
import type { ModelMemoryStatus } from "./types";

export function useMemoryStatus(): ModelMemoryStatus | null {
  const [status, setStatus] = useState<ModelMemoryStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .getMemoryStatus()
      .then((s) => {
        if (!cancelled) setStatus(s);
      })
      .catch(() => {
        /* non-fatal — no budget means no verdict (graceful degradation) */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return status;
}
