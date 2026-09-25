import { useState, useRef, useEffect, useMemo, useCallback } from "react";
import { Button } from "@heroui/react";
import {
  Layers, Mic, RefreshCw, Zap,
  ChevronLeft, ChevronRight, Grid3X3, Rows3,
} from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { api } from "../api/PondApiClient";
import { nextCardId } from "../state/reducer";
import type { ChatEvent } from "../api/types";
import { ScheduleDebriefCard } from "../components/ScheduleDebriefCard";
import { GooseAvatar } from "../hub/views/chat/GooseAvatar";
import { HubIco, micEl } from "../hub/primitives/HubIco";
import { HP_PATHS } from "../hub/primitives/icons";
import "../hub/views/chat.css";
import { CardChrome }    from "../hub/views/canvas/CardChrome";
import { WeatherCard }   from "../hub/views/canvas/WeatherCard";
import { CalendarCard }  from "../hub/views/canvas/CalendarCard";
import { MapsCard }      from "../hub/views/canvas/MapsCard";
import { CryptoCard }    from "../hub/views/canvas/CryptoCard";
import { SmartHomeCard } from "../hub/views/canvas/SmartHomeCard";
import { NewsCard }      from "../hub/views/canvas/NewsCard";
import "../hub/views/canvas-mcp.css";

const HUB_CARDS = [
  { app: "giap-weather",       Comp: WeatherCard },
  { app: "giap-calendar",      Comp: CalendarCard },
  { app: "giap-homeassistant", Comp: SmartHomeCard },
  { app: "giap-finance",       Comp: CryptoCard },
  { app: "giap-maps",          Comp: MapsCard },
  { app: "giap-news",          Comp: NewsCard },
];
import {
  encodeWav, downsampleTo16k, calculateRms,
  createVadState, advanceVad, DEFAULT_VAD_CONFIG,
} from "../modes/voice/webAudioUtils";

// Trigger MCP-UI card registrations
import "../mcp-ui";
import {
  findCardRenderer,
  findCardByHint,
  McpCardShell,
  GenericCard,
  McpAppHost,
} from "../mcp-ui";


// ── Types ──────────────────────────────────────────────────────

interface CanvasCard {
  id: number;
  kind: string;
  tool: string;
  title: string;
  data: Record<string, unknown>;
  /** Card type from the UI hint; beats tool-name matching. */
  renderHint?: string;
  /** MCP App HTML, rendered in a sandboxed iframe. */
  appHtml?: string;
  /** MCP App resource URI for refetching */
  appResourceUri?: string;
}

interface ChatMessage {
  role: "user" | "assistant" | "tool";
  text: string;
  tool?: string;
  status?: "running" | "ok";
  /** An error bubble: later text starts a new bubble instead of appending to it. */
  error?: boolean;
}

// ── Canvas section ─────────────────────────────────────────────

