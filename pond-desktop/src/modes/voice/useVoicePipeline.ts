// ────────────────────────────────────────────────────────────
// useVoicePipeline — Orchestration hook for the voice mode
//
// Owns the state machine, creates the VoiceBackend, wires
// callbacks to AppContext dispatch, and manages conversational
// turn-taking, countdown timers, and silence detection.
// ────────────────────────────────────────────────────────────

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

  // Keep latest state accessible without stale closures
  const stateRef = useRef(state);
  stateRef.current = state;

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

      // Wire callbacks → dispatch
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
        });
      };

      backend.onWakeInterrupt = () => {
        // Barge-in: pipeline was cancelled by wake listener
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      };

      backend.onDismissed = (isExit) => {
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        // If soft dismissal and wake word configured, return to wait
        if (!isExit && wakeWord) {
          setTimeout(() => dispatch({ type: "SET_VOICE_STATE", payload: "wait" }), 500);
        }
      };
    });

    return () => {
      destroyed = true;
      backendRef.current?.destroy();
      backendRef.current = null;
    };
    // Re-create if serverUrl changes
  }, [state.serverUrl]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Load settings (wake word, max duration) ────────────

  useEffect(() => {
    api.getSettings().then((s) => {
      const dur = (s as Record<string, unknown>).voice_recording_duration_secs;
      if (typeof dur === "number" && dur > 0) { setMaxSecs(dur); setSecsLeft(dur); }

      const ww = (s as Record<string, unknown>).voice_wake_word;
      if (typeof ww === "string" && ww.trim()) setWakeWord(ww.trim());
    }).catch(() => {});
  }, []);

  // ── Wake word listener management ──────────────────────

  useEffect(() => {
    const backend = backendRef.current;
    if (!backend || !wakeWord) return;

    // Load calibrated variants
    api.getSettings().then((s) => {
      const variants: string[] = (s as Record<string, unknown>).voice_wake_word_transcriptions as string[] ?? [];
      backend.startWakeListener(wakeWord, variants);
      dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
    }).catch(() => {
      backend.startWakeListener(wakeWord, []);
      dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
    });

    return () => { backend.stopWakeListener(); };
  }, [wakeWord, dispatch]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Conversational turn-taking ─────────────────────────
  // After Goose finishes speaking, auto-start recording.
  // If no speech within 8s, return to wake listening or idle.

  useEffect(() => {
    const prev = prevStateRef.current;
    const curr = state.voiceState;
    prevStateRef.current = curr;

    if (prev === "speaking" && curr === "idle") {
      // Goose finished speaking — auto-listen for next turn
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
          });
        } else {
          // No speech detected — return to wake listening or idle
          dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
        }
      });

      // 8s no-speech timeout
      if (noSpeechTimerRef.current) clearTimeout(noSpeechTimerRef.current);
      noSpeechTimerRef.current = setTimeout(() => {
        if (stateRef.current.voiceState === "recording") {
          backend.abortRecording();
          dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
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

    // Start countdown
    setSecsLeft(maxSecs);
    countdownTimerRef.current = setInterval(() => {
      setSecsLeft((s) => Math.max(0, s - 1));
    }, 1000);
    autoStopTimerRef.current = setTimeout(() => {
      stopAndSend();
    }, maxSecs * 1000);

    // VAD recording
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
        });
      } else {
        dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
      }
    });
  }

  function stopAndSend() {
    // Force-stop recording and send what we have
    clearTimers();
    const backend = backendRef.current;
    if (!backend) return;
    // abortRecording will trigger the recordWithVad promise to resolve with null
    // We need a different approach — just let VAD naturally resolve
    backend.abortRecording();
    dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
  }

  function abort() {
    clearTimers();
    const backend = backendRef.current;
    if (!backend) return;
    backend.abortRecording();
    backend.cancelPipeline();
    dispatch({ type: "SET_VOICE_STATE", payload: wakeWord ? "wait" : "idle" });
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
