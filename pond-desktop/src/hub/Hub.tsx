import { useState } from "react";
import type { ComponentType } from "react";
import { HubDrawer, type DrawerNav } from "./HubDrawer";
import { ShellBar } from "./ShellBar";
import { useAppState, useAppDispatch } from "../state/useAppContext";
import type { GuiSection } from "../desktopState";
import type { ScheduleRunNotification } from "../api/types";
import { HomeView } from "./views/Home";
import { ChatHubView } from "./views/ChatHub";
import { CanvasHubView } from "./views/CanvasHub";
import { RoutinesView } from "./views/Routines";
import { AgentsView } from "./views/AgentsHub";
import { SettingsHubView } from "./views/SettingsHub";
import { NotificationsView } from "./views/Notifications";
import { ModelsDetail } from "./views/settings/Models";
import { PromptsDetail } from "./views/settings/Prompts";
import { VoiceDetail } from "./views/settings/Voice";
import { MemoryDetail } from "./views/settings/Memory";
import { ExtensionsDetail } from "./views/settings/Extensions";
import { LogsDetail } from "./views/settings/Logs";
import { BackgroundJobsDetail } from "./views/settings/BackgroundJobsDetail";
import { PrivacyDetail } from "./views/settings/Privacy";
import { NetworkDetail } from "./views/settings/Network";
import { RoomsDetail } from "./views/settings/Rooms";
import { CamerasDetail } from "./views/settings/Cameras";
import { ConnectionsDetail } from "./views/settings/Connections";
import { NotificationsDetail } from "./views/settings/Notifications";
import { AppearanceView } from "./views/settings/Appearance";
import { AccountDetail } from "./views/settings/Account";
import { HubOverlay } from "./overlays/HubOverlay";
import type { SettingsRowId } from "./data/settingsConfig";

// ─── The drawer speaks GuiSection; the hub speaks its own routes ──
// "notifications" is not in the IconRail list; the rail-foot and Home bells open it.
// Deliberately partial: a section with no hub screen falls through to the classic shell.
const HUB_ROUTE_FOR: Partial<Record<GuiSection, string>> = {
  dashboard:     "home",
  chat:          "chat",
  canvas:        "canvas",
  schedules:     "routines",
  recipes:       "agents",
  settings:      "settings",
  notifications: "notifications",
  models:        "models",
  prompts:       "prompts",
  context:       "memory",
  extensions:    "extensions",
  logs:          "logs",
};

const SECTION_FOR_HUB_ROUTE: Record<string, GuiSection> = {
  home:          "dashboard",
  chat:          "chat",
  canvas:        "canvas",
  routines:      "schedules",
  agents:        "recipes",
  settings:      "settings",
  notifications: "notifications",
  models:        "models",
  prompts:       "prompts",
  memory:        "context",
  extensions:    "extensions",
  logs:          "logs",
};

// ─── Detail screen registry ───────────────────────────────────
type DetailComponent = ComponentType<{ go: (r: string) => void }>;

const SETTINGS_VIEWS: Record<SettingsRowId, DetailComponent> = {
  models:        ModelsDetail,
  prompts:       PromptsDetail,
  voice:         VoiceDetail,
  memory:        MemoryDetail,
  extensions:    ExtensionsDetail,
  logs:          LogsDetail,
  background:    BackgroundJobsDetail,
  network:       NetworkDetail,
  privacy:       PrivacyDetail,
  rooms:         RoomsDetail,
  cameras:       CamerasDetail,
  connections:   ConnectionsDetail,
  notifications: NotificationsDetail,
  // AppearanceView doesn't accept a go prop — wrap it so the prop is silently dropped
  appearance:    () => <AppearanceView />,
  account:       AccountDetail,
};

const SETTINGS_ROUTE_IDS = Object.keys(SETTINGS_VIEWS) as SettingsRowId[];

// ─── localStorage helpers ─────────────────────────────────────
function readStoredRoute(): string {
  try {
    const stored = localStorage.getItem("goosehub_route");
    if (stored) return stored;
  } catch {
    // localStorage not available
  }
  return "home";
}

function writeStoredRoute(route: string): void {
  try {
    localStorage.setItem("goosehub_route", route);
  } catch {
    // ignore
  }
}

// ─── Hub shell ────────────────────────────────────────────────
export function Hub() {
  const [route, setRoute] = useState<string>(readStoredRoute);
  const [menuOpen, setMenuOpen] = useState(false);
  const appState = useAppState();
  const dispatch = useAppDispatch();

  // <HubOverlay /> below handles the hub:device / hub:camera / hub:category events.

  function go(r: string) {
    setRoute(r);
    writeStoredRoute(r);
  }

  // Sub-screens mark "Settings" unless they are one of the Manage chips.
  const drawerActive: GuiSection = SECTION_FOR_HUB_ROUTE[route] ?? "settings";

  function navigate(nav: DrawerNav) {
    if (nav.kind === "voice") {
      dispatch({ type: "SET_MODE", payload: "voice" });
      return;
    }
    const hubRoute = HUB_ROUTE_FOR[nav.section];
    if (hubRoute) {
      go(hubRoute);
      return;
    }
    // No hub screen for this one. Hand it to the classic shell, which has one.
    dispatch({ type: "SET_SECTION", payload: nav.section });
  }

  /** Sections go through `navigate`, which sends ones with no hub screen to the classic shell. */
  function goToSection(section: GuiSection) {
    navigate({ kind: "section", section });
  }

  /** A notification's call to action, keeping the run "View on Canvas" needs. */
  function openFromNotification(section: string, run?: ScheduleRunNotification) {
    if (run) dispatch({ type: "SET_DEBRIEF_CONTEXT", payload: { type: "debrief", run } });
    goToSection(section as GuiSection);
  }

  function renderView() {
    // Top-level routes
    if (route === "home")          return <HomeView go={goToSection} />;
    if (route === "chat")          return <ChatHubView />;
    if (route === "canvas")        return <CanvasHubView />;
    if (route === "routines")      return <RoutinesView />;
    if (route === "agents")        return <AgentsView go={go} />;
    if (route === "settings")      return <SettingsHubView go={go} />;
    if (route === "notifications") return <NotificationsView go={openFromNotification} />;

    // Settings sub-routes
    if (SETTINGS_ROUTE_IDS.includes(route as SettingsRowId)) {
      const Detail = SETTINGS_VIEWS[route as SettingsRowId];
      return <Detail go={go} />;
    }

    return <HomeView go={goToSection} />;
  }

  return (
    <div className="ghub ghub--stacked">
      <ShellBar
        onMenu={() => setMenuOpen(true)}
        onBell={() => go("notifications")}
        unread={appState.unreadRunCount}
      />
      <main className="ghub__main" key={route}>
        {renderView()}
      </main>
      <HubDrawer
        open={menuOpen}
        onClose={() => setMenuOpen(false)}
        active={drawerActive}
        onNavigate={navigate}
      />
      <HubOverlay />
    </div>
  );
}
