export type DesktopMode = "gui" | "voice" | "canvas";

export type GuiSection =
  | "dashboard"
  | "chat"
  | "devices"
  | "pairing"
  | "schedules"
  | "memory"
  | "skills"
  | "extensions"
  | "models"
  | "prompts"
  | "settings"
  | "faces"
  | "agent"
  | "canvas"
  | "logs"
  | "hub";

export const DESKTOP_MODES: DesktopMode[] = ["gui", "voice", "canvas"];

export type SidebarGroup = {
  label: string | null;
  sections: Array<{ section: GuiSection; label: string; icon: string }>;
};

export const SIDEBAR_GROUPS: SidebarGroup[] = [
  {
    label: "MAIN",
    sections: [
      { section: "dashboard", label: "Dashboard", icon: "dashboard" },
      { section: "chat",      label: "Chat",      icon: "chat" },
    ],
  },
  {
    label: "MANAGE",
    sections: [
      { section: "devices",   label: "Devices",   icon: "devices" },
      { section: "pairing",   label: "Pairing",   icon: "pairing" },
      { section: "schedules", label: "Schedules", icon: "clock" },
      { section: "memory",    label: "Memory",    icon: "memory" },
      { section: "skills",    label: "Skills",    icon: "skills" },
      { section: "logs",      label: "Logs",      icon: "logs" },
    ],
  },
  {
    label: "CONFIGURE",
    sections: [
      { section: "models",     label: "Models",     icon: "model" },
      { section: "prompts",    label: "Prompts",    icon: "prompt" },
      { section: "settings",   label: "Settings",   icon: "settings" },
      { section: "extensions", label: "Extensions", icon: "extensions" },
    ],
  },
];

/** Flat ordered list of all sidebar items with section key + display label. */
export const DESKTOP_SECTIONS: Array<{ section: GuiSection; label: string }> =
  SIDEBAR_GROUPS.flatMap((g) => g.sections.map(({ section, label }) => ({ section, label })));

// Include all valid sections — some are routable but not in the sidebar
// "hub" is hidden from the classic sidebar; entry is via Settings > "Preview Goose Hub"
const HIDDEN_SECTIONS: GuiSection[] = ["faces", "agent", "canvas", "hub"];
const SECTION_SET = new Set<GuiSection>([
  ...DESKTOP_SECTIONS.map((s) => s.section),
  ...HIDDEN_SECTIONS,
]);

export function normalizeDesktopMode(value: string | null | undefined): DesktopMode {
  return value === "voice" || value === "canvas" || value === "gui" ? value : "gui";
}

export function normalizeGuiSection(value: string | null | undefined): GuiSection {
  return value && SECTION_SET.has(value as GuiSection)
    ? (value as GuiSection)
    : "dashboard";
}

export function getDesktopSectionLabel(section: GuiSection): string {
  for (const group of SIDEBAR_GROUPS) {
    for (const s of group.sections) {
      if (s.section === section) return s.label;
    }
  }
  return section.charAt(0).toUpperCase() + section.slice(1);
}

export function getDesktopInitials(name: string): string {
  return (
    name
      .split(" ")
      .map((word) => word[0])
      .filter(Boolean)
      .slice(0, 2)
      .join("")
      .toUpperCase() || "?"
  );
}
