// ─── HP Icon Dictionary ────────────────────────────────────────
// SVG `d` path strings, then the multi-element icons as elements; HubIco.tsx draws either.

export const HP_PATHS = {
  bulb:        "M9 18h6M10 22h4M12 2a7 7 0 0 0-4 12.7c.6.5 1 1.2 1 2V17h6v-.3c0-.8.4-1.5 1-2A7 7 0 0 0 12 2z",
  bulbOff:     "M9 18h6M10 22h4M8.5 8.5A7 7 0 0 1 12 2a7 7 0 0 1 4 12.7c-.6.5-1 1.2-1 2V17H9M3 3l18 18",
  flame:       "M12 2c1 3 4 4.5 4 8a4 4 0 0 1-8 0c0-1 .3-1.8.7-2.5C9 9 9.5 10 10.5 10 9.8 7.5 11 4 12 2z M8.5 13a3.5 3.5 0 0 0 7 0c0-2-1.5-3-2-4.5",
  plug:        "M9 2v6M15 2v6M7 8h10v3a5 5 0 0 1-10 0V8zM12 16v6",
  cctv:        "M3 7l14-4 1.5 4.5L4.5 12 3 7zM4.2 11.5L6 17M9 9.5V13a2 2 0 0 1-2 2H4M19 17h2M20 15v4",
  shield:      "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z",
  shieldCheck: "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10zM9 12l2 2 4-4",
  thermo:      "M14 14.76V4.5a2.5 2.5 0 0 0-5 0v10.26a4.5 4.5 0 1 0 5 0z",
  cloudSun:    "M10 4a4 4 0 0 0-1.8 7.6M16 6a1 1 0 0 0-1 0M6 16a4 4 0 0 1 1-7.9 5 5 0 0 1 9.6-1A4 4 0 0 1 17 15H7a3 3 0 0 1-1-.2z",
  cloud:       "M17.5 19H9a7 7 0 1 1 6.7-9h.8a4.5 4.5 0 0 1 1 9z",
  rain:        "M16 13a5 5 0 0 0 0-10 6 6 0 0 0-11.5 2A4.5 4.5 0 0 0 5 14M8 18l-1 3M12 18l-1 3M16 18l-1 3",
  music:       "M9 18V5l12-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0zM21 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0z",
  play:        "M6 4l14 8-14 8V4z",
  skipF:       "M5 4l10 8-10 8V4zM19 5v14",
  skipB:       "M19 20L9 12l10-8v16zM5 19V5",
  chevR:       "M9 6l6 6-6 6",
  chevL:       "M15 6l-6 6 6 6",
  chevD:       "M6 9l6 6 6-6",
  wifi:        "M5 12.5a10 10 0 0 1 14 0M8.5 16a5 5 0 0 1 7 0M12 19.5h.01M2 9a15 15 0 0 1 20 0",
  check:       "M20 6L9 17l-5-5",
  plus:        "M12 5v14M5 12h14",
  sliders:     "M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6",
  home:        "M3 11l9-8 9 8M5 10v10a1 1 0 0 0 1 1h4v-6h4v6h4a1 1 0 0 0 1-1V10",
  sofa:        "M4 11V8a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v3M2 13a2 2 0 0 1 2-2 2 2 0 0 1 2 2v3h12v-3a2 2 0 0 1 4 0v5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1zM6 19v2M18 19v2",
  bed:         "M3 7v13M3 12h18a2 2 0 0 1 2 2v6M3 16h20M6 10h5a2 2 0 0 1 2 2",
  utensils:    "M3 2v7a3 3 0 0 0 3 3 3 3 0 0 0 3-3V2M6 2v20M16 2c-1.5 0-3 2-3 6s1.5 4 3 4v10",
  tree:        "M12 2L7 9h3l-4 6h5v5h2v-5h5l-4-6h3z",
  moon:        "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z",
  away:        "M13 4l-2 5h4l-2 5M5 21l3-7M19 21l-3-7M9 14h6",
  droplet:     "M12 2.7l5.7 5.7a8 8 0 1 1-11.4 0z",
  wind:        "M9.6 4.6A2 2 0 1 1 11 8H2m10.6 11.4A2 2 0 1 0 14 16H2m15.7-8.3A2.5 2.5 0 1 1 19.5 12H2",
  note:        "M21 3H3v18h12l6-6V3zM15 21v-6h6",
  list:        "M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01",
  bell:        "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0",
  goose:       "M15.5 7.5a3.5 3.5 0 0 0-7 0c0 1 .3 1.7.8 2.5L4 16c-.6 1 .2 2.3 1.4 2.3H15a4 4 0 0 0 4-4c0-2.2-1.5-3.5-3.5-3.5M16 7h.01",
  mic:         "rect:9,2,6,12,3 path:M5 10a7 7 0 0 0 14 0M12 17v4M8 21h8", // compound — use micEl below
  // Home header icons
  chat:        "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  keyboard:    "M4 6h16a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1zM7 10h.01M11 10h.01M15 10h.01M17 10h.01M7 13h.01M17 13h.01M9 13h6",
  // Rail icons (from goose-hub-shell.jsx)
  railHome:     "M3 9.6L12 3l9 6.6V20a1 1 0 0 1-1 1h-5v-7H9v7H4a1 1 0 0 1-1-1z",
  railChat:     "M21 11.5a8.4 8.4 0 0 1-12.6 7.3L3 21l2.2-5.4A8.4 8.4 0 1 1 21 11.5z",
  railCanvas:   "M4 4h7v7H4zM13 4h7v7h-7zM13 13h7v7h-7zM4 13h7v7H4z",
  railRoutines: "M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z",
  railSettings: "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM19.4 13a7.7 7.7 0 0 0 0-2l2-1.5-2-3.5-2.4 1a7.5 7.5 0 0 0-1.7-1L15 3H9l-.3 2.5a7.5 7.5 0 0 0-1.7 1l-2.4-1-2 3.5 2 1.5a7.7 7.7 0 0 0 0 2l-2 1.5 2 3.5 2.4-1a7.5 7.5 0 0 0 1.7 1L9 21h6l.3-2.5a7.5 7.5 0 0 0 1.7-1l2.4 1 2-3.5z",
  x:           "M18 6L6 18M6 6l12 12",
  pencil:      "M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7M18.5 2.5a2.1 2.1 0 0 1 3 3L12 15l-4 1 1-4z",
  // Overlays
  alert:       "M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0zM12 9v4M12 17h.01",
  minus:       "M5 12h14",
  bolt:        "M13 2L3 14h7l-1 8 11-12h-7z",
  cam:         "M2 8a2 2 0 0 1 2-2h2l1.5-2h5L16 6h2a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2zM12 9.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z",
  micOff:      "M1 1l22 22M9 9v3a3 3 0 0 0 5.1 2.1M15 9.3V5a3 3 0 0 0-5.9-.6M17 17A7 7 0 0 1 5 12M12 19v3",
  max:         "M8 3H5a2 2 0 0 0-2 2v3M16 3h3a2 2 0 0 1 2 2v3M16 21h3a2 2 0 0 0 2-2v-3M8 21H5a2 2 0 0 1-2-2v-3",
  person:      "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM5 21a8 8 0 0 1 14 0",
  speaker:     "M11 5L6 9H2v6h4l5 4zM19 5a10 10 0 0 1 0 14M15.5 8.5a5 5 0 0 1 0 7",
  // Generic device-kind icons (host, sensor, mobile, edge, …)
  cpu:         "M20 17a2 2 0 0 0 2-2V4a2 2 0 0 0-2-2H9.5a2 2 0 0 0-2 1.57L5 14.5A2 2 0 0 0 7 17M16 17H7M12 17v4M8 21h8",
  pulse:       "M22 12h-4l-3 9L9 3l-3 9H2",
  phone:       "M17 2H7a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V4a2 2 0 0 0-2-2zM12 18h.01",
  chip:        "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3",
  heart:       "M12 21s-7.5-4.6-10-9.2C.6 8.6 2 5 5.5 5c2 0 3.4 1.1 4.5 2.6C11.1 6.1 12.5 5 14.5 5 18 5 19.4 8.6 22 11.8 19.5 16.4 12 21 12 21z",
} as const;

