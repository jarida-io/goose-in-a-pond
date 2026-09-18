import {
  type DesktopMode,
  type GuiSection,
  normalizeDesktopMode,
  normalizeGuiSection,
} from "../desktopState";
import type { ScheduleRunNotification, DebriefContext } from "../api/types";
import { defaultServerUrl } from "../api/PondApiClient";
import { isDesktopShell } from "../shell";

export type VoiceState =
  "idle" | "wait" | "recording" | "thinking" | "speaking" | "error";

export interface TranscriptMessage {
  id: number;
  role: "user" | "agent";
  text: string;
  timestamp: number;
}

export interface ContextCard {
  id: number;
  tool: string;
  /** MCP request ID — used to match ToolResult events back to their ToolCall card */
  callId?: string;
  data: Record<string, unknown>;
  timestamp_ms: number;
  /** Explicit card type from MCP-APP UI hint — takes priority over tool name pattern matching */
  renderHint?: string;
}

const TRANSCRIPT_CAP = 50;

export interface LastResponseMeta {
  modelName: string;
  modelRole: string;
  completionTokens: number;
}

export interface ScheduleToast {
  id: string;
  schedule_id: string;
  schedule_label: string;
  status: "completed" | "failed" | "running";
  result?: string;
  error?: string;
  timestamp: number;
}

export interface AppState {
  mode: DesktopMode;
  section: GuiSection;
  serverOnline: boolean;
  serverStarting: boolean;
  serverUrl: string;
  sessionToken: string | null;
  sessionId: string | null;
  needsOnboarding: boolean;
  voiceState: VoiceState;
  voiceError: string | null;
  transcript: TranscriptMessage[];
  contextCards: ContextCard[];
  voiceRequestId: number;
  lastResponseMeta: LastResponseMeta | null;
  scheduleToasts: ScheduleToast[];
  latestScheduleResult: ScheduleToast | null;
  /** Persistent schedule run notifications (survives page navigation, unlike toasts). */
  scheduleRuns: ScheduleRunNotification[];
  /** Number of unread schedule runs. */
  unreadRunCount: number;
  /** When set, Canvas should render a debrief card for this run. */
  debriefContext: DebriefContext | null;
}

export type AppAction =
  | { type: "SET_MODE"; payload: DesktopMode }
  | { type: "SET_SECTION"; payload: GuiSection }
  | { type: "SERVER_ONLINE" }
  | { type: "SERVER_OFFLINE"; payload?: string }
  | { type: "SERVER_STARTING" }
  | { type: "SET_SERVER_URL"; payload: string }
  | { type: "SET_SESSION_TOKEN"; payload: string | null }
  | { type: "SET_SESSION_ID"; payload: string | null }
  | { type: "SET_VOICE_STATE"; payload: VoiceState }
  | { type: "SET_VOICE_ERROR"; payload: string | null }
  | { type: "APPEND_TRANSCRIPT"; payload: TranscriptMessage }
  | { type: "APPEND_AGENT_TOKEN"; payload: { token: string; done: boolean } }
  | { type: "CLEAR_TRANSCRIPT" }
  | { type: "PUSH_CONTEXT_CARD"; payload: ContextCard }
  | {
      type: "UPDATE_CONTEXT_CARD";
      payload: { callId: string; data: Record<string, unknown>; tool?: string };
    }
  | { type: "CLEAR_CONTEXT_CARDS" }
  | { type: "VOICE_ACTIVATE" }
  | { type: "SET_LAST_RESPONSE_META"; payload: LastResponseMeta }
  | { type: "SET_NEEDS_ONBOARDING"; payload: boolean }
  | { type: "SCHEDULE_RESULT"; payload: ScheduleToast }
  | { type: "DISMISS_TOAST"; payload: string }
  | { type: "SET_SCHEDULE_RUNS"; payload: ScheduleRunNotification[] }
  | { type: "ADD_SCHEDULE_RUN"; payload: ScheduleRunNotification }
  | {
      type: "UPDATE_SCHEDULE_RUN";
      payload: {
        id: string;
        status: "completed" | "failed";
        result?: string;
        error?: string;
      };
    }
  | { type: "MARK_RUN_READ"; payload: string }
  | { type: "MARK_ALL_RUNS_READ" }
  | { type: "SET_DEBRIEF_CONTEXT"; payload: DebriefContext | null }
  | { type: "CLEAR_DEBRIEF_CONTEXT" };

let _transcriptIdCounter = 0;
let _cardIdCounter = 0;
export function nextTranscriptId(): number {
  return ++_transcriptIdCounter;
}
export function nextCardId(): number {
  return ++_cardIdCounter;
}

