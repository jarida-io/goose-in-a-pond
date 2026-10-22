/**
 * Theme, accent and density, persisted and written inline on <html> before first paint: the app's
 * only writer of design tokens (`InkProvider` runs in `context` mode). Inline beats `:root`, so
 * `design-tokens.css` still governs the tokens Ink doesn't define.
 */
import { useSyncExternalStore } from "react";
import {
  ACCENTS,
  createTheme,
  cssVariables,
  ratio,
  readable,
  type AccentName,
  type InkTheme,
} from "@jarida/ink";

// ─── Types ────────────────────────────────────────────────────────────────────

export type ThemeChoice = "Light" | "Dark" | "Auto";
export type DensityChoice = "Comfortable" | "Compact";

export type { AccentName };

export interface ThemeState {
  theme: ThemeChoice;
  accent: AccentName;
  density: DensityChoice;
  /** Auto resolved by time of day. */
  resolvedTheme: "light" | "dark";
  /** Resolved theme for `InkProvider` and JS token reads; built here so DOM and context never disagree. */
  ink: InkTheme;
}

// ─── Accent palettes ──────────────────────────────────────────────────────────

/**
 * `@jarida/ink`'s `[base, pressed, tint, paper]` ramps. Purple's base is #7C3AED, not the mark's
 * #8C4BFF: collapsing the two either dulls the logo or fails text contrast.
 */
export const ACCENT_PALETTES = ACCENTS;

// ─── localStorage keys ────────────────────────────────────────────────────────

const KEY_THEME   = "goosehub_theme";
const KEY_ACCENT  = "goosehub_accent";
const KEY_DENSITY = "goosehub_density";

// ─── Helpers ──────────────────────────────────────────────────────────────────

function resolveTheme(theme: ThemeChoice): "light" | "dark" {
  if (theme === "Dark") return "dark";
  if (theme === "Light") return "light";
  // Auto: dark between 19:00 and 05:59
  const h = new Date().getHours();
  return h >= 19 || h < 6 ? "dark" : "light";
}

function isThemeChoice(v: unknown): v is ThemeChoice {
  return v === "Light" || v === "Dark" || v === "Auto";
}

function isAccentName(v: unknown): v is AccentName {
  return v === "Purple" || v === "Blue" || v === "Teal" || v === "Coral" || v === "Magenta";
}

function isDensityChoice(v: unknown): v is DensityChoice {
  return v === "Comfortable" || v === "Compact";
}

function readFromStorage(): { theme: ThemeChoice; accent: AccentName; density: DensityChoice } {
  try {
    const t = localStorage.getItem(KEY_THEME);
    const a = localStorage.getItem(KEY_ACCENT);
    const d = localStorage.getItem(KEY_DENSITY);
    return {
      theme:   isThemeChoice(t)   ? t : "Light",
      accent:  isAccentName(a)    ? a : "Purple",
      density: isDensityChoice(d) ? d : "Comfortable",
    };
  } catch {
    return { theme: "Light", accent: "Purple", density: "Comfortable" };
  }
}

function applyToDOM(theme: ThemeChoice, accent: AccentName, density: DensityChoice): InkTheme {
  const resolved = resolveTheme(theme);
  const ink = createTheme({
    scheme: resolved,
    accent,
    density: density === "Compact" ? "compact" : "comfortable",
  });

  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.dataset.density = density.toLowerCase();

  // Includes --pp/--pp-600/--pp-100/--pp-50, which existing stylesheets' `var(--pp, …)` rely on.
  for (const [name, value] of Object.entries(cssVariables(ink))) {
    root.style.setProperty(name, value);
  }

  // Ink's own --border-ink/--shadow-ink are a fixed brand purple regardless of accent
  // (see @jarida/ink's InkPalette.ink). Every hard-shadow/ink-edge component in this app
  // reads one of those two, so re-pointing them at the chosen accent here is what makes
  // the whole app's shadows track Appearance → Accent instead of only the elements that
  // read --color-accent directly.
  //
  // Floored at AA's nonText ratio (3:1, a border/shadow's own floor, not body text's 4.5:1):
  // an accent picked for how it reads as a fill isn't guaranteed to read as a thin edge
  // against --color-bg, especially in dark scheme (plan §8 flagged the old fixed #5D23C2
  // measuring under 3:1 on #131119 -- the risk moved with the accent, it didn't go away).
  // readable() is a no-op whenever the raw accent already clears the floor.
  const edgeColor = readable(ink.color.accent, ink.color.bg, "nonText");
  if (edgeColor !== ink.color.accent) {
    // eslint-disable-next-line no-console -- a silent accent substitution is a hard debug later.
    console.warn(
      `[theme] ${accent} only clears ${ratio(ink.color.accent, ink.color.bg).toFixed(2)}:1 on ` +
        `--color-bg in ${resolved} mode; lightened --border-ink/--shadow-ink to ${edgeColor} for 3:1.`,
    );
  }
  root.style.setProperty("--border-ink", edgeColor);
  root.style.setProperty("--shadow-ink", `${ink.edge.rest}px ${ink.edge.rest}px 0 ${edgeColor}`);

  // The cssVariables() loop above just set --color-bg inline to the library's own
  // "#FFF9F9"/"#131119", which (inline always beats a stylesheet rule) silently
  // overrode whatever design-tokens.css said -- the reason editing that file's
  // --color-bg had no visible effect. Removing the inline value lets design-tokens.css's
  // own :root / :root[data-theme="dark"] rule win back, the same light/dark switch
  // `root.dataset.theme` above already drives for every other per-theme override there.
  root.style.removeProperty("--color-bg");

  return ink;
}

// ─── Store internals ──────────────────────────────────────────────────────────

const initial = readFromStorage();

let _state: ThemeState = {
  ...initial,
  resolvedTheme: resolveTheme(initial.theme),
  ink: createTheme({
    scheme: resolveTheme(initial.theme),
    accent: initial.accent,
    density: initial.density === "Compact" ? "compact" : "comfortable",
  }),
};

const _listeners = new Set<() => void>();

function _notify(): void {
  _listeners.forEach((fn) => fn());
}

function _snapshot(): ThemeState {
  return _state;
}

// ─── Public setters ───────────────────────────────────────────────────────────

function setTheme(theme: ThemeChoice): void {
  try { localStorage.setItem(KEY_THEME, theme); } catch { /* ignore */ }
  _state = { ..._state, theme, resolvedTheme: resolveTheme(theme) };
  _state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };
  _notify();
}

function setAccent(accent: AccentName): void {
  try { localStorage.setItem(KEY_ACCENT, accent); } catch { /* ignore */ }
  _state = { ..._state, accent };
  _state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };
  _notify();
}

function setDensity(density: DensityChoice): void {
  try { localStorage.setItem(KEY_DENSITY, density); } catch { /* ignore */ }
  _state = { ..._state, density };
  _state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };
  _notify();
}

// ─── Bootstrap: apply immediately on module load ──────────────────────────────

_state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };

// ─── useSyncExternalStore subscription ───────────────────────────────────────

function _subscribe(listener: () => void): () => void {
  _listeners.add(listener);
  return () => _listeners.delete(listener);
}

// ─── Hook ─────────────────────────────────────────────────────────────────────

export function useTheme(): ThemeState & {
  setTheme: (t: ThemeChoice) => void;
  setAccent: (a: AccentName) => void;
  setDensity: (d: DensityChoice) => void;
} {
  const state = useSyncExternalStore(_subscribe, _snapshot, _snapshot);
  return { ...state, setTheme, setAccent, setDensity };
}