export type HpIconKey = keyof typeof HP_PATHS;

/** Icons for non-smart-home devices, keyed by the Devices section's register types. */
export const SUBTYPE_ICON: Record<string, string> = {
  host: HP_PATHS.cpu,
  sensor: HP_PATHS.pulse,
  gotg: HP_PATHS.phone,
  smart_speaker: HP_PATHS.speaker,
  pond: HP_PATHS.goose,
  edge: HP_PATHS.chip,
};

// ─── Compound SVG elements (multi-path icons from the HP dictionary) ──────────

export const lockEl = (
  <>
    <rect x="5" y="11" width="14" height="10" rx="2" />
    <path d="M8 11V7a4 4 0 0 1 8 0v4" />
  </>
);

export const unlockEl = (
  <>
    <rect x="5" y="11" width="14" height="10" rx="2" />
    <path d="M8 11V7a4 4 0 0 1 7.5-2" />
  </>
);

export const sunEl = (
  <>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </>
);

export const cameraEl = (
  <>
    <path d="M2 8a2 2 0 0 1 2-2h2l1.5-2h5L14 6h2a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2z" transform="translate(1 0)" />
    <circle cx="11" cy="13" r="3.5" />
  </>
);

export const micEl = (
  <>
    <rect x="9" y="2" width="6" height="12" rx="3" />
    <path d="M5 10a7 7 0 0 0 14 0M12 17v4M8 21h8" />
  </>
);

export const pauseEl = (
  <>
    <rect x="6" y="4" width="4" height="16" rx="1" />
    <rect x="14" y="4" width="4" height="16" rx="1" />
  </>
);

export const dotsEl = (
  <>
    <circle cx="5" cy="12" r="1.6" />
    <circle cx="12" cy="12" r="1.6" />
    <circle cx="19" cy="12" r="1.6" />
  </>
);

export const filmEl = (
  <>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M7 3v18M17 3v18M3 8h4M17 8h4M3 16h4M17 16h4M3 12h18" />
  </>
);

export const focusEl = (
  <>
    <circle cx="12" cy="12" r="3" />
    <circle cx="12" cy="12" r="8" />
  </>
);
