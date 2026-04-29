export type DesktopMode = "gui" | "voice" | "canvas";

export type GuiSection =
  | "dashboard"
  | "chat"
  | "devices"
  | "schedules"
  | "memory"
  | "skills"
  | "models"
  | "prompts"
  | "settings"
  | "faces"
  | "agent"
  | "canvas"
  | "logs";

export const DESKTOP_MODES: DesktopMode[] = ["gui", "voice", "canvas"];

export type SidebarGroup = {
  label: string | null;
  sections: Array<{ section: GuiSection; label: string; icon: string }>;
};

export const SIDEBAR_GROUPS: SidebarGroup[] = [
  {
    label: "MAIN",
    sections: [
      { section: "dashboard", label: "Dashboard", icon: "home" },
      { section: "chat",      label: "Chat",      icon: "chat" },
      { section: "canvas",   label: "Canvas",   icon: "canvas" },
    ],
  },
  {
    label: "MANAGE",
    sections: [
      { section: "devices",   label: "Devices",   icon: "devices" },
      { section: "schedules", label: "Schedules", icon: "clock" },
      { section: "memory",    label: "Memory",    icon: "memory" },
      { section: "skills",    label: "Skills",    icon: "skills" },
      { section: "logs",      label: "Logs",      icon: "logs" },
    ],
  },
  {
    label: "CONFIGURE",
    sections: [
      { section: "models",   label: "Models",   icon: "model" },
      { section: "prompts",  label: "Prompts",  icon: "prompt" },
      { section: "faces",    label: "Faces",    icon: "face" },
      { section: "settings", label: "Settings", icon: "settings" },
    ],
  },
  {
    label: null,
    sections: [
      { section: "agent", label: "Agent", icon: "agent" },
    ],
  },
];

/** Flat ordered list of all sidebar items with section key + display label. */
export const DESKTOP_SECTIONS: Array<{ section: GuiSection; label: string }> =
  SIDEBAR_GROUPS.flatMap((g) => g.sections.map(({ section, label }) => ({ section, label })));

const SECTION_SET = new Set<GuiSection>(DESKTOP_SECTIONS.map((s) => s.section));

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
