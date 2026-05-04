import {
  type DesktopMode,
  type GuiSection,
  normalizeDesktopMode,
  normalizeGuiSection,
} from "../desktopState";

export type VoiceState = "idle" | "wait" | "recording" | "thinking" | "speaking" | "error";

export interface TranscriptMessage {
  id: number;
  role: "user" | "agent";
  text: string;
  timestamp: number;
}

export interface ContextCard {
  id: number;
  tool: string;
  data: Record<string, unknown>;
  timestamp_ms: number;
}

const TRANSCRIPT_CAP = 50;

export interface LastResponseMeta {
  modelName: string;
  modelRole: string;
  completionTokens: number;
}

export interface CurrentSpeaker {
  profileId: string;
  displayName: string;
  confidence: number;
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
  currentSpeaker: CurrentSpeaker | null;
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
  | { type: "CLEAR_CONTEXT_CARDS" }
  | { type: "VOICE_ACTIVATE" }
  | { type: "SET_LAST_RESPONSE_META"; payload: LastResponseMeta }
  | { type: "SET_NEEDS_ONBOARDING"; payload: boolean }
  | { type: "SET_CURRENT_SPEAKER"; payload: CurrentSpeaker | null };

let _transcriptIdCounter = 0;
let _cardIdCounter = 0;
export function nextTranscriptId(): number { return ++_transcriptIdCounter; }
export function nextCardId(): number { return ++_cardIdCounter; }

export function buildInitialState(): AppState {
  const storedMode = normalizeDesktopMode(localStorage.getItem("giap-mode"));
  const storedSection = normalizeGuiSection(localStorage.getItem("giap-section"));
  const storedUrl = localStorage.getItem("giap-server-url") || "http://127.0.0.1:4000";
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
    currentSpeaker: null,
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

    case "SET_VOICE_STATE":
      // Preserve voiceError when entering error state so the message survives
      // the two-dispatch sequence (SET_VOICE_ERROR then SET_VOICE_STATE "error").
      return {
        ...state,
        voiceState: action.payload,
        voiceError: action.payload === "error" ? state.voiceError : null,
      };

    case "SET_VOICE_ERROR":
      return { ...state, voiceError: action.payload };

    case "APPEND_TRANSCRIPT": {
      const updated = [...state.transcript, action.payload];
      const trimmed = updated.length > TRANSCRIPT_CAP
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
      const trimmed = updated.length > TRANSCRIPT_CAP
        ? updated.slice(updated.length - TRANSCRIPT_CAP)
        : updated;
      void done; // token streaming is complete when done=true, no state change needed
      return { ...state, transcript: trimmed };
    }

    case "CLEAR_TRANSCRIPT":
      return { ...state, transcript: [], contextCards: [] };

    case "PUSH_CONTEXT_CARD":
      return { ...state, contextCards: [...state.contextCards, action.payload] };

    case "CLEAR_CONTEXT_CARDS":
      return { ...state, contextCards: [] };

    case "VOICE_ACTIVATE":
      return { ...state, voiceRequestId: state.voiceRequestId + 1 };

    case "SET_LAST_RESPONSE_META":
      return { ...state, lastResponseMeta: action.payload };

    case "SET_NEEDS_ONBOARDING":
      return { ...state, needsOnboarding: action.payload };

    case "SET_CURRENT_SPEAKER":
      return { ...state, currentSpeaker: action.payload };

    default:
      return state;
  }
}
