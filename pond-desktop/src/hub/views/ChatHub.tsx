import { useState, useRef, useEffect, useCallback } from "react";
import { WarmupBanner } from "../../components/WarmupBanner";
import { Paperclip } from "lucide-react";
import { api } from "../../api/PondApiClient";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { useChatRun, sendTurn } from "../../state/chatRunStore";
import type { Message } from "../../state/chatRunStore";
import { CONTINUE_TURN_MESSAGE } from "../../api/types";
import type { ContextWarning, TurnStats } from "../../api/types";
import { HubIco, micEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { GooseAvatar } from "./chat/GooseAvatar";
import { TypingIndicator } from "./chat/TypingIndicator";
import { ResultCard } from "./chat/ResultCard";
import type { CardKind } from "./chat/ResultCard";
import { TurnStatsFooter } from "../../components/TurnStatsFooter";
import { ContextPressureNote } from "../../components/ContextPressureNote";
import { SubagentTree } from "../../components/SubagentTree";
import type { SubagentRun } from "../../components/SubagentTree";
import { AttachmentTray } from "../../components/AttachmentTray";
import { prepareImage, validateAttachmentSet } from "../../lib/imageAttach";
import type { PreparedImage } from "../../lib/imageAttach";
import "./chat.css";

// ── Types ─────────────────────────────────────────────────────

/** A rendered row: a projection of the shared `Message` (not a parallel model), or a seed row. */
interface Row {
  id: string;
  who: "user" | "goose";
  text: string;
  card?: CardKind;
  streaming?: boolean;
  turnStats?: TurnStats;
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** Set when the server said the context window is filling. */
  contextWarning?: ContextWarning;
  delegations?: SubagentRun[];
  images?: string[];
}

// ── Constants ─────────────────────────────────────────────────

function makeSeed(userName: string): Row[] {
  const greeting = userName
    ? `Morning, ${userName}. The house is set to Good Morning — lights are easing up and coffee’s brewing. Anything you need?`
    : "Morning! The house is set to Good Morning — lights are easing up and coffee’s brewing. Anything you need?";
  return [
    { id: "seed-0", who: "goose", text: greeting },
    { id: "seed-1", who: "user", text: "What’s the weather looking like?" },
    {
      id: "seed-2",
      who: "goose",
      text: "Partly cloudy and mild today — here’s your day:",
      card: "weather",
    },
    { id: "seed-3", who: "user", text: "Is the front door locked?" },
    {
      id: "seed-4",
      who: "goose",
      text: "Yes — the front door is locked. Tap to toggle it from here:",
      card: "lock",
    },
  ];
}

const CHIPS = [
  "Set Movie Time",
  "Lock everything",
  "Bedroom to 67°",
  "Show the driveway",
  "New sticky note",
];

/** Project a stored message into what this surface renders. */
function toRow(m: Message): Row {
  // One inline card: the last tool a turn called is the one its answer is about.
  const lastTool = m.cards?.[m.cards.length - 1]?.tool;
  return {
    id: String(m.id),
    who: m.role === "user" ? "user" : "goose",
    text: m.text,
    card: lastTool ? toolToCard(lastTool) : undefined,
    streaming: m.streaming,
    turnStats: m.turnStats,
    turnLimit: m.turnLimit,
    contextWarning: m.contextWarning,
    delegations: m.delegations,
    images: m.images,
  };
}

function toolToCard(toolName: string): CardKind | undefined {
  if (!toolName) return undefined;
  const lower = toolName.toLowerCase();
  if (lower.includes("weather")) return "weather";
  if (lower.includes("device") || lower.includes("lock")) return "lock";
  if (lower.includes("routine") || lower.includes("recipe") || lower.includes("scene")) return "movie";
  return undefined;
}

// ── Component ─────────────────────────────────────────────────

export function ChatHubView() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  // In the shared store, so a turn survives the remount on a Hub route change and Chat shows the same one.
  const run = useChatRun();
  const { messages, busy } = run;

  // Presentation only: shown until the first real message, never stored.
  const [seed, setSeed] = useState<Row[]>(() => makeSeed(""));

  // Populate the greeting with the real user name once settings are loaded
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getSettings()
      .then((s) => {
        setSeed(makeSeed(s.user_name?.trim() ?? ""));
        setShowTurnStats(s.show_turn_stats ?? false);
      })
      .catch(() => {});
  }, [state.serverOnline]);
  const [text, setText] = useState("");
  const [showTurnStats, setShowTurnStats] = useState(false);
  const [attachments, setAttachments] = useState<PreparedImage[]>([]);
  const [attachError, setAttachError] = useState<string | null>(null);
  // Fail-open: attaching is disabled only once the model is confirmed to lack vision.
  const [visionCapable, setVisionCapable] = useState(true);
  const [capabilitiesKnown, setCapabilitiesKnown] = useState(false);

  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  const attachDisabled = capabilitiesKnown && !visionCapable;
  const attachTitle = attachDisabled
    ? "The active model cannot read images. Switch to a vision-capable model such as gemma-4-E2B-it."
    : "Attach image";

  const removeAttachment = useCallback((index: number) => {
    setAttachments((prev) => {
      const target = prev[index];
      if (target) URL.revokeObjectURL(target.previewUrl);
      return prev.filter((_, i) => i !== index);
    });
  }, []);

  const addFiles = useCallback(async (files: File[]) => {
    if (files.length === 0) return;
    const prepared: PreparedImage[] = [];
    let firstError: string | null = null;
    for (const file of files) {
      try {
        prepared.push(await prepareImage(file));
      } catch (e) {
        firstError = e instanceof Error ? e.message : "Could not read that image.";
      }
    }
    if (prepared.length === 0) {
      if (firstError) setAttachError(firstError);
      return;
    }
    const capErr = validateAttachmentSet(attachments, prepared);
    if (capErr) {
      prepared.forEach((p) => URL.revokeObjectURL(p.previewUrl));
      setAttachError(capErr);
      return;
    }
    setAttachError(firstError); // surface a partial-batch MIME rejection, if any
    setAttachments((prev) => [...prev, ...prepared]);
  }, [attachments]);

  const fileInputRef = useRef<HTMLInputElement>(null);

  function onAttachClick() {
    fileInputRef.current?.click();
  }

  function onFileInputChange(e: React.ChangeEvent<HTMLInputElement>) {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    void addFiles(files);
  }

  function onPaste(e: React.ClipboardEvent<HTMLInputElement>) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) => f.type.startsWith("image/"));
    if (files.length === 0) return;
    e.preventDefault();
    void addFiles(files);
  }

  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, busy]);

  /** Hands the turn to `chatRunStore`, which owns the stream; this keeps only the composer's part. */
  const sendMessage = useCallback(
    (raw?: string) => {
      const t = (raw ?? text).trim();
      if ((!t && attachments.length === 0) || busy) return;

      setText("");

      // The store owns revoking previewUrls; revoking here would blank the sent message's thumbnail.
      sendTurn({
        text: t,
        images: attachments.map((a) => ({ data: a.data, mime_type: a.mime_type })),
        previewUrls: attachments.map((a) => a.previewUrl),
      });
      setAttachments([]);
      setAttachError(null);
    },
    [text, attachments, busy],
  );

  // Take focus back when the model stops.
  const prevBusyRef = useRef(busy);
  useEffect(() => {
    const wasBusy = prevBusyRef.current;
    prevBusyRef.current = busy;
    if (!busy && wasBusy) inputRef.current?.focus();
  }, [busy]);


  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  function goToVoice() {
    dispatch({ type: "SET_MODE", payload: "voice" });
  }

  const canSend = (text.trim().length > 0 || attachments.length > 0) && !busy;

  const rows: Row[] = messages.length > 0 ? messages.map(toRow) : seed;

  return (
    <div className="chat2">
      <WarmupBanner />
      {/* Header */}
      <header className="chat2__head">
        <div className="chat2__id">
          <GooseAvatar size={40} />
          <div>
            <div className="chat2__name">Goose</div>
            <div className="chat2__status">
              <span className="chat2__dot" aria-hidden="true" />
              On-device &middot; listening
            </div>
          </div>
        </div>
        <button
          className="chat2__voice-btn"
          onClick={goToVoice}
          aria-label="Switch to voice mode"
          title="Voice mode"
        >
          <HubIco d={micEl} size={20} color="#7C3AED" />
        </button>
      </header>

      {/* Thread */}
      <div
        className="chat2__thread"
        ref={scrollRef}
        role="log"
        aria-label="Chat conversation"
        aria-live="polite"
      >
        {rows.map((m) => (
          <div key={m.id} className={`ch-row ch-row--${m.who}`}>
            {m.who === "goose" && <GooseAvatar />}
            <div className="ch-bubble-wrap">
              {/* PAI-6 P6. Live, not gated on `!m.streaming`: a delegating
                  turn is blocked inside one tool call for the whole of its
                  child's run. */}
              {m.who === "goose" && m.delegations && m.delegations.length > 0 && (
                <SubagentTree runs={m.delegations} />
              )}
              <div className={`ch-bubble ch-bubble--${m.who}`}>
                {m.images && m.images.length > 0 && (
                  <div className="ch-bubble__images">
                    {m.images.map((src, i) => (
                      <img key={i} src={src} alt={`Attached image ${i + 1}`} className="ch-bubble__image" />
                    ))}
                  </div>
                )}
                {m.text || (m.streaming ? " " : "")}
              </div>
              {m.card && !m.streaming && (
                <div className="ch-card">
                  <ResultCard kind={m.card} />
                </div>
              )}
              {m.who === "goose" && !m.streaming && m.turnLimit !== undefined && (
                <div className="turn-limit">
                  <span className="turn-limit__note">
                    Stopped after {m.turnLimit} steps.
                  </span>
                  <button
                    className="turn-limit__btn"
                    onClick={() => sendMessage(CONTINUE_TURN_MESSAGE)}
                    disabled={busy}
                  >
                    <HubIco d={HP_PATHS.play} size={12} color="currentColor" /> Continue
                  </button>
                </div>
              )}
              {m.who === "goose" && !m.streaming && m.contextWarning && (
                <ContextPressureNote
                  warning={m.contextWarning}
                  sessionId={run.sessionId ?? null}
                />
              )}
              {m.who === "goose" && !m.streaming && showTurnStats && m.turnStats && (
                <TurnStatsFooter stats={m.turnStats} />
              )}
            </div>
          </div>
        ))}
        {busy && rows[rows.length - 1]?.text === "" && (
          <TypingIndicator />
        )}
      </div>

      {/* Suggestion chips */}
      <div
        className="chat2__chips"
        role="group"
        aria-label="Quick suggestions"
      >
        {CHIPS.map((c) => (
          <button
            key={c}
            className="ch-chip"
            onClick={() => sendMessage(c)}
            disabled={busy}
            type="button"
          >
            {c}
          </button>
        ))}
      </div>

      {/* Pending image attachments */}
      <AttachmentTray attachments={attachments} onRemove={removeAttachment} />
      {attachError && <p className="attach-error" role="alert">{attachError}</p>}

      {/* Input */}
      <div className="chat2__input">
        <button
          className="ch-mic"
          onClick={goToVoice}
          aria-label="Switch to voice mode"
          title="Voice input"
          type="button"
        >
          <HubIco d={micEl} size={19} color="#fff" />
        </button>
        <button
          className="ch-attach"
          onClick={onAttachClick}
          disabled={busy || attachDisabled}
          aria-label="Attach image"
          title={attachTitle}
          type="button"
        >
          <Paperclip size={18} />
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept="image/*"
          multiple
          hidden
          onChange={onFileInputChange}
        />
        <input
          ref={inputRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          onPaste={onPaste}
          placeholder="Message Goose or speak a command…"
          disabled={busy}
          aria-label="Message input"
          autoComplete="off"
          spellCheck={false}
        />
        <button
          className="ch-send"
          onClick={() => sendMessage()}
          disabled={!canSend}
          aria-label="Send message"
          type="button"
        >
          <HubIco d={HP_PATHS.chevR} size={18} color="#fff" sw={2.5} />
        </button>
      </div>
    </div>
  );
}
