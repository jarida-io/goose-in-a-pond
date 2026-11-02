import { useCallback, useEffect, useState } from "react";
import { api } from "../api/PondApiClient";
import { ApiError, type PrefillReport } from "../api/types";

/** Often enough that a build reads as moving, rarely enough that a Jetson is not asked about it every second. */
export const PREFILL_ACTIVE_POLL_MS = 5_000;
export const PREFILL_IDLE_POLL_MS = 60_000;

/** Every agent's rebuild is asked for under this key. */
export const ALL_AGENTS = "*";

export interface PrefillsState {
  /** `null` until the first answer, and for good on a pond that has no such route. */
  report: PrefillReport | null;
  /** False once the pond answered that it has no prefill status; nothing about prefills is then shown. */
  supported: boolean;
  /** Agents (or `ALL_AGENTS`) a rebuild is being asked for. */
  busy: Readonly<Record<string, true>>;
  errors: Readonly<Record<string, string>>;
  /** Asks again now, then carries on at the usual pace. */
  refresh: () => void;
  rebuild: (agent: string) => Promise<boolean>;
  rebuildAll: () => Promise<boolean>;
}

function isMissingRoute(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 405 || e.status === 501);
}

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** The pond's prefill status: asked every 5 s while it builds, every 60 s otherwise, and a failed ask is retried slowly and silently. */
export function useAgentPrefills(): PrefillsState {
  const [report, setReport] = useState<PrefillReport | null>(null);
  const [supported, setSupported] = useState(true);
  const [round, setRound] = useState(0);
  const [busy, setBusy] = useState<Record<string, true>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});

  useEffect(() => {
    if (!supported) return;
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll(): Promise<void> {
      try {
        const next = await api.getAgentPrefills();
        if (!live) return;
        setReport(next);
        timer = setTimeout(() => void poll(), next.building.active ? PREFILL_ACTIVE_POLL_MS : PREFILL_IDLE_POLL_MS);
      } catch (e) {
        if (!live) return;
        if (isMissingRoute(e)) {
          setSupported(false);
          return;
        }
        timer = setTimeout(() => void poll(), PREFILL_IDLE_POLL_MS);
      }
    }
    void poll();
    return () => {
      live = false;
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [supported, round]);

  const refresh = useCallback(() => setRound((n) => n + 1), []);

  const ask = useCallback(
    async (key: string, send: () => Promise<void>): Promise<boolean> => {
      setBusy((all) => ({ ...all, [key]: true }));
      setErrors((all) => {
        if (!(key in all)) return all;
        const { [key]: _cleared, ...rest } = all;
        return rest;
      });
      try {
        await send();
        refresh();
        return true;
      } catch (e) {
        setErrors((all) => ({ ...all, [key]: `Could not ask for a rebuild. ${reasonOf(e)}` }));
        return false;
      } finally {
        setBusy((all) => {
          const { [key]: _done, ...rest } = all;
          return rest;
        });
      }
    },
    [refresh],
  );

  const rebuild = useCallback((agent: string) => ask(agent, () => api.rebuildAgentPrefill(agent)), [ask]);
  const rebuildAll = useCallback(() => ask(ALL_AGENTS, () => api.rebuildAllPrefills()), [ask]);

  return { report, supported, busy, errors, refresh, rebuild, rebuildAll };
}