export function buildInitialState(): AppState {
  const storedMode = normalizeDesktopMode(localStorage.getItem("giap-mode"));
  // The classic (sections) UI is the default landing surface. The Goose Hub is
  // still a preview, so never *start* in it even if it was the last-viewed
  // section. This used to say the preview was "reachable via Settings >
  // Preview Goose Hub"; that control is gone and the sentence outlived it by
  // long enough to send an E2E test looking for a button nobody had built in
  // months. The only way in now is the `giap-force-hub` opt-in below — a
  // persisted "hub" is
  // coerced back to the classic UI on launch/reload so the preview is not
  // sticky. An explicit opt-in (`giap-force-hub`, used by hub E2E tests and
  // available for dev) bypasses the coercion.
  const rawSection = normalizeGuiSection(localStorage.getItem("giap-section"));
  const forceHub = localStorage.getItem("giap-force-hub") === "1";
  const storedSection: GuiSection =
    rawSection === "hub" && !forceHub ? "dashboard" : rawSection;
  // In the desktop shell the injected URL wins over anything persisted: the
  // shell knows which port its own sidecar bound, and a stored value is at best
  // stale. Anyone who hit the double-spawn bug has 127.0.0.1:4001 saved here,
  // and without this the fixed build would still talk to a dead port. In a
  // plain browser the stored value is the point -- that is where a user types
  // a LAN address.
  const injectedUrl = defaultServerUrl();
  const storedUrl = isDesktopShell()
    ? injectedUrl
    : localStorage.getItem("giap-server-url") || injectedUrl;
  const storedToken = localStorage.getItem("giap-session-token") || null;

  return {
    mode: storedMode,
    section: storedSection,
    serverOnline: false,
    serverStarting: true,
    serverUrl: storedUrl,
    sessionToken: storedToken,
    sessionId: null,
    needsOnboarding: false,
    voiceState: "idle",
    voiceError: null,
    transcript: [],
    contextCards: [],
    voiceRequestId: 0,
    lastResponseMeta: null,
    scheduleToasts: [],
    latestScheduleResult: null,
    scheduleRuns: [],
    unreadRunCount: 0,
    debriefContext: null,
  };
}

