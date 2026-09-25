import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import { WarmupBanner } from "../components/WarmupBanner";
import { ArrowLeft, ArrowUp, Brain, Check, ChevronDown, Copy, Cpu, Loader2, Paperclip, Pencil, PenSquare, PlayCircle, RefreshCw, ThumbsDown, ThumbsUp, Wand2, Wrench, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState, useAppDispatch } from "../state/AppContext";
import {
  useChatRun,
  getChatRun,
  hasLiveThread,
  acknowledgeCompletion,
  sendTurn,
  openSession,
  followExternalSession,
  resetConversation,
  truncateFrom,
  patchMessage,
} from "../state/chatRunStore";
import type { Message } from "../state/chatRunStore";
import { ToolCallChip } from "../components/ToolCallChip";
import { ChatHistory, type OpenOrigin } from "./ChatHistory";
import { ThinkingPlaceholder } from "../components/ThinkingPlaceholder";
import { ThinkingDisclosure } from "../hub/views/chat/ThinkingDisclosure";
import { AttachmentTray } from "../components/AttachmentTray";
import { TypingIndicator } from "../hub/views/chat/TypingIndicator";
import { Goose } from "../components/Goose";
import { greeting, subtitle } from "../components/quips";
import { HubIco, micEl } from "../hub/primitives/HubIco";
import { CONTINUE_TURN_MESSAGE } from "../api/types";
import type { ModelEntry, SessionSummary } from "../api/types";
import { TurnStatsFooter } from "../components/TurnStatsFooter";
import { ContextPressureNote } from "../components/ContextPressureNote";
import { SubagentTree } from "../components/SubagentTree";
import { prepareImage, validateAttachmentSet } from "../lib/imageAttach";
import type { PreparedImage } from "../lib/imageAttach";

const CHIPS = [
  "What can you help me with?",
  "Check the weather",
  "Set a schedule",
  "Show my devices",
  "Manage my models",
];

