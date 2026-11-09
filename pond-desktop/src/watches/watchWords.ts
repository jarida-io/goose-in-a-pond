// What the watches screens say, in the words a household would use.

import type { WebWatch, WebWatchFilters, WebWatchItem } from "../api/types";

export const WATCHES_TITLE = "Watches";
export const WATCHES_INTRO = "Pages Goose checks for you, and tells you when something you want turns up.";
export const NO_WATCHES_TITLE = "Nothing watched yet";
export const NO_WATCHES_BODY =
  "Ask Goose in a chat to watch a page for you, such as \"tell me when a Subaru Forester under 1.5 million is listed on this site\". A saved listing's Watch price does it in one tap.";
export const START_IN_CHAT = "Ask in a chat";
export const LOADING_WATCHES = "Loading your watches...";
export const LOADING_WATCH = "Loading the watch...";
export const RETRY = "Try again";
export const ALL_WATCHES = "All watches";
export const OPEN_PAGE = "Open the page";

export const EDIT_HEADING = "What to look for";
export const LOOKING_FOR = "In your words";
export const WORDS = "Words in the title";
export const WORDS_HINT = "Separate them with commas. Every one must be in a title for it to match.";
export const MAX_PRICE = "Highest price";
export const MIN_YEAR = "Oldest year";
export const MAX_MILEAGE = "Most mileage, in km";
export const CHECK_EVERY = "Check every";
export const SAVE_CHANGES = "Save changes";
export const SAVING = "Saving...";
export const SAVED = "Saved.";
export const PAUSE = "Pause";
export const RESUME = "Resume";
export const DELETE = "Delete";
export const DELETE_TITLE = "Delete watch";

export const FOUND_HEADING = "On the page now";
export const NOTHING_FOUND = "The first check has not run yet.";
export const EMPTY_PAGE = "The page listed nothing at the last check.";
export const CHANGES_HEADING = "What changed";
export const NO_CHANGES = "No changes yet. Goose lists what turns up, leaves or changes price here.";
export const MATCH = "Match";
export const MATCH_HINT = "What you asked for, so you were told at once.";

export const CHECK_EVERY_HOURS = [1, 2, 3, 6, 12, 24, 48, 72, 168] as const;

export function everyWords(hours: number): string {
  if (hours === 1) return "hour";
  if (hours === 168) return "week";
  if (hours % 24 === 0) return hours === 24 ? "day" : `${hours / 24} days`;
  return `${hours} hours`;
}

export function deleteQuestion(title: string): string {
  return `Delete the watch on "${title}"? Goose stops checking the page and forgets what changed. This cannot be undone.`;
}

function ago(iso: string, now: Date): string {
  const mins = Math.round((now.getTime() - Date.parse(iso)) / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  return days === 1 ? "yesterday" : `${days} days ago`;
}

function inTime(iso: string, now: Date): string {
  const mins = Math.round((Date.parse(iso) - now.getTime()) / 60_000);
  if (mins <= 1) return "soon";
  if (mins < 60) return `in ${mins} min`;
  const hours = Math.round(mins / 60);
  return hours < 24 ? `in ${hours} h` : `in ${Math.round(hours / 24)} days`;
}

export function foundAgo(iso: string, now: Date = new Date()): string {
  return ago(iso, now);
}

/** "Every 6 hours. Checked 2 h ago, next in 4 h." */
export function checkWords(watch: WebWatch, now: Date = new Date()): string {
  const every = `Every ${everyWords(watch.interval_hours)}.`;
  if (watch.paused) return `Paused. ${every}`;
  const last = watch.last_checked_at ? `Checked ${ago(watch.last_checked_at, now)}` : "Not checked yet";
  return `${every} ${last}, next ${inTime(watch.next_check_at, now)}.`;
}

/** Said only when checks have been failing, which is when it needs saying. */
export function failureWords(watch: WebWatch): string | null {
  if (watch.failures === 0) return null;
  return watch.failures === 1
    ? "The last check could not read the page. Goose tries again later."
    : `The last ${watch.failures} checks could not read the page. Goose waits longer between tries until one works.`;
}

export function countWords(watch: WebWatch): string {
  if (watch.last_checked_at === null && watch.items_count === 0) return "Waiting for the first check";
  const items = `${watch.items_count} ${watch.items_count === 1 ? "item" : "items"}`;
  return `${items}, ${watch.matching_count} ${watch.matching_count === 1 ? "matches" : "match"}`;
}

/** "KES 1,450,000 · 2017 · 98,000 km", the facts an item states. */
export function itemFacts(item: WebWatchItem): string {
  const price = item.price ? (item.currency && !item.price.includes(item.currency) ? `${item.currency} ${item.price}` : item.price) : null;
  return [price, item.year, item.mileage].filter(Boolean).join(" · ");
}

export function siteOf(address: string): string {
  try {
    return new URL(address).hostname.replace(/^www\./, "");
  } catch {
    return address;
  }
}

/** The form's text fields as filters; a blank or unreadable number is no filter. */
export function filtersFrom(form: { words: string; maxPrice: string; minYear: string; maxMileage: string }): WebWatchFilters {
  const number = (text: string) => {
    const n = Number(text.replace(/[\s,]/g, ""));
    return text.trim() !== "" && Number.isFinite(n) && n > 0 ? n : undefined;
  };
  const words = form.words
    .split(",")
    .map((w) => w.trim())
    .filter(Boolean);
  const minYear = number(form.minYear);
  const filters: WebWatchFilters = {};
  if (words.length > 0) filters.words = words;
  const maxPrice = number(form.maxPrice);
  if (maxPrice !== undefined) filters.max_price = maxPrice;
  if (minYear !== undefined) filters.min_year = Math.round(minYear);
  const maxMileage = number(form.maxMileage);
  if (maxMileage !== undefined) filters.max_mileage = maxMileage;
  return filters;
}

export function formFrom(watch: WebWatch) {
  const f = watch.filters ?? {};
  return {
    lookingFor: watch.looking_for,
    words: (f.words ?? []).join(", "),
    maxPrice: f.max_price !== undefined ? String(f.max_price) : "",
    minYear: f.min_year !== undefined ? String(f.min_year) : "",
    maxMileage: f.max_mileage !== undefined ? String(f.max_mileage) : "",
    interval: watch.interval_hours,
  };
}

export type WatchForm = ReturnType<typeof formFrom>;