export function reducer(state: AppState, action: AppAction): AppState {
  switch (action.type) {
    case "SET_MODE": {
      localStorage.setItem("giap-mode", action.payload);
      return { ...state, mode: action.payload };
    }

    case "SET_SECTION": {
      localStorage.setItem("giap-section", action.payload);
      return { ...state, section: action.payload };
    }

    case "SERVER_ONLINE":
      return { ...state, serverOnline: true, serverStarting: false };

    case "SERVER_OFFLINE":
      return { ...state, serverOnline: false, serverStarting: false };

    case "SERVER_STARTING":
      return { ...state, serverStarting: true };

    case "SET_SERVER_URL": {
      localStorage.setItem("giap-server-url", action.payload);
      return { ...state, serverUrl: action.payload };
    }

    case "SET_SESSION_TOKEN": {
      if (action.payload) {
        localStorage.setItem("giap-session-token", action.payload);
      } else {
        localStorage.removeItem("giap-session-token");
      }
      return { ...state, sessionToken: action.payload };
    }

    case "SET_SESSION_ID":
      return { ...state, sessionId: action.payload };

    // Clearing the error on a state change is right for every state EXCEPT
    // "error" itself. Every producer dispatches SET_VOICE_ERROR and then
    // SET_VOICE_STATE("error"), so the unconditional reset wiped the message
    // the previous action had just set and the UI only ever showed the generic
    // "Error" label. Entering the error state must preserve the reason.
    case "SET_VOICE_STATE":
      return {
        ...state,
        voiceState: action.payload,
        voiceError: action.payload === "error" ? state.voiceError : null,
      };

    case "SET_VOICE_ERROR":
      return { ...state, voiceError: action.payload };

    case "APPEND_TRANSCRIPT": {
      const updated = [...state.transcript, action.payload];
      const trimmed =
        updated.length > TRANSCRIPT_CAP
          ? updated.slice(updated.length - TRANSCRIPT_CAP)
          : updated;
      return { ...state, transcript: trimmed };
    }

    case "APPEND_AGENT_TOKEN": {
      const { token, done } = action.payload;
      if (state.transcript.length === 0) return state;
      const last = state.transcript[state.transcript.length - 1];
      if (last.role !== "agent") return state;
      const updated = [
        ...state.transcript.slice(0, -1),
        { ...last, text: last.text + token },
      ];
      const trimmed =
        updated.length > TRANSCRIPT_CAP
          ? updated.slice(updated.length - TRANSCRIPT_CAP)
          : updated;
      void done; // token streaming is complete when done=true, no state change needed
      return { ...state, transcript: trimmed };
    }

    case "CLEAR_TRANSCRIPT":
      return { ...state, transcript: [], contextCards: [] };

    case "PUSH_CONTEXT_CARD":
      return {
        ...state,
        contextCards: [...state.contextCards, action.payload],
      };

    case "UPDATE_CONTEXT_CARD": {
      // Merge data into the most-recent card matching the callId.
      const { callId, data, tool } = action.payload;
      let updated = false;
      const cards = state.contextCards.map((c) => {
        if (!updated && c.callId === callId) {
          updated = true;
          return { ...c, data: { ...c.data, ...data } };
        }
        return c;
      });
      if (updated) return { ...state, contextCards: cards };
      // No matching tool_call card. The tool_call event normally pushes the card
      // before its result arrives, but if the result is orphaned (the call card
      // was never pushed, or the transcript was cleared between call and result)
      // and the event carries a tool name, surface it as its own card instead of
      // silently dropping the result. Without a tool name there is nothing
      // meaningful to render, so keep the no-op (and the stable state reference)
      // to avoid orphan cards and spurious re-renders.
      if (tool) {
        const card: ContextCard = {
          id: nextCardId(),
          tool,
          callId,
          data,
          timestamp_ms: Date.now(),
        };
        return { ...state, contextCards: [...state.contextCards, card] };
      }
      return state;
    }

    case "CLEAR_CONTEXT_CARDS":
      return { ...state, contextCards: [] };

    case "VOICE_ACTIVATE":
      return { ...state, voiceRequestId: state.voiceRequestId + 1 };

    case "SET_LAST_RESPONSE_META":
      return { ...state, lastResponseMeta: action.payload };

    case "SET_NEEDS_ONBOARDING":
      return { ...state, needsOnboarding: action.payload };

    case "SCHEDULE_RESULT": {
      const incoming = action.payload;
      let existing = state.scheduleToasts;
      // If this is a completion event, replace the "running" toast for same schedule
      if (incoming.status !== "running") {
        existing = existing.filter(
          (t) =>
            !(t.status === "running" && t.schedule_id === incoming.schedule_id),
        );
      }
      return {
        ...state,
        scheduleToasts: [incoming, ...existing].slice(0, 10),
        latestScheduleResult: incoming,
      };
    }

    case "DISMISS_TOAST":
      return {
        ...state,
        scheduleToasts: state.scheduleToasts.filter(
          (t) => t.id !== action.payload,
        ),
      };

    case "SET_SCHEDULE_RUNS": {
      const runs = action.payload;
      return {
        ...state,
        scheduleRuns: runs,
        unreadRunCount: runs.filter((r) => !r.read).length,
      };
    }

    case "ADD_SCHEDULE_RUN": {
      const run = action.payload;
      // Avoid duplicates — replace if same id exists (e.g. running -> completed)
      const filtered = state.scheduleRuns.filter((r) => r.id !== run.id);
      const updated = [run, ...filtered].slice(0, 50);
      return {
        ...state,
        scheduleRuns: updated,
        unreadRunCount: updated.filter((r) => !r.read).length,
      };
    }

    case "UPDATE_SCHEDULE_RUN": {
      const { id, status, result, error } = action.payload;
      const updated = state.scheduleRuns.map((r) =>
        r.id === id
          ? {
              ...r,
              status,
              result: result ?? r.result,
              error: error ?? r.error,
              excerpt: (result ?? r.result ?? error ?? "").slice(0, 80),
            }
          : r,
      );
      return {
        ...state,
        scheduleRuns: updated,
        unreadRunCount: updated.filter((r) => !r.read).length,
      };
    }

    case "MARK_RUN_READ": {
      const updated = state.scheduleRuns.map((r) =>
        r.id === action.payload ? { ...r, read: true } : r,
      );
      return {
        ...state,
        scheduleRuns: updated,
        unreadRunCount: updated.filter((r) => !r.read).length,
      };
    }

    case "MARK_ALL_RUNS_READ": {
      return {
        ...state,
        scheduleRuns: state.scheduleRuns.map((r) => ({ ...r, read: true })),
        unreadRunCount: 0,
      };
    }

    case "SET_DEBRIEF_CONTEXT":
      return { ...state, debriefContext: action.payload };

    case "CLEAR_DEBRIEF_CONTEXT":
      return { ...state, debriefContext: null };

    default:
      return state;
  }
}
