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
  takeRefusedDraft,
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
import {
  ImageSupportStatus,
  COMPOSER_GATE_LINE,
  refusalClientClause,
} from "../components/ImageSupportStatus";
import { useVisionStatus } from "../api/useVisionStatus";
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
import { useSuggestedPrompts } from "../hooks/useSuggestedPrompts";


export function Chat() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  // The starting quips, from the suggestion engine rather than a fixed list.
  // See `useSuggestedPrompts`.
  const chips = useSuggestedPrompts(state.sessionId);

  // The transcript, the turn in flight and the queue behind it belong to the
  // store, not to this component: pressing anything in the sidebar unmounts
  // Chat, and a turn is not a property of whichever screen happens to be
  // showing. See `state/chatRunStore`.
  const run = useChatRun();
  const { messages, busy, queued, turnSeed, loadingSession } = run;

  const [input, setInput]                   = useState("");
  const [sessions, setSessions]             = useState<SessionSummary[]>([]);
  const [retitling, setRetitling]           = useState(false);
  const [editingTitle, setEditingTitle]     = useState(false);
  const [titleDraft, setTitleDraft]         = useState("");
  const titleInputRef                       = useRef<HTMLInputElement>(null);
  /* Mirrors of render values, so the title callbacks can stay stable without
     depending on things declared further down the component. */
  const chatTitleRef                        = useRef("");
  const titleDraftRef                       = useRef("");
  const renameSessionRef                    = useRef<(id: string, title: string) => void>(() => {});
  /**
   * `null` until the session list says which screen this should be — unless a
   * turn is live or finished-but-unseen, in which case the answer is already
   * known and resolving it lazily avoids a frame of wall skeleton in front of a
   * conversation that is mid-sentence.
   */
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
  // Fail-open: an unknown/failed capabilities fetch never disables attaching —
  // it only disables once we've SUCCESSFULLY confirmed the model lacks vision.
  const [visionCapable, setVisionCapable]         = useState(true);
  const [capabilitiesKnown, setCapabilitiesKnown] = useState(false);
  // `thinking_mode` is a server setting ("auto" | "on" | "off") the agent reads
  // each turn, so this toggle changes real behaviour rather than just a label.
  const [thinkingMode, setThinkingMode]           = useState<string>("auto");
  const [thinkingSaving, setThinkingSaving]       = useState(false);
  // What to restore when switching back on. Toggling off then on would
  // otherwise collapse an explicit "on" into "auto" and quietly lose the
  // distinction.
  const lastThinkingOnRef                         = useRef<string>("auto");
  // Used only to personalise the greeting; blank is fine and handled there.
  const [userName, setUserName]                   = useState<string>("");
  // The currently *configured* provider/model, read once alongside the other
  // settings below — used as the model-selector's fallback label so a
  // provider with no per-turn model_name history (mesh, right after being
  // selected in Settings) still shows correctly instead of the old hardcoded
  // "local model" guess. `meshEnabled` also gates whether "mesh" is injected
  // into the quick-switcher below, same principle as the Settings catalogue's
  // provider dropdown (#132): it isn't a downloadable model, so it can never
  // appear via the `listModels()` scan on its own.
  const [configuredProvider, setConfiguredProvider] = useState<string | null>(null);
  const [meshEnabled, setMeshEnabled]               = useState(false);

  const bottomRef        = useRef<HTMLDivElement>(null);
  const textareaRef      = useRef<HTMLTextAreaElement>(null);
  const modelSelectorRef = useRef<HTMLDivElement>(null);
  const fileInputRef     = useRef<HTMLInputElement>(null);

  // Picture support's own lifecycle — download progress, readiness, a device
  // that declines the encoder entirely. `useVisionStatus` is the primary
  // attach decision now; `capabilities.vision` above is the fallback while a
  // per-model answer is unknown (an older server, or the probe still in
  // flight).
  const { status: visionStatus, refresh: refreshVisionStatus } = useVisionStatus();
  // Whether the household has just now reached for the paperclip or tried to
  // paste — the only moment a PERMANENT reason (not_declared /
  // not_on_this_device) earns a line; see ImageSupportStatus.
  const [attachReasonShown, setAttachReasonShown] = useState(false);

  const visionKind = visionStatus?.state.kind;
  const visionKnown = !!visionStatus && visionKind !== "unknown";
  // gate.blocked: status known && kind !== "ready".
  const gateBlocked = visionKnown
    ? visionKind !== "ready"
    : capabilitiesKnown && !visionCapable;
  const attachTitle = !gateBlocked
    ? "Attach image"
    : (visionStatus?.message ??
        (visionKind === "not_declared"
          ? "This model cannot look at pictures. To send one, choose a model marked Reads pictures on the Models page."
          : "The active model cannot read images. Switch to a model marked Reads pictures on the Models page."));

  /**
   * Revoke previews still sitting in the tray when this component goes away.
   *
   * The tray is the one piece of chat state that genuinely dies with the view:
   * a sent image's preview is owned by the store from `sendTurn` onward, but an
   * unsent one has no owner left once Chat unmounts, and before the store
   * existed those object URLs simply leaked. Reads a ref rather than
   * `attachments`, because a cleanup with the array as a dependency would
   * revoke a live thumbnail every time another image was added.
   */
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

  function onPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
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

  // Load vision capability once the server is reachable. Failure degrades to
  // "let the server explain" (fail open) rather than hiding the affordance.
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  // A refused turn (409/413/415/...) hands its draft back here rather than
  // leaving an error bubble nobody can act on. `run.refusedDraft` is
  // referentially stable across commits that do not touch it, so this only
  // fires once per refusal, and `takeRefusedDraft` clears it so a second
  // effect run (StrictMode) cannot restore the same draft twice.
  useEffect(() => {
    if (!run.refusedDraft) return;
    const draft = takeRefusedDraft();
    if (!draft) return;
    if (!input.trim()) setInput(draft.text);
    setAttachments(draft.attachments);
    setAttachError(draft.message + refusalClientClause(draft.code));
    refreshVisionStatus();
    // `input` deliberately excluded: this must run exactly once per refusal,
    // not on every keystroke afterward.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run.refusedDraft]);

  /**
   * Follow a session id set from OUTSIDE — a deep link, or the
   * `session-created` event AppContext listens for.
   *
   * Keyed on app state actually changing, not on it disagreeing with the store.
   * Those are different questions: opening a conversation from the wall sets
   * the store first and dispatches second, so a disagreement is usually just
   * this component's own change on its way round, and treating it as external
   * would reload the history we already have — or, when the dispatch does not
   * come back at all, quietly drop the open conversation.
   *
   * A cleared id records and stops. Somebody else clearing app state is not an
   * instruction to throw away the transcript; "New chat" is, and it says so
   * through `resetConversation`.
   */
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
    // "mesh" (#132) has no catalog row — it borrows a trusted peer's compute,
    // it isn't a file this Pond downloaded — so it can never appear via the
    // scan above. Synthesised here, gated on mesh_enabled, same principle as
    // the Settings catalogue's provider dropdown.
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
      // "mesh" has no catalog row to activate — `POST /activate/{category}/…`
      // only knows gguf/llamafile/ollama/whisper/tts_* categories and would
      // 400 on "mesh". Setting chat_provider directly is the same mechanism
      // the Settings catalogue's Provider dropdown uses (#132).
      if (provider === "mesh") {
        await api.updateSettings({ chat_provider: "mesh" });
        setConfiguredProvider("mesh");
      } else {
        await api.activateModel(provider, name, "chat");
      }
      dispatch({ type: "SET_LAST_RESPONSE_META", payload: { modelName: name, modelRole: "chat", completionTokens: 0 } });
      // The new model may have a different (or no) encoder, or none at all
      // for mesh -- ask again rather than waiting for the next poll tick.
      refreshVisionStatus();
    } catch (e) {
      console.warn("Model switch failed:", e);
    } finally {
      setModelSwitching(false);
      setShowModelSelector(false);
    }
  }, [dispatch, refreshVisionStatus]);

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

  // Load show_turn_stats once the server is reachable (cold-start safe:
  // re-runs on the offline->online transition like the session loader below).
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

  /**
   * Decide what this section opens on.
   *
   * The wall, not the last conversation. Resuming whatever happened to be most
   * recent answers a question nobody asked — you came here to pick something —
   * and it made the newest conversation the only one with a route to it.
   *
   * The one exception is a pond with no conversations at all: an empty wall is
   * a dead end, so a first-time visit lands in a new chat, which is an
   * invitation to type.
   *
   * A conversation already open in app state does NOT override this. Coming
   * back to Chat lands on the wall even mid-conversation, and the card for the
   * open one is one press away. That costs a click when you were only passing
   * through another section, and it buys a section that always opens somewhere
   * you can steer from.
   *
   * The one thing that DOES override it is a turn still running, or one that
   * finished while nothing was mounted to show it — `hasLiveThread`, resolved
   * in `view`'s initialiser above, so this effect's early return covers it.
   * Arriving to find your own answer already written and never seen is not a
   * choice about where to steer; it is the thing you came back for.
   */
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

  // The resume path above skips the list, but "All chats" still has to have
  // something behind it, so fetch it whichever screen we opened on.
  useEffect(() => {
    if (!state.serverOnline) return;
    refreshSessions();
  }, [state.serverOnline, refreshSessions]);

  /**
   * Stop resuming into a turn this surface has now shown.
   *
   * Without it, `hasLiveThread` would stay true forever and every later visit
   * would reopen the same finished thread — which is the deliberate wall
   * behaviour above, undone.
   */
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
    // Leaving a turn on purpose: stop it, rather than leaving the model
    // generating an answer this window will never show.
    void openSession(id, { stopCurrentRun: true });
  }, [dispatch]);

  /** Start typing a name for this conversation. */
  const beginTitleEdit = useCallback(() => {
    if (!getChatRun().sessionId) return;
    setTitleDraft(chatTitleRef.current);
    setEditingTitle(true);
  }, []);

  /**
   * Store the typed name, or abandon it if nothing changed.
   *
   * An empty box is treated as "I changed my mind", not as "call it nothing":
   * clearing a title and walking away is far more likely to be a slip than an
   * instruction, and there is no undo for the name it would replace.
   */
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

  // Select the whole name when editing starts: the common case is replacing it,
  // and a caret parked at the end makes that a delete-and-retype.
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
   * Ask the model to name this conversation, now.
   *
   * Distinct from `renameSession`, which stores a name you typed. This one
   * obeys rather than protects — the server replaces a name that still fits and
   * one typed by hand, because asking for the conversation in front of you is
   * consent about that conversation.
   *
   * The refresh afterwards is the feedback: the header title and the history
   * list both read from `sessions`, so both catch up in one go.
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
    // Optimistically update the row, then persist. Refresh reconciles with the
    // server's stored/derived title on success or failure.
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
    // If the deleted conversation was the active one, drop back to a blank chat.
    if (getChatRun().sessionId === id) {
      newConversation();
    }
    refreshSessions();
  // newConversation is a stable component-scope function; safe to omit.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshSessions]);

  /**
   * Hand a turn to the store, keeping only what belongs to the composer.
   *
   * The turn itself, the queue behind it and every frame it produces live in
   * `chatRunStore` so they survive this component being unmounted. What stays
   * here is the box you typed into: the draft, the attachment tray, and the
   * textarea's height.
   */
  const sendMessage = useCallback((directText?: string) => {
    const text = (directText ?? input).trim();
    if ((!text && attachments.length === 0) || !state.serverOnline) return;

    // A reply is still streaming: the store holds this one rather than dropping
    // it, so the composer never has to wait for the model. Attachments are NOT
    // queued -- they belong to the turn they were attached to, and silently
    // re-binding them to a later message would send an image with the wrong
    // question -- so a busy send with an empty box is a no-op rather than a
    // silent discard of the tray.
    if (busy && !text) return;

    // Gated here rather than by disabling Send: this is the one path every
    // way of sending funnels through (the button, Enter, a suggestion chip,
    // Continue), so gating here covers all of them at once.
    if (attachments.length > 0 && gateBlocked) {
      setAttachError(COMPOSER_GATE_LINE);
      return;
    }

    setInput("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";

    if (busy) {
      sendTurn({ text });
      return;
    }

    // The bubble keeps its own copy of each previewUrl and the store now owns
    // revoking them, so the tray is cleared here WITHOUT revoking -- doing so
    // would blank the thumbnail on the message just sent.
    sendTurn({ text, attachments });
    setAttachments([]);
    setAttachError(null);
  }, [input, attachments, busy, state.serverOnline, gateBlocked]);

  // Two things `sendMessage`'s old `finally` did that belong to the view rather
  // than to the turn: the session list carries the title the server derives
  // from the first exchange, and the composer takes focus back when the model
  // stops. Keyed on the store's completed-turn counter, so they fire once per
  // turn even when the turn finished while this component was unmounted.
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

  // Drop everything from `msgId` onward in the LOCAL list — used by both edit
  // and refresh right before resending, so the stale pair never briefly shows
  // next to the fresh one.
  const truncateLocalFrom = useCallback((msgId: number) => {
    truncateFrom(msgId);
  }, []);

  // Shared by refresh (same text) and edit-submit (new text): truncate the
  // persisted history from this user message onward, then resend through the
  // normal send path — no separate regenerate endpoint, `/chat/stream`
  // already knows how to append a fresh turn.
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

  // Called from the AGENT bubble, but truncateAndResend needs a USER message
  // to delete-from-and-resend — regenerating means "redo the answer to the
  // prompt right before this one," so walk back to find it. Truncating from
  // there removes both the old prompt row and its stale answer; resending
  // the same text creates a fresh pair, keeping exactly one user/answer per
  // turn (there's no lighter "keep the prompt, only replace the answer"
  // primitive — see truncateAndResend's own comment).
  const refreshResponse = useCallback((agentMsg: Message) => {
    const idx = messages.findIndex((m) => m.id === agentMsg.id);
    const precedingUser = idx === -1 ? undefined : [...messages.slice(0, idx)].reverse().find((m) => m.role === "user");
    if (!precedingUser) return;
    void truncateAndResend(precedingUser, precedingUser.text);
  }, [messages, truncateAndResend]);

  // `null` clears a vote — clicking the already-active thumb toggles it off.
  // Optimistic: flips locally first, reverts only if the PUT fails.
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

  // Held in state so the greeting is chosen once per conversation: recomputing
  // it on render would reshuffle the line while someone was reading it.
  const [quipSeed, setQuipSeed] = useState(() => Date.now());
  const greetingLine = useMemo(() => greeting(userName, quipSeed), [userName, quipSeed]);
  const subtitleLine = useMemo(() => subtitle(quipSeed), [quipSeed]);

  // What this conversation is about. The server titles a session after the
  // first exchange, so a brand-new chat has nothing to show yet.
  // What the backend says it is doing, from the stream's `status` frames
  // ("Agent working…", "Using tool: …"). Shown in the working strip, so the
  // line under the composer is the server's account of itself rather than a
  // client-side guess.
  const lastMsg = messages[messages.length - 1];
  const liveStatus =
    lastMsg?.role === "agent" && lastMsg.streaming ? lastMsg.status : undefined;

  const chatTitle =
    sessions.find((sn) => sn.id === state.sessionId)?.title?.trim() || "New Chat";

  // Mirror the current render values so the title callbacks can read them
  // without listing them as dependencies — `renameSession` is declared above
  // but `chatTitle` is not, and a stale closure here would rename a
  // conversation to the name it had two renders ago.
  chatTitleRef.current = chatTitle;
  titleDraftRef.current = titleDraft;
  renameSessionRef.current = renameSession;

  /** Flip thinking on or off, persisting it. Optimistic, reverted on failure. */
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

  // `lastResponseMeta` only exists once a turn in THIS session has actually
  // completed, so a freshly opened chat — or one right after switching
  // providers in Settings — fell back to a hardcoded "local model" guess
  // regardless of what's actually configured. `configuredProvider` (fetched
  // alongside the other settings above) is accurate from the start; still
  // falls back to the old guess only if that fetch hasn't resolved yet.
  const modelLabel = state.lastResponseMeta?.modelName ?? configuredProvider ?? "local model";

  return (
    // `null` means the session list has not answered yet. Showing the wall's
    // skeleton rather than the composer avoids a flash of new-chat for someone
    // who is about to land on the wall.
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
                /* Names the action AND the current title. Announced as its own
                   text alone, this is a heading that happens to be focusable
                   and nothing says it can be edited. */
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
                    // Reasoning is over once the answer starts arriving, even
                    // though the turn itself is still streaming.
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

      {/* Picture support's own status, then any pending attachments. */}
      <ImageSupportStatus status={visionStatus} revealed={attachReasonShown} />
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
          disabled={!state.serverOnline}
          aria-disabled={gateBlocked || undefined}
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
          {chips.map((c, i) => (
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
