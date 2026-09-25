// UI only: delegates to useVoiceSession (desktop shell) or useVoicePipeline (browser).

import React, { useEffect, useRef, useState } from "react";
import { Button } from "@heroui/react";
import { ChevronLeft, Mic, Square, Trash2, Radio, Volume2, RotateCcw } from "lucide-react";
import { VoiceOrb } from "../../components/VoiceOrb";
import { VoiceStream } from "./VoiceStream";
import { api } from "../../api/PondApiClient";
import { TranscriptFeed } from "../../components/TranscriptFeed";
import { VoiceSwitcher } from "../../components/VoiceSwitcher";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { useVoicePipeline } from "./useVoicePipeline";
import { useVoiceSession } from "./useVoiceSession";
import { isDesktopShell } from "../../shell";

// ── CountdownRing (inline SVG) ─────────────────────────────

function CountdownRing({ seconds, maxSeconds }: { seconds: number; maxSeconds: number }) {
  const r = 12;
  const c = 2 * Math.PI * r;
  const frac = maxSeconds > 0 ? seconds / maxSeconds : 0;
  return (
    <svg width={32} height={32} viewBox="0 0 32 32" style={{ flexShrink: 0 }}>
      <circle cx={16} cy={16} r={r} fill="none" stroke="var(--grey-200)" strokeWidth={2.5} />
      <circle
        cx={16} cy={16} r={r} fill="none"
        stroke="var(--color-accent)" strokeWidth={2.5}
        strokeDasharray={c} strokeDashoffset={c * (1 - frac)}
        strokeLinecap="round"
        transform="rotate(-90 16 16)"
        style={{ transition: "stroke-dashoffset 1s linear" }}
      />
      <text x={16} y={16} textAnchor="middle" dominantBaseline="central"
        style={{ fontSize: 9, fontFamily: "var(--font-mono)", fill: "var(--color-text-secondary)" }}>
        {seconds}
      </text>
    </svg>
  );
}

// ── State labels and colors ────────────────────────────────

const STATE_LABELS: Record<string, string> = {
  idle:      "Tap to talk",
  wait:      "Listening for wake word…",
  recording: "Listening…",
  thinking:  "Thinking…",
  speaking:  "Speaking…",
  error:     "Error",
  // connecting is a VoiceMode-local transient label, not a VoiceState
};

const STATE_COLORS: Record<string, string> = {
  idle:      "#8E8E93",
  wait:      "#8C4BFF",
  recording: "#8C4BFF",
  thinking:  "#FF9500",
  speaking:  "#34C759",
  error:     "#FF3B30",
  connecting: "#8E8E93",
};

// ── VoiceMode (Tauri child-process path) ──────────────────

