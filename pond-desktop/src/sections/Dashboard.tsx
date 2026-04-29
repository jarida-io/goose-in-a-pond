import { useState, useEffect } from "react";
import {
  Button,
  Card,
  CardContent,
  Switch,
  Separator,
  Kbd,
} from "@heroui/react";
import {
  Mic,
  MessageSquare,
  Cpu,
  Settings,
  FileText,
  Clock,
  RefreshCw,
  ChevronRight,
} from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { api } from "../api/PondApiClient";
import type { SessionSummary } from "../api/types";

/* ── Helpers ─────────────────────────────────────────────────────────────────── */

/** Abbreviate "org/model-name-long" to "model-name-long", max 32 chars. */
function abbreviateModel(name: string): string {
  const bare = name.includes("/") ? name.split("/").pop() ?? name : name;
  return bare.length > 32 ? bare.slice(0, 29) + "\u2026" : bare;
}

/** Human-friendly token count: 812 -> "812", 4200 -> "~4.2k", 14000 -> "~14k" */
function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10000) return "~" + (n / 1000).toFixed(1) + "k";
  return "~" + Math.round(n / 1000) + "k";
}

/** Heights for each VU bar (tallest in the middle). */
const VU_BAR_HEIGHTS = [8, 14, 20, 28, 20, 14, 8];
const VU_BAR_COUNT = VU_BAR_HEIGHTS.length;

