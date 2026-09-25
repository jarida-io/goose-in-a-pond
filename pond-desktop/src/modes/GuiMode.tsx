import { Sidebar } from "../components/Sidebar";
import { ToastContainer } from "../components/Toast";
import { useAppState } from "../state/AppContext";
import type { GuiSection } from "../desktopState";
import { Hub } from "../hub/Hub";
import { HubOverlay } from "../hub/overlays/HubOverlay";

import { Dashboard } from "../sections/Dashboard";
import { Chat } from "../sections/Chat";
import { Devices } from "../sections/Devices";
import { Mesh } from "../sections/Mesh";
import { Pairing } from "../sections/Pairing";
import { Schedules } from "../sections/Schedules";
import { Context } from "../sections/Context";
import { Skills } from "../sections/Skills";
import { Recipes } from "../sections/Recipes";
import { Models } from "../sections/Models";
import { Prompts } from "../sections/Prompts";
import { SettingsCatalogueView } from "../settings/SettingsCatalogue";
import { Faces } from "../sections/Faces";
import { Canvas } from "../sections/Canvas";
import { Logs } from "../sections/Logs";
import { Extensions } from "../sections/Extensions";
import { Notifications } from "../sections/Notifications";

function SectionContent({ section }: { section: GuiSection }) {
  switch (section) {
    case "dashboard": return <Dashboard />;
    case "chat":      return <Chat />;
    case "devices":   return <Devices />;
    case "mesh":      return <Mesh />;
    case "pairing":   return <Pairing />;
    case "schedules":     return <Schedules />;
    case "notifications": return <Notifications />;
    case "skills":     return <Skills />;
    case "recipes":    return <Recipes />;
    case "extensions": return <Extensions />;
    case "models":    return <Models />;
    case "prompts":   return <Prompts />;
    case "context":   return <Context />;
    case "settings":  return <SettingsCatalogueView />;
    case "faces":     return <Faces />;
    case "canvas":    return <Canvas />;
    case "logs":      return <Logs />;
    case "hub":       return null; // rendered by GuiMode before this switch
  }
}

export function GuiMode() {
  const state = useAppState();

  if (state.section === "hub") {
    return <Hub />;
  }

  return (
    <div className="app-shell">
      <Sidebar />
      <div className="app-main">
        <main className="app-content">
          <SectionContent section={state.section} />
        </main>
      </div>
      <ToastContainer />
      <HubOverlay />
    </div>
  );
}
