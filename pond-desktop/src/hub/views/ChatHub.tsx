import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import { WarmupBanner } from "../../components/WarmupBanner";
import { Paperclip } from "lucide-react";
import { api } from "../../api/PondApiClient";
import { useAppState, useAppDispatch } from "../../state/useAppContext";
import { useChatRun, sendTurn, takeRefusedDraft } from "../../state/chatRunStore";
import type { Message } from "../../state/chatRunStore";
import { CONTINUE_TURN_MESSAGE } from "../../api/types";
import type { ContextWarning, TurnStats } from "../../api/types";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS, micEl } from "../primitives/icons";
import { GooseAvatar } from "./chat/GooseAvatar";
import { TypingIndicator } from "./chat/TypingIndicator";
import { ResultCard } from "./chat/ResultCard";
import type { CardKind } from "./chat/ResultCard";
import { WebResultCard, webCardOf } from "./chat/WebResultCard";
import type { ContextCard } from "../../state/reducer";
import { TurnStatsFooter } from "../../components/TurnStatsFooter";
import { ContextPressureNote } from "../../components/ContextPressureNote";
import { SubagentTree } from "../../components/SubagentTree";
import { LongTaskCards, LongTaskStrays } from "../../longtasks/LongTaskCards";
import { useLongTasks } from "../../longtasks/useLongTasks";
import { KeptResultNote } from "../../components/KeptResultNote";
import type { KeptResult } from "../../state/keptResults";
import type { SubagentRun } from "../../components/subagentRuns";
import { AttachmentTray } from "../../components/AttachmentTray";
import { ImageSupportStatus } from "../../components/ImageSupportStatus";
import {
  ATTACH_BLOCKED_COPY,
  COMPOSER_GATE_LINE,
  NOT_DECLARED_COPY,
  refusalClientClause,
} from "../../components/imageSupportCopy";
import { useVisionStatus } from "../../api/useVisionStatus";
import { prepareImage, validateAttachmentSet } from "../../lib/imageAttach";
import type { PreparedImage } from "../../lib/imageAttach";
import { useSuggestedPrompts } from "../../hooks/useSuggestedPrompts";
import { useConversationModel } from "../../hooks/useConversationModel";
import { NoModelPicks } from "../../components/models/NoModelPicks";
import "./chat.css";

// ── Types ─────────────────────────────────────────────────────

