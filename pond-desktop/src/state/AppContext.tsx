import {
  useReducer,
  useEffect,
  useRef,
  type ReactNode,
} from "react";
import { invoke, listen, isDesktopShell } from "../shell";
import { api } from "../api/PondApiClient";
import { isSignInRequired } from "../api/hostCredential";
import { refreshHomeData } from "../hub/state/hubDataStore";
import { setChatRunBridge, resumeActiveRun } from "./chatRunStore";
import { DispatchCtx, StateCtx } from "./useAppContext";
import { reducer, buildInitialState } from "./reducer";
import {
  followHouseholdNotices,
  followScheduleRuns,
  inferRecipe,
} from "./liveFeeds";
import type { ScheduleRunNotification } from "../api/types";

/** Survives one dropped beat inside the registry's 5-minute online window; stays clear of the per-IP budget. */
const SELF_HEARTBEAT_MS = 120_000;

export function AppContextProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, undefined, buildInitialState);

  // The live feeds live here, not in a section, so toasts and the bell work on every page.
  // `serverUrl` is a dependency so a sidecar that moved ports gets fresh connections.
  useEffect(() => {
    if (!state.serverOnline) return;
    const controller = new AbortController();
    void followScheduleRuns(api, dispatch, controller.signal);
    void followHouseholdNotices(api, dispatch, controller.signal);

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
      .catch((err) => {
        console.warn("[schedules] recent runs could not be read (non-fatal):", err);
      });

    return () => controller.abort();
  }, [state.serverOnline, state.serverUrl]);

  // hubDataStore fetches at import, racing a cold server boot; refetch once online.
  useEffect(() => {
    if (!state.serverOnline) return;
    void refreshHomeData();
  }, [state.serverOnline]);

  // A tab refused its host credential while re-pairing shows the sign-in notice, and drops it
  // once another tab stores a session it can use.
  useEffect(
    () =>
      api.onSignInRequired((required) =>
        dispatch({ type: "SET_SIGN_IN_REQUIRED", payload: required }),
      ),
    [],
  );

  // The module-scope chat driver (`chatRunStore`) outlives the Chat section, so it gets state
  // and dispatch from here. `dispatch` is stable, hence absent from the deps.
  useEffect(
    () =>
      setChatRunBridge({
        serverOnline: state.serverOnline,
        onSessionId: (id) => dispatch({ type: "SET_SESSION_ID", payload: id }),
        onResponseMeta: (meta) =>
          dispatch({ type: "SET_LAST_RESPONSE_META", payload: meta }),
        onContextCard: (card) =>
          dispatch({ type: "PUSH_CONTEXT_CARD", payload: card }),
      }),
    [state.serverOnline],
  );

  // Keeps this machine's device row fresh. Not gated on visibility: a minimised desktop is still online.
  useEffect(() => {
    if (!state.serverOnline || !state.sessionToken) return;
    const beat = () => {
      // Non-fatal: the next beat is already scheduled.
      void api.heartbeatSelf().catch(() => {});
    };
    beat();
    const timer = setInterval(beat, SELF_HEARTBEAT_MS);
    return () => clearInterval(timer);
  }, [state.serverOnline, state.sessionToken]);

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
          dispatch({ type: "SET_SIGN_IN_REQUIRED", payload: false });
          if (token) {
            dispatch({ type: "SET_SESSION_TOKEN", payload: token });
          } else {
            console.warn("Could not establish a session (pairing rejected).");
          }
          await ensureOnboarded();
          // The import-time hubDataStore fetch ran without a token and cached mock data; refetch.
          void refreshHomeData();
        })
        .catch((err) => {
          if (isSignInRequired(err)) {
            console.warn("The Pond refused the host credential; showing the sign-in notice.");
            dispatch({ type: "SET_SIGN_IN_REQUIRED", payload: true });
            return;
          }
          console.warn("Connect failed (non-fatal):", err);
        });
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
