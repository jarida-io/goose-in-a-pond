// ────────────────────────────────────────────────────────────
// What is on Home, on which page, and at what size — decided by the household.
//
// Two axes now, where there was one. A card has a PAGE (the track swipes
// between them) and a SIZE (s/m/l, which is the only thing that decides how
// much room it takes). The flat `order` list that preceded this could say
// neither.
//
// THE CATALOGUE IS THREE CARDS, down from eight, and every deletion is a data
// decision rather than a taste one:
//
//   suggestion  is no longer a card. It is the permanent left column, and a
//               Home you can arrange the asking off is a Home that stops
//               asking.
//   cameras     has no stream. The widget painted a hand-drawn SVG scene under
//               a hardcoded LIVE badge, and an absent `last_seen` was rendered
//               as now. A fabricated security feed is the highest-harm thing on
//               the old screen.
//   scenes      showed five mock scenes to a pond with no schedules, and marked
//               one "in effect" on list position alone.
//   routines    keeps its own screen, where Run is genuinely wired.
//   todos       was never populated by the loader, and the widget beside it
//               reads a different localStorage key entirely.
//
// The reconcile below already drops ids the catalogue no longer holds, so a
// stored layout carrying any of those five repairs itself on the next read.
//
// RECONCILE ON READ, EXCEPT FOR v1. Reconciling rather than versioning is what
// stops a card removed from the catalogue stranding a household on a Home that
// renders nothing. The one payload that MUST be migrated instead is the shipped
// v1 `{order, hidden}`: it is a valid object, so a v2 reader would find no
// `pages`, fall through to the default, and silently reset every household's
// arrangement without anything failing.
//
// Persisted to localStorage rather than the settings table on purpose. It is a
// per-panel preference — the kitchen screen and the study screen are looking at
// the same pond and reasonably want different Homes — and `giap-section` next
// to it already works this way.
// ────────────────────────────────────────────────────────────

import { useSyncExternalStore } from "react";

/** Every card Home can show. */
export type CardId = "weather" | "devices" | "nowPlaying";

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
  /**
   * The size a card arrives at when the household has never placed it.
   *
   * This replaces the old `span: 1 | 2`, which was dead data — nothing read it,
   * so the store's idea of a card's width and the component's idea were two
   * unconnected facts that happened to agree. This one is load-bearing: it is
   * the only answer to what size a card nobody has sized should be.
   */
  defaultSize: CardSize;
}

/** More pages than this is a filing cabinet, not a glance. */
export const MAX_PAGES = 3;

/**
 * The catalogue, in the order the arrange sheet's Available list offers them.
 *
 * Every entry is backed by a real slice of `HomeData` that the pond populates
 * — nothing here is a placeholder for data we do not have (DESIGN.md §3).
 */
export const CARDS: readonly CardSpec[] = [
  { id: "devices", title: "Devices", hint: "Lights, locks, plugs and thermostats", defaultSize: "l" },
  { id: "weather", title: "Weather", hint: "Now, and the days ahead", defaultSize: "m" },
  { id: "nowPlaying", title: "Music", hint: "What is playing, and the controls", defaultSize: "s" },
] as const;

const CARD_IDS = new Set<string>(CARDS.map((c) => c.id));
const SIZES = new Set<string>(["s", "m", "l"]);

function specOf(id: CardId): CardSpec {
  // Non-null by construction: every CardId has a row, and the set above is
  // derived from the same array.
  return CARDS.find((c) => c.id === id) as CardSpec;
}

export interface DashboardLayout {
  version: 2;
  /** One entry per page, each holding that page's cards in display order. */
  pages: PlacedCard[][];
  /** Everything the household has taken off. Kept so the sheet can offer it back. */
  hidden: CardId[];
}

/**
 * Today's Home, exactly.
 *
 * Changing this changes what a household sees on first run and after a reset.
 * Adding a card here is a product decision; adding one in the sheet is theirs.
 */
export const DEFAULT_LAYOUT: DashboardLayout = {
  version: 2,
  pages: [
    [
      { id: "weather", size: "m" },
      { id: "devices", size: "l" },
    ],
    [{ id: "nowPlaying", size: "s" }],
  ],
  hidden: [],
};

// Unchanged from v1, because rule 2 below migrates rather than resets. A new
// key would be a silent reset with extra steps.
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
 * Repair a set of pages into something renderable.
 *
 * The failure modes multiply with the second axis, so they are enumerated
 * rather than left to fall out of the code:
 *
 *   unknown id on any page      dropped
 *   the same id on two pages    first occurrence wins — a per-list dedupe would
 *                               not have caught this, and two live copies of one
 *                               card is a store that disagrees with itself
 *   unknown or absent size      the card's defaultSize; a card with no size
 *                               renders with no size class at all
 *   an empty page               dropped, unless dropping it would leave none
 *   zero pages                  the default, since there is nothing to render
 *                               and no route back to the sheet from a blank one
 *   more than MAX_PAGES         the trailing pages are MERGED into the last kept
 *                               page rather than discarded — losing a card the
 *                               household placed is worse than a crowded page
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

  // Recomputed from the union of every page rather than filtered against one
  // list, because with pages there is no single list to filter against. A card
  // claimed as both placed and hidden is placed.
  const hidden = CARDS.map((c) => c.id).filter((id) => !seen.has(id));

  return { version: 2, pages, hidden };
}

/** A payload written by the release before pages existed. */
function isV1(raw: Record<string, unknown>): boolean {
  return Array.isArray(raw.order) && !Array.isArray(raw.pages);
}

/**
 * Carry a v1 `{order, hidden}` across, keeping the arrangement.
 *
 * Everything that was on Home stays on Home, in the same order, on one page, at
 * each card's default size — v1 had no size to preserve. The result then goes
 * through the v2 reconcile like any other, so cards this release dropped fall
 * out here rather than needing a second rule.
 */
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
    // Unreadable or unavailable storage (a private window, a wiped panel) is
    // not an error worth surfacing — it means "no preference expressed yet".
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
    // The arrangement still applies for this session; it just will not survive
    // a reload. Losing the preference is better than losing the interaction.
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
 * Take a card off Home. It goes back to the sheet rather than being forgotten.
 *
 * The guard is on the LAST CARD ANYWHERE, not on each page. An empty page is
 * still arrangeable, because the strip's Arrange control sits above the track
 * rather than inside it; a Home with nothing on it at all is the circular
 * failure — no cards, and so no visible route to the sheet that would fix it.
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

/**
 * Move a card one place within its own page.
 *
 * A splice, not the swap the flat list used: adjacent swap and splice stop
 * being the same operation the moment cards occupy different amounts of space,
 * and with s/m/l they now do. Bounds-checked, no wrap, and deliberately does
 * NOT cross a page boundary — "up" past the top of a page silently meaning
 * "the previous page" is a move nobody asked for. `moveCardToPage` is the
 * explicit one.
 */
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
 * Move a card to another page, at the end of it.
 *
 * Creates one page beyond the last, up to MAX_PAGES, so a household can spread
 * out without a separate "add a page" control to find.
 *
 * RETURNS THE PAGE THE CARD LANDED ON, which is not always the page that was
 * asked for. Taking the last card off a page empties it, `compact` drops it,
 * and every page after it shifts down one — so "move Weather to page 2" can
 * leave Weather on page 1. The caller follows the card with this value; a
 * caller that follows its own argument instead scrolls the track to a page the
 * card is not on. Null when nothing moved.
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
  // Counted before the compaction rather than searched for after it: the target
  // holds the card, so its index once the empties go is however many pages
  // ahead of it still have something on them.
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
