import { useState } from "react";
import type { ComponentType } from "react";
import { IconRail } from "./IconRail";
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
import { PrivacyDetail } from "./views/settings/Privacy";
import { RoomsDetail } from "./views/settings/Rooms";
import { CamerasDetail } from "./views/settings/Cameras";
import { ConnectionsDetail } from "./views/settings/Connections";
import { NotificationsDetail } from "./views/settings/Notifications";
import { AppearanceView } from "./views/settings/Appearance";
import { AccountDetail } from "./views/settings/Account";
import { HubOverlay } from "./overlays/HubOverlay";
import type { SettingsRowId } from "./data/settingsConfig";

// ─── Route types ──────────────────────────────────────────────
// "notifications" is not in the IconRail list; the rail-foot and Home bells open it.
type HubRoute = "home" | "chat" | "canvas" | "routines" | "settings" | "notifications";
const TOP_NAV: HubRoute[] = ["home", "chat", "canvas", "routines", "settings", "notifications"];

// ─── Detail screen registry ───────────────────────────────────
type DetailComponent = ComponentType<{ go: (r: string) => void }>;

const SETTINGS_VIEWS: Record<SettingsRowId, DetailComponent> = {
  models:        ModelsDetail,
  prompts:       PromptsDetail,
  voice:         VoiceDetail,
  memory:        MemoryDetail,
  extensions:    ExtensionsDetail,
  logs:          LogsDetail,
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

  // <HubOverlay /> below handles the hub:device / hub:camera / hub:category events.

  function go(r: string) {
    setRoute(r);
    writeStoredRoute(r);
  }

  // Sub-screens light "settings" in the rail; "notifications" keeps its own key for BellShortcut.
  const railActive: HubRoute = TOP_NAV.includes(route as HubRoute)
    ? (route as HubRoute)
    : "settings";

  function renderView() {
    // Top-level routes
    if (route === "home")          return <HomeView go={go} />;
    if (route === "chat")          return <ChatHubView />;
    if (route === "canvas")        return <CanvasHubView />;
    if (route === "routines")      return <RoutinesView />;
    if (route === "settings")      return <SettingsHubView go={go} />;
    if (route === "notifications") return <NotificationsView go={go} />;

    // Settings sub-routes
    if (SETTINGS_ROUTE_IDS.includes(route as SettingsRowId)) {
      const Detail = SETTINGS_VIEWS[route as SettingsRowId];
      return <Detail go={go} />;
    }

    return <HomeView go={go} />;
  }

  return (
    <div className="ghub">
      <IconRail active={railActive} go={go} />
      <main className="ghub__main" key={route}>
        {renderView()}
      </main>
      <HubOverlay />
    </div>
  );
}
