import { useState } from "react";
import type { ComponentType } from "react";
import { HubDrawer, type DrawerNav } from "./HubDrawer";
import { ShellBar } from "./ShellBar";
import { useAppState, useAppDispatch } from "../state/AppContext";
import type { GuiSection } from "../desktopState";
import type { ScheduleRunNotification } from "../api/types";
import { HomeView } from "./views/Home";
import { ChatHubView } from "./views/ChatHub";
import { CanvasHubView } from "./views/CanvasHub";
import { RoutinesView } from "./views/Routines";
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
import { RoomsDetail } from "./views/settings/Rooms";
import { CamerasDetail } from "./views/settings/Cameras";
import { ConnectionsDetail } from "./views/settings/Connections";
import { NotificationsDetail } from "./views/settings/Notifications";
// Appearance owns its own state via useTheme(); it does not navigate, so the
// wrapper below silently drops the `go` prop.
import { AppearanceView } from "./views/settings/Appearance";
import { AccountDetail } from "./views/settings/Account";
import { HubOverlay } from "./overlays/HubOverlay";
import type { SettingsRowId } from "./data/settingsConfig";

// ─── Route types ──────────────────────────────────────────────
// "notifications" is reached from the bell in the shell bar and from the Home
// header bell, never from the drawer's list — a notification you have to open
// a menu to find is one you find late.
type HubRoute = "home" | "chat" | "canvas" | "routines" | "settings" | "notifications";

// ─── The drawer speaks GuiSection; the hub speaks HubRoute ────
//
// One drawer serves both shells, so it emits the vocabulary they share. These
// two tables are that translation, and they are deliberately partial: a section
// with no hub screen is NOT an error and must not be silently swallowed. It
// falls through to SET_SECTION, which leaves the hub and lands in the classic
// shell — which is the blend, not a failure of it.
const HUB_ROUTE_FOR: Partial<Record<GuiSection, string>> = {
  dashboard:     "home",
  chat:          "chat",
  canvas:        "canvas",
  schedules:     "routines",
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
  settings:      "settings",
  notifications: "notifications",
  models:        "models",
  prompts:       "prompts",
  memory:        "context",
  extensions:    "extensions",
  logs:          "logs",
};

// ─── Detail screen registry ───────────────────────────────────
// ComponentType<{ go: (r: string) => void }> — each detail screen receives go()
type DetailComponent = ComponentType<{ go: (r: string) => void }>;

const SETTINGS_VIEWS: Record<SettingsRowId, DetailComponent> = {
  models:        ModelsDetail,
  prompts:       PromptsDetail,
  voice:         VoiceDetail,
  memory:        MemoryDetail,
  extensions:    ExtensionsDetail,
  logs:          LogsDetail,
  background:    BackgroundJobsDetail,
  privacy:       PrivacyDetail,
  rooms:         RoomsDetail,
  cameras:       CamerasDetail,
  connections:   ConnectionsDetail,
  notifications: NotificationsDetail,
  // AppearanceView doesn't accept a go prop — wrap it so the prop is silently dropped
  appearance:    (_props: { go: (r: string) => void }) => <AppearanceView />,
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

  // hub:device / hub:camera / hub:category CustomEvents are now handled by
  // <HubOverlay /> below, which mounts the appropriate modal (DeviceControl,
  // CameraModal, CategorySheet).

  function go(r: string) {
    setRoute(r);
    writeStoredRoute(r);
  }

  // A settings sub-screen marks "Settings" in the drawer unless the sub-screen
  // is itself one of the twelve under Manage, in which case it marks that chip.
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

  /**
   * One way a GuiSection is resolved in this shell, not two.
   *
   * Home's own calls to action emit sections -- "Add your first device" emits
   * `devices` -- and they used to be handed to `go`, which speaks hub routes.
   * `devices` matched nothing, fell through `renderView`'s fallback, and
   * re-rendered the very screen the household had just tapped to leave, with
   * the drawer marking Settings and the bad route persisted to localStorage.
   * `navigate` is the function that already knows a section with no hub screen
   * belongs to the classic shell, so everything goes through it.
   */
  function goToSection(section: GuiSection) {
    navigate({ kind: "section", section });
  }

  /**
   * A notification's call to action, carrying the run it was raised for.
   *
   * The run is the whole point of "View on Canvas": without it Canvas opens
   * with nothing to show and the household is left to work out what they were
   * meant to be looking at. The classic shell has always set this context; the
   * hub dropped it, because its `go` speaks routes and knows nothing about
   * runs. Sections are resolved through `goToSection` for the same reason
   * everything else is.
   */
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
    if (route === "settings")      return <SettingsHubView go={go} />;
    if (route === "notifications") return <NotificationsView go={openFromNotification} />;

    // Settings sub-routes
    if (SETTINGS_ROUTE_IDS.includes(route as SettingsRowId)) {
      const Detail = SETTINGS_VIEWS[route as SettingsRowId];
      return <Detail go={go} />;
    }

    // Fallback
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