/* ── Component ───────────────────────────────────────────────────────────────── */

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  /* Recent chat sessions */
  const [recentSessions, setRecentSessions] = useState<SessionSummary[]>([]);
  useEffect(() => {
    if (!state.serverOnline || !state.sessionToken) return;
    let cancelled = false;
    api.listSessions()
      .then((sessions) => {
        if (cancelled) return;
        const sorted = [...sessions]
          .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
          .slice(0, 5);
        setRecentSessions(sorted);
      })
      .catch(() => {});
    return () => { cancelled = true; };
  }, [state.serverOnline, state.sessionToken]);

  function openSession(id: string) {
    dispatch({ type: "SET_SESSION_ID", payload: id });
    dispatch({ type: "SET_SECTION", payload: "chat" });
  }

  /* VU meter animation state */
  const [vuLevel, setVuLevel] = useState(0);

  const voiceEnabled =
    state.voiceState === "recording" ||
    state.voiceState === "wait" ||
    state.voiceState === "thinking" ||
    state.voiceState === "speaking";

  useEffect(() => {
    if (!voiceEnabled) {
      setVuLevel(0);
      return;
    }
    const id = setInterval(() => {
      setVuLevel(Math.floor(Math.random() * (VU_BAR_COUNT + 1)));
    }, 220);
    return () => clearInterval(id);
  }, [voiceEnabled]);

  /* Resolve voice state label */
  const voiceLabel = voiceEnabled
    ? state.voiceState.toUpperCase()
    : state.voiceState === "idle"
      ? "IDLE"
      : "OFF";

  /* Active model role data (fall back to mock when no response yet) */
  const activeModel = state.lastResponseMeta
    ? abbreviateModel(state.lastResponseMeta.modelName)
    : "gemma-4-E4B";

  const roles: Array<{
    label: string;
    variant: "secondary" | "warning" | "success";
    model: string;
  }> = [
    { label: "Chat", variant: "secondary", model: activeModel },
    { label: "Think", variant: "warning", model: activeModel },
    { label: "Task", variant: "success", model: activeModel },
  ];

  return (
    <div className="screen">
      {/* ── Page header ──────────────────────────────────────── */}
      <div className="page-header">
        <h1 className="page-header__title">Dashboard</h1>
      </div>

      {/* ── Voice card ───────────────────────────────────────── */}
      <Card shadow="none" className="card--voice">
        <CardContent>
          <div className="voice-card">
            {/* Left: icon + text */}
            <div className="voice-card__main">
              <div className="voice-card__icon">
                <Mic size={20} />
              </div>
              <div className="voice-card__text">
                <h3>Voice Mode</h3>
                <p>
                  Talk to Pond hands-free. Uses your mic, whisper transcription,
                  and a local TTS voice.
                </p>
                <span className="voice-card__hint">
                  Press <Kbd>&#8984;&#8679;V</Kbd> or{" "}
                  <Kbd>Ctrl+Shift+V</Kbd> from anywhere
                </span>
              </div>
            </div>

            {/* Right: VU meter + state + switch */}
            <div className="voice-card__right">
              <div className="vu">
                {VU_BAR_HEIGHTS.map((h, i) => (
                  <div
                    key={i}
                    className={`vu__bar${i < vuLevel ? " is-lit" : ""}`}
                    style={{ height: h }}
                  />
                ))}
              </div>
              <span className="voice-card__state">{voiceLabel}</span>
              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  cursor: "pointer",
                }}
              >
                <span className="voice-card__switch-label">Hands-free</span>
                <Switch
                  size="sm"
                  isSelected={voiceEnabled}
                  onValueChange={() => {
                    if (voiceEnabled) {
                      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                    } else {
                      dispatch({ type: "VOICE_ACTIVATE" });
                    }
                  }}
                />
              </label>
            </div>
          </div>

          {/* CTA buttons */}
          <div className="voice-card__cta">
            <Button
              size="sm"
              color="secondary"
              isDisabled={!state.serverOnline}
              onPress={() => dispatch({ type: "SET_MODE", payload: "voice" })}
            >
              <Mic size={14} /> Open Voice Mode
            </Button>
            <Button
              size="sm"
              variant="flat"
              onPress={() => dispatch({ type: "SET_SECTION", payload: "settings" })}
            >
              <Settings size={14} /> Voice settings
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* ── Dashboard grid (status + quick actions) ──────────── */}
      <div className="dash-grid">
        {/* Server status card */}
        <Card shadow="none" className="giap-card">
          <CardContent>
            <div className="card-header" style={{ padding: 0 }}>
              <span className="card__label">Server status</span>
            </div>

            <div className="server-row" style={{ marginTop: 10 }}>
              <div className="server-row__state">
                <span
                  className="server-row__dot"
                  style={{
                    background: state.serverOnline
                      ? "var(--color-success)"
                      : state.serverStarting
                        ? "var(--color-warning)"
                        : "var(--grey-400)",
                    boxShadow: state.serverOnline
                      ? "0 0 0 3px rgba(52,199,89,0.15)"
                      : "none",
                  }}
                />
                <span>
                  {state.serverOnline
                    ? "Connected"
                    : state.serverStarting
                      ? "Starting\u2026"
                      : "Offline"}
                </span>
              </div>
              <code className="server-row__url" style={{ fontSize: 12 }}>
                {state.serverUrl}
              </code>
            </div>

            <Separator className="card__divider" />

            <div className="metrics">
              <div className="metric">
                <div className="metric__label">Uptime</div>
                <div className={`metric__value${state.serverOnline ? " metric__value--ok" : ""}`}>
                  {state.serverOnline ? "Online" : "--"}
                </div>
              </div>
              <div className="metric">
                <div className="metric__label">Latency</div>
                <div className="metric__value">
                  {state.serverOnline ? "<10ms" : "--"}
                </div>
              </div>
              <div className="metric">
                <div className="metric__label">Memory</div>
                <div className="metric__value">
                  {state.serverOnline ? "Normal" : "--"}
                </div>
              </div>
              <div className="metric">
                <div className="metric__label">Active model</div>
                <div className="metric__value">
                  {state.lastResponseMeta
                    ? abbreviateModel(state.lastResponseMeta.modelName)
                    : state.serverOnline
                      ? "gemma-4-E4B"
                      : "--"}
                </div>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Quick actions card */}
        <Card shadow="none" className="giap-card">
          <CardContent>
            <div className="card-header" style={{ padding: 0 }}>
              <span className="card__label">Quick actions</span>
            </div>

            <div className="quick-actions">
              <button
                className="quick-action"
                onClick={() => dispatch({ type: "SET_SECTION", payload: "chat" })}
              >
                <span className="quick-action__icon">
                  <MessageSquare size={15} />
                </span>
                <span className="quick-action__label">Open Chat</span>
                <ChevronRight size={14} className="quick-action__arrow" />
              </button>

              <button
                className="quick-action"
                onClick={() => dispatch({ type: "SET_SECTION", payload: "models" })}
              >
                <span className="quick-action__icon">
                  <Cpu size={15} />
                </span>
                <span className="quick-action__label">Manage Models</span>
                <ChevronRight size={14} className="quick-action__arrow" />
              </button>

              <button
                className="quick-action"
                onClick={() => dispatch({ type: "SET_SECTION", payload: "prompts" })}
              >
                <span className="quick-action__icon">
                  <FileText size={15} />
                </span>
                <span className="quick-action__label">Edit Prompts</span>
                <ChevronRight size={14} className="quick-action__arrow" />
              </button>

              <button
                className="quick-action"
                onClick={() =>
                  dispatch({ type: "SET_SECTION", payload: "schedules" })
                }
              >
                <span className="quick-action__icon">
                  <Clock size={15} />
                </span>
                <span className="quick-action__label">Schedules</span>
                <ChevronRight size={14} className="quick-action__arrow" />
              </button>
            </div>
          </CardContent>
        </Card>
      </div>

      {/* ── Active model roles ───────────────────────────────── */}
      <Card shadow="none" className="giap-card">
        <CardContent>
          <div
            className="card-header"
            style={{ padding: 0, marginBottom: 10 }}
          >
            <span className="card__label">Active model roles</span>
            <div className="card-header__right">
              <Button
                isIconOnly
                size="sm"
                variant="light"
                aria-label="Refresh roles"
              >
                <RefreshCw size={14} />
              </Button>
            </div>
          </div>

          <div className="role-grid">
            {roles.map((r) => (
              <div
                key={r.label}
                className={`role-chip role-chip--${r.variant}`}
              >
                <div className="role-chip__bar" />
                <div className="role-chip__body">
                  <div className="role-chip__head">
                    <span className="role-chip__role">{r.label}</span>
                  </div>
                  <code style={{ fontSize: 12 }}>{r.model}</code>
                </div>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>

      {/* ── Recent conversations ─────────────────────────────── */}
      {state.serverOnline && recentSessions.length > 0 && (
        <Card shadow="none" className="giap-card">
          <CardContent>
            <div className="card-header" style={{ padding: 0, marginBottom: 10 }}>
              <span className="card__label">Recent conversations</span>
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              {recentSessions.map((s) => {
                const ago = timeAgo(s.updated_at);
                return (
                  <button
                    key={s.id}
                    onClick={() => openSession(s.id)}
                    className="quick-action"
                    style={{ textAlign: "left" }}
                  >
                    <span className="quick-action__icon"><MessageSquare size={14} /></span>
                    <span className="quick-action__label" style={{ flex: 1 }}>
                      {s.title || `Session ${s.id.slice(0, 8)}`}
                    </span>
                    <span style={{ fontSize: 11, color: "var(--grey-500)", whiteSpace: "nowrap" }}>{ago}</span>
                    <ChevronRight size={14} className="quick-action__arrow" />
                  </button>
                );
              })}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return `${days}d ago`;
}
