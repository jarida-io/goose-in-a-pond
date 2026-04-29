import { Sidebar } from "../components/Sidebar";
import { useAppState } from "../state/AppContext";
import type { GuiSection } from "../desktopState";

// Lazy section imports
import { Dashboard } from "../sections/Dashboard";
import { Chat } from "../sections/Chat";
import { Devices } from "../sections/Devices";
import { Schedules } from "../sections/Schedules";
import { Memory } from "../sections/Memory";
import { Skills } from "../sections/Skills";
import { Models } from "../sections/Models";
import { Prompts } from "../sections/Prompts";
import { Settings } from "../sections/Settings";
import { Faces } from "../sections/Faces";
import { Agent } from "../sections/Agent";
import { Canvas } from "../sections/Canvas";
import { Logs } from "../sections/Logs";

function SectionContent({ section }: { section: GuiSection }) {
  switch (section) {
    case "dashboard": return <Dashboard />;
    case "chat":      return <Chat />;
    case "devices":   return <Devices />;
    case "schedules": return <Schedules />;
    case "memory":    return <Memory />;
    case "skills":    return <Skills />;
    case "models":    return <Models />;
    case "prompts":   return <Prompts />;
    case "settings":  return <Settings />;
    case "faces":     return <Faces />;
    case "agent":     return <Agent />;
    case "canvas":    return <Canvas />;
    case "logs":      return <Logs />;
  }
}

export function GuiMode() {
  const state = useAppState();

  return (
    <div className="app-shell">
      <Sidebar />
      <div className="app-main">
        <main className="app-content">
          <SectionContent section={state.section} />
        </main>
      </div>
    </div>
  );
}
