// Hex mirrors of design-tokens.css for canvas drawing, where a fillStyle can't take a CSS variable;
// use var(--color-*) in DOM/JSX.

import type { VoiceState } from "../state/reducer";

// ── Voice/detector state colors ────────────────────────────────────────────────
// For canvas use. Must stay in sync with --orb-* tokens in design-tokens.css.
export const ORB_STATE_COLORS: Record<VoiceState, string> = {
  idle:      "#8E8E93",
  wait:      "#8C4BFF",
  recording: "#3B9EFF",
  thinking:  "#FF9500",
  speaking:  "#34C759",
  error:     "#FF3B30",
};

// ── LLM role colors ────────────────────────────────────────────────────────────
// Hex values for canvas / inline style use. CSS vars: --color-role-{key}.
export const ROLE_COLOR_HEX: Record<string, string> = {
  chat:  "#8C4BFF",
  think: "#FF9500",
  task:  "#30a46c",
  asr:   "#007AFF",
  tts:   "#e5a000",
};

// CSS variable references for role colors — use in JSX style/className.
export const ROLE_COLOR_CSS: Record<string, string> = {
  chat:  "var(--color-role-chat)",
  think: "var(--color-role-think)",
  task:  "var(--color-role-task)",
  asr:   "var(--color-role-asr)",
  tts:   "var(--color-role-tts)",
};

export const ROLE_COLOR_SOFT_CSS: Record<string, string> = {
  chat:  "var(--color-role-chat-soft)",
  think: "var(--color-role-think-soft)",
  task:  "var(--color-role-task-soft)",
  asr:   "var(--color-role-asr-soft)",
  tts:   "var(--color-role-tts-soft)",
};
