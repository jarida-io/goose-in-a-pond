import { useState, useRef, useEffect } from "react";
import { Button, Chip } from "@heroui/react";
import {
  Layers, Mic, RefreshCw, ExternalLink, Zap, X, Send,
  MessageSquare, ChevronLeft, ChevronRight,
} from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";

// ── Design tokens ──────────────────────────────────────────────
const C = {
  purple: "#7C3AED",
  purpleMid: "#8C4BFF",
  purpleBg: "#F4ECFF",
  purpleBorder: "rgba(140,75,255,0.25)",
  grey50: "#FAFAFA",
  grey100: "#F5F5F5",
  grey200: "#EDEDED",
  grey300: "#D4D4D4",
  grey500: "#A3A3A3",
  grey600: "#737373",
  grey700: "#525252",
  grey800: "#262626",
  grey900: "#111111",
  white: "#FFFFFF",
  green: "#16A34A",
};

// ── Types ──────────────────────────────────────────────────────

interface CanvasCard {
  id: number;
  kind: string;
  tool: string;
  title: string;
}

interface ChatMessage {
  role: "user" | "assistant" | "tool";
  text: string;
  tool?: string;
  status?: "running" | "ok";
}

// ── Placeholder card ───────────────────────────────────────────
// Renders a generic empty card slot — actual MCP-UI rendering will
// be wired later when the tool-call protocol is connected.

function PlaceholderCard({ card, onClose }: { card: CanvasCard; onClose: () => void }) {
  return (
    <div style={cardStyles.root}>
      <div style={cardStyles.chrome}>
        <div style={cardStyles.chromeLeft}>
          <Zap size={12} style={{ color: C.purple }} />
          <code style={cardStyles.tool}>{card.tool}</code>
        </div>
        <button onClick={onClose} style={cardStyles.closeBtn} aria-label="Close card">
          <X size={14} />
        </button>
      </div>
      <div style={cardStyles.body}>
        <div style={cardStyles.placeholder}>
          <Layers size={32} style={{ color: C.grey300 }} />
          <span style={cardStyles.placeholderTitle}>{card.title}</span>
          <span style={cardStyles.placeholderHint}>
            MCP tool result will render here
          </span>
        </div>
      </div>
    </div>
  );
}

const cardStyles: Record<string, React.CSSProperties> = {
  root: {
    background: C.white,
    borderRadius: 14,
    border: `1px solid ${C.grey200}`,
    overflow: "hidden",
    animation: "ob-fade 0.3s ease both",
  },
  chrome: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "8px 12px",
    background: C.grey50,
    borderBottom: `1px solid ${C.grey100}`,
    fontSize: 12,
  },
  chromeLeft: {
    display: "flex",
    alignItems: "center",
    gap: 6,
  },
  tool: {
    fontFamily: "var(--font-mono)",
    fontSize: 11,
    color: C.grey600,
  },
  closeBtn: {
    background: "none",
    border: "none",
    cursor: "pointer",
    color: C.grey500,
    padding: 2,
    borderRadius: 4,
  },
  body: {
    padding: 24,
  },
  placeholder: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: 10,
    padding: "32px 16px",
    textAlign: "center",
  },
  placeholderTitle: {
    fontSize: 14,
    fontWeight: 600,
    color: C.grey700,
  },
  placeholderHint: {
    fontSize: 12,
    color: C.grey500,
    maxWidth: "28ch",
    lineHeight: 1.5,
  },
};

// ── Suggestion chips ───────────────────────────────────────────
// These are placeholders for the types of MCP-UI cards that can
// render. When wired, clicking one would send a prompt to the LLM.

