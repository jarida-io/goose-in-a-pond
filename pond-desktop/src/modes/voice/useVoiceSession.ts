// Runs the persistent `pond-server chat --json-events` child; the shell re-emits its NDJSON as voice-* events.
// EVENT OWNERSHIP RULE: no other component may listen for voice-* events.

import { useEffect, useRef, useCallback, useState } from "react";
import { invoke, listen } from "../../shell";
import { useAppDispatch, useAppState } from "../../state/AppContext";
import { nextTranscriptId, nextCardId } from "../../state/reducer";

// ── Contract state-string mapping ───────────────────────────────────────────

const CHILD_STATE_MAP: Record<string, "idle" | "wait" | "recording" | "thinking" | "speaking" | "error"> = {
  // Contract-defined server states
  wait:      "wait",
  listen:    "recording",
  thinking:  "thinking",
  speak:     "speaking",
  // Legacy keys, so an old emitter can't break the UI.
  idle:         "idle",
  transcribing: "thinking",
  recording:    "recording",
  speaking:     "speaking",
  error:        "error",
};

// ── End-reason classification ────────────────────────────────────────────────
// By reason, not code: a signal-killed child reports code=null with reason "crashed".

function isCleanExit(code: number | null, reason: string): boolean {
  if (reason === "stdin_eof" || reason === "dismissed") return true;
  return false;
}

// ── Startup-failure messages ─────────────────────────────────────────────────
// `failed_to_start`: the child died before `ready`. The shell sends its stderr tail as `detail`, the only
// place the cause appears: under `--json-events` the banner macro is a no-op, so stdout is empty.

/** Longest `detail` excerpt to put in a user-facing message. */
const DETAIL_MAX_CHARS = 300;

/** Last non-blank stderr line, where a fatal error lands; truncated so a stack trace can't fill the screen. */
export function lastMeaningfulLine(detail: string | null | undefined): string | null {
  if (!detail) return null;
  const lines = detail.split("\n").map((l) => l.trim()).filter(Boolean);
  if (lines.length === 0) return null;
  const last = lines[lines.length - 1];
  return last.length > DETAIL_MAX_CHARS ? `${last.slice(0, DETAIL_MAX_CHARS)}…` : last;
}

/** The user-facing message for an abnormal `voice-session-ended`. */
export function endedErrorMessage(
  code: number | null,
  reason: string,
  detail?: string | null,
): string {
  if (reason !== "failed_to_start") {
    return `Voice session exited (code ${code ?? "none"}, reason: ${reason})`;
  }
  const line = lastMeaningfulLine(detail);
  return line
    ? `Voice mode could not start: ${line}`
    : `Voice mode could not start — the voice process exited during startup (code ${code ?? "none"}) without reporting a reason.`;
}

// ── Public API ───────────────────────────────────────────────────────────────

export interface VoiceSessionAPI {
  /** True once voice-ready fires and until voice-session-ended fires. */
  sessionActive: boolean;
  /** True while the shell has started spawning but before voice-ready fires. */
  connecting: boolean;
  /** True from voice-warmup "warming" until it ends, while the model loads and the prompt prefix precompiles. */
  warmingUp: boolean;
  /** Start the persistent child-process session. Returns the session uuid. */
  startSession(): Promise<string | null>;
  /** Close child stdin to request clean exit; kills after 3s if still alive. */
  stopSession(): Promise<void>;
  /** Clear transcript and context cards. */
  clearConversation(): void;
  /** Live mic RMS level (0-1) during wait/recording; 0 otherwise. */
  audioLevel: number;
}

// ── Hook ─────────────────────────────────────────────────────────────────────

