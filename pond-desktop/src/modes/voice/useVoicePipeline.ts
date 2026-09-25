// Voice-mode orchestration for the browser path; the desktop shell uses useVoiceSession.

import { useState, useEffect, useRef, useCallback } from "react";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { nextTranscriptId } from "../../state/reducer";
import { api } from "../../api/PondApiClient";
import { resolveVoiceDecision } from "../../voiceSummon";
import {
  createVoiceBackend,
  type VoiceBackend,
  type VoiceState,
} from "./VoiceBackend";

const NO_SPEECH_TIMEOUT_MS = 8_000;

export interface VoicePipelineAPI {
  audioLevel: number;
  wakeWord: string;
  maxSecs: number;
  secsLeft: number;
  startRecording(): void;
  stopAndSend(): void;
  abort(): void;
  clearConversation(): void;
}

export function useVoicePipeline(): VoicePipelineAPI {
  const state = useAppState();
  const dispatch = useAppDispatch();

  const [audioLevel, setAudioLevel] = useState(0);
  const [wakeWord, setWakeWord] = useState("");
  const [maxSecs, setMaxSecs] = useState(30);
  const [secsLeft, setSecsLeft] = useState(30);

  const backendRef = useRef<VoiceBackend | null>(null);
  const prevStateRef = useRef<VoiceState>("idle");
  const noSpeechTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const countdownTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const autoStopTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const wakeVariantsRef = useRef<string[]>([]);
  // `voice_thinking_tone_enabled`, read in effect closures (hence a ref); undefined until loaded = on.
  const thinkingToneRef = useRef<boolean | undefined>(undefined);

  const stateRef = useRef(state);
  stateRef.current = state;

  // Re-arms the wake listener on paths back to "wait" that skip runPipeline (whose finally does it).
  // A ref reassigned each render, so effect closures never see a stale `wakeWord`.
  const restartWakeListenerRef = useRef<() => void>(() => {});
  restartWakeListenerRef.current = () => {
    const backend = backendRef.current;
    if (backend && wakeWord) backend.startWakeListener(wakeWord, wakeVariantsRef.current);
  };

  // ── Cleanup helpers ────────────────────────────────────

  const clearTimers = useCallback(() => {
    if (noSpeechTimerRef.current) { clearTimeout(noSpeechTimerRef.current); noSpeechTimerRef.current = null; }
    if (countdownTimerRef.current) { clearInterval(countdownTimerRef.current); countdownTimerRef.current = null; }
    if (autoStopTimerRef.current) { clearTimeout(autoStopTimerRef.current); autoStopTimerRef.current = null; }
    setSecsLeft(maxSecs);
  }, [maxSecs]);

  // ── Backend creation + callback wiring ─────────────────

  useEffect(() => {
    let destroyed = false;
    const serverUrl = state.serverUrl || "http://127.0.0.1:4000";

    createVoiceBackend(serverUrl).then((backend) => {
      if (destroyed) { backend.destroy(); return; }
      backendRef.current = backend;

      backend.onAudioLevel = (level) => setAudioLevel(level);

      backend.onStateChange = (s) => {
        dispatch({ type: "SET_VOICE_STATE", payload: s });
      };

      backend.onTranscript = (text) => {
        dispatch({
          type: "APPEND_TRANSCRIPT",
          payload: { id: nextTranscriptId(), role: "user", text, timestamp: Date.now() },
        });
      };

      backend.onAgentToken = (token, done) => {
        if (done) {
          dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });
        } else if (token) {
          // Seed agent message on first token
          dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token, done: false } });
        }
      };

      backend.onToolCall = (data) => {
        dispatch({
          type: "PUSH_CONTEXT_CARD",
          payload: { id: Date.now(), tool: data.tool, data: data.data, timestamp_ms: Date.now() },
        });
      };

      backend.onError = (msg) => {
        dispatch({ type: "SET_VOICE_ERROR", payload: msg });
        dispatch({ type: "SET_VOICE_STATE", payload: "error" });
      };

      backend.onSessionId = (id) => {
        dispatch({ type: "SET_SESSION_ID", payload: id });
      };

      backend.onResponseMeta = (meta) => {
        dispatch({
          type: "SET_LAST_RESPONSE_META",
          payload: {
            modelName: meta.modelName,
            modelRole: meta.modelRole,
            completionTokens: meta.completionTokens,
          },
        });
      };

      backend.onWakeDetected = async (wav) => {
        dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
        // Seed empty agent message for streaming
        dispatch({
          type: "APPEND_TRANSCRIPT",
          payload: { id: nextTranscriptId(), role: "agent", text: "", timestamp: Date.now() },
        });
        const s = stateRef.current;
        await backend.runPipeline(wav, {
          stripWakeWord: wakeWord || undefined,
          sessionId: s.sessionId ?? undefined,
          authToken: s.sessionToken ?? undefined,
          serverUrl,
          thinkingTone: thinkingToneRef.current,
        });
      };

      backend.onWakeInterrupt = () => {
        // Barge-in: pipeline was cancelled by wake listener
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      };

      backend.onDismissed = (isExit) => {
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        if (!isExit && wakeWord) {
          setTimeout(() => {
            dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
            restartWakeListenerRef.current();
          }, 500);
        }
      };
    });

    return () => {
      destroyed = true;
      backendRef.current?.destroy();
      backendRef.current = null;
    };
  }, [state.serverUrl]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Load settings ──────────────────────────────────────

  useEffect(() => {
    api.getSettings().then((s) => {
      const dur = s.voice_recording_duration_secs;
      if (typeof dur === "number" && dur > 0) { setMaxSecs(dur); setSecsLeft(dur); }

      const ww = s.voice_wake_word;
      if (typeof ww === "string" && ww.trim()) setWakeWord(ww.trim());

      if (typeof s.voice_thinking_tone_enabled === "boolean") {
        thinkingToneRef.current = s.voice_thinking_tone_enabled;
      }
    }).catch(() => {});
  }, []);

  // ── Wake word listener management ──────────────────────

  useEffect(() => {
    const backend = backendRef.current;
    if (!backend || !wakeWord) return;

    // Load calibrated variants
    api.getSettings().then((s) => {
      const variants: string[] = s.voice_wake_word_transcriptions ?? [];
      wakeVariantsRef.current = variants;
      backend.startWakeListener(wakeWord, variants);
      dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
    }).catch(() => {
      wakeVariantsRef.current = [];
      backend.startWakeListener(wakeWord, []);
      dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
    });

    return () => { backend.stopWakeListener(); };
  }, [wakeWord, dispatch]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Conversational turn-taking ─────────────────────────
  // After a reply, auto-record the next turn. "thinking" -> "idle" counts too: that is a barge-in
  // mid-thought, because onWakeInterrupt jumps straight to "idle".

  useEffect(() => {
    const prev = prevStateRef.current;
    const curr = state.voiceState;
    prevStateRef.current = curr;

    if ((prev === "speaking" || prev === "thinking") && curr === "idle") {
      const backend = backendRef.current;
      if (!backend) return;

      dispatch({ type: "SET_VOICE_STATE", payload: "recording" });
      backend.recordWithVad(stateRef.current.sessionToken ?? undefined, stateRef.current.sessionId ?? undefined).then((blob) => {
        if (blob) {
          dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
          dispatch({
            type: "APPEND_TRANSCRIPT",
            payload: { id: nextTranscriptId(), role: "agent", text: "", timestamp: Date.now() },
          });
          const s = stateRef.current;
          backend.runPipeline(blob, {
            sessionId: s.sessionId ?? undefined,
            authToken: s.sessionToken ?? undefined,
            serverUrl: s.serverUrl || "http://127.0.0.1:4000",
            thinkingTone: thinkingToneRef.current,
          });
        } else {
          // No speech detected — return to wake listening or idle
          dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
          restartWakeListenerRef.current();
        }
      });

      if (noSpeechTimerRef.current) clearTimeout(noSpeechTimerRef.current);
      noSpeechTimerRef.current = setTimeout(() => {
        if (stateRef.current.voiceState === "recording") {
          backend.abortRecording();
          dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
          restartWakeListenerRef.current();
        }
      }, NO_SPEECH_TIMEOUT_MS);
    }

    // Clear no-speech timer if user started speaking
    if (curr === "thinking" || curr === "speaking") {
      if (noSpeechTimerRef.current) { clearTimeout(noSpeechTimerRef.current); noSpeechTimerRef.current = null; }
    }
  }, [state.voiceState, wakeWord, dispatch]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Hotkey activation ──────────────────────────────────

  useEffect(() => {
    if (state.voiceRequestId === 0) return;
    const decision = resolveVoiceDecision({
      serverHealthy: state.serverOnline,
      isRecording: state.voiceState === "recording",
      isProcessing: state.voiceState === "thinking" || state.voiceState === "speaking",
    });
    if (decision === "start-recording") startRecording();
    else if (decision === "stop-and-send") stopAndSend();
  }, [state.voiceRequestId]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Countdown cleanup on state change ──────────────────

  useEffect(() => {
    if (state.voiceState !== "recording") clearTimers();
  }, [state.voiceState, clearTimers]);

  // ── Unmount cleanup ────────────────────────────────────

  useEffect(() => {
    return () => clearTimers();
  }, [clearTimers]);

  // ── Public API ─────────────────────────────────────────

  function startRecording() {
    const backend = backendRef.current;
    if (!backend) return;

    clearTimers();
    dispatch({ type: "SET_VOICE_STATE", payload: "recording" });

    setSecsLeft(maxSecs);
    countdownTimerRef.current = setInterval(() => {
      setSecsLeft((s) => Math.max(0, s - 1));
    }, 1000);
    autoStopTimerRef.current = setTimeout(() => {
      stopAndSend();
    }, maxSecs * 1000);

    backend.recordWithVad(stateRef.current.sessionToken ?? undefined, stateRef.current.sessionId ?? undefined).then((blob) => {
      clearTimers();
      if (blob) {
        dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
        dispatch({
          type: "APPEND_TRANSCRIPT",
          payload: { id: nextTranscriptId(), role: "agent", text: "", timestamp: Date.now() },
        });
        const s = stateRef.current;
        backend.runPipeline(blob, {
          sessionId: s.sessionId ?? undefined,
          authToken: s.sessionToken ?? undefined,
          serverUrl: s.serverUrl || "http://127.0.0.1:4000",
          thinkingTone: thinkingToneRef.current,
        });
      } else {
        dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
        restartWakeListenerRef.current();
      }
    });
  }

  function stopAndSend() {
    clearTimers();
    const backend = backendRef.current;
    if (!backend) return;
    // TODO: abortRecording resolves recordWithVad with null, so nothing is sent; let VAD finish instead.
    backend.abortRecording();
    dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
    restartWakeListenerRef.current();
  }

  function abort() {
    clearTimers();
    const backend = backendRef.current;
    if (!backend) return;
    backend.abortRecording();
    backend.cancelPipeline();
    dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
    restartWakeListenerRef.current();
  }

  function clearConversation() {
    dispatch({ type: "CLEAR_TRANSCRIPT" });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
  }

  return {
    audioLevel,
    wakeWord,
    maxSecs,
    secsLeft,
    startRecording,
    stopAndSend,
    abort,
    clearConversation,
  };
}
