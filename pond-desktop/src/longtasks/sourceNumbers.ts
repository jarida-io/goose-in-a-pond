// The report's citations as numbered sources: the pond lists them once each, in the order first
// cited, so a source's number is its place in that list.

import type { LongTaskSource } from "../api/types";

const WEB = /^https?:\/\//i;

function key(url: string): string {
  return url.trim().replace(/\/+$/, "");
}

/** The sources a browser may open, each with its number. */
export function numberedSources(sources: LongTaskSource[]): { sources: LongTaskSource[]; numberOf: (url: string) => number | null } {
  const web = sources.filter((s) => WEB.test(s.url));
  const numbers = new Map<string, number>();
  web.forEach((s, i) => {
    if (!numbers.has(key(s.url))) numbers.set(key(s.url), i + 1);
  });
  return { sources: web, numberOf: (url) => numbers.get(key(url)) ?? null };
}

/** "www.kra.go.ke" reads as "kra.go.ke"; an address that will not parse is shown whole. */
export function siteOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/**
 * The text without the "## Sources" list the pond appends, which the numbered sources row
 * replaces. Only a last section that is nothing but a list of links is taken off.
 */
export function withoutSourcesList(markdown: string): string {
  const lines = markdown.replace(/\s+$/, "").split("\n");
  let start = -1;
  for (let i = lines.length - 1; i >= 0; i--) {
    if (/^##\s+Sources\s*$/i.test(lines[i].trim())) {
      start = i;
      break;
    }
    if (/^#{1,6}\s/.test(lines[i])) return markdown;
  }
  if (start < 0) return markdown;
  const rest = lines.slice(start + 1).filter((l) => l.trim() !== "");
  if (rest.length === 0 || !rest.every((l) => /^\s*[-*]\s+\[[^\]]*\]\([^)]*\)\s*$/.test(l))) return markdown;
  return lines.slice(0, start).join("\n").replace(/\s+$/, "") + "\n";
}

/** Link words that only point, which the chip's number says better on its own. */
export function wordsOnlyPoint(label: string, href: string): boolean {
  const words = label.trim();
  return words === "" || key(words) === key(href) || /^\[?\d+\]?$/.test(words);
}
