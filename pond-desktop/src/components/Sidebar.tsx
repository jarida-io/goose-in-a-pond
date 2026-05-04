import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@heroui/react";
import { SIDEBAR_GROUPS, type GuiSection } from "../desktopState";
import { useAppState, useAppDispatch } from "../state/AppContext";
import logoSrc from "../assets/logo.png";
import {
  LayoutDashboard,
  MessageCircle,
  Monitor,
  CalendarClock,
  Brain,
  Sparkles,
  ScrollText,
  Box,
  PenLine,
  Settings,
  Bot,
  ChevronLeft,
  ChevronRight,
  Mic,
  ScanFace,
  Layers,
} from "lucide-react";

// ── Icon map — standard lucide icons matching each section's intent ──────────

const NAV_ICONS: Record<string, React.ElementType> = {
  home:     LayoutDashboard,
  chat:     MessageCircle,
  devices:  Monitor,
  clock:    CalendarClock,
  memory:   Brain,
  skills:   Sparkles,
  logs:     ScrollText,
  model:    Box,
  prompt:   PenLine,
  settings: Settings,
  face:     ScanFace,
  "voice-id": Mic,
  canvas:   Layers,
  agent:    Bot,
};

function NavIcon({ name }: { name: string }) {
  const Icon = NAV_ICONS[name];
  if (!Icon) return <span style={{ width: 16, height: 16, display: "inline-block" }} />;
  return <Icon size={16} strokeWidth={1.8} />;
}

// ── Sidebar Component ─────────────────────────────────────────────────────────

export function Sidebar() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  const [collapsed, setCollapsed] = useState<boolean>(
    () => localStorage.getItem("pond_sidebar_collapsed") === "true"
  );

  useEffect(() => {
    function onResize() {
      if (window.innerWidth < 768 && !localStorage.getItem("pond_sidebar_collapsed")) {
        setCollapsed(true);
      }
    }
    window.addEventListener("resize", onResize);
    onResize();
    return () => window.removeEventListener("resize", onResize);
  }, []);

  function toggleCollapsed() {
    const next = !collapsed;
    setCollapsed(next);
    localStorage.setItem("pond_sidebar_collapsed", String(next));
  }

  function navigate(section: GuiSection) {
    dispatch({ type: "SET_SECTION", payload: section });
  }

  async function switchToVoice() {
    dispatch({ type: "SET_MODE", payload: "voice" });
  }

  async function switchToCanvas() {
    dispatch({ type: "SET_MODE", payload: "canvas" });
    try { await invoke("show_canvas"); } catch { /* ignore */ }
  }

  return (
    <aside
      className={`sidebar ${collapsed ? "is-collapsed" : ""}`}
      aria-label="Navigation"
      style={collapsed ? { width: "var(--sidebar-width-collapsed)", minWidth: "var(--sidebar-width-collapsed)" } : undefined}
    >
      {/* ── Brand ── */}
      <div className="sidebar__brand" style={collapsed ? { justifyContent: "center", padding: "12px 8px 14px" } : undefined}>
        <div className="sidebar__logo">
          <img src={logoSrc} alt="" style={{ width: 26, height: 26, objectFit: "contain" }} />
        </div>
        {!collapsed && <span className="sidebar__brand-name">Goose In A Pond</span>}
      </div>

      {/* ── Navigation groups ── */}
      <nav className="sidebar__nav">
        {SIDEBAR_GROUPS.map((group, gi) => (
          <div className="sidebar__group" key={gi}>
            {group.label && !collapsed && (
              <div className="sidebar__group-label">{group.label}</div>
            )}
            {group.sections.map((item) => {
              const active = state.section === item.section;
              return (
                <button
                  key={item.section}
                  className={`sidebar__item ${active ? "is-active" : ""}`}
                  onClick={() => navigate(item.section)}
                  aria-current={active ? "page" : undefined}
                  aria-label={item.label}
                  title={collapsed ? item.label : undefined}
                  style={collapsed ? { justifyContent: "center", padding: "0 4px" } : undefined}
                >
                  <span className="sidebar__icon">
                    <NavIcon name={item.icon} />
                  </span>
                  {!collapsed && <span>{item.label}</span>}
                </button>
              );
            })}
          </div>
        ))}
      </nav>

      {/* ── Footer ── */}
      <div className="sidebar__footer">
        {/* Server status */}
        <div className="sidebar__status" style={collapsed ? { justifyContent: "center" } : undefined}>
          <span className={`sidebar__status-dot ${state.serverOnline ? "is-connected" : ""}`} />
          {!collapsed && (
            <span>
              {state.serverOnline ? "Connected" : state.serverStarting ? "Starting\u2026" : "Offline"}
            </span>
          )}
        </div>

        {/* Mode buttons */}
        <div className="sidebar__actions" style={collapsed ? { flexDirection: "column" } : undefined}>
          <Button
            size="sm"
            variant="bordered"
            className="sidebar__action-btn"
            onPress={switchToVoice}
            aria-label="Voice mode"
          >
            <Mic size={14} />
            {!collapsed && "Voice"}
          </Button>
          <Button
            size="sm"
            variant="bordered"
            className="sidebar__action-btn"
            onPress={switchToCanvas}
            aria-label="Canvas mode"
          >
            <Layers size={14} />
            {!collapsed && "Canvas"}
          </Button>
        </div>

        {/* Collapse toggle */}
        <button
          className="sidebar__collapse-btn"
          onClick={toggleCollapsed}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          style={{
            display: "flex", alignItems: "center", justifyContent: "center",
            height: 24, background: "transparent", border: "none", cursor: "pointer",
            color: "var(--grey-400)", borderRadius: "var(--radius-sm)",
            margin: "2px 8px 0", width: "calc(100% - 16px)",
            transition: "color 0.15s",
          }}
        >
          {collapsed ? <ChevronRight size={14} /> : <ChevronLeft size={14} />}
        </button>
      </div>
    </aside>
  );
}
