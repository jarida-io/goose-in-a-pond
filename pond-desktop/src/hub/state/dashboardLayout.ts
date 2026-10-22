// Which cards Home shows, in what order, chosen by the household. In localStorage rather than
// the settings table: it is per panel, and two panels on one pond may want different Homes.

import { useSyncExternalStore } from "react";

export type CardId = "weather" | "devices" | "nowPlaying" | "tonight" | "remoteAccess";

/** How much room a card takes. The only width input there is. */
export type CardSize = "s" | "m" | "l";

export interface PlacedCard {
  id: CardId;
  size: CardSize;
}

export interface CardSpec {
  id: CardId;
  /** What the household calls it in the arrange sheet. */
  title: string;
  /** One line, shown while arranging, saying what the card is for. */
  hint: string;
  /** The size a card gets when the household has never sized it. */
  defaultSize: CardSize;
}

/** More pages than this is a filing cabinet, not a glance. */
export const MAX_PAGES = 3;

/** In arrange-sheet order. Each entry must be backed by a real `HomeData` slice (DESIGN.md §3). */
export const CARDS: readonly CardSpec[] = [
  { id: "devices", title: "Devices", hint: "Lights, locks, plugs and thermostats", defaultSize: "l" },
  { id: "weather", title: "Weather", hint: "Now, and the days ahead", defaultSize: "m" },
  { id: "nowPlaying", title: "Music", hint: "What is playing, and the controls", defaultSize: "s" },
  { id: "tonight", title: "Tonight", hint: "Routines left before midnight", defaultSize: "s" },
  { id: "remoteAccess", title: "Remote Access", hint: "Away-from-home connection status", defaultSize: "s" },
] as const;

const CARD_IDS = new Set<string>(CARDS.map((c) => c.id));
const SIZES = new Set<string>(["s", "m", "l"]);

function specOf(id: CardId): CardSpec {
  // Non-null: every CardId has a row in CARDS.
  return CARDS.find((c) => c.id === id) as CardSpec;
}

export interface DashboardLayout {
  version: 2;
  /** One entry per page, each holding that page's cards in display order. */
  pages: PlacedCard[][];
  /** Everything the household has taken off. Kept so the sheet can offer it back. */
  hidden: CardId[];
}

/** First-run and post-reset Home; adding a card here is a product decision. */
export const DEFAULT_LAYOUT: DashboardLayout = {
  version: 2,
  pages: [
    [
      { id: "weather", size: "m" },
      { id: "devices", size: "l" },
    ],
    [
      { id: "nowPlaying", size: "s" },
      { id: "tonight", size: "s" },
    ],
  ],
  // Available from the arrange sheet rather than placed: adding a card to every existing
  // Home is a product decision (line above this struct), and this one wasn't it.
  hidden: ["remoteAccess"],
};

// Same key as v1, whose payload is migrated; a new key would silently reset every Home.
const KEY = "giap-dashboard-layout";

let current: DashboardLayout = DEFAULT_LAYOUT;
let loaded = false;
const subs = new Set<() => void>();

function emit(): void {
  for (const s of subs) s();
}

/** Deep-enough copy that a mutator can splice without touching the live snapshot. */
function clonePages(pages: PlacedCard[][]): PlacedCard[][] {
  return pages.map((p) => p.map((c) => ({ ...c })));
}

/**
 * Repairs stored pages: unknown ids and empty pages drop (none left: the default), a duplicate
 * keeps its first copy, a bad size gets the default, pages past MAX_PAGES merge into the last.
 */
function reconcile(rawPages: unknown[]): DashboardLayout {
  const seen = new Set<CardId>();
  const pages: PlacedCard[][] = [];

  for (const rawPage of rawPages) {
    if (!Array.isArray(rawPage)) continue;
    const page: PlacedCard[] = [];
    for (const entry of rawPage) {
      if (typeof entry !== "object" || entry === null) continue;
      const { id, size } = entry as { id?: unknown; size?: unknown };
      if (typeof id !== "string" || !CARD_IDS.has(id)) continue;
      const cardId = id as CardId;
      if (seen.has(cardId)) continue;
      seen.add(cardId);
      page.push({
        id: cardId,
        size: typeof size === "string" && SIZES.has(size) ? (size as CardSize) : specOf(cardId).defaultSize,
      });
    }
    if (page.length > 0) pages.push(page);
  }

  if (pages.length === 0) return DEFAULT_LAYOUT;

  if (pages.length > MAX_PAGES) {
    const overflow = pages.splice(MAX_PAGES);
    for (const page of overflow) pages[MAX_PAGES - 1].push(...page);
  }

  // Recomputed from every page's cards, so a card both placed and hidden counts as placed.
  const hidden = CARDS.map((c) => c.id).filter((id) => !seen.has(id));

  return { version: 2, pages, hidden };
}

