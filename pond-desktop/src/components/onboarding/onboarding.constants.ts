// Onboarding wizard constants and static data.

import type {
  StepMeta,
  OnboardingDraft,
  LangOption,
  PromptStyle,
  WakePreset,
} from "./onboarding.types";

// ── Steps ──────────────────────────────────────────────────

export const STEPS: StepMeta[] = [
  { id: "welcome",     label: "Welcome",              caption: "Say hi to Goose",     required: false },
  { id: "about-you",   label: "About you",            caption: "Name and profile",    required: true  },
  { id: "locale",      label: "Language & location",   caption: "Locale, weather",     required: true  },
  { id: "personality", label: "Personality & identity", caption: "How Goose talks",     required: false },
  { id: "wake-word",   label: "Wake word",             caption: "How to summon",       required: false },
  { id: "complete",    label: "All set",               caption: "Hello, world",        required: false },
];

// ── FE / BE step mapping ───────────────────────────────────
// 7 wizard steps vs 10 backend `OnboardingStep` variants; `Completed` is set only by
// POST /onboard/complete, and the merged personality step reports GooseIdentity.

/** FE step id → backend `OnboardingStep` name to POST when that step persists. */
export const FE_STEP_TO_BE: Record<string, string> = {
  welcome:     "Welcome",
  "about-you": "Basics",
  locale:      "Location",
  personality: "GooseIdentity",
  "wake-word": "WakeWord",
  model:       "Model",
  complete:    "Extensions",
};

/** Backend `OnboardingStep` name → FE step index to resume at. */
const BE_TO_FE_INDEX: Record<string, number> = {
  Welcome:       0,
  Basics:        1,
  Accessibility: 1, // folded into about-you
  Location:      2,
  Personality:   3, // merged into the personality FE step
  GooseIdentity: 3,
  WakeWord:      4,
  Model:         5,
  Extensions:    6,
  Completed:     6, // handled separately; keep at the last visible step
};

/** FE step index to resume at for a backend `OnboardingStep` name; unknown names give 0. */
export function beStepToFeIndex(beStep: string | undefined | null): number {
  if (!beStep) return 0;
  return BE_TO_FE_INDEX[beStep] ?? 0;
}

// ── Avatars ────────────────────────────────────────────────

export const AVATARS = ["\u{1F986}", "\u{1F427}", "\u{1F985}", "\u{1F99C}", "\u{1F438}", "\u{1F989}", "\u{1F43B}", "\u{1F98A}", "\u{1F431}", "\u{1F436}"];

// ── Languages ──────────────────────────────────────────────

export const LANGUAGES: LangOption[] = [
  { key: "en", label: "English" },
  { key: "fr", label: "Fran\u00e7ais" },
  { key: "es", label: "Espa\u00f1ol" },
  { key: "de", label: "Deutsch" },
  { key: "sw", label: "Kiswahili" },
  { key: "pt", label: "Portugu\u00eas" },
  { key: "ja", label: "\u65e5\u672c\u8a9e" },
  { key: "zh", label: "\u4e2d\u6587" },
];

// ── Prompt styles ──────────────────────────────────────────

/** Prompt style metadata — icons are rendered via lucide-react in StepPersonality. */
export const PROMPT_STYLES: PromptStyle[] = [
  { value: "balanced",  label: "Balanced",  icon: "", desc: "Warm and practical. Just enough detail." },
  { value: "concise",   label: "Concise",   icon: "", desc: "Short, action-first. Skips the small talk." },
  { value: "technical", label: "Technical",  icon: "", desc: "Step-by-step. Detailed narration for tinkerers." },
  { value: "warm",      label: "Warm",       icon: "", desc: "Conversational, like a helpful neighbour." },
];

// ── Wake word presets ──────────────────────────────────────

export const WAKE_PRESETS: WakePreset[] = [
  { value: "goose",       label: '"Goose"',       desc: "Short and memorable." },
  { value: "hey goose",   label: '"Hey Goose"',   desc: "Natural call-and-response." },
  { value: "ok computer", label: '"OK Computer"', desc: "Classic command style." },
  { value: "custom",      label: "Custom phrase",  desc: "Say anything you like." },
];

// ── Default draft ──────────────────────────────────────────

export const DEFAULT_DRAFT: OnboardingDraft = {
  userName: "",
  preferredName: "",
  birthday: "",
  avatar: "\ud83e\udd86",
  atypicalSpeech: false,
  slowSpeech: false,
  highContrast: false,
  reduceMotion: false,
  language: "en",
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
  locationName: "",
  latitude: 0,
  longitude: 0,
  enableWeather: false,
  promptStyle: "balanced",
  personality: "friendly and helpful",
  assistantName: "Goose",
  ttsVoice: "af_heart",
  // Percent of natural pace, as on the settings screen.
  ttsRate: 100,
  wakeWord: "goose",
  wakeWordCustom: "",
  enableMcpMemory: true,
  enableHomeAssistant: false,
  enableCalendar: false,
};
