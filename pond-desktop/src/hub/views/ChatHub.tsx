import { useState, useRef, useEffect, useCallback } from "react";
import { WarmupBanner } from "../../components/WarmupBanner";
import { Paperclip } from "lucide-react";
import { api } from "../../api/PondApiClient";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { useChatRun, sendTurn, takeRefusedDraft } from "../../state/chatRunStore";
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
import {
  ImageSupportStatus,
  COMPOSER_GATE_LINE,
  refusalClientClause,
} from "../../components/ImageSupportStatus";
import { useVisionStatus } from "../../api/useVisionStatus";
import { prepareImage, validateAttachmentSet } from "../../lib/imageAttach";
import type { PreparedImage } from "../../lib/imageAttach";
import { useSuggestedPrompts } from "../../hooks/useSuggestedPrompts";
import "./chat.css";

// ── Types ─────────────────────────────────────────────────────

/**
 * One rendered row.
 *
 * The Hub shows fewer things about a turn than the Chat section does -- no
 * thinking disclosure, no per-message actions, one inline card rather than a
 * list of tool chips -- so it renders a PROJECTION of the shared `Message`
 * rather than keeping a parallel model. Keeping two models was how the two
 * surfaces came to disagree about what a frame means.
 *
 * The seed rows below are the other reason this type exists: they are
 * presentation, not conversation, so they never enter the store.
 */
interface Row {
  id: string;
  who: "user" | "goose";
  text: string;
  card?: CardKind;
  streaming?: boolean;
  turnStats?: TurnStats;
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** Set when the server said the context window is filling (PAI-4 P7b). */
  contextWarning?: ContextWarning;
  /** PAI-6 P6. Delegations this turn started. */
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


/** Project a stored message into what this surface renders. */
function toRow(m: Message): Row {
  // The Hub shows ONE inline card, and the last tool a turn called is the one
  // its answer is about -- an answer that checked the weather and then the
  // locks is about the locks.
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

// Resolve tool call to an inline card kind
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

  // The conversation lives in the shared store, so a turn started here keeps
  // running when the Hub changes route -- which remounts this whole subtree --
  // and the Chat section shows the same conversation rather than a second one.
  const run = useChatRun();
  const { messages, busy } = run;

  // The composer's chips, grounded. See `useSuggestedPrompts` for why the five
  // hardcoded ones went: three of them named hardware a pond may not own.
  const chips = useSuggestedPrompts(state.sessionId);

  // Presentation, not conversation: an empty pond opens on something to read
  // rather than a blank pane. The seed is replaced by the first real message
  // and never enters the store.
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
  // Fail-open: an unknown/failed capabilities fetch never disables attaching —
  // it only disables once we've SUCCESSFULLY confirmed the model lacks vision.
  const [visionCapable, setVisionCapable] = useState(true);
  const [capabilitiesKnown, setCapabilitiesKnown] = useState(false);
  // Picture support's own lifecycle — download progress, readiness, a device
  // that declines the encoder entirely. `useVisionStatus` replaces this
  // fetch-once probe as the primary attach decision; `capabilities.vision`
  // below is now only the fallback while a per-model answer is unknown.
  const { status: visionStatus, refresh: refreshVisionStatus } = useVisionStatus();
  // Whether the household has just now reached for the paperclip or tried to
  // paste — the only moment a PERMANENT reason (not_declared /
  // not_on_this_device) earns a line; see ImageSupportStatus.
  const [attachReasonShown, setAttachReasonShown] = useState(false);

  // Load vision capability once the server is reachable.
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  const visionKind = visionStatus?.state.kind;
  const visionKnown = !!visionStatus && visionKind !== "unknown";
  // gate.blocked: status known && kind !== "ready". While the richer status
  // is unknown, fall back to the coarser capabilities probe rather than
  // failing open outright — a model this pond has already confirmed cannot
  // see pictures should not be offered as if it could.
  const gateBlocked = visionKnown
    ? visionKind !== "ready"
    : capabilitiesKnown && !visionCapable;
  const attachTitle = !gateBlocked
    ? "Attach image"
    : (visionStatus?.message ??
        (visionKind === "not_declared"
          ? "This model cannot look at pictures. To send one, choose a model marked Reads pictures on the Models page."
          : "The active model cannot read images. Switch to a model marked Reads pictures on the Models page."));

  // A refused turn (409/413/415/...) hands its draft back here rather than
  // leaving an error bubble nobody can act on. `run.refusedDraft` is
  // referentially stable across commits that do not touch it, so this only
  // fires once per refusal, and `takeRefusedDraft` clears it so a second
  // effect run (StrictMode) cannot restore the same draft twice.
  useEffect(() => {
    if (!run.refusedDraft) return;
    const draft = takeRefusedDraft();
    if (!draft) return;
    if (!text.trim()) setText(draft.text);
    setAttachments(draft.attachments);
    setAttachError(draft.message + refusalClientClause(draft.code));
    refreshVisionStatus();
    // `text` deliberately excluded: this must run exactly once per refusal,
    // not on every keystroke afterward.
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

  // The paperclip is NEVER disabled for vision reasons — a tap always opens
  // the file picker. What a tap DOES do, when picture support is not ready,
  // is surface the reason: the button's title (a mouse hover) and, now, an
  // on-demand ImageSupportStatus line reachable by touch, which a disabled
  // button's title attribute never was.
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
  // Auto-scroll to bottom on new messages
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, busy]);

  /**
   * Hand a turn to the store, keeping only what belongs to the composer.
   *
   * The stream loop that used to live here is gone: it was a second copy of
   * the Chat section's, and two copies is how one surface came to handle
   * frames the other did not. Both now fold the same stream in `chatRunStore`,
   * and this file decides only what to draw.
   */
  const sendMessage = useCallback(
    (raw?: string) => {
      const t = (raw ?? text).trim();
      if ((!t && attachments.length === 0) || busy) return;
      // Gated here rather than by disabling Send: this is the one path every
      // way of sending funnels through (the button, Enter, a suggestion chip,
      // Continue), so gating here covers all of them at once.
      if (attachments.length > 0 && gateBlocked) {
        setAttachError(COMPOSER_GATE_LINE);
        return;
      }

      setText("");

      // The bubble keeps its own copy of each previewUrl and the store owns
      // revoking them, so clear the tray WITHOUT revoking -- doing so would
      // blank the thumbnail on the message just sent.
      sendTurn({ text: t, attachments });
      setAttachments([]);
      setAttachError(null);
    },
    [text, attachments, busy, gateBlocked],
  );

  // Take focus back when the model stops -- what the old stream loop's
  // `finally` did, and the only part of it that belonged to this component.
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

  // The seed stands in only until there is a real conversation to show.
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
      </div>

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