export function Canvas() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  const [cards, setCards] = useState<CanvasCard[]>([]);
  const [thread, setThread] = useState<ChatMessage[]>([
    {
      role: "assistant",
      text: "Welcome to Canvas. I'll render visual results here when tools return structured data. Try one of the suggestions below.",
    },
  ]);
  const [draft, setDraft] = useState("");
  const [dockOpen, setDockOpen] = useState(true);
  const [voiceMode, setVoiceMode] = useState(false);
  const [layout, setLayout] = useState<"grid" | "stack">("grid");
  const [streaming, setStreaming] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const sessionIdRef = useRef<string | undefined>(undefined);
  /** Track in-flight tool calls so tool_result can update the right card */
  const pendingToolsRef = useRef<Map<string, number>>(new Map());

  // ── Voice recording state ──────────────────────────────────
  type VoiceRecState = "idle" | "recording" | "transcribing";
  const [voiceRecState, setVoiceRecState] = useState<VoiceRecState>("idle");
  const [audioLevel, setAudioLevel] = useState(0);
  const voiceCancelledRef = useRef(false);
  const micCtxRef = useRef<{
    stream: MediaStream;
    audioContext: AudioContext;
    source: MediaStreamAudioSourceNode;
    analyser: AnalyserNode;
    processor: ScriptProcessorNode;
    chunks: Float32Array[];
    sampleRate: number;
    pump: ReturnType<typeof setInterval> | null;
  } | null>(null);

  const closeMic = useCallback(() => {
    const ctx = micCtxRef.current;
    if (!ctx) return;
    if (ctx.pump) clearInterval(ctx.pump);
    try { ctx.processor.disconnect(); } catch { /* ok */ }
    try { ctx.source.disconnect(); } catch { /* ok */ }
    ctx.stream.getTracks().forEach((t) => t.stop());
    if (ctx.audioContext.state !== "closed") ctx.audioContext.close().catch(() => {});
    micCtxRef.current = null;
    setAudioLevel(0);
  }, []);

  useEffect(() => () => closeMic(), [closeMic]);

  const startVoiceRecording = useCallback(async () => {
    if (voiceRecState !== "idle" || streaming || !state.serverOnline) return;
    voiceCancelledRef.current = false;
    closeMic();

    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { sampleRate: { ideal: 16000 }, channelCount: { exact: 1 }, echoCancellation: true, noiseSuppression: true } as MediaTrackConstraints,
      });
      const audioContext = new AudioContext();
      const source = audioContext.createMediaStreamSource(stream);
      const analyser = audioContext.createAnalyser();
      analyser.fftSize = 2048;
      source.connect(analyser);
      const processor = audioContext.createScriptProcessor(4096, 1, 1);
      const chunks: Float32Array[] = [];
      processor.onaudioprocess = (e) => { chunks.push(new Float32Array(e.inputBuffer.getChannelData(0))); };
      source.connect(processor);
      processor.connect(audioContext.destination);

      const ctx = { stream, audioContext, source, analyser, processor, chunks, sampleRate: audioContext.sampleRate, pump: null as ReturnType<typeof setInterval> | null };
      micCtxRef.current = ctx;
      setVoiceRecState("recording");

      // VAD-based auto-stop
      const vad = createVadState();
      const td = new Float32Array(analyser.fftSize);
      const t0 = Date.now();

      const wavBlob = await new Promise<Blob | null>((resolve) => {
        ctx.pump = setInterval(() => {
          if (voiceCancelledRef.current || !micCtxRef.current) {
            if (ctx.pump) clearInterval(ctx.pump);
            setAudioLevel(0);
            resolve(null);
            return;
          }
          analyser.getFloatTimeDomainData(td);
          const rms = calculateRms(td);
          setAudioLevel(rms);

          if (Date.now() - t0 >= DEFAULT_VAD_CONFIG.maxDurationMs || advanceVad(vad, rms, Date.now(), DEFAULT_VAD_CONFIG)) {
            if (ctx.pump) clearInterval(ctx.pump);
            setAudioLevel(0);
            const total = chunks.reduce((s, c) => s + c.length, 0);
            const merged = new Float32Array(total);
            let off = 0;
            for (const c of chunks) { merged.set(c, off); off += c.length; }
            const wav = encodeWav(downsampleTo16k(merged, ctx.sampleRate), 16000);
            resolve(new Blob([wav], { type: "audio/wav" }));
          }
        }, 30);
      });

      closeMic();
      if (!wavBlob || voiceCancelledRef.current) { setVoiceRecState("idle"); return; }

      setVoiceRecState("transcribing");
      const result = await api.transcribe(await wavBlob.arrayBuffer());
      const text = result.text?.trim();
      setVoiceRecState("idle");

      if (text) sendPrompt(text);
    } catch (err) {
      closeMic();
      setVoiceRecState("idle");
      console.warn("Canvas voice error:", err);
    }
  }, [voiceRecState, streaming, state.serverOnline, closeMic]); // eslint-disable-line react-hooks/exhaustive-deps

  const cancelVoiceRecording = useCallback(() => {
    voiceCancelledRef.current = true;
    closeMic();
    setVoiceRecState("idle");
  }, [closeMic]);

  // Cache of tool metadata (includes _meta.ui for MCP Apps detection)
  const toolMetaRef = useRef<Map<string, { resourceUri?: string }>>(new Map());
  useEffect(() => {
    if (!state.serverOnline) return;
    api.listTools().then((tools: Array<{ name: string; _meta?: { ui?: { resourceUri?: string } } }>) => {
      const map = new Map<string, { resourceUri?: string }>();
      for (const t of tools) {
        if (t._meta?.ui?.resourceUri) {
          map.set(t.name, { resourceUri: t._meta.ui.resourceUri });
        }
      }
      toolMetaRef.current = map;
    }).catch(() => {});
  }, [state.serverOnline]);

  // Prompt suggestions that trigger real LLM tool calls
  const promptSuggestions = useMemo(() => [
    { key: "weather", label: "Weather", prompt: "What's the weather like right now?" },
    { key: "schedules", label: "Schedules", prompt: "List my schedules" },
    { key: "memories", label: "Memories", prompt: "What do you remember about me?" },
    { key: "news", label: "News", prompt: "What's in the news today?" },
    { key: "wikipedia", label: "Wikipedia", prompt: "Tell me about Nairobi on Wikipedia" },
    { key: "time", label: "Time", prompt: "What time is it?" },
  ], []);

  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [thread]);

  // Handle debrief context — when navigating from schedule runs
  const debriefHandledRef = useRef<string | null>(null);
  useEffect(() => {
    if (!state.debriefContext) { debriefHandledRef.current = null; return; }
    if (debriefHandledRef.current === state.debriefContext.run.id) return;
    debriefHandledRef.current = state.debriefContext.run.id;
    const { run } = state.debriefContext;
    const debriefCard: CanvasCard = {
      id: Date.now(),
      kind: "debrief",
      tool: "giap-schedule__debrief",
      title: `Debrief: ${run.scheduleName}`,
      data: {
        schedule_id: run.scheduleId,
        schedule_name: run.scheduleName,
        status: run.status,
        result: run.result,
        error: run.error,
        started_at: run.startedAt,
        finished_at: run.finishedAt,
        duration_ms: run.durationMs,
        recipe: run.recipe,
        excerpt: run.excerpt,
      },
    };
    setCards((c) => [debriefCard, ...c]);
    setThread((t) => [
      ...t,
      { role: "tool", text: "", tool: "giap-schedule__debrief", status: "ok" as const },
      { role: "assistant", text: `Here's the debrief for "${run.scheduleName}".` },
    ]);
    dispatch({ type: "CLEAR_DEBRIEF_CONTEXT" });
  }, [state.debriefContext, dispatch]);

  function send() { sendPrompt(draft); }

  function closeCard(id: number) {
    setCards((c) => c.filter((card) => card.id !== id));
  }

  async function sendPrompt(prompt: string) {
    const text = prompt.trim();
    if (!text || streaming || !state.serverOnline) return;
    setDraft("");
    setStreaming(true);

    setThread((t) => [
      ...t,
      { role: "user", text },
      { role: "assistant", text: "" },
    ]);

    try {
      api.setToken(state.sessionToken);
      for await (const event of api.chatStream(text, sessionIdRef.current, state.sessionToken ?? undefined, true)) {
        const ev = event as ChatEvent;

        if (ev.type === "text" && (ev.content ?? ev.token)) {
          const raw = ev.content ?? ev.token ?? "";
          setThread((t) => {
            const last = t[t.length - 1];
            if (!last || last.role !== "assistant") return t;
            // See Chat.tsx: the error arm overwrites `text`, this one appends.
            if (last.error) return [...t, { role: "assistant" as const, text: raw }];
            return [...t.slice(0, -1), { ...last, text: last.text + raw }];
          });

        } else if (ev.type === "status" && ev.content) {
          setThread((t) => {
            const last = t[t.length - 1];
            if (!last || last.role !== "assistant") return t;
            return [...t.slice(0, -1), { ...last, text: last.text || ev.content! }];
          });

        } else if (ev.type === "tool_call" && ev.tool) {
          setThread((t) => [...t, { role: "tool", text: "", tool: ev.tool!, status: "running" }]);

          const cardId = nextCardId();
          const toolName = ev.tool;
          const reg = findCardByHint(toolName) ?? findCardRenderer(toolName);

          const meta = toolMetaRef.current.get(toolName);
          let appHtml: string | undefined;
          if (meta?.resourceUri) {
            try {
              appHtml = await api.getMcpResource(meta.resourceUri);
            } catch {
              // Fallback to React registry if resource fetch fails
            }
          }

          setCards((c) => [{
            id: cardId,
            kind: reg?.key ?? "generic",
            tool: toolName,
            title: reg?.label ?? toolName,
            data: {},
            ...(appHtml ? { appHtml, appResourceUri: meta?.resourceUri } : {}),
          }, ...c]);

          pendingToolsRef.current.set(toolName, cardId);
          if (ev.id) pendingToolsRef.current.set(`__id:${ev.id}`, cardId);

        } else if (ev.type === "tool_result" && (ev.tool || ev.id)) {
          const matchTool = ev.tool || ev.id || "";
          setThread((t) => t.map((m) =>
            m.role === "tool" && (m.tool === ev.tool || m.tool === matchTool)
              ? { ...m, status: "ok" as const }
              : m
          ));

          const cardData = ev.ui?.data ?? { result: ev.content };
          const renderHint = ev.ui?.card_type;
          const cardId = (ev.tool && pendingToolsRef.current.get(ev.tool))
            ?? (ev.id && pendingToolsRef.current.get(`__id:${ev.id}`));
          if (cardId != null) {
            setCards((c) => c.map((card) =>
              card.id === cardId
                ? { ...card, data: cardData, ...(renderHint ? { renderHint } : {}) }
                : card
            ));
            if (ev.tool) pendingToolsRef.current.delete(ev.tool);
            if (ev.id) pendingToolsRef.current.delete(`__id:${ev.id}`);
          }

        } else if (ev.type === "error" || ev.error) {
          setThread((t) => {
            const last = t[t.length - 1];
            if (!last || last.role !== "assistant") return t;
            return [...t.slice(0, -1), { ...last, text: `Error: ${ev.error ?? "Unknown error"}`, error: true }];
          });

        } else if (ev.done && ev.session_id) {
          sessionIdRef.current = ev.session_id;
          dispatch({ type: "SET_SESSION_ID", payload: ev.session_id });
          if (ev.model_name && ev.model_role) {
            dispatch({
              type: "SET_LAST_RESPONSE_META",
              payload: {
                modelName: ev.model_name,
                modelRole: ev.model_role,
                completionTokens: ev.usage?.completion_tokens ?? 0,
              },
            });
          }
        }
      }
    } catch (e) {
      setThread((t) => {
        const last = t[t.length - 1];
        if (!last || last.role !== "assistant") return t;
        return [...t.slice(0, -1), { ...last, text: `Error: ${String(e)}` }];
      });
    } finally {
      setStreaming(false);
    }
  }

  function renderCard(card: CanvasCard) {
    if (card.appHtml) {
      const toolResult = Object.keys(card.data).length > 0
        ? { content: [{ type: "text", text: JSON.stringify(card.data) }] }
        : undefined;
      return (
        <McpAppHost
          key={card.id}
          html={card.appHtml}
          toolName={card.tool}
          toolResult={toolResult}
          toolInput={card.data}
          onClose={() => closeCard(card.id)}
          onToolCall={async (name, args) => {
            const result = await api.callTool(name, args);
            return { content: [{ type: "text", text: typeof result === "string" ? result : JSON.stringify(result) }] };
          }}
          onOpenUrl={(url) => window.open(url, "_blank", "noopener,noreferrer")}
        />
      );
    }

    if (card.kind === "debrief" && card.data) {
      const run = {
        id: String(card.data.schedule_id ?? card.id),
        scheduleId: String(card.data.schedule_id ?? ""),
        scheduleName: String(card.data.schedule_name ?? card.title),
        status: (card.data.status as "completed" | "failed" | "running") ?? "completed",
        result: (card.data.result as string) ?? null,
        error: (card.data.error as string) ?? null,
        startedAt: String(card.data.started_at ?? ""),
        finishedAt: (card.data.finished_at as string) ?? null,
        durationMs: (card.data.duration_ms as number) ?? null,
        read: true,
        excerpt: String(card.data.excerpt ?? ""),
        recipe: (card.data.recipe as string) ?? null,
      };
      return (
        <ScheduleDebriefCard key={card.id} run={run} onClose={() => closeCard(card.id)} />
      );
    }

    const hintReg = card.renderHint ? findCardByHint(card.renderHint) : null;
    const reg = hintReg ?? findCardRenderer(card.tool);
    if (reg) {
      const Renderer = reg.component;
      return (
        <McpCardShell key={card.id} tool={card.tool} label={reg.label} onClose={() => closeCard(card.id)}>
          <Renderer data={card.data} toolName={card.tool} variant="normal" />
        </McpCardShell>
      );
    }
    return (
      <McpCardShell key={card.id} tool={card.tool} label={card.title} onClose={() => closeCard(card.id)}>
        <GenericCard data={card.data} toolName={card.tool} />
      </McpCardShell>
    );
  }

  return (
    <div className="screen screen--canvas">
      {/* ── Toolbar ──────────────────────────────────────── */}
      <div className="canvas-toolbar">
        <div className="canvas-toolbar__left">
          <h1 className="page-header__title">Canvas</h1>
          <span className="canvas-chip">
            <Layers size={12} /> MCP-UI
          </span>
          <span className={`canvas-chip${voiceMode ? "" : " canvas-chip--live"}`}>
            <span className={`status-dot ${voiceMode ? "" : "status-dot--online"}`} />
            {voiceMode ? "voice mode" : "live"}
          </span>
        </div>
        <div className="canvas-toolbar__right">
          {/* Layout toggle */}
          <div className="canvas-layout-toggle">
            <button
              onClick={() => setLayout("grid")}
              title="Grid layout"
              aria-label="Grid layout"
              aria-pressed={layout === "grid"}
              className={`canvas-layout-toggle__btn${layout === "grid" ? " is-active" : ""}`}
            >
              <Grid3X3 size={14} />
            </button>
            <button
              onClick={() => setLayout("stack")}
              title="Stack layout"
              aria-label="Stack layout"
              aria-pressed={layout === "stack"}
              className={`canvas-layout-toggle__btn${layout === "stack" ? " is-active" : ""}`}
            >
              <Rows3 size={14} />
            </button>
          </div>
          <Button
            size="sm"
            variant={voiceMode ? "primary" : "outline"}
            onPress={() => setVoiceMode((v) => !v)}
            aria-pressed={voiceMode}
          >
            <Mic size={14} /> Voice mode
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onPress={() => setCards([])}
            isDisabled={cards.length === 0}
          >
            <RefreshCw size={14} /> Clear
          </Button>
        </div>
      </div>

      {/* ── Stage: canvas + floating dock ───────────────── */}
      <div className={`canvas-stage${dockOpen ? "" : " dock-collapsed"}`}>

        {/* Full-bleed canvas pane */}
        <div className="canvas-pane">
          <div className="canvas-pane__head">
            <div className="canvas-pane__title">
              <Layers size={14} />
              <span>Canvas</span>
              <span className="canvas-count">{cards.length === 0 ? HUB_CARDS.length : cards.length}</span>
            </div>
            <span className="canvas-pane__hint">
              {cards.length === 0 ? "Live MCP cards — chat with Goose to add more" : "Auto-renders when tools return UI"}
            </span>
          </div>

          <div className={`canvas-pane__body${cards.length === 0 && !state.debriefContext ? " canvas-pane__body--hub" : ""}`}>
            {state.debriefContext ? (
              <div className="canvas-grid">
                <ScheduleDebriefCard
                  run={state.debriefContext.run}
                  onClose={() => dispatch({ type: "CLEAR_DEBRIEF_CONTEXT" })}
                />
              </div>
            ) : cards.length === 0 ? (
              <div className="mcpc">
                <div className="mcpc__board">
                  {HUB_CARDS.map(({ app, Comp }) => (
                    <CardChrome key={app} app={app}>
                      <Comp />
                    </CardChrome>
                  ))}
                </div>
              </div>
            ) : (
              <div className={`canvas-grid${layout === "stack" ? " canvas-grid--stack" : ""}`}>
                {cards.map((card) => renderCard(card))}
              </div>
            )}
          </div>
        </div>

        {/* Floating chat dock */}
        <div className={`chat-dock${dockOpen ? " is-open" : " is-collapsed"}`}>
          {dockOpen ? (
            <>
              {/* Dock header */}
              <div className="chat-dock__head">
                <div className="chat2__id">
                  <GooseAvatar size={32} />
                  <div>
                    <div className="chat2__name">Goose</div>
                    <div className="chat2__status">
                      <span className="chat2__dot" aria-hidden="true" />
                      {voiceMode ? "voice" : "chat"}
                    </div>
                  </div>
                </div>
                <button
                  className="chat-dock__icon-btn"
                  onClick={() => setDockOpen(false)}
                  title="Hide chat"
                  aria-label="Hide chat"
                  aria-expanded={dockOpen}
                >
                  <ChevronLeft size={14} />
                </button>
              </div>

              {/* Thread */}
              <div ref={scrollRef} className="chat-dock__body">
                {thread.map((m, i) => {
                  if (m.role === "tool") {
                    return (
                      <div key={i} className={`tool-call tool-call--${m.status}`}>
                        <Zap size={12} />
                        <code>{m.tool}</code>
                        <span className="tool-call__status">
                          {m.status === "running"
                            ? <span className="stream-dots"><span /><span /><span /></span>
                            : "returned"}
                        </span>
                      </div>
                    );
                  }
                  return (
                    <div key={i} className={`ch-row ${m.role === "user" ? "ch-row--user" : "ch-row--goose"}`}>
                      {m.role === "assistant" && <GooseAvatar />}
                      <div className="ch-bubble-wrap">
                        <div className={`ch-bubble ${m.role === "user" ? "ch-bubble--user" : "ch-bubble--goose"}`}>
                          {m.text || (streaming && i === thread.length - 1
                            ? <span className="stream-dots"><span /><span /><span /></span>
                            : "")}
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>

              {/* Suggestion chips — visible when thread is short */}
              {!voiceMode && thread.filter((m) => m.role !== "tool").length <= 1 && (
                <div className="chat2__chips" role="group" aria-label="Quick suggestions">
                  {promptSuggestions.map((s) => (
                    <button
                      key={s.key}
                      className="ch-chip"
                      onClick={() => sendPrompt(s.prompt)}
                      disabled={streaming}
                      type="button"
                    >
                      {s.label}
                    </button>
                  ))}
                </div>
              )}

              {/* Composer / voice orb */}
              {voiceMode ? (
                <div className="voice-bar">
                  <button
                    className={`voice-orb${voiceRecState === "recording" ? " is-recording" : ""}${voiceRecState === "transcribing" ? " is-transcribing" : ""}`}
                    aria-label={voiceRecState === "recording" ? "Stop recording" : "Tap to speak"}
                    onClick={voiceRecState === "recording" ? cancelVoiceRecording : startVoiceRecording}
                    disabled={voiceRecState === "transcribing" || streaming}
                  >
                    <span className="voice-orb__ring" style={voiceRecState === "recording" ? { transform: `scale(${1 + audioLevel * 8})`, opacity: 0.6 } : undefined} />
                    <span className="voice-orb__ring voice-orb__ring--2" style={voiceRecState === "recording" ? { transform: `scale(${1 + audioLevel * 12})`, opacity: 0.3 } : undefined} />
                    <Mic size={20} />
                  </button>
                  <div className="voice-bar__caption">
                    {voiceRecState === "recording" ? "Listening..." : voiceRecState === "transcribing" ? "Transcribing..." : "Tap to speak"}
                  </div>
                </div>
              ) : (
                <div className="chat2__input">
                  <button
                    className="ch-mic"
                    onClick={() => setVoiceMode(true)}
                    aria-label="Switch to voice"
                    title="Voice input"
                    type="button"
                  >
                    <HubIco d={micEl} size={17} color="#fff" />
                  </button>
                  <textarea
                    className="chat2__textarea"
                    placeholder="Ask anything…"
                    value={draft}
                    rows={1}
                    onChange={(e) => {
                      setDraft(e.target.value);
                      const el = e.target;
                      el.style.height = "auto";
                      el.style.height = `${Math.min(el.scrollHeight, 100)}px`;
                    }}
                    onKeyDown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); send(); } }}
                    disabled={streaming}
                  />
                  <button
                    className="ch-send"
                    onClick={send}
                    disabled={!draft.trim() || streaming}
                    aria-label="Send message"
                    type="button"
                  >
                    {streaming
                      ? <Zap size={14} color="#fff" />
                      : <HubIco d={HP_PATHS.chevR} size={18} color="#fff" sw={2.5} />}
                  </button>
                </div>
              )}
            </>
          ) : (
            <button
              className="chat-dock__pill"
              onClick={() => setDockOpen(true)}
              title="Show chat"
              aria-expanded={dockOpen}
            >
              <GooseAvatar size={24} />
              <span>Chat</span>
              <span className="chat-dock__count">
                {thread.filter((m) => m.role !== "tool").length}
              </span>
              <ChevronRight size={12} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

