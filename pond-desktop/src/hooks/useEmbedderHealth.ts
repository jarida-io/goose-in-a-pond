// What the pond says about its Memory embedder and how much of the store it can reach. Polls only
// while the embedder is still moving toward ready, so a settled pond costs no requests.

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import type { ContextIndexHealth } from "../api/types";
import { embedderWatching, isFetching, resolvedMemoryName } from "../sections/models/memoryEmbedder";

const POLL_MS = 3_000;
/** After this many looks a switch that has not finished is slow, not fast, so ask less often. */
const SLOW_AFTER = 40;
const SLOW_POLL_MS = 15_000;

async function fileIsComingDown(): Promise<boolean> {
  try {
    const { downloads } = await Promise.resolve().then(() => api.getDownloadProgress());
    return (downloads ?? []).some(
      (d) => d.category === "embedding" && d.status !== "done" && d.status !== "cancelled",
    );
  } catch {
    return false;
  }
}

export interface UseEmbedderHealth {
  health: ContextIndexHealth | null;
  /** The first read has not come back. */
  loading: boolean;
  error: string | null;
  /** The pond's file for the chosen model is coming down, so a missing file is progress. */
  downloading: boolean;
  refresh: () => Promise<void>;
  /** Asks the pond to load the chosen model again, keeping every vector it has; rejects with the pond's words. */
  retry: () => Promise<void>;
  retrying: boolean;
}

/** `onWatch` runs before each look while the embedder is still switching, e.g. to see a download arrive. */
export function useEmbedderHealth(opts: { onWatch?: () => void } = {}): UseEmbedderHealth {
  const [health, setHealth] = useState<ContextIndexHealth | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [retrying, setRetrying] = useState(false);

  const alive = useRef(true);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const looks = useRef(0);
  const onWatch = useRef(opts.onWatch);
  onWatch.current = opts.onWatch;

  const read = useCallback(async (): Promise<ContextIndexHealth | null> => {
    try {
      // A partial `api` (a test double) throws synchronously, which reads like a failed request.
      const next = await Promise.resolve().then(() => api.getContextIndexHealth());
      // The index says only that the file is missing; the tracker says whether it is on its way.
      const fetching = isFetching(next?.embedder?.reason) ? await fileIsComingDown() : false;
      if (alive.current) {
        setHealth(next);
        setDownloading(fetching);
        setError(null);
      }
      return next;
    } catch (e) {
      if (alive.current) setError(String(e));
      return null;
    } finally {
      if (alive.current) setLoading(false);
    }
  }, []);

  const watch = useCallback(
    (latest: ContextIndexHealth | null) => {
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
      if (!alive.current || !embedderWatching(latest)) {
        looks.current = 0;
        return;
      }
      const wait = looks.current < SLOW_AFTER ? POLL_MS : SLOW_POLL_MS;
      looks.current += 1;
      timer.current = setTimeout(async () => {
        timer.current = null;
        if (typeof document !== "undefined" && document.visibilityState === "hidden") {
          watch(latest);
          return;
        }
        onWatch.current?.();
        watch(await read());
      }, wait);
    },
    [read],
  );

  const refresh = useCallback(async () => {
    looks.current = 0;
    watch(await read());
  }, [read, watch]);

  const retry = useCallback(async () => {
    setRetrying(true);
    try {
      // The same assignment again: a failed load of the model already chosen is asked for once more.
      const name = resolvedMemoryName(await api.getActiveRoles());
      if (name) await api.activateModel("embedding", name, "embedding");
      await refresh();
    } finally {
      if (alive.current) setRetrying(false);
    }
  }, [refresh]);

  useEffect(() => {
    alive.current = true;
    void refresh();
    return () => {
      alive.current = false;
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
    };
    // Once, on mount: the callbacks are stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { health, loading, error, downloading, refresh, retry, retrying };
}