const SUGGEST_CHIPS = [
  { key: "ride", label: "Ride", icon: "🚗" },
  { key: "bio", label: "Biography", icon: "📚" },
  { key: "shop", label: "Shopping", icon: "🛒" },
  { key: "weather", label: "Weather", icon: "🌤️" },
  { key: "flight", label: "Flight", icon: "✈️" },
];

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
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
  }, [thread]);

  function addPlaceholderCard(key: string) {
    const chip = SUGGEST_CHIPS.find((c) => c.key === key);
    if (!chip) return;
    const tool = `mcp.${key}.placeholder`;
    setThread((t) => [
      ...t,
      { role: "user", text: `Show ${chip.label.toLowerCase()} card` },
      { role: "tool", text: "", tool, status: "ok" },
      { role: "assistant", text: `Here's a placeholder for the ${chip.label} card. Connect the MCP tool to see real data.` },
    ]);
    setCards((c) => [{ id: Date.now(), kind: key, tool, title: `${chip.icon} ${chip.label}` }, ...c]);
  }

  function closeCard(id: number) {
    setCards((c) => c.filter((card) => card.id !== id));
  }

  function send() {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    setThread((t) => [
      ...t,
      { role: "user", text },
      { role: "assistant", text: "Canvas is in placeholder mode. Try the suggestion chips to see card layouts." },
    ]);
  }

  return (
    <div className="screen screen--canvas" style={layoutStyles.root}>
      <style>{`
        @keyframes ob-fade { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: translateY(0); } }
        @keyframes pulse-ring { 0%, 100% { transform: scale(1); opacity: 0.3; } 50% { transform: scale(1.5); opacity: 0; } }
      `}</style>

      {/* ── Toolbar ──────────────────────────────────────── */}
      <div style={layoutStyles.toolbar}>
        <div style={layoutStyles.toolbarLeft}>
          <h1 style={layoutStyles.title}>Canvas</h1>
          <Chip size="sm" variant="soft">
            <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
              <Layers size={12} /> MCP-UI
            </span>
          </Chip>
          <Chip size="sm" variant="soft" color={voiceMode ? "secondary" : "success"}>
            <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
              <span style={{
                width: 6, height: 6, borderRadius: "50%",
                background: voiceMode ? C.purpleMid : C.green,
                display: "inline-block",
              }} />
              {voiceMode ? "voice mode" : "live"}
            </span>
          </Chip>
        </div>
        <div style={layoutStyles.toolbarRight}>
          <Button
            size="sm"
            variant={voiceMode ? "solid" : "outline"}
            color={voiceMode ? "secondary" : "default"}
            onPress={() => setVoiceMode((v) => !v)}
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
          <Button size="sm" variant="outline">
            <ExternalLink size={14} /> Pop out
          </Button>
        </div>
      </div>

      {/* ── Stage: canvas + floating dock ───────────────── */}
      <div style={layoutStyles.stage}>

        {/* Full-bleed canvas pane */}
        <div style={layoutStyles.canvasPane}>
          <div style={layoutStyles.canvasHead}>
            <div style={layoutStyles.canvasTitle}>
              <Layers size={14} />
              <span>Canvas</span>
              <Chip size="sm" variant="soft">{cards.length}</Chip>
            </div>
            <span style={layoutStyles.canvasHint}>Auto-renders when tools return UI</span>
          </div>

          <div style={layoutStyles.canvasBody}>
            {cards.length === 0 ? (
              <div style={layoutStyles.emptyState}>
                <Layers size={36} style={{ color: C.grey300 }} />
                <div style={{ fontSize: 16, fontWeight: 600, color: C.grey700, marginTop: 8 }}>
                  Nothing rendered yet
                </div>
                <div style={{ fontSize: 13, color: C.grey500, maxWidth: "34ch", lineHeight: 1.5 }}>
                  Ask Pond for a ride, a biography, or shopping suggestions and the result will materialize here.
                </div>
              </div>
            ) : (
              <div style={layoutStyles.cardGrid}>
                {cards.map((card) => (
                  <PlaceholderCard key={card.id} card={card} onClose={() => closeCard(card.id)} />
                ))}
              </div>
            )}
          </div>
        </div>

        {/* Floating chat dock */}
        <div style={{
          ...layoutStyles.dock,
          ...(dockOpen ? layoutStyles.dockOpen : layoutStyles.dockCollapsed),
        }}>
          {dockOpen ? (
            <>
              <div style={layoutStyles.dockHead}>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <div style={layoutStyles.avatar}>P</div>
                  <span style={{ fontWeight: 600, fontSize: 14, color: C.grey900 }}>Pond</span>
                  <span style={{ fontSize: 12, color: C.grey500 }}>{voiceMode ? "voice" : "chat"}</span>
                </div>
                <button onClick={() => setDockOpen(false)} style={layoutStyles.iconBtn} title="Hide chat">
                  <ChevronLeft size={14} />
                </button>
              </div>

              <div ref={scrollRef} style={layoutStyles.dockBody}>
                {thread.map((m, i) => {
                  if (m.role === "tool") {
                    return (
                      <div key={i} style={layoutStyles.toolChip}>
                        <Zap size={12} style={{ color: C.purple }} />
                        <code style={{ fontSize: 11, color: C.grey600 }}>{m.tool}</code>
                        <span style={{ fontSize: 11, color: m.status === "ok" ? C.green : C.grey500 }}>
                          {m.status === "ok" ? "returned" : "running…"}
                        </span>
                      </div>
                    );
                  }
                  return (
                    <div key={i} style={{
                      ...layoutStyles.bubble,
                      ...(m.role === "user" ? layoutStyles.bubbleUser : layoutStyles.bubbleAssistant),
                    }}>
                      <div style={{ fontSize: 11, fontWeight: 600, color: m.role === "user" ? C.purpleMid : C.grey500, marginBottom: 4 }}>
                        {m.role === "user" ? "You" : "Pond"}
                      </div>
                      <div style={{ fontSize: 13, lineHeight: 1.55 }}>{m.text}</div>
                    </div>
                  );
                })}
              </div>

              {/* Suggest chips */}
              {!voiceMode && (
                <div style={layoutStyles.suggest}>
                  <span style={{ fontSize: 11, color: C.grey500, fontWeight: 600 }}>Try</span>
                  {SUGGEST_CHIPS.map((chip) => (
                    <button
                      key={chip.key}
                      onClick={() => addPlaceholderCard(chip.key)}
                      style={layoutStyles.suggestChip}
                    >
                      {chip.icon} {chip.label}
                    </button>
                  ))}
                </div>
              )}

              {/* Composer or voice orb */}
              {voiceMode ? (
                <div style={layoutStyles.voiceBar}>
                  <div style={layoutStyles.voiceOrb}>
                    <Mic size={20} />
                  </div>
                  <span style={{ fontSize: 12, color: C.grey500 }}>Tap to speak</span>
                </div>
              ) : (
                <div style={layoutStyles.composer}>
                  <input
                    style={layoutStyles.composerInput}
                    placeholder="Ask anything…"
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") send(); }}
                  />
                  <Button
                    isIconOnly
                    size="sm"
                    color="secondary"
                    onPress={send}
                    isDisabled={!draft.trim()}
                  >
                    <Send size={14} />
                  </Button>
                </div>
              )}
            </>
          ) : (
            <button onClick={() => setDockOpen(true)} style={layoutStyles.dockPill} title="Show chat">
              <div style={layoutStyles.avatar}>P</div>
              <span style={{ fontWeight: 600, fontSize: 13 }}>Chat</span>
              <span style={{ fontSize: 11, color: C.grey500, background: C.grey100, padding: "1px 6px", borderRadius: 999 }}>
                {thread.filter((m) => m.role !== "tool").length}
              </span>
              <ChevronRight size={12} style={{ color: C.grey500 }} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Styles ──────────────────────────────────────────────────────

const layoutStyles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    height: "100%",
    maxWidth: "none",
    background: C.grey50,
  },
  toolbar: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "12px 20px",
    borderBottom: `1px solid ${C.grey200}`,
    background: C.white,
    flexShrink: 0,
  },
  toolbarLeft: {
    display: "flex",
    alignItems: "center",
    gap: 10,
  },
  toolbarRight: {
    display: "flex",
    alignItems: "center",
    gap: 8,
  },
  title: {
    margin: 0,
    fontSize: 18,
    fontWeight: 700,
    color: C.grey900,
  },
  stage: {
    flex: 1,
    display: "flex",
    position: "relative",
    overflow: "hidden",
    minHeight: 0,
  },
  canvasPane: {
    flex: 1,
    display: "flex",
    flexDirection: "column",
    minWidth: 0,
  },
  canvasHead: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "10px 20px",
    borderBottom: `1px solid ${C.grey100}`,
    background: C.white,
  },
  canvasTitle: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    fontWeight: 600,
    fontSize: 13,
    color: C.grey700,
  },
  canvasHint: {
    fontSize: 11,
    color: C.grey500,
  },
  canvasBody: {
    flex: 1,
    overflowY: "auto",
    padding: 24,
  },
  emptyState: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    height: "100%",
    gap: 6,
    textAlign: "center",
  },
  cardGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fill, minmax(320px, 1fr))",
    gap: 16,
  },
  // ── Dock ──
  dock: {
    position: "absolute",
    left: 0,
    top: 0,
    bottom: 0,
    display: "flex",
    flexDirection: "column",
    zIndex: 10,
    transition: "width 0.25s ease, opacity 0.2s",
  },
  dockOpen: {
    width: 360,
    background: "rgba(255,255,255,0.92)",
    backdropFilter: "blur(18px)",
    WebkitBackdropFilter: "blur(18px)",
    borderRight: `1px solid ${C.grey200}`,
  },
  dockCollapsed: {
    width: 52,
    background: "transparent",
  },
  dockHead: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "12px 16px",
    borderBottom: `1px solid ${C.grey100}`,
    flexShrink: 0,
  },
  dockBody: {
    flex: 1,
    overflowY: "auto",
    padding: "12px 14px",
    display: "flex",
    flexDirection: "column",
    gap: 10,
  },
  dockPill: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: 8,
    padding: "16px 8px",
    background: "rgba(255,255,255,0.9)",
    backdropFilter: "blur(12px)",
    border: `1px solid ${C.grey200}`,
    borderRadius: 12,
    cursor: "pointer",
    margin: "12px 4px",
    fontSize: 12,
    color: C.grey700,
  },
  avatar: {
    width: 28,
    height: 28,
    borderRadius: 8,
    background: C.purpleBg,
    color: C.purpleMid,
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    fontSize: 13,
    fontWeight: 700,
    flexShrink: 0,
  },
  iconBtn: {
    background: "none",
    border: "none",
    cursor: "pointer",
    color: C.grey500,
    padding: 4,
    borderRadius: 6,
  },
  bubble: {
    padding: "10px 14px",
    borderRadius: 12,
    maxWidth: "92%",
    color: C.grey800,
  },
  bubbleUser: {
    alignSelf: "flex-end",
    background: C.purpleBg,
    border: `1px solid ${C.purpleBorder}`,
  },
  bubbleAssistant: {
    alignSelf: "flex-start",
    background: C.white,
    border: `1px solid ${C.grey200}`,
  },
  toolChip: {
    display: "flex",
    alignItems: "center",
    gap: 6,
    padding: "5px 10px",
    borderRadius: 8,
    background: C.grey50,
    border: `1px solid ${C.grey100}`,
    alignSelf: "center",
  },
  suggest: {
    display: "flex",
    alignItems: "center",
    gap: 6,
    padding: "8px 14px",
    borderTop: `1px solid ${C.grey100}`,
    flexWrap: "wrap",
    flexShrink: 0,
  },
  suggestChip: {
    padding: "4px 10px",
    borderRadius: 999,
    border: `1px solid ${C.grey200}`,
    background: C.white,
    fontSize: 12,
    cursor: "pointer",
    color: C.grey700,
    fontWeight: 500,
    whiteSpace: "nowrap",
  },
  voiceBar: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: 10,
    padding: "16px 14px",
    borderTop: `1px solid ${C.grey100}`,
    flexShrink: 0,
  },
  voiceOrb: {
    width: 52,
    height: 52,
    borderRadius: "50%",
    background: C.purpleMid,
    color: C.white,
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    cursor: "pointer",
    boxShadow: "0 4px 16px rgba(124,58,237,0.3)",
  },
  composer: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    padding: "10px 14px",
    borderTop: `1px solid ${C.grey100}`,
    flexShrink: 0,
  },
  composerInput: {
    flex: 1,
    padding: "8px 12px",
    borderRadius: 8,
    border: `1px solid ${C.grey200}`,
    background: C.white,
    fontSize: 13,
    color: C.grey800,
    outline: "none",
  },
};
