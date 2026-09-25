import React, {
  createContext,
  useContext,
  useReducer,
  useEffect,
  useRef,
  type ReactNode,
} from "react";
import { invoke, listen, isDesktopShell } from "../shell";
import { api } from "../api/PondApiClient";
import { refreshHomeData } from "../hub/state/hubDataStore";
import { setChatRunBridge, resumeActiveRun } from "./chatRunStore";
import {
  reducer,
  buildInitialState,
  nextTranscriptId,
  nextCardId,
  type AppState,
  type AppAction,
  type TranscriptMessage,
  type ContextCard,
  type ScheduleToast,
} from "./reducer";
import type { ScheduleRunNotification } from "../api/types";

const StateCtx = createContext<AppState | null>(null);
const DispatchCtx = createContext<React.Dispatch<AppAction> | null>(null);

export function AppContextProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, undefined, buildInitialState);
  const scheduleEsRef = useRef<EventSource | null>(null);

  // Schedule-result SSE lives here, not in a section, so toasts show on every page.
  useEffect(() => {
    if (!state.serverOnline) return;

    if (scheduleEsRef.current) {
      scheduleEsRef.current.close();
      scheduleEsRef.current = null;
    }

    const baseUrl = state.serverUrl || "http://127.0.0.1:4000";
    const url = `${baseUrl}/api/v1/schedules/events`;
    const es = new EventSource(url);
    scheduleEsRef.current = es;

    es.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data) as {
          id?: string;
          schedule_id?: string;
          schedule_label?: string;
          status?: string;
          result?: string;
          error?: string;
        };
        const status = data.status;
        if (
          status !== "completed" &&
          status !== "failed" &&
          status !== "running"
        )
          return;
        const toast: ScheduleToast = {
          id: data.id || data.schedule_id || String(Date.now()),
          schedule_id: data.schedule_id || data.id || "",
          schedule_label: data.schedule_label || data.schedule_id || "Schedule",
          status: status as "completed" | "failed" | "running",
          result: data.result,
          error: data.error,
          timestamp: Date.now(),
        };
        dispatch({ type: "SCHEDULE_RESULT", payload: toast });

        const notification: ScheduleRunNotification = {
          id: toast.id,
          scheduleId: toast.schedule_id,
          scheduleName: toast.schedule_label,
          status: toast.status,
          result: toast.result ?? null,
          error: toast.error ?? null,
          startedAt: new Date().toISOString(),
          finishedAt:
            toast.status !== "running" ? new Date().toISOString() : null,
          durationMs: null,
          read: false,
          excerpt: (toast.result ?? toast.error ?? "").slice(0, 80),
          recipe: inferRecipe(toast.schedule_label),
        };

        // ADD_SCHEDULE_RUN dedupes by id, so this also covers a missed "running" event.
        dispatch({ type: "ADD_SCHEDULE_RUN", payload: notification });
      } catch {
        // ignore parse errors
      }
    };

    api
      .getAllRecentRuns(5)
      .then((runs) => {
        const notifications: ScheduleRunNotification[] = runs.map((r) => ({
          id: r.id,
          scheduleId: r.schedule_id,
          scheduleName: r.schedule_name,
          status: r.status,
          result: r.result ?? null,
          error: r.error ?? null,
          startedAt: r.started_at,
          finishedAt: r.finished_at ?? null,
          durationMs: r.duration_ms ?? null,
          read: true,
          excerpt: (r.result ?? r.error ?? "").slice(0, 80),
          recipe: inferRecipe(r.schedule_name),
        }));
        dispatch({ type: "SET_SCHEDULE_RUNS", payload: notifications });
      })
      .catch(() => {
        /* schedule runs fetch failed — non-fatal */
      });

    return () => {
      es.close();
      scheduleEsRef.current = null;
    };
  }, [state.serverOnline, state.serverUrl]);

  // hubDataStore fetches at import, racing a cold server boot; refetch once online.
  useEffect(() => {
    if (!state.serverOnline) return;
    void refreshHomeData();
  }, [state.serverOnline]);

  // The module-scope chat driver (`chatRunStore`) outlives the Chat section, so it gets state
  // and dispatch from here. `dispatch` is stable, hence absent from the deps.
  useEffect(
    () =>
      setChatRunBridge({
        sessionToken: state.sessionToken,
        serverOnline: state.serverOnline,
        onSessionId: (id) => dispatch({ type: "SET_SESSION_ID", payload: id }),
        onResponseMeta: (meta) =>
          dispatch({ type: "SET_LAST_RESPONSE_META", payload: meta }),
        onContextCard: (card) =>
          dispatch({ type: "PUSH_CONTEXT_CARD", payload: card }),
      }),
    [state.sessionToken, state.serverOnline],
  );

  // Resume a turn started by a previous window. Asked once, after the bridge above has a token;
  // an ordinary cold start costs one 404.
  const resumeAskedRef = useRef(false);
  useEffect(() => {
    if (!state.serverOnline || resumeAskedRef.current) return;
    resumeAskedRef.current = true;
    void resumeActiveRun().catch(() => {
      // Non-fatal: the persisted messages still show the conversation, and resumeActiveRun warns.
    });
  }, [state.serverOnline]);

  useEffect(() => {
    const unlisten: Array<() => void> = [];

    if (!isDesktopShell()) {
      // Browser dev / Playwright: no shell, so go online at once; loopback needs no auth token.
      dispatch({ type: "SERVER_ONLINE" });
      api
        .getOnboardingStatus()
        .then((status) => {
          if (!status.onboarded) {
            dispatch({ type: "SET_NEEDS_ONBOARDING", payload: true });
          }
        })
        .catch(() => {});
      // The import-time hubDataStore fetch may have lost the race and cached mock data; refetch.
      void refreshHomeData();
      return;
    }

    // Only flags the wizard; never completes onboarding itself.
    const ensureOnboarded = async () => {
      try {
        const status = await api.getOnboardingStatus();
        if (!status.onboarded) {
          dispatch({ type: "SET_NEEDS_ONBOARDING", payload: true });
        }
      } catch (err) {
        console.warn("Onboarding check failed (non-fatal):", err);
      }
    };

    // Shared by the probe and the server-status event; the flag stops a double handshake.
    let onlineHandled = false;
    const handleServerOnline = () => {
      if (onlineHandled) return;
      onlineHandled = true;
      dispatch({ type: "SERVER_ONLINE" });
      // Reuses a persisted token or refresh token; pairs afresh only when neither works.
      api
        .connect()
        .then(async (token) => {
          if (token) {
            dispatch({ type: "SET_SESSION_TOKEN", payload: token });
          } else {
            console.warn("Could not establish a session (pairing rejected).");
          }
          await ensureOnboarded();
          // The import-time hubDataStore fetch ran without a token and cached mock data; refetch.
          void refreshHomeData();
        })
        .catch((err) => console.warn("Connect failed (non-fatal):", err));
    };

    // The sidecar bound another port; request builders read the base per call, so no reload.
    unlisten.push(
      listen("server-url", (url) => {
        api.setBase(url);
        dispatch({ type: "SET_SERVER_URL", payload: url });
      }),
    );

    // Server online/offline status — reactive path.
    unlisten.push(
      listen("server-status", (online) => {
        if (online) {
          handleServerOnline();
        } else {
          // Server went offline — reset so a subsequent online event retriggers.
          onlineHandled = false;
          dispatch({ type: "SERVER_OFFLINE" });
        }
      }),
    );

    // Active probe: shell events aren't buffered, so a server-status sent before mount is lost.
    let probeCancelled = false;
    (async () => {
      // Every 250 ms for up to 60 s, so the dashboard appears as soon as the server accepts.
      for (let i = 0; i < 240; i++) {
        if (probeCancelled || onlineHandled) return;
        try {
          const healthy = await invoke("server_health");
          if (healthy) {
            handleServerOnline();
            return;
          }
        } catch {
          // server_health command not registered yet — keep polling.
        }
        await new Promise((r) => setTimeout(r, 250));
      }
    })();
    unlisten.push(() => {
      probeCancelled = true;
    });

    unlisten.push(
      listen("server-starting", () => {
        dispatch({ type: "SERVER_STARTING" });
      }),
    );

    // Global voice activation hotkey
    unlisten.push(
      listen("desktop-summon", () => {
        dispatch({ type: "VOICE_ACTIVATE" });
      }),
    );

    // recording-started/-aborted are deliberately unhandled: VoiceMode owns voice state.

    // macOS menu bar — View menu items
    unlisten.push(
      listen("canvas-toggle", () => {
        dispatch({ type: "SET_SECTION", payload: "canvas" });
      }),
    );

    unlisten.push(
      listen("switch-to-voice", () => {
        dispatch({ type: "SET_MODE", payload: "voice" });
      }),
    );

    return () => {
      unlisten.forEach((u) => u());
    };
  }, []);

  return (
    <StateCtx.Provider value={state}>
      <DispatchCtx.Provider value={dispatch}>{children}</DispatchCtx.Provider>
    </StateCtx.Provider>
  );
}

/** Infer a recipe identifier from the schedule name for debrief card routing. */
function inferRecipe(name: string): string | null {
  const lower = name.toLowerCase();
  if (/morning|briefing|daily.*summ/.test(lower)) return "daily-summary";
  if (/weekly.*report|week.*summ/.test(lower)) return "weekly-report";
  if (/compact|consolidat|memory.*clean/.test(lower)) return "compact-memory";
  return null;
}

export function useAppState(): AppState {
  const ctx = useContext(StateCtx);
  if (!ctx)
    throw new Error("useAppState must be used within AppContextProvider");
  return ctx;
}

export function useAppDispatch(): React.Dispatch<AppAction> {
  const ctx = useContext(DispatchCtx);
  if (!ctx)
    throw new Error("useAppDispatch must be used within AppContextProvider");
  return ctx;
}
