import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import { ApiError, type LongTask } from "../api/types";
import { isEnded, isMoving } from "./longTaskWords";

/** A task being planned or worked moves on its own, so it is asked after every 5 s. */
export const LONG_TASK_POLL_MS = 5_000;
/** One paused or waiting on an approval changes when a person acts, often elsewhere: asked after rarely. */
export const LONG_TASK_SLOW_POLL_MS = 30_000;

export interface LongTasksState {
  tasks: Readonly<Record<string, LongTask>>;
  /** False once the pond answered that it has no long tasks; nothing about them is then shown. */
  supported: boolean;
  /** Ids the pond has no task for. */
  gone: Readonly<Record<string, true>>;
  /** Ids that could not be read just now, with why. */
  unreadable: Readonly<Record<string, string>>;
  stopping: Readonly<Record<string, true>>;
  stopErrors: Readonly<Record<string, string>>;
  stop: (id: string) => Promise<void>;
  retry: (id: string) => void;
}

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function isMissingRoute(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 405 || e.status === 501);
}

function without<T>(all: Readonly<Record<string, T>>, key: string): Record<string, T> {
  if (!(key in all)) return all as Record<string, T>;
  const { [key]: _removed, ...rest } = all;
  return rest;
}

function pollDelay(tasks: Readonly<Record<string, LongTask>>): number | null {
  const open = Object.values(tasks).filter((t) => !isEnded(t.state));
  if (open.length === 0) return null;
  return open.some((t) => isMoving(t.state)) ? LONG_TASK_POLL_MS : LONG_TASK_SLOW_POLL_MS;
}

/**
 * One conversation's long tasks: those it started (listed by session), and any a message points at
 * by id. Open ones are asked after again and again, and an ended one never is.
 */
export function useLongTasks(sessionId: string | null, anchored: readonly string[]): LongTasksState {
  const [tasks, setTasks] = useState<Record<string, LongTask>>({});
  const [supported, setSupported] = useState(true);
  const [listed, setListed] = useState(false);
  const [gone, setGone] = useState<Record<string, true>>({});
  const [unreadable, setUnreadable] = useState<Record<string, string>>({});
  const [stopping, setStopping] = useState<Record<string, true>>({});
  const [stopErrors, setStopErrors] = useState<Record<string, string>>({});
  const [round, setRound] = useState(0);
  const asked = useRef(new Set<string>());
  const tasksRef = useRef(tasks);
  tasksRef.current = tasks;
  const lastSession = useRef<string | null>(null);

  // Another conversation starts clean. Arriving at a first id from none keeps what a live turn found.
  useEffect(() => {
    const before = lastSession.current;
    lastSession.current = sessionId;
    if (before !== null && before !== sessionId) {
      asked.current = new Set();
      setTasks({});
      setGone({});
      setUnreadable({});
      setListed(false);
    }
  }, [sessionId]);

  const merge = useCallback((found: readonly LongTask[]) => {
    setTasks((prev) => {
      const next = { ...prev };
      for (const task of found) next[task.id] = task;
      return next;
    });
    setGone((prev) => found.reduce((all, task) => without(all, task.id), prev));
    setUnreadable((prev) => found.reduce((all, task) => without(all, task.id), prev));
  }, []);

  const load = useCallback(
    async (id: string): Promise<void> => {
      try {
        merge([await api.getLongTask(id)]);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) setGone((all) => ({ ...all, [id]: true }));
        else setUnreadable((all) => ({ ...all, [id]: reasonOf(e) }));
      }
    },
    [merge],
  );

  useEffect(() => {
    if (!sessionId || !supported) return;
    let live = true;
    void (async () => {
      try {
        const list = await api.listLongTasks(sessionId);
        if (live) merge(list);
      } catch (e) {
        // Anything but a missing route is left to the per-task reads, which say what failed.
        if (live && isMissingRoute(e)) setSupported(false);
      } finally {
        if (live) setListed(true);
      }
    })();
    return () => {
      live = false;
    };
  }, [sessionId, supported, merge]);

  const anchoredKey = anchored.join(",");
  useEffect(() => {
    if (!supported || (sessionId && !listed)) return;
    for (const id of anchoredKey === "" ? [] : anchoredKey.split(",")) {
      if (tasksRef.current[id] || asked.current.has(id)) continue;
      asked.current.add(id);
      void load(id);
    }
  }, [anchoredKey, supported, sessionId, listed, load]);

  const delay = pollDelay(tasks);
  const openKey = Object.values(tasks)
    .filter((t) => !isEnded(t.state))
    .map((t) => t.id)
    .sort()
    .join(",");
  useEffect(() => {
    if (!supported || delay === null) return;
    let live = true;
    const timer = setTimeout(() => {
      void (async () => {
        await Promise.all(openKey.split(",").filter(Boolean).map(load));
        if (live) setRound((n) => n + 1);
      })();
    }, delay);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [supported, delay, openKey, round, load]);

  const retry = useCallback(
    (id: string) => {
      asked.current.add(id);
      setUnreadable((all) => without(all, id));
      void load(id);
    },
    [load],
  );

  const stop = useCallback(
    async (id: string): Promise<void> => {
      setStopping((all) => ({ ...all, [id]: true }));
      setStopErrors((all) => without(all, id));
      try {
        await api.cancelLongTask(id);
        await load(id);
      } catch (e) {
        // A 409 means it had already ended: the fresh read says how, so it is no error.
        if (e instanceof ApiError && e.status === 409) await load(id);
        else setStopErrors((all) => ({ ...all, [id]: `Could not stop it. ${reasonOf(e)}` }));
      } finally {
        setStopping((all) => without(all, id));
      }
    },
    [load],
  );

  return { tasks, supported, gone, unreadable, stopping, stopErrors, stop, retry };
}