/** A rendered row: a projection of the shared `Message` (not a parallel model), or a seed row. */
interface Row {
  id: string;
  who: "user" | "goose";
  text: string;
  card?: CardKind;
  /** The web's results for the person, when the turn's last tool brought them. */
  webCard?: ContextCard;
  streaming?: boolean;
  turnStats?: TurnStats;
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** Set when the server said the context window is filling. */
  contextWarning?: ContextWarning;
  delegations?: SubagentRun[];
  longTaskIds?: string[];
  images?: string[];
  keptResults?: KeptResult[];
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

/** Project a stored message into what this surface renders. */
function toRow(m: Message): Row {
  // One inline card: the last tool a turn called is the one its answer is about.
  const last = m.cards?.[m.cards.length - 1];
  const lastTool = last?.tool;
  return {
    id: String(m.id),
    who: m.role === "user" ? "user" : "goose",
    text: m.text,
    card: lastTool ? toolToCard(lastTool) : undefined,
    webCard: webCardOf(last) ?? undefined,
    streaming: m.streaming,
    turnStats: m.turnStats,
    turnLimit: m.turnLimit,
    contextWarning: m.contextWarning,
    delegations: m.delegations,
    longTaskIds: m.longTaskIds,
    images: m.images,
    keptResults: m.keptResults,
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

  const startedTasks = useMemo(() => messages.flatMap((m) => m.longTaskIds ?? []), [messages]);
  const startedTaskSet = useMemo(() => new Set(startedTasks), [startedTasks]);
  const longTasks = useLongTasks(run.sessionId ?? null, startedTasks);

  const chips = useSuggestedPrompts(state.sessionId);
  const conversationModel = useConversationModel();

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
  // Primary attach gate; the `capabilities.vision` probe is the fallback while this is unknown.
  const { status: visionStatus, refresh: refreshVisionStatus } = useVisionStatus();
  // Set when they reach for the paperclip or paste: only then does a permanent reason show.
  const [attachReasonShown, setAttachReasonShown] = useState(false);

  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  const visionKind = visionStatus?.state.kind;
  const visionKnown = !!visionStatus && visionKind !== "unknown";
  // Until the vision status is known, fall back to the capabilities probe rather than fail open.
  const gateBlocked = visionKnown
    ? visionKind !== "ready"
    : capabilitiesKnown && !visionCapable;
  const attachTitle = !gateBlocked
    ? "Attach image"
    : (visionStatus?.message ??
        (visionKind === "not_declared" ? NOT_DECLARED_COPY : ATTACH_BLOCKED_COPY));

  // A refused turn (409/413/415...) hands its draft back here, once: `takeRefusedDraft` clears
  // it, so a StrictMode re-run can't restore it twice.
  useEffect(() => {
    if (!run.refusedDraft) return;
    const draft = takeRefusedDraft();
    if (!draft) return;
    if (!text.trim()) setText(draft.text);
    setAttachments(draft.attachments);
    setAttachError(draft.message + refusalClientClause(draft.code));
    if (draft.code === "no_model") conversationModel.markNone();
    refreshVisionStatus();
    // `text` excluded: once per refusal, not per keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run.refusedDraft]);

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

  // Never disabled for vision: a disabled button's title can't reach touch, so a tap shows why.
  function onAttachClick() {
    if (gateBlocked) setAttachReasonShown(true);
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
    if (gateBlocked) {
      setAttachReasonShown(true);
      setAttachError(COMPOSER_GATE_LINE);
      return;
    }
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
      // Gated here, not on Send: every way of sending (button, Enter, chip, Continue) lands here.
      if (attachments.length > 0 && gateBlocked) {
        setAttachError(COMPOSER_GATE_LINE);
        return;
      }

      setText("");

      // The store owns revoking previewUrls; revoking here would blank the sent message's thumbnail.
      sendTurn({ text: t, attachments });
      setAttachments([]);
      setAttachError(null);
    },
    [text, attachments, busy, gateBlocked],
  );

  // Take focus back when the model stops.
  const prevBusyRef = useRef(busy);
  useEffect(() => {
    const wasBusy = prevBusyRef.current;
    prevBusyRef.current = busy;
    if (!busy && wasBusy) {
      inputRef.current?.focus();
      if (conversationModel.state === "none") conversationModel.refresh();
    }
  }, [busy, conversationModel]);

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

  const noModel = conversationModel.state === "none";
  // With no model chosen the sample conversation would be a promise the pond cannot keep.
  const rows: Row[] = messages.length > 0 ? messages.map(toRow) : noModel ? [] : seed;

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
              {m.who === "goose" && <LongTaskCards ids={m.longTaskIds} longTasks={longTasks} variant="hub" />}
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
              {m.who === "goose" && m.keptResults?.map((kept, i) => (
                <KeptResultNote key={`${kept.callId}-${i}`} result={kept} />
              ))}
              {m.card && !m.streaming && (
                <div className="ch-card">
                  <ResultCard kind={m.card} />
                </div>
              )}
              {m.who === "goose" && m.webCard && !m.streaming && (
                <div className="ch-card">
                  <WebResultCard card={m.webCard} />
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
        {messages.length > 0 && !run.loadingSession && (
          <LongTaskStrays anchored={startedTaskSet} longTasks={longTasks} variant="hub" />
        )}
        {busy && rows[rows.length - 1]?.text === "" && (
          <TypingIndicator />
        )}
        {noModel && <NoModelPicks />}
      </div>

      {/* Suggestion chips: a turn without a model would be refused, so none are offered. */}
      {!noModel && <div
        className="chat2__chips"
        role="group"
        aria-label="Quick suggestions"
      >
        {chips.map((c) => (
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
      </div>}

      {/* Picture support's own status, then any pending attachments. */}
      <ImageSupportStatus status={visionStatus} revealed={attachReasonShown} />
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
          disabled={busy}
          aria-disabled={gateBlocked || undefined}
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
