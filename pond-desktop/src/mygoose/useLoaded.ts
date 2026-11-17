import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import type { EngineJobSummary } from "../api/types";
import { isNotServed, isUnknownCaller } from "./problems";

export type Loaded<T> =
  | { status: "loading" }
  | { status: "failed"; problem: unknown }
  | { status: "ready"; value: T };

/** Why the whole of My Goose is closed here, rather than one panel. */
export type Refusal = "unknown_caller" | "not_served";

/** Tells the screen, so it shows one notice rather than an error per panel. */
export const RefusalContext = createContext<(why: Refusal) => void>(() => {});

/** A running job moves on its own, so it is asked after often; a queued one waits its turn. */
export const POLL_RUNNING_MS = 5_000;
export const POLL_QUEUED_MS = 30_000;

export function pollFor(jobs: ReadonlyArray<EngineJobSummary | null | undefined>): number | null {
  if (jobs.some((job) => job?.state === "running")) return POLL_RUNNING_MS;
  if (jobs.some((job) => job?.state === "queued")) return POLL_QUEUED_MS;
  return null;
}

export interface LoadedHandle<T> {
  state: Loaded<T>;
  /** Asks again without blanking what is shown. */
  reload: () => void;
  /** Shows the loading state, then asks again. */
  retry: () => void;
  /** Takes an answer a change already returned, so the screen need not ask again. */
  replace: (value: T) => void;
}

export function useLoaded<T>(load: () => Promise<T>, pollMs: (value: T) => number | null = () => null): LoadedHandle<T> {
  const refused = useContext(RefusalContext);
  const [state, setState] = useState<Loaded<T>>({ status: "loading" });
  const [round, setRound] = useState(0);
  // Held in refs so a caller's inline closures do not restart the load every render.
  const loadRef = useRef(load);
  loadRef.current = load;
  const pollRef = useRef(pollMs);
  pollRef.current = pollMs;

  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void (async () => {
      try {
        const value = await loadRef.current();
        if (!live) return;
        setState({ status: "ready", value });
        const delay = pollRef.current(value);
        if (delay !== null) timer = setTimeout(() => setRound((n) => n + 1), delay);
      } catch (e) {
        if (!live) return;
        if (isUnknownCaller(e)) refused("unknown_caller");
        else if (isNotServed(e)) refused("not_served");
        // A failed poll keeps what is already on screen.
        setState((current) => (current.status === "ready" ? current : { status: "failed", problem: e }));
      }
    })();
    return () => {
      live = false;
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [round, refused]);

  const reload = useCallback(() => setRound((n) => n + 1), []);
  const retry = useCallback(() => {
    setState({ status: "loading" });
    setRound((n) => n + 1);
  }, []);
  const replace = useCallback((value: T) => setState({ status: "ready", value }), []);

  return { state, reload, retry, replace };
}

const SAID_MS = 3_200;

export function useSaid(): { said: string | null; say: (text: string) => void } {
  const [said, setSaid] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );

  const say = useCallback((text: string) => {
    if (timer.current !== null) clearTimeout(timer.current);
    setSaid(text);
    timer.current = setTimeout(() => {
      setSaid(null);
      timer.current = null;
    }, SAID_MS);
  }, []);

  return { said, say };
}