/** A payload written by the release before pages existed. */
function isV1(raw: Record<string, unknown>): boolean {
  return Array.isArray(raw.order) && !Array.isArray(raw.pages);
}

/** A v1 `{order}` becomes one page, same order, default sizes, then goes through reconcile. */
function migrateV1(order: unknown[]): DashboardLayout {
  const placed = order
    .filter((id): id is CardId => typeof id === "string" && CARD_IDS.has(id))
    .map((id) => ({ id, size: specOf(id).defaultSize }));
  return reconcile([placed]);
}

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
  const raw = stored as Record<string, unknown>;

  if (isV1(raw)) return migrateV1(raw.order as unknown[]);
  if (!Array.isArray(raw.pages)) return DEFAULT_LAYOUT;
  return reconcile(raw.pages);
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

/** Every card placed anywhere, in page order. */
export function placedCards(l: DashboardLayout): PlacedCard[] {
  return l.pages.flat();
}

/** Which page holds a card, and where on it. Both -1 when it is not placed. */
function locate(pages: PlacedCard[][], id: CardId): { page: number; at: number } {
  for (let p = 0; p < pages.length; p += 1) {
    const at = pages[p].findIndex((c) => c.id === id);
    if (at >= 0) return { page: p, at };
  }
  return { page: -1, at: -1 };
}

/** Drop pages nothing is left on, never below one. */
function compact(pages: PlacedCard[][]): PlacedCard[][] {
  const kept = pages.filter((p) => p.length > 0);
  return kept.length > 0 ? kept : [[]];
}

/** Put a hidden card on a page, at the end where it can be seen to have arrived. */
export function showCard(id: CardId, page = 0): void {
  const l = ensureLoaded();
  if (locate(l.pages, id).page >= 0) return;
  const pages = clonePages(l.pages);
  const target = Math.max(0, Math.min(pages.length - 1, page));
  pages[target].push({ id, size: specOf(id).defaultSize });
  write({ version: 2, pages, hidden: l.hidden.filter((h) => h !== id) });
}

/**
 * Takes a card off Home, back to the sheet. The last card on Home stays (an emptied page is
 * fine): a Home with no cards has no visible route back to the sheet.
 */
export function hideCard(id: CardId): void {
  const l = ensureLoaded();
  const { page, at } = locate(l.pages, id);
  if (page < 0) return;
  if (placedCards(l).length <= 1) return;
  const pages = clonePages(l.pages);
  pages[page].splice(at, 1);
  write({ version: 2, pages: compact(pages), hidden: [...l.hidden, id] });
}

/** Move a card one place; explicit moves keep reordering keyboard-operable (DESIGN.md §6). */
export function moveCard(id: CardId, delta: -1 | 1): void {
  const l = ensureLoaded();
  const { page, at } = locate(l.pages, id);
  if (page < 0) return;
  const to = at + delta;
  if (to < 0 || to >= l.pages[page].length) return;
  const pages = clonePages(l.pages);
  const [card] = pages[page].splice(at, 1);
  pages[page].splice(to, 0, card);
  write({ version: 2, pages, hidden: l.hidden });
}

/**
 * Moves a card to the end of a page (one past the last makes a new page, up to MAX_PAGES).
 * Returns the page it landed on (an emptied page shifts later ones down), or null if unmoved.
 */
export function moveCardToPage(id: CardId, page: number): number | null {
  const l = ensureLoaded();
  const from = locate(l.pages, id);
  if (from.page < 0 || from.page === page) return null;
  const appending = page === l.pages.length;
  if (page < 0 || page > l.pages.length) return null;
  if (appending && l.pages.length >= MAX_PAGES) return null;

  const pages = clonePages(l.pages);
  if (appending) pages.push([]);
  const [card] = pages[from.page].splice(from.at, 1);
  pages[page].push(card);
  // Index after compaction = non-empty pages before the target (which holds the card).
  const landedOn = pages.slice(0, page).filter((p) => p.length > 0).length;
  write({ version: 2, pages: compact(pages), hidden: l.hidden });
  return landedOn;
}

/** Resize a placed card. Nothing else about its position changes. */
export function setCardSize(id: CardId, size: CardSize): void {
  const l = ensureLoaded();
  const { page, at } = locate(l.pages, id);
  if (page < 0) return;
  const pages = clonePages(l.pages);
  pages[page][at] = { id, size };
  write({ version: 2, pages, hidden: l.hidden });
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