export function useVoiceSession(): VoiceSessionAPI {
  const dispatch = useAppDispatch();

  // The chat view's session, in a ref so `startSession` keeps a stable identity (effects depend on it).
  // Subscribing to all app state costs token-batch re-renders; audioLevel already re-renders ~30 Hz.
  const appSessionId = useAppState().sessionId;
  const appSessionIdRef = useRef<string | null>(appSessionId);
  appSessionIdRef.current = appSessionId;

  const [sessionActive, setSessionActive] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [warmingUp, setWarmingUp] = useState(false);
  const [audioLevel, setAudioLevel] = useState(0);

  // Filters stale events from a previous child; set on start and voice-ready, cleared on ended.
  const activeSessionIdRef = useRef<string | null>(null);

  const unlistenersRef = useRef<Array<() => void>>([]);

  // Tokens buffered for one APPEND_AGENT_TOKEN per animation frame.
  const pendingTokensRef = useRef<string>("");
  const rafHandleRef = useRef<number | null>(null);

  // ── flashError helper ────────────────────────────────────────────────────
  // Persists until the user dismisses or retries; deliberately no auto-clear timer.

  const flashError = useCallback((msg: string) => {
    dispatch({ type: "SET_VOICE_ERROR", payload: msg });
    dispatch({ type: "SET_VOICE_STATE", payload: "error" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── flushPendingTokens ───────────────────────────────────────────────────

  const flushPendingTokens = useCallback(() => {
    rafHandleRef.current = null;
    if (pendingTokensRef.current.length === 0) return;
    const batch = pendingTokensRef.current;
    pendingTokensRef.current = "";
    dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: batch, done: false } });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Shell event listeners ────────────────────────────────────────────────
  // See EVENT OWNERSHIP RULE at the top of this file.

  useEffect(() => {
    // Registration is synchronous: nothing races teardown, and no event slips in before subscribing.

    // voice-warmup: warming | ready | skipped | failed. Precedes voice-ready.
    unlistenersRef.current.push(
      listen("voice-warmup", (payload) => {
        setWarmingUp(payload === "warming");
      }),
    );

    // voice-ready: once per session, after models load; the child enters the wait loop.
    unlistenersRef.current.push(
      listen("voice-ready", (payload) => {
        setConnecting(false);
        setWarmingUp(false);
        setSessionActive(true);
        if (payload?.session_id) {
          activeSessionIdRef.current = payload.session_id;
          dispatch({ type: "SET_SESSION_ID", payload: payload.session_id });
        }
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
      }),
    );

    // voice-state: wait | listen | thinking | speak
    unlistenersRef.current.push(
      listen("voice-state", (payload) => {
        const mapped = CHILD_STATE_MAP[payload] ?? "idle";
        dispatch({ type: "SET_VOICE_STATE", payload: mapped });
      }),
    );

    // voice-transcript: confirmed user utterance post-ASR
    unlistenersRef.current.push(
      listen("voice-transcript", (payload) => {
        const userMsg = {
          id: nextTranscriptId(),
          role: "user" as const,
          text: payload.text,
          timestamp: Date.now(),
        };
        dispatch({ type: "APPEND_TRANSCRIPT", payload: userMsg });
        // Seed an empty agent message so APPEND_AGENT_TOKEN can target it.
        dispatch({
          type: "APPEND_TRANSCRIPT",
          payload: {
            id: nextTranscriptId(),
            role: "agent" as const,
            text: "",
            timestamp: Date.now(),
          },
        });
      }),
    );

    // voice-token: assistant token delta, batched via rAF
    unlistenersRef.current.push(
      listen("voice-token", (payload) => {
        pendingTokensRef.current += payload.content;
        if (rafHandleRef.current === null) {
          rafHandleRef.current = requestAnimationFrame(flushPendingTokens);
        }
      }),
    );

    // voice-tool-call: push a context card for the in-flight tool
    unlistenersRef.current.push(
      listen("voice-tool-call", (payload) => {
        dispatch({
          type: "PUSH_CONTEXT_CARD",
          payload: {
            id: nextCardId(),
            tool: payload.tool,
            callId: payload.id,
            data: { id: payload.id },
            timestamp_ms: Date.now(),
          },
        });
      }),
    );

    // voice-tool-result: merged into its call's card by id; `tool` keeps an orphan result visible.
    unlistenersRef.current.push(
      listen("voice-tool-result", (payload) => {
        dispatch({
          type: "UPDATE_CONTEXT_CARD",
          payload: {
            callId: payload.id,
            tool: payload.tool,
            data: { result: payload.content },
          },
        });
      }),
    );

    // voice-done: turn complete; flush any buffered tokens first, then mark done.
    unlistenersRef.current.push(
      listen("voice-done", (payload) => {
        if (rafHandleRef.current !== null) {
          cancelAnimationFrame(rafHandleRef.current);
          flushPendingTokens();
        }
        dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });
        if (payload?.session_id) {
          dispatch({ type: "SET_SESSION_ID", payload: payload.session_id });
        }
        // Child returns to the wait loop after each completed turn.
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
      }),
    );

    // voice-error: non-fatal child error.
    unlistenersRef.current.push(
      listen("voice-error", (payload) => {
        const msg =
          typeof payload === "string"
            ? payload
            : payload?.message ?? "Voice session error";
        flashError(msg);
      }),
    );

    // voice-audio-level: live mic RMS during wait/recording, throttled Rust-side.
    unlistenersRef.current.push(
      listen("voice-audio-level", (payload) => {
        setAudioLevel(payload.rms);
      }),
    );

    // voice-session-ended: child exited, cleanly or not.
    unlistenersRef.current.push(
      listen("voice-session-ended", (payload) => {
        const incomingId = payload?.session_id ?? null;
        // A mismatched session_id belongs to a previous child; drop it.
        if (incomingId !== null && activeSessionIdRef.current !== null && incomingId !== activeSessionIdRef.current) {
          return;
        }
        setSessionActive(false);
        setConnecting(false);
        setAudioLevel(0);
        activeSessionIdRef.current = null;

        // Best-effort, so the shell restores its wake listener when the child exits on its own.
        invoke("stop_voice_session").catch(() => {});

        const code = payload?.code ?? null;
        const reason = payload?.reason ?? "unknown";
        if (!isCleanExit(code, reason)) {
          flashError(endedErrorMessage(code, reason, payload?.detail));
        } else {
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        }
      }),
    );

    return () => {
      unlistenersRef.current.forEach((u) => u());
      unlistenersRef.current = [];
      if (rafHandleRef.current !== null) {
        cancelAnimationFrame(rafHandleRef.current);
        rafHandleRef.current = null;
        if (pendingTokensRef.current.length > 0) {
          dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: pendingTokensRef.current, done: false } });
          pendingTokensRef.current = "";
        }
      }
      setAudioLevel(0);
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── startSession ─────────────────────────────────────────────────────────

  const startSession = useCallback(async (): Promise<string | null> => {
    try {
      setConnecting(true);
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      // Resumes the chat view's session (history, Goose session, agent mid-thought); `null` starts fresh
      // and the child returns the id it used.
      const sessionId = await invoke("start_voice_session", {
        sessionId: appSessionIdRef.current,
      });
      activeSessionIdRef.current = sessionId ?? null;
      // Optimistic; voice-ready confirms it once models load.
      if (sessionId) {
        dispatch({ type: "SET_SESSION_ID", payload: sessionId });
      }
      return sessionId ?? null;
    } catch (err) {
      setConnecting(false);
      const msg = err instanceof Error ? err.message : String(err);
      flashError(msg);
      return null;
    }
  }, [flashError]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── stopSession ──────────────────────────────────────────────────────────

  const stopSession = useCallback(async (): Promise<void> => {
    try {
      // Also restarts the shell's wake listener if it was running; voice-session-ended follows.
      await invoke("stop_voice_session");
    } catch {
      // Non-fatal — the session-ended event will still arrive and reset state.
    } finally {
      // Optimistically reset state; the voice-session-ended event will confirm.
      setSessionActive(false);
      setConnecting(false);
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── clearConversation ─────────────────────────────────────────────────────

  const clearConversation = useCallback(() => {
    dispatch({ type: "CLEAR_TRANSCRIPT" });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return { sessionActive, connecting,
    warmingUp, startSession, stopSession, clearConversation, audioLevel };
}
