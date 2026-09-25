// Which cards Home shows, in what order, chosen by the household. In localStorage rather than
// the settings table: it is per panel, and two panels on one pond may want different Homes.

import { useSyncExternalStore } from "react";

/** Every card Home can show. Ordered as the default layout lists them. */
export type CardId =
  | "suggestion"
  | "devices"
  | "weather"
  | "nowPlaying"
  | "scenes"
  | "cameras"
  | "routines"
  | "todos";

export interface CardSpec {
  id: CardId;
  /** What the household calls it in the edit sheet. */
  title: string;
  /** One line, shown while editing, saying what the card is for. */
  hint: string;
  /** Columns on a wide grid; devices take two because a one-tile row is not a glance. */
  span: 1 | 2;
}

/** The catalogue, in edit-sheet order; every entry is backed by real `HomeData` (DESIGN.md §3). */
export const CARDS: readonly CardSpec[] = [
  { id: "suggestion", title: "Suggestion", hint: "The one thing asking for you", span: 2 },
  { id: "devices", title: "Devices", hint: "Lights, locks, plugs and thermostats", span: 2 },
  { id: "weather", title: "Weather", hint: "Now, and the days ahead", span: 1 },
  { id: "nowPlaying", title: "Music", hint: "What is playing, and the controls", span: 1 },
  { id: "scenes", title: "Scenes", hint: "Whole-house settings you can run", span: 1 },
  { id: "cameras", title: "Cameras", hint: "The latest frame from each", span: 1 },
  { id: "routines", title: "Routines", hint: "What runs on its own, and when", span: 1 },
  { id: "todos", title: "To-do", hint: "What you asked to be reminded of", span: 1 },
] as const;

const CARD_IDS = new Set<string>(CARDS.map((c) => c.id));

export interface DashboardLayout {
  /** Visible cards, in display order. */
  order: CardId[];
  /** Switched-off cards, kept so the edit sheet can offer them back. */
  hidden: CardId[];
}

/** First-run and post-reset Home; adding a card here is a product decision. */
export const DEFAULT_LAYOUT: DashboardLayout = {
  order: ["suggestion", "devices", "weather", "nowPlaying"],
  hidden: ["scenes", "cameras", "routines", "todos"],
};

const KEY = "giap-dashboard-layout";

let current: DashboardLayout = DEFAULT_LAYOUT;
let loaded = false;
const subs = new Set<() => void>();

function emit(): void {
  for (const s of subs) s();
}

/** Stored layout reconciled with `CARDS`: unknown ids dropped, cards added since go to `hidden`. */
function read(): DashboardLayout {
  let stored: unknown;
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_LAYOUT;
    stored = JSON.parse(raw);
  } catch {
    // Unavailable storage (private window, wiped panel) just means no preference yet.
    return DEFAULT_LAYOUT;
  }

  if (typeof stored !== "object" || stored === null) return DEFAULT_LAYOUT;
  const raw = stored as Partial<Record<keyof DashboardLayout, unknown>>;

  const keep = (v: unknown): CardId[] =>
    Array.isArray(v) ? (v.filter((x) => typeof x === "string" && CARD_IDS.has(x)) as CardId[]) : [];

  const order = dedupe(keep(raw.order));
  const hidden = dedupe(keep(raw.hidden)).filter((id) => !order.includes(id));

  // Cards the stored layout never mentioned are new since it was written.
  const seen = new Set<CardId>([...order, ...hidden]);
  const unseen = CARDS.map((c) => c.id).filter((id) => !seen.has(id));

  // An empty Home has no way back to the edit sheet, so it gets the default instead.
  if (order.length === 0) return DEFAULT_LAYOUT;

  return { order, hidden: [...hidden, ...unseen] };
}

function dedupe(ids: CardId[]): CardId[] {
  return [...new Set(ids)];
}

function write(next: DashboardLayout): void {
  current = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // Still applies for this session; it just won't survive a reload.
  }
  emit();
}

function ensureLoaded(): DashboardLayout {
  if (!loaded) {
    current = read();
    loaded = true;
  }
  return current;
}

function subscribe(fn: () => void): () => void {
  ensureLoaded();
  subs.add(fn);
  return () => subs.delete(fn);
}

function snapshot(): DashboardLayout {
  return ensureLoaded();
}

export function useDashboardLayout(): DashboardLayout {
  return useSyncExternalStore(subscribe, snapshot, () => DEFAULT_LAYOUT);
}

export function getDashboardLayout(): DashboardLayout {
  return ensureLoaded();
}

/** Put a hidden card on Home, at the end where it can be seen to have arrived. */
export function showCard(id: CardId): void {
  const l = ensureLoaded();
  if (l.order.includes(id)) return;
  write({ order: [...l.order, id], hidden: l.hidden.filter((h) => h !== id) });
}

/** Take a card off Home. It goes back to the sheet rather than being forgotten. */
export function hideCard(id: CardId): void {
  const l = ensureLoaded();
  if (!l.order.includes(id)) return;
  const order = l.order.filter((o) => o !== id);
  // The last card stays: an empty Home has no way back to the edit sheet.
  if (order.length === 0) return;
  write({ order, hidden: [...l.hidden, id] });
}

/** Move a card one place; explicit moves keep reordering keyboard-operable (DESIGN.md §6). */
export function moveCard(id: CardId, delta: -1 | 1): void {
  const l = ensureLoaded();
  const from = l.order.indexOf(id);
  if (from < 0) return;
  const to = from + delta;
  if (to < 0 || to >= l.order.length) return;
  const order = [...l.order];
  [order[from], order[to]] = [order[to], order[from]];
  write({ order, hidden: l.hidden });
}

/** Back to the Home the release ships with. */
export function resetLayout(): void {
  write(DEFAULT_LAYOUT);
}

/** Testing seam: forget what was read so the next call re-reads storage. */
export function __resetLayoutCache(): void {
  loaded = false;
  current = DEFAULT_LAYOUT;
}
