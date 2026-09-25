export type DesktopMode = "gui" | "voice";

export type GuiSection =
  | "dashboard"
  | "chat"
  | "devices"
  | "mesh"
  | "pairing"
  | "schedules"
  | "notifications"
  | "skills"
  | "context"
  | "recipes"
  | "extensions"
  | "models"
  | "prompts"
  | "settings"
  | "faces"
  | "canvas"
  | "logs"
  | "hub";

export const DESKTOP_MODES: DesktopMode[] = ["gui", "voice"];

export type SidebarGroup = {
  label: string | null;
  sections: Array<{ section: GuiSection; label: string; icon: string }>;
};

export const SIDEBAR_GROUPS: SidebarGroup[] = [
  {
    label: "MAIN",
    sections: [
      { section: "dashboard", label: "Home", icon: "dashboard" },
      { section: "chat",      label: "Chat",      icon: "chat" },
    ],
  },
  {
    label: "MANAGE",
    sections: [
      { section: "devices",   label: "Devices",   icon: "devices" },
      { section: "mesh",      label: "Mesh",       icon: "mesh" },
      { section: "pairing",   label: "Pairing",   icon: "pairing" },
      { section: "schedules", label: "Schedules", icon: "clock" },
      { section: "context",   label: "Context",   icon: "memory" },
      { section: "skills",    label: "Skills",    icon: "skills" },
      { section: "recipes",   label: "Recipes",   icon: "recipes" },
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

// Routable but not in the sidebar; "hub" is entered via Settings > "Preview Goose Hub".
const HIDDEN_SECTIONS: GuiSection[] = ["faces", "canvas", "hub"];
const SECTION_SET = new Set<GuiSection>([
  ...DESKTOP_SECTIONS.map((s) => s.section),
  ...HIDDEN_SECTIONS,
]);

export function normalizeDesktopMode(value: string | null | undefined): DesktopMode {
  return value === "voice" || value === "gui" ? value : "gui";
}

/** Renamed section ids and their new home, so a persisted old id does not reopen on the dashboard. */
const RENAMED_SECTIONS: Record<string, GuiSection> = {
  memory: "context",
  connections: "context",
};

export function normalizeGuiSection(value: string | null | undefined): GuiSection {
  if (!value) return "dashboard";
  if (SECTION_SET.has(value as GuiSection)) return value as GuiSection;
  return RENAMED_SECTIONS[value] ?? "dashboard";
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
