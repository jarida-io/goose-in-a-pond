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
  Puzzle,
  Settings,
  Bot,
  ChevronLeft,
  ChevronRight,
  Mic,
  ScanFace,
  Layers,
  QrCode,
} from "lucide-react";

// ── Icon map — standard lucide icons matching each section's intent ──────────

const NAV_ICONS: Record<string, React.ElementType> = {
  dashboard:  LayoutDashboard,
  home:       LayoutDashboard,
  chat:       MessageCircle,
  devices:    Monitor,
  pairing:    QrCode,
  clock:      CalendarClock,
  memory:     Brain,
  skills:     Sparkles,
  logs:       ScrollText,
  extensions: Puzzle,
  model:      Box,
  prompt:     PenLine,
  settings:   Settings,
  face:       ScanFace,
  canvas:     Layers,
  agent:      Bot,
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
    // Try Tauri native canvas window first; fall back to inline section
    try {
      await invoke("show_canvas");
      dispatch({ type: "SET_MODE", payload: "canvas" });
    } catch {
      dispatch({ type: "SET_SECTION", payload: "canvas" });
    }
  }

  return (
    <aside
      className={`sidebar ${collapsed ? "is-collapsed" : ""}`}
      aria-label="Navigation"
    >
      {/* ── Brand ── */}
      <div className={`sidebar__brand${collapsed ? " sidebar__brand--collapsed" : ""}`}>
        <div className="sidebar__logo">
          <img src={logoSrc} alt="" style={{ width: 26, height: 26, objectFit: "contain" }} />
        </div>
        {!collapsed && <span className="sidebar__brand-name">Goose In A Pond</span>}
      </div>

      {/* ── Mode buttons (Voice + Canvas) — at top ── */}
      <div className="sidebar__actions">
        <Button
          size="sm"
          variant="outline"
          isIconOnly={collapsed}
          className="sidebar__action-btn"
          onPress={switchToVoice}
          aria-label="Voice mode"
        >
          <Mic size={14} />
          {!collapsed && "Voice"}
        </Button>
        <Button
          size="sm"
          variant="outline"
          isIconOnly={collapsed}
          className="sidebar__action-btn"
          onPress={switchToCanvas}
          aria-label="Canvas mode"
        >
          <Layers size={14} />
          {!collapsed && "Canvas"}
        </Button>
      </div>

      {/* ── Navigation groups ── */}
      <nav className="sidebar__nav">
        {SIDEBAR_GROUPS.map((group, gi) => (
          <div className="sidebar__group" key={gi}>
            {group.label && !collapsed && (
              <div className="sidebar__group-label">{group.label.charAt(0) + group.label.slice(1).toLowerCase()}</div>
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
        <div className="sidebar__status">
          <span className={`sidebar__status-dot ${state.serverOnline ? "is-connected" : ""}`} />
          {!collapsed && (
            <span>
              {state.serverOnline ? "Connected" : state.serverStarting ? "Starting\u2026" : "Offline"}
            </span>
          )}
        </div>

        {/* Collapse toggle */}
        <button
          className="sidebar__collapse-btn"
          onClick={toggleCollapsed}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        >
          {collapsed ? <ChevronRight size={14} /> : <ChevronLeft size={14} />}
        </button>
      </div>
    </aside>
  );
}