export function Chat() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  // Turn state lives in `state/chatRunStore`: any sidebar press unmounts Chat, and the turn must survive it.
  const run = useChatRun();
  const { messages, busy, queued, turnSeed, loadingSession } = run;

  const [input, setInput]                   = useState("");
  const [sessions, setSessions]             = useState<SessionSummary[]>([]);
  const [retitling, setRetitling]           = useState(false);
  const [editingTitle, setEditingTitle]     = useState(false);
  const [titleDraft, setTitleDraft]         = useState("");
  const titleInputRef                       = useRef<HTMLInputElement>(null);
  // Render-value mirrors, assigned further down, so the title callbacks stay stable.
  const chatTitleRef                        = useRef("");
  const titleDraftRef                       = useRef("");
  const renameSessionRef                    = useRef<(id: string, title: string) => void>(() => {});
  /** `null` until the session list decides, except that a live or unseen turn opens the thread at once. */
  const [view, setView]                     = useState<"history" | "thread" | null>(
    () => (hasLiveThread() ? "thread" : null),
  );
  /** Where in the pane the opened card was, so the chat grows out of it. */
  const [openOrigin, setOpenOrigin]         = useState<OpenOrigin | null>(null);
  const [showModelSelector, setShowModelSelector] = useState(false);
  const [availableModels, setAvailableModels]     = useState<ModelEntry[]>([]);
  const [modelSwitching, setModelSwitching]       = useState(false);
  const [showTurnStats, setShowTurnStats]         = useState(false);
  const [attachments, setAttachments]             = useState<PreparedImage[]>([]);
  const [attachError, setAttachError]             = useState<string | null>(null);
  // Which user message (by local id) is being edited inline, if any.
  const [editingId, setEditingId]                 = useState<number | null>(null);
  const [editText, setEditText]                   = useState("");
  // Fail open: attaching is disabled only once the model is confirmed to lack vision.
  const [visionCapable, setVisionCapable]         = useState(true);
  const [capabilitiesKnown, setCapabilitiesKnown] = useState(false);
  // `thinking_mode` is a server setting ("auto" | "on" | "off") the agent reads each turn.
  const [thinkingMode, setThinkingMode]           = useState<string>("auto");
  const [thinkingSaving, setThinkingSaving]       = useState(false);
  // Restored on re-enable, so an explicit "on" doesn't come back as "auto".
  const lastThinkingOnRef                         = useRef<string>("auto");
  // Used only to personalise the greeting; blank is fine and handled there.
  const [userName, setUserName]                   = useState<string>("");
  // Configured provider: the model label's fallback before any turn reports a model_name. `meshEnabled`
  // gates injecting "mesh" into the switcher, since no `listModels()` scan can find it.
  const [configuredProvider, setConfiguredProvider] = useState<string | null>(null);
  const [meshEnabled, setMeshEnabled]               = useState(false);

  const bottomRef        = useRef<HTMLDivElement>(null);
  const textareaRef      = useRef<HTMLTextAreaElement>(null);
  const modelSelectorRef = useRef<HTMLDivElement>(null);
  const fileInputRef     = useRef<HTMLInputElement>(null);

  const attachDisabled = capabilitiesKnown && !visionCapable;
  const attachTitle = attachDisabled
    ? "The active model cannot read images. Switch to a vision-capable model such as gemma-4-E2B-it."
    : "Attach image";

  // On unmount, revoke unsent tray previews (sent ones belong to the store). Via a ref: a cleanup
  // depending on `attachments` would revoke a live thumbnail each time an image was added.
  const attachmentsRef = useRef<PreparedImage[]>([]);
  attachmentsRef.current = attachments;
  useEffect(() => () => {
    attachmentsRef.current.forEach((a) => URL.revokeObjectURL(a.previewUrl));
  }, []);

  const clearAttachments = useCallback(() => {
    setAttachments((prev) => {
      prev.forEach((p) => URL.revokeObjectURL(p.previewUrl));
      return [];
    });
    setAttachError(null);
  }, []);

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

  function onAttachClick() {
    fileInputRef.current?.click();
  }

  function onFileInputChange(e: React.ChangeEvent<HTMLInputElement>) {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    void addFiles(files);
  }

  function onPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) => f.type.startsWith("image/"));
    if (files.length === 0) return;
    e.preventDefault();
    void addFiles(files);
  }

  // A failed fetch leaves attaching on (fail open); the server explains any refusal.
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  // Follows a session id set from outside (deep link, `session-created`). Keyed on app state changing, not
  // on disagreeing with the store: our own wall opens disagree briefly. A cleared id keeps the transcript.
  const lastExternalIdRef = useRef<string | undefined>(state.sessionId ?? undefined);
  useEffect(() => {
    const newId = state.sessionId ?? undefined;
    if (newId === lastExternalIdRef.current) return;
    lastExternalIdRef.current = newId;
    if (!newId || newId === run.sessionId) return;
    clearAttachments();
    void followExternalSession(newId);
  }, [state.sessionId, run.sessionId, clearAttachments]);

  const refreshSessions = useCallback(() => {
    api.listSessions().then(setSessions).catch(() => {});
  }, []);

  const openModelSelector = useCallback(() => {
    setShowModelSelector(true);
    Promise.all([api.listModels(), api.listOllamaModels()])
      .then(([localModels, { models: ollamaModels }]) => {
        const ollamaEntries: ModelEntry[] = (ollamaModels ?? []).map((m) => {
          const sizeMb = m.size ? Math.round(m.size / (1024 * 1024)) : undefined;
          return {
            id: `ollama/${m.name}`,
            provider: "ollama",
            name: m.name,
            display_name: m.name,
            is_active: false,
            ram_estimate_mb: sizeMb,
            size_mb: sizeMb,
            category: "ollama",
            downloaded: true,
          };
        });
        setAvailableModels([...localModels, ...ollamaEntries]);
      })
      .catch(() => {
        api.listModels().then(setAvailableModels).catch(() => {});
      });
  }, []);

  const chatModels = useMemo(() => {
    const real = availableModels.filter((m) => {
      if (m.downloaded === false) return false;
      const cat = (m.category ?? m.provider ?? "").toLowerCase();
      if (cat === "whisper" || cat.startsWith("tts")) return false;
      return true;
    });
    // "mesh" is a peer's compute, not a downloaded file, so no scan finds it; synthesised when mesh_enabled.
    if (!meshEnabled) return real;
    const meshEntry: ModelEntry = {
      id: "mesh/mesh",
      provider: "mesh",
      name: "mesh",
      display_name: "Mesh (trusted peer)",
      is_active: configuredProvider === "mesh",
      downloaded: true,
    };
    return [...real, meshEntry];
  }, [availableModels, meshEnabled, configuredProvider]);

  const groupedModels = useMemo(() => {
    const map = new Map<string, ModelEntry[]>();
    for (const m of chatModels) {
      const group = map.get(m.provider) ?? [];
      group.push(m);
      map.set(m.provider, group);
    }
    return Array.from(map.entries());
  }, [chatModels]);

  const handleModelSwitch = useCallback(async (provider: string, name: string) => {
    setModelSwitching(true);
    try {
      // `POST /activate/…` would 400 on "mesh", which has no catalog row; set chat_provider directly.
      if (provider === "mesh") {
        await api.updateSettings({ chat_provider: "mesh" });
        setConfiguredProvider("mesh");
      } else {
        await api.activateModel(provider, name, "chat");
      }
      dispatch({ type: "SET_LAST_RESPONSE_META", payload: { modelName: name, modelRole: "chat", completionTokens: 0 } });
    } catch (e) {
      console.warn("Model switch failed:", e);
    } finally {
      setModelSwitching(false);
      setShowModelSelector(false);
    }
  }, [dispatch]);

  // Close model selector on outside click / Escape
  useEffect(() => {
    if (!showModelSelector) return;
    function handleClick(e: MouseEvent) {
      if (modelSelectorRef.current && !modelSelectorRef.current.contains(e.target as Node)) {
        setShowModelSelector(false);
      }
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") setShowModelSelector(false);
    }
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [showModelSelector]);

  useEffect(() => {
    if (!state.serverOnline) return;
    api.getSettings().then((s) => {
      setShowTurnStats(s.show_turn_stats ?? false);
      setUserName(s.user_name ?? "");
      const mode = s.thinking_mode ?? "auto";
      setThinkingMode(mode);
      if (mode !== "off") lastThinkingOnRef.current = mode;
      setConfiguredProvider(s.chat_provider ?? null);
      setMeshEnabled(s.mesh_enabled ?? false);
    }).catch(() => {});
  }, [state.serverOnline]);

  // Opens on the wall, even mid-conversation; an empty pond opens a new chat instead. Only a live or
  // unseen turn overrides it, via `view`'s initialiser and the early return.
  useEffect(() => {
    if (!state.serverOnline || view !== null) return;
    api.listSessions()
      .then((list) => {
        setSessions(list);
        setView(list.length > 0 ? "history" : "thread");
      })
      .catch((err) => {
        console.warn("Could not list conversations (non-fatal):", err);
        setView("thread");
      });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.serverOnline]);

  // Fetched whichever screen opened, so "All chats" always has a list behind it.
  useEffect(() => {
    if (!state.serverOnline) return;
    refreshSessions();
  }, [state.serverOnline, refreshSessions]);

  // Acknowledge a shown turn, or `hasLiveThread` stays true and every visit reopens it.
  useEffect(() => {
    if (view !== "thread" || busy) return;
    acknowledgeCompletion();
  }, [view, busy, run.completedTurns]);

  /** Open one conversation from the wall, growing it out of the card pressed. */
  const openConversation = useCallback((id: string, origin: OpenOrigin) => {
    setOpenOrigin(origin);
    setView("thread");
    acknowledgeCompletion();
    dispatch({ type: "SET_SESSION_ID", payload: id });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
    // Opening another conversation stops the current turn rather than let it answer unseen.
    void openSession(id, { stopCurrentRun: true });
  }, [dispatch]);

  const beginTitleEdit = useCallback(() => {
    if (!getChatRun().sessionId) return;
    setTitleDraft(chatTitleRef.current);
    setEditingTitle(true);
  }, []);

  /** An emptied box abandons the edit: likelier a slip than a wish for no name, and there's no undo. */
  const commitTitle = useCallback(() => {
    const id = getChatRun().sessionId;
    setEditingTitle(false);
    const next = titleDraftRef.current.trim();
    if (!id || !next || next === chatTitleRef.current) return;
    renameSessionRef.current(id, next);
  }, []);

  /** Back to the wall. Also an explicit "I am done with that turn". */
  const showHistory = useCallback(() => {
    setOpenOrigin(null);
    setView("history");
    acknowledgeCompletion();
    refreshSessions();
  }, [refreshSessions]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // Select the whole name: the common edit replaces it.
  useEffect(() => {
    if (editingTitle) {
      titleInputRef.current?.focus();
      titleInputRef.current?.select();
    }
  }, [editingTitle]);

  function newConversation() {
    resetConversation();
    acknowledgeCompletion();
    setQuipSeed(Date.now());
    // A new chat is not opened from a card, so it has no point to grow out of.
    setOpenOrigin(null);
    setView("thread");
    dispatch({ type: "SET_SESSION_ID", payload: null });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
    clearAttachments();
    textareaRef.current?.focus();
  }

  /**
   * Asks the model to name this conversation now, replacing even a hand-typed name: asking is consent.
   * The refresh is the feedback, since header and history both read `sessions`.
   */
  const retitleCurrent = useCallback(async () => {
    const id = getChatRun().sessionId;
    if (!id || retitling) return;
    setRetitling(true);
    try {
      const result = await api.retitleSession(id);
      if (result.outcome === "retitled" && result.title) {
        setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, title: result.title! } : s)));
      }
    } catch (err) {
      // Non-fatal: the old name stands, and the refresh below reconciles.
      console.warn("Rename failed (non-fatal):", err);
    } finally {
      setRetitling(false);
      refreshSessions();
    }
  }, [retitling, refreshSessions]);

  const renameSession = useCallback(async (id: string, title: string) => {
    // Optimistic; the refresh reconciles with the server's title either way.
    setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, title } : s)));
    try {
      await api.renameSession(id, title);
    } catch (err) {
      console.warn("Rename failed (non-fatal):", err);
    } finally {
      refreshSessions();
    }
  }, [refreshSessions]);

  const deleteSession = useCallback(async (id: string) => {
    try {
      await api.deleteSession(id);
    } catch (err) {
      console.warn("Delete failed (non-fatal):", err);
    }
    if (getChatRun().sessionId === id) {
      newConversation();
    }
    refreshSessions();
  // newConversation is a stable component-scope function; safe to omit.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshSessions]);

  /** Hands the turn to the store; only the composer's draft, tray and textarea height stay here. */
  const sendMessage = useCallback((directText?: string) => {
    const text = (directText ?? input).trim();
    if ((!text && attachments.length === 0) || !state.serverOnline) return;

    // While busy the store queues the text, but not attachments (they'd pair an image with the wrong
    // question), so a busy send with an empty box is a no-op that keeps the tray.
    if (busy && !text) return;

    setInput("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";

    if (busy) {
      sendTurn({ text });
      return;
    }

    // The store owns these previews: clear the tray without revoking, or the sent thumbnail blanks.
    sendTurn({
      text,
      images: attachments.map((a) => ({ data: a.data, mime_type: a.mime_type })),
      previewUrls: attachments.map((a) => a.previewUrl),
    });
    setAttachments([]);
    setAttachError(null);
  }, [input, attachments, busy, state.serverOnline]);

  // On the busy -> idle edge: refresh the list (it carries the server-derived title) and refocus.
  const prevBusyRef = useRef(busy);
  useEffect(() => {
    const wasBusy = prevBusyRef.current;
    prevBusyRef.current = busy;
    if (busy || !wasBusy) return;
    refreshSessions();
    textareaRef.current?.focus();
  }, [busy, refreshSessions]);

  const copyMessageText = useCallback((text: string) => {
    void navigator.clipboard.writeText(text).catch(() => {});
  }, []);

  // Local truncation before a resend, so the stale pair never shows beside the fresh one.
  const truncateLocalFrom = useCallback((msgId: number) => {
    truncateFrom(msgId);
  }, []);

  // Refresh and edit both truncate persisted history from here and resend; there's no regenerate endpoint.
  const truncateAndResend = useCallback(async (msg: Message, text: string) => {
    const sessionId = getChatRun().sessionId;
    if (!msg.backendId || !sessionId || busy) return;
    try {
      await api.deleteMessagesFrom(sessionId, msg.backendId);
    } catch (e) {
      console.error("Failed to truncate session before resend:", e);
      return;
    }
    truncateLocalFrom(msg.id);
    void sendMessage(text);
  }, [busy, sendMessage, truncateLocalFrom]);

  const startEditing = useCallback((msg: Message) => {
    setEditingId(msg.id);
    setEditText(msg.text);
  }, []);

  const cancelEditing = useCallback(() => {
    setEditingId(null);
    setEditText("");
  }, []);

  const submitEdit = useCallback((msg: Message) => {
    const trimmed = editText.trim();
    if (!trimmed) return;
    setEditingId(null);
    void truncateAndResend(msg, trimmed);
  }, [editText, truncateAndResend]);

  // Regenerates from the preceding user message, so the old pair is replaced by one fresh pair.
  const refreshResponse = useCallback((agentMsg: Message) => {
    const idx = messages.findIndex((m) => m.id === agentMsg.id);
    const precedingUser = idx === -1 ? undefined : [...messages.slice(0, idx)].reverse().find((m) => m.role === "user");
    if (!precedingUser) return;
    void truncateAndResend(precedingUser, precedingUser.text);
  }, [messages, truncateAndResend]);

  // Clicking the active thumb clears the vote (`null`). Optimistic; reverted if the PUT fails.
  const setFeedback = useCallback((msg: Message, liked: boolean) => {
    const sessionId = getChatRun().sessionId;
    if (!msg.backendId || !sessionId) return;
    const backendId = msg.backendId;
    const prevLiked = msg.liked ?? null;
    const next = prevLiked === liked ? null : liked;
    patchMessage(msg.id, { liked: next });
    api.setMessageFeedback(sessionId, backendId, next).catch((e) => {
      console.error("Failed to save feedback:", e);
      patchMessage(msg.id, { liked: prevLiked });
    });
  }, []);

  // State, so the greeting is picked once per conversation, not reshuffled on every render.
  const [quipSeed, setQuipSeed] = useState(() => Date.now());
  const greetingLine = useMemo(() => greeting(userName, quipSeed), [userName, quipSeed]);
  const subtitleLine = useMemo(() => subtitle(quipSeed), [quipSeed]);

  // The server's own `status` frames ("Agent working…"), not a client-side guess.
  const lastMsg = messages[messages.length - 1];
  const liveStatus =
    lastMsg?.role === "agent" && lastMsg.streaming ? lastMsg.status : undefined;

  // The server titles a session after its first exchange; until then it's "New Chat".
  const chatTitle =
    sessions.find((sn) => sn.id === state.sessionId)?.title?.trim() || "New Chat";

  // Refreshed each render: a stale closure would rename a conversation to an earlier name.
  chatTitleRef.current = chatTitle;
  titleDraftRef.current = titleDraft;
  renameSessionRef.current = renameSession;

  /** Optimistic; reverted if the save fails. */
  async function toggleThinking() {
    if (thinkingSaving) return;
    const next = thinkingMode === "off" ? lastThinkingOnRef.current : "off";
    const previous = thinkingMode;
    if (previous !== "off") lastThinkingOnRef.current = previous;
    setThinkingMode(next);
    setThinkingSaving(true);
    try {
      await api.updateSettings({ thinking_mode: next as "auto" | "on" | "off" });
    } catch {
      setThinkingMode(previous);
    } finally {
      setThinkingSaving(false);
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      sendMessage();
    }
  }

  function onInput(e: React.ChangeEvent<HTMLTextAreaElement>) {
    setInput(e.target.value);
    const el = e.target;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }

  // `lastResponseMeta` exists only after a turn completes; `configuredProvider` covers the time before.
  const modelLabel = state.lastResponseMeta?.modelName ?? configuredProvider ?? "local model";

  return (
    // While `view` is null, the wall's skeleton avoids a flash of new chat before the wall.
    view !== "thread" ? (
      <ChatHistory
        sessions={sessions}
        loading={view === null}
        onOpen={openConversation}
        onNewChat={newConversation}
        onDelete={deleteSession}
      />
    ) : (
    <div
      className="chat2"
      data-entering={openOrigin ? "true" : undefined}
      style={openOrigin
        ? ({ "--open-x": `${openOrigin.x}px`, "--open-y": `${openOrigin.y}px` } as React.CSSProperties)
        : undefined}
    >
      <WarmupBanner />
      {/* Header — what this conversation is, and the way back to the others.
          The assistant's name and status used to live here; neither told you
          anything you could act on, and the status is already in the sidebar.

          Every control is a 44px target with a word on it. A bare icon in a
          transparent circle is small, ambiguous and hard to hit; a labelled
          pill is none of those, and at this size it still reads as desktop
          chrome rather than a phone toolbar. */}
      <header className="chat2__head">
        <button
          className="chat2__back"
          onClick={showHistory}
          aria-label="All conversations"
          title="All conversations"
          type="button"
        >
          <ArrowLeft size={17} aria-hidden="true" />
          <span>All chats</span>
        </button>

        {/* The title renames itself. It is the biggest target in the header and
            the thing the name belongs to, so it carries the edit rather than a
            separate control — and it is the ONLY way to set a name by hand now
            that the history dropdown is gone. That matters more than it looks:
            a hand-typed name is the one kind the background pass will never
            overwrite, and without a route to it that protection could never be
            asked for. */}
        {editingTitle ? (
          <input
            ref={titleInputRef}
            className="chat2__topicEdit"
            aria-label="Conversation name"
            value={titleDraft}
            onChange={(e) => setTitleDraft(e.target.value)}
            onBlur={commitTitle}
            onKeyDown={(e) => {
              if (e.key === "Enter") { e.preventDefault(); commitTitle(); }
              if (e.key === "Escape") { e.preventDefault(); setEditingTitle(false); }
            }}
          />
        ) : (
          <h1 className="chat2__topic" title={chatTitle}>
            {/* Only a control once there is something to name. An unsaved
                conversation is titled "New Chat", and a disabled button saying
                that sits in the accessibility tree competing with the actual
                New chat control beside it — two things with one name, one of
                which does nothing. */}
            {state.sessionId ? (
              <button
                type="button"
                className="chat2__topicBtn"
                onClick={beginTitleEdit}
                /* Names the action and the title; the title alone reads as a focusable heading. */
                aria-label={`Rename conversation: ${chatTitle}`}
                title="Click to rename"
              >
                {chatTitle}
              </button>
            ) : (
              <span className="chat2__topicBtn chat2__topicBtn--static">{chatTitle}</span>
            )}
          </h1>
        )}
        {/* Ask for a better name for this conversation. Sits with the title
            rather than in the header's right-hand cluster, because it acts on
            the title and nothing else there does. */}
        <button
          className="chat2__retitle"
          onClick={retitleCurrent}
          disabled={!state.sessionId || retitling}
          aria-label="Rename this conversation"
          aria-busy={retitling || undefined}
          title="Rename this conversation"
          type="button"
        >
          {retitling
            ? <Loader2 size={14} className="chat2__retitle-spin" aria-hidden="true" />
            : <Wand2 size={14} aria-hidden="true" />}
          <span>{retitling ? "Renaming…" : "Rename"}</span>
        </button>
        {/* One control here, not three. The History dropdown was a second,
            worse copy of the wall the back button already returns to — same
            list, less room, and it hid the one action this corner is for. */}
        <div className="chat2__head-right">
          <button
            className="chat2__headbtn"
            onClick={newConversation}
            aria-label="New chat"
            title="New chat"
            type="button"
          >
            <PenSquare size={17} aria-hidden="true" />
            <span>New chat</span>
          </button>
        </div>
      </header>

      {/* Thread */}
      <div className="chat2__thread" role="log" aria-live="polite" aria-label="Chat conversation">
        {loadingSession && (
          <div className="chat-skeleton" aria-busy="true" aria-label="Loading conversation">
            {([88, 64, 72] as const).map((w, i) => (
              <div key={i} className={`chat-skeleton__row${i % 2 !== 0 ? " chat-skeleton__row--right" : ""}`}>
                <div className="chat-skeleton__line" style={{ width: `${w}%` }} />
                <div className="chat-skeleton__line" style={{ width: `${Math.round(w * 0.65)}%` }} />
              </div>
            ))}
          </div>
        )}

        {!loadingSession && messages.length === 0 && (
          <div className="chat-empty">
            <div className="chat-empty__eyebrow">New chat</div>
            <div className="chat-empty__body">
              <div className="chat-empty__copy">
                <p className="chat-empty__title">{greetingLine}</p>
                <p className="chat-empty__hint">{subtitleLine}</p>
              </div>
              <Goose state={busy ? "working" : "idle"} size={150} />
            </div>
          </div>
        )}

        {messages.map((msg) => {
          const hasText     = msg.text && msg.text.trim().length > 0;
          const hasCards    = (msg.cards?.length ?? 0) > 0;
          const hasThinking = (msg.thinkingBlocks?.length ?? 0) > 0;
          if (msg.role === "agent" && !hasText && !hasCards && !hasThinking && !msg.streaming) return null;
          return (
            <div key={msg.id} className={`ch-row ${msg.role === "user" ? "ch-row--user" : "ch-row--goose"}`}>
              <div className="ch-bubble-wrap">
                {/* Tool call chips */}
                {msg.role === "agent" && msg.cards && msg.cards.length > 0 && !msg.streaming && (
                  <div className="tool-call-chips" role="list" aria-label="Tools used">
                    {msg.cards.map((card) => (
                      <ToolCallChip key={card.id} card={card} onAction={sendMessage} />
                    ))}
                  </div>
                )}
                {/* History tool indicators */}
                {msg.role === "agent" && msg.historyToolNames && msg.historyToolNames.length > 0 && (
                  <div className="tool-call-chips" role="list" aria-label="Tools used">
                    {msg.historyToolNames.map((name) => (
                      <span key={name} className="tool-history-chip">
                        <Wrench size={10} aria-hidden />
                        {name.replace(/_/g, " ")}
                      </span>
                    ))}
                  </div>
                )}
                {/* Thinking block */}
                {msg.role === "agent" && msg.thinkingBlocks && msg.thinkingBlocks.length > 0 && (
                  <ThinkingDisclosure
                    blocks={msg.thinkingBlocks}
                    // Reasoning ends when the answer starts, though the turn still streams.
                    active={Boolean(msg.streaming) && !msg.text}
                    ms={
                      msg.thinkingStartedAt !== undefined && msg.thinkingEndedAt !== undefined
                        ? msg.thinkingEndedAt - msg.thinkingStartedAt
                        : undefined
                    }
                  />
                )}
                {/* Delegation tree. Deliberately NOT gated on `!msg.streaming`:
                    the whole point is that a turn which is blocked inside a
                    `delegate` tool call stops looking like a hung spinner. */}
                {msg.role === "agent" && msg.delegations && msg.delegations.length > 0 && (
                  <SubagentTree runs={msg.delegations} />
                )}
                {/* Bubble */}
                <div className={`ch-bubble ${msg.role === "user" ? "ch-bubble--user" : `ch-bubble--goose${msg.error ? " ch-bubble--error" : ""}`}`}>
                  {msg.images && msg.images.length > 0 && (
                    <div className="ch-bubble__images">
                      {msg.images.map((src, i) => (
                        <img key={i} src={src} alt={`Attached image ${i + 1}`} className="ch-bubble__image" />
                      ))}
                    </div>
                  )}
                  {msg.role === "user" && editingId === msg.id ? (
                    <div className="ch-bubble__edit">
                      <textarea
                        className="ch-bubble__edit-input"
                        value={editText}
                        onChange={(e) => setEditText(e.target.value)}
                        onKeyDown={(e) => {
                          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); submitEdit(msg); }
                          if (e.key === "Escape") { e.preventDefault(); cancelEditing(); }
                        }}
                        autoFocus
                        rows={Math.min(8, Math.max(2, editText.split("\n").length))}
                      />
                      <div className="ch-bubble__edit-actions">
                        <button type="button" className="ch-bubble__edit-btn ch-bubble__edit-btn--cancel" onClick={cancelEditing}>
                          <X size={12} aria-hidden /> Cancel
                        </button>
                        <button
                          type="button"
                          className="ch-bubble__edit-btn ch-bubble__edit-btn--save"
                          onClick={() => submitEdit(msg)}
                          disabled={!editText.trim()}
                        >
                          <Check size={12} aria-hidden /> Save &amp; resend
                        </button>
                      </div>
                    </div>
                  ) : (
                    msg.text || (msg.streaming
                      ? msg.status
                        ? <ThinkingPlaceholder status={msg.status} />
                        : <span className="stream-dots"><span /><span /><span /></span>
                      : "")
                  )}
                </div>
                {/* Copy / edit — user messages only */}
                {msg.role === "user" && editingId !== msg.id && (
                  <div className="ch-bubble__actions" role="group" aria-label="Message actions">
                    <button type="button" className="ch-bubble__action" title="Copy" onClick={() => copyMessageText(msg.text)}>
                      <Copy size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className="ch-bubble__action"
                      title="Edit and resend"
                      disabled={!msg.backendId || busy}
                      onClick={() => startEditing(msg)}
                    >
                      <Pencil size={13} aria-hidden />
                    </button>
                  </div>
                )}
                {/* Copy / regenerate / like / dislike — agent messages only; like/dislike feeds (or excludes from) training data */}
                {msg.role === "agent" && !msg.streaming && (
                  <div className="ch-bubble__actions" role="group" aria-label="Message actions">
                    <button type="button" className="ch-bubble__action" title="Copy" onClick={() => copyMessageText(msg.text)}>
                      <Copy size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className="ch-bubble__action"
                      title="Regenerate response"
                      disabled={!msg.backendId || busy}
                      onClick={() => refreshResponse(msg)}
                    >
                      <RefreshCw size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className={`ch-bubble__action${msg.liked === true ? " ch-bubble__action--liked" : ""}`}
                      title="Good response — use for training"
                      disabled={!msg.backendId}
                      onClick={() => setFeedback(msg, true)}
                    >
                      <ThumbsUp size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className={`ch-bubble__action${msg.liked === false ? " ch-bubble__action--disliked" : ""}`}
                      title="Bad response — exclude from training"
                      disabled={!msg.backendId}
                      onClick={() => setFeedback(msg, false)}
                    >
                      <ThumbsDown size={13} aria-hidden />
                    </button>
                  </div>
                )}
                {/* Model role + token meta */}
                {msg.role === "agent" && msg.modelRole && !msg.streaming && (
                  <span className="bubble__meta">
                    {msg.modelRole}
                    {msg.tokenUsage && msg.tokenUsage.completion_tokens > 0 && (
                      <> · {msg.tokenUsage.completion_tokens} tokens</>
                    )}
                  </span>
                )}
                {/* Turn budget exhausted — offer a continuation turn */}
                {msg.role === "agent" && !msg.streaming && msg.turnLimit !== undefined && (
                  <div className="turn-limit">
                    <span className="turn-limit__note">
                      Stopped after {msg.turnLimit} steps.
                    </span>
                    <button
                      className="turn-limit__btn"
                      onClick={() => sendMessage(CONTINUE_TURN_MESSAGE)}
                      disabled={busy}
                    >
                      <PlayCircle size={12} aria-hidden /> Continue
                    </button>
                  </div>
                )}
                {/* Context window filling — pressure line + "Compact now".
                    Deliberately NOT gated on showTurnStats: `show_turn_stats`
                    defaults to false in Rust, so borrowing that gate would ship
                    this invisible on every default install, which is exactly
                    why TurnStatsFooter was rejected as the host. */}
                {msg.role === "agent" && !msg.streaming && msg.contextWarning && (
                  <ContextPressureNote
                    warning={msg.contextWarning}
                    sessionId={run.sessionId ?? null}
                  />
                )}
                {/* Inference stats footer */}
                {msg.role === "agent" && !msg.streaming && showTurnStats && msg.turnStats && (
                  <TurnStatsFooter stats={msg.turnStats} />
                )}
              </div>
            </div>
          );
        })}


        {/* Messages typed while Goose was still answering. Shown in place, muted,
            so the queue is visible rather than a silent buffer. */}
        {queued.map((q, i) => (
          <div className="ch-row ch-row--user ch-row--queued" key={`q-${i}`}>
            <div className="ch-bubble-wrap">
              <div className="ch-bubble">{q}</div>
              <span className="ch-queued-note">Queued</span>
            </div>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>

      {/* Pending image attachments */}
      <AttachmentTray attachments={attachments} onRemove={removeAttachment} />
      {attachError && <p className="attach-error" role="alert">{attachError}</p>}

      {/* Composer — one surface. The textarea, its two quiet actions and the
          send button share a single bordered box that lifts on focus, so the
          place you type is unmistakably the centre of the screen rather than
          one control among four.

          Deliberately NOT disabled while Goose is answering: typing during a
          reply queues the message instead of being swallowed. */}
      {busy && <TypingIndicator seed={turnSeed} />}

      <div className={`chat2__composer${busy ? " is-busy" : ""}`}>
        <button
          className="ch-icon-btn"
          onClick={onAttachClick}
          disabled={!state.serverOnline || attachDisabled}
          aria-label="Attach image"
          onMouseDown={(e) => e.preventDefault()}
          title={attachTitle}
          type="button"
        >
          <Paperclip size={17} aria-hidden="true" />
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept="image/*"
          multiple
          hidden
          onChange={onFileInputChange}
        />
        <textarea
          ref={textareaRef}
          className="chat2__textarea"
          value={input}
          onChange={onInput}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          placeholder={busy ? "Queue a message…" : "Message Goose…"}
          disabled={!state.serverOnline}
          aria-label="Message input"
          rows={1}
        />
        <button
          className="ch-icon-btn"
          onClick={() => dispatch({ type: "SET_MODE", payload: "voice" })}
          aria-label="Switch to voice mode"
          onMouseDown={(e) => e.preventDefault()}
          title="Voice mode"
          type="button"
        >
          <HubIco d={micEl} size={17} color="currentColor" />
        </button>
        <button
          className="ch-send"
          onClick={() => sendMessage()}
          onMouseDown={(e) => e.preventDefault()}
          disabled={(!input.trim() && attachments.length === 0) || !state.serverOnline}
          aria-label={busy ? "Queue message" : "Send message"}
          title={busy ? "Queue message" : "Send message"}
          type="button"
        >
          <ArrowUp size={18} strokeWidth={2.5} aria-hidden="true" />
        </button>
      </div>

      {/* Quips, below the composer — a starting point, not a header. */}
      {!loadingSession && messages.length === 0 && (
        <div className="chat2__chips" role="group" aria-label="Suggestions">
          {CHIPS.map((c, i) => (
            <button
              key={c}
              className="ch-chip"
              style={{ animationDelay: `${60 + i * 45}ms` }}
              onClick={() => sendMessage(c)}
              disabled={!state.serverOnline}
              type="button"
            >
              {c}
            </button>
          ))}
        </div>
      )}

      {/* Hint bar — model selector + keyboard shortcut */}
      <div className="chat2__hint">
        <div ref={modelSelectorRef} className="model-selector-wrap">
          <button
            className={`model-selector-trigger${showModelSelector ? " is-open" : ""}`}
            onClick={() => showModelSelector ? setShowModelSelector(false) : openModelSelector()}
            disabled={modelSwitching}
            aria-label="Select model"
            aria-expanded={showModelSelector}
          >
            <Cpu size={11} />
            <span className="model-selector-trigger__label">
              {modelSwitching ? "Switching…" : modelLabel}
            </span>
            {modelSwitching ? <Loader2 size={10} className="spin" /> : <ChevronDown size={10} />}
          </button>

          {showModelSelector && (
            <div className="model-selector-dropdown">
              <div className="model-selector-dropdown__header">
                <span>Switch Model</span>
              </div>
              <div className="model-selector-dropdown__list">
                {groupedModels.length === 0 && (
                  <div className="model-selector-dropdown__empty">No models available</div>
                )}
                {groupedModels.map(([provider, group]) => (
                  <div key={provider}>
                    <div className="model-selector-dropdown__group-label">
                      {provider.charAt(0).toUpperCase() + provider.slice(1)}
                    </div>
                    {group.map((m) => {
                      const isActive = modelLabel === m.name || modelLabel === (m.display_name ?? m.name);
                      return (
                        <button
                          key={m.id}
                          className={`model-selector-dropdown__item${isActive ? " is-active" : ""}`}
                          onClick={() => handleModelSwitch(m.provider, m.name)}
                          disabled={modelSwitching}
                        >
                          <span className="model-selector-dropdown__item-name">{m.name}</span>
                          <span className="model-selector-dropdown__item-meta">
                            {m.size_mb
                              ? m.size_mb >= 1024
                                ? `${(m.size_mb / 1024).toFixed(1)} GB`
                                : `${m.size_mb} MB`
                              : ""}
                            {isActive && <Check size={12} color="var(--color-accent)" />}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* Thinking. Writes `thinking_mode`, which the agent reads on the next
            turn — so this is the real setting, not a display preference. */}
        <button
          type="button"
          className={`think-toggle${thinkingMode !== "off" ? " is-on" : ""}`}
          onClick={toggleThinking}
          disabled={thinkingSaving || !state.serverOnline}
          role="switch"
          aria-checked={thinkingMode !== "off"}
          aria-label="Thinking mode"
          title={
            thinkingMode === "off"
              ? "Thinking off — Goose answers directly"
              : thinkingMode === "on"
                ? "Thinking on for every model"
                : "Thinking on where the model supports it"
          }
        >
          <Brain size={11} aria-hidden="true" />
          <span className="think-toggle__label">Thinking</span>
          <span className="think-toggle__state">
            {thinkingMode === "off" ? "Off" : thinkingMode === "on" ? "On" : "Auto"}
          </span>
        </button>

        <span className="chat2__hint-sep">·</span>
        <span className="chat2__hint-kbd">Cmd + Enter to send</span>
      </div>
    </div>
    )
  );
}
