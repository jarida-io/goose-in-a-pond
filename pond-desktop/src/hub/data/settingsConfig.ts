// ─── Settings IA config ────────────────────────────────────────
// Ported from goose-hub-settings.jsx SETTINGS constant (lines 70-91).
// Icon path strings come from icons.ts (HP_PATHS) or HX_PATHS defined here.

// ─── HX icon dictionary (extra icons used across settings screens) ─────────
// Ported from goose-hub-home.jsx HX constant.
export const HX_PATHS = {
  chat:      "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  user:      "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM5 21a8 8 0 0 1 14 0",
  cpu:       "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3",
  prompt:    "M4 4h16v12H5.5L4 17.5zM8 9h8M8 12h5",
  logs:      "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6M9 13h6M9 17h6",
  puzzle:    "M14 7h2a2 2 0 0 1 2 2v2m0 0h1.5a1.5 1.5 0 0 1 0 3H18v2a2 2 0 0 1-2 2h-2m0 0v1.5a1.5 1.5 0 0 1-3 0V19H9a2 2 0 0 1-2-2v-2m0 0H5.5a1.5 1.5 0 0 1 0-3H7V9a2 2 0 0 1 2-2h2V5.5a1.5 1.5 0 0 1 3 0z",
  voice:     "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v3",
  cctv:      "M3 7l14-4 1.5 4.5L4.5 12 3 7zM4.2 11.5L6 17M9 9.5V13a2 2 0 0 1-2 2H4M19 17h2M20 15v4",
  shield:    "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z",
  bell:      "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0",
  palette:   "M12 22a10 10 0 1 1 0-20 8 8 0 0 1 0 16h-2a2 2 0 0 0 0 4zM7.5 11a1 1 0 1 0 0-2 1 1 0 0 0 0 2zM12 7.5a1 1 0 1 0 0-2 1 1 0 0 0 0 2zM16.5 11a1 1 0 1 0 0-2 1 1 0 0 0 0 2z",
} as const;

// ─── HP home icon re-export (used for "rooms" row) ─────────────
const HP_HOME = "M3 11l9-8 9 8M5 10v10a1 1 0 0 0 1 1h4v-6h4v6h4a1 1 0 0 0 1-1V10";

// ─── SettingsRowId ─────────────────────────────────────────────
export type SettingsRowId =
  | "connections"
  | "models"
  | "prompts"
  | "voice"
  | "memory"
  | "extensions"
  | "logs"
  | "background"
  | "privacy"
  | "rooms"
  | "cameras"
  | "notifications"
  | "appearance"
  | "account";

// ─── SettingsGroup ─────────────────────────────────────────────
export type SettingsGroupName =
  | "Assistant & AI"
  | "Automations"
  | "System"
  | "Home"
  | "General";

export type SettingsRow = {
  id: SettingsRowId;
  iconPath: string;
  color: string;
  bg: string;
  label: string;
  sub: string;
  value?: string;
  badge?: string;
};

export type SettingsGroup = {
  group: SettingsGroupName;
  rows: SettingsRow[];
};

// ─── SETTINGS constant ─────────────────────────────────────────
// Icon path strings use HX_PATHS or HP_PATHS values directly.
export const SETTINGS: SettingsGroup[] = [
  {
    group: "Assistant & AI",
    rows: [
      {
        id: "models",
        iconPath: HX_PATHS.cpu,
        color: "#7C3AED",
        bg: "#EDE9FE",
        label: "Models",
        sub: "Local LLMs for chat, think, task",
        value: "gemma-4-E4B",
      },
      {
        id: "prompts",
        iconPath: HX_PATHS.prompt,
        color: "#2563EB",
        bg: "#DBEAFE",
        label: "Prompts",
        sub: "System prompt & personality",
        value: "Concise",
      },
      {
        id: "voice",
        iconPath: HX_PATHS.voice,
        color: "#DB2777",
        bg: "#FCE7F3",
        label: "Voice",
        sub: "Whisper speech · Piper TTS",
        value: "en-lessac",
      },
      {
        id: "connections",
        iconPath:
          "M4 7h16a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1zM3 9l9 5 9-5M8 3v4M16 3v4",
        color: "#1F6F63",
        bg: "#D7EDE8",
        label: "Accounts",
        sub: "Calendar and mail the pond can read",
        value: "",
      },
      {
        id: "memory",
        iconPath:
          "M8 3v2M16 3v2M8 19v2M16 19v2M3 8h2M3 16h2M19 8h2M19 16h2M6 6h12a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1zM9 9h6v6H9z",
        color: "#0D9488",
        bg: "#CCFBF1",
        label: "Memory",
        sub: "What Goose remembers about you",
        value: "5 notes",
      },
    ],
  },
  {
    // Its own group rather than a row under System, because what it holds is
    // not a system fact -- it is the work the pond does on its own, which is
    // the same thing "Automations" means on the classic surface. The two
    // taxonomies are separate structures and always have been; this is the one
    // heading it is worth spending to make them agree on.
    group: "Automations",
    rows: [
      {
        id: "background",
        // Three arrows around a circle: work that goes round on its own.
        iconPath:
          "M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6",
        color: "#4338CA",
        bg: "#E0E7FF",
        label: "Background jobs",
        sub: "What the pond does while nobody is talking to it",
        value: "",
      },
    ],
  },
  {
    group: "System",
    rows: [
      {
        id: "extensions",
        iconPath: HX_PATHS.puzzle,
        color: "#EA580C",
        bg: "#FFEDD5",
        label: "Extensions (MCP)",
        sub: "Connected tools & servers",
        value: "6 connected",
      },
      {
        id: "logs",
        iconPath: HX_PATHS.logs,
        color: "#475569",
        bg: "#F1F5F9",
        label: "Logs",
        sub: "Server activity & diagnostics",
      },
      {
        id: "privacy",
        iconPath: HX_PATHS.shield,
        color: "#16A34A",
        bg: "#DCFCE7",
        label: "Privacy",
        sub: "Everything runs on-device",
        badge: "On-device",
      },
    ],
  },
  {
    group: "Home",
    rows: [
      {
        id: "rooms",
        iconPath: HP_HOME,
        color: "#7C3AED",
        bg: "#EDE9FE",
        label: "Rooms & Devices",
        sub: "6 rooms · 18 devices",
      },
      {
        id: "cameras",
        iconPath: HX_PATHS.cctv,
        color: "#0EA5E9",
        bg: "#E0F2FE",
        label: "Cameras",
        sub: "3 live feeds",
      },
      {
        id: "notifications",
        iconPath: HX_PATHS.bell,
        color: "#D97706",
        bg: "#FEF3C7",
        label: "Notifications",
        sub: "Alerts & schedule debriefs",
      },
    ],
  },
  {
    group: "General",
    rows: [
      {
        id: "appearance",
        iconPath: HX_PATHS.palette,
        color: "#8B5CF6",
        bg: "#F3E8FF",
        label: "Appearance",
        sub: "Theme, accent & day/night",
        value: "Light",
      },
      {
        id: "account",
        iconPath: HX_PATHS.user,
        color: "#475569",
        bg: "#F1F5F9",
        label: "Account",
        sub: "Jerry · Goose In A Pond",
      },
    ],
  },
];