function VoiceModeChildProcess() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const session = useVoiceSession();

  const { voiceState } = state;
  const isConnecting = session.connecting;
  const isError     = voiceState === "error";
  const serverDown  = !state.serverOnline;
  const [isSwitcherOpen, setSwitcherOpen] = useState(false);

  // Flash the orb on wait -> recording so wake-word detection is visible, not just a label change.
  const prevVoiceStateRef = useRef(voiceState);
  const [pulseKey, setPulseKey] = useState(0);
  useEffect(() => {
    if (prevVoiceStateRef.current === "wait" && voiceState === "recording") {
      setPulseKey((k) => k + 1);
    }
    prevVoiceStateRef.current = voiceState;
  }, [voiceState]);

  // The stop is deferred a tick so a fast remount (StrictMode double-invoke, quick re-entry) can
  // cancel it instead of killing the child and respawning it, which reloads the models.
  const pendingStopRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ── Conversation name ──────────────────────────────────────
  // The server names a session a turn or two in, hence the per-turn refetch; never show the uuid.
  const [title, setTitle] = useState<string | null>(null);
  const turnCount = state.transcript.filter((m) => m.role === "user").length;

  useEffect(() => {
    const sid = state.sessionId;
    if (!sid) { setTitle(null); return; }
    let cancelled = false;
    api.listSessions()
      .then((sessions) => {
        if (cancelled) return;
        const mine = sessions.find((s) => s.id === sid);
        const named = mine?.title?.trim();
        setTitle(named ? named : null);
      })
      .catch(() => { /* offline — the fallback heading still reads correctly */ });
    return () => { cancelled = true; };
  }, [state.sessionId, turnCount]);

  useEffect(() => {
    if (pendingStopRef.current !== null) {
      // Remounted before the deferred stop fired: keep the running session.
      clearTimeout(pendingStopRef.current);
      pendingStopRef.current = null;
    } else {
      session.startSession();
    }

    return () => {
      pendingStopRef.current = setTimeout(() => {
        pendingStopRef.current = null;
        session.stopSession();
      }, 0);
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const stateLabel = serverDown
    ? "Server offline"
    : session.warmingUp
      ? "Warming up…"
      : isConnecting
        ? "Starting voice session…"
        : isError && state.voiceError
          ? state.voiceError
          : STATE_LABELS[voiceState] ?? "Tap to talk";

  const stateColor = isConnecting
    ? STATE_COLORS.connecting
    : STATE_COLORS[voiceState] ?? "#8E8E93";

  function backToGui() {
    dispatch({ type: "SET_MODE", payload: "gui" });
  }

  return (
    <div className="vm-root">

      {/* Header — no rule beneath it. The screen is one continuous field, and
          a divider here cut the conversation into stacked panels. */}
      <div className="vm-header">
        <Button variant="ghost" size="sm" onPress={backToGui} aria-label="Back">
          <ChevronLeft size={14} /> Back
        </Button>
        <div className="vm-header__actions">
          <Button
            variant="ghost" size="sm"
            onPress={() => setSwitcherOpen((o) => !o)}
            aria-label="Switch voice"
          >
            <Volume2 size={13} />
          </Button>
          <VoiceSwitcher isOpen={isSwitcherOpen} onClose={() => setSwitcherOpen(false)} />
          <Button
            variant="ghost" size="sm"
            onPress={session.clearConversation}
            isDisabled={state.transcript.length === 0}
            aria-label="Clear conversation"
          >
            <Trash2 size={13} />
          </Button>
        </div>
      </div>

      <h1 className="vm-title" title={title ?? undefined}>
        {title ?? "New conversation"}
      </h1>

      {/* The field: the conversation the orb sits in. One scroll container,
          three columns — what the pond said on the left, what you said on the
          right, and a reserved channel down the middle the orb occupies. The
          conversation parts around it instead of being filed away in a drawer. */}
      <div className="vm-field">
        <div className="vm-orb-layer" aria-hidden={false}>
          <VoiceOrb
            state={isConnecting ? "idle" : voiceState}
            size="xl"
            audioLevel={session.audioLevel}
            pulseKey={pulseKey}
          />

          <div className="vm-state-row">
            {isConnecting && (
              <Radio size={14} style={{ color: "var(--color-accent)", opacity: 0.8 }} />
            )}
            <span className="vm-state-label" style={{ color: stateColor }}>
              {stateLabel}
            </span>
          </div>
        </div>

        <VoiceStream
          messages={state.transcript}
          contextCards={state.contextCards}
        />
      </div>

      {/* Action bar */}
      <div className="vm-action-bar">
        {isError && (
          <>
            <Button
              variant="ghost"
              onPress={() => {
                dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                dispatch({ type: "SET_VOICE_ERROR", payload: null });
              }}
            >
              Dismiss
            </Button>
            <Button
              variant="ghost"
              onPress={() => session.startSession()}
            >
              <RotateCcw size={13} /> Retry
            </Button>
          </>
        )}

        {!isConnecting && !isError && session.sessionActive && (
          <Button
            variant="ghost" size="sm"
            onPress={() => session.stopSession()}
          >
            <Square size={13} fill="currentColor" /> Stop session
          </Button>
        )}

        <p className="vm-hint">
          {/* &#8984; and &#8679;, not &lceil;/&Sigma; — the previous entities
              rendered as the literal maths symbols "⌈ΣV". */}
          <kbd className="vm-kbd">&#8984;&#8679;V</kbd>
          {" / "}
          <kbd className="vm-kbd">Ctrl+Shift+V</kbd>
          {" to activate"}
        </p>
      </div>
    </div>
  );
}

// ── VoiceMode (browser HTTP pipeline path) ─────────────────

function VoiceModePipeline() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const pipeline = useVoicePipeline();

  const { voiceState } = state;
  const isRecording = voiceState === "recording";
  const isThinking  = voiceState === "thinking";
  const isSpeaking  = voiceState === "speaking";
  const isIdle      = voiceState === "idle";
  const isWaiting   = voiceState === "wait";
  const isError     = voiceState === "error";
  const serverDown  = !state.serverOnline;
  const stateColor  = STATE_COLORS[voiceState] ?? "#8E8E93";
  const stateLabel  = serverDown
    ? "Server offline"
    : isError && state.voiceError
      ? state.voiceError
      : STATE_LABELS[voiceState];

  const lastMsg = state.transcript.length > 0
    ? state.transcript[state.transcript.length - 1]
    : null;
  const showCaption = lastMsg !== null && (isRecording || isThinking || isSpeaking);

  function backToGui() {
    dispatch({ type: "SET_MODE", payload: "gui" });
  }

  return (
    <div className="vm-root">

      {/* Header */}
      <div className="vm-header">
        <Button variant="ghost" size="sm" onPress={backToGui} aria-label="Back">
          <ChevronLeft size={14} /> Back
        </Button>
        {state.sessionId && (
          <span className="vm-session-hint">Session {state.sessionId.slice(0, 6)}</span>
        )}
        <Button
          variant="ghost" size="sm"
          onPress={pipeline.clearConversation}
          isDisabled={state.transcript.length === 0}
          aria-label="Clear conversation"
        >
          <Trash2 size={13} />
        </Button>
      </div>

      {/* Stage: orb + state label + live caption */}
      <div className="vm-stage">
        <VoiceOrb state={voiceState} size="xl" audioLevel={pipeline.audioLevel} />

        <div className="vm-state-row">
          {isRecording && (
            <CountdownRing seconds={pipeline.secsLeft} maxSeconds={pipeline.maxSecs} />
          )}
          <span className="vm-state-label" style={{ color: stateColor }}>
            {stateLabel}
          </span>
        </div>

        {showCaption && (
          <p className={`vm-live-caption vm-live-caption--${lastMsg!.role}`}>
            {lastMsg!.text || "…"}
          </p>
        )}
      </div>

      {/* Transcript panel */}
      {state.transcript.length > 0 && (
        <div className="vm-transcript">
          <TranscriptFeed
            messages={state.transcript}
            contextCards={state.contextCards}
            compact
          />
        </div>
      )}

      {/* Action bar */}
      <div className="vm-action-bar">
        {isIdle && (
          <Button
            variant="primary"
            onPress={pipeline.startRecording}
            isDisabled={serverDown}
            className="vm-primary-btn"
          >
            <Mic size={16} /> Start Listening
          </Button>
        )}

        {isWaiting && (
          <div className="vm-wait-row">
            <span className="vm-wait-indicator">
              <span className="vm-wait-pulse" />
              Listening for &ldquo;{pipeline.wakeWord}&rdquo;
            </span>
            <Button variant="ghost" size="sm" onPress={pipeline.startRecording} isDisabled={serverDown}>
              <Mic size={13} /> Record now
            </Button>
          </div>
        )}

        {isRecording && (
          <div className="vm-recording-row">
            <Button variant="ghost" onPress={pipeline.stopAndSend} className="vm-stop-btn">
              <Square size={14} fill="#FF3B30" color="#FF3B30" /> Send
            </Button>
            <Button variant="ghost" size="sm" onPress={pipeline.abort} className="vm-abort-btn">
              Cancel
            </Button>
          </div>
        )}

        {isThinking && (
          <div className="vm-busy-row">
            <span className="vm-busy-dots">&bull;&bull;&bull;</span>
            <span className="vm-busy-label">Thinking&hellip;</span>
          </div>
        )}

        {isSpeaking && (
          <Button variant="ghost" onPress={pipeline.abort} className="vm-interrupt-btn">
            Interrupt
          </Button>
        )}

        {isError && (
          <Button
            variant="ghost"
            onPress={() => {
              dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
              dispatch({ type: "SET_VOICE_ERROR", payload: null });
            }}
          >
            Dismiss
          </Button>
        )}

        <p className="vm-hint">
          {/* &#8984; and &#8679;, not &lceil;/&Sigma; — the previous entities
              rendered as the literal maths symbols "⌈ΣV". */}
          <kbd className="vm-kbd">&#8984;&#8679;V</kbd>
          {" / "}
          <kbd className="vm-kbd">Ctrl+Shift+V</kbd>
          {" to activate"}
        </p>
      </div>
    </div>
  );
}

// ── Public export: selects the correct path at runtime ──────

export function VoiceMode() {
  // Test the bridge itself, not a framework global: sniffing one misroutes silently when the shell changes.
  if (isDesktopShell()) {
    return <VoiceModeChildProcess />;
  }
  return <VoiceModePipeline />;
}
