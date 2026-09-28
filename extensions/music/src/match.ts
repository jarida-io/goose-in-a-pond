/**
 * Playlist name matching, and whose playlist was meant.
 *
 * Separate from server.ts because that starts a readline loop at module scope,
 * so importing it to test a pure function would start a server.
 */

/** Lowercase, drop emoji and punctuation, collapse runs of whitespace. */
export function normalizeName(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^\p{Letter}\p{Number}]+/gu, " ")
    .trim()
    .replace(/\s+/g, " ");
}

/**
 * Filler around a spoken playlist name, stripped from the query only (it dilutes the match).
 *
 * "spotify" stays filler: "the Spotify playlist Randoms" names the platform, not
 * the owner. Ownership is claimed by the possessive and trailing "by", which
 * splitOwnerHint reads before this runs.
 */
const QUERY_FILLER = new Set([
  "the", "a", "an", "my", "our", "from", "in", "on", "of", "please", "playlist",
  "playlists", "list", "library", "spotify", "called", "named", "one",
]);

/** Drops filler, keeping the original if that would leave nothing to match on. */
export function contentWords(normalized: string): string {
  const kept = normalized.split(" ").filter(w => w && !QUERY_FILLER.has(w));
  return kept.length > 0 ? kept.join(" ") : normalized;
}

/** 0–1 match score; compares without spaces too, since people say "sautisol" for "Sauti sol". */
export function playlistMatchScore(query: string, playlistName: string): number {
  const q = contentWords(normalizeName(query));
  const n = normalizeName(playlistName);
  if (!q || !n) return 0;
  if (q === n) return 1;

  const qs = q.replace(/ /g, "");
  const ns = n.replace(/ /g, "");
  if (qs === ns) return 0.95;

  // Prefix is the common case; weighting containment by coverage stops short names tying with it.
  if (ns.startsWith(qs)) return 0.92;
  if (ns.includes(qs)) return 0.75 + 0.15 * (qs.length / ns.length);
  if (qs.includes(ns)) return 0.7 + 0.15 * (ns.length / qs.length);

  const qWords = q.split(" ");
  const nSet = new Set(n.split(" "));
  const overlap = qWords.filter(w => nSet.has(w)).length;
  // Kept below the containment band so a partial word match never outranks one.
  return Math.min(0.65, overlap / qWords.length);
}

/** Spotify's editorial account id; shared with parsePlaylist so the two cannot drift. */
export const EDITORIAL_OWNER = "spotify";

/** What a query said about who owns the playlist. */
export interface OwnerHint {
  name: string;
  owner: string | null;
  /** Spotify itself — editorial, which this app can only see once followed. */
  editorial: boolean;
}

/**
 * Splits off an owner: a trailing "by X", or the possessive "X's ...".
 *
 * A named person is believed only when they own something here, since playlist
 * names contain "by" too. Spotify is the exception — believed even when it owns
 * nothing, so the caller can say editorial playlists are out of reach instead of
 * quietly playing one of the user's own.
 */
export function splitOwnerHint(
  query: string,
  playlists: { owner: string }[]
): OwnerHint {
  const possessive = query.match(/^\s*(.+?)'s\s+(.+)$/i);
  if (possessive && normalizeName(possessive[1]) === EDITORIAL_OWNER && possessive[2].trim()) {
    return { name: possessive[2].trim(), owner: possessive[1].trim(), editorial: true };
  }

  const m = query.match(/^(.*?)\s+by\s+([^,]+)$/i);
  if (!m) return { name: query, owner: null, editorial: false };

  const candidate = normalizeName(m[2]);
  const name = m[1].trim();
  if (!name) return { name: query, owner: null, editorial: false };

  if (candidate === EDITORIAL_OWNER) {
    return { name, owner: m[2].trim(), editorial: true };
  }

  const known = playlists.some(p => {
    const o = normalizeName(p.owner);
    return o === candidate || o.includes(candidate) || candidate.includes(o);
  });

  return known
    ? { name, owner: m[2].trim(), editorial: false }
    : { name: query, owner: null, editorial: false };
}

/** Naming an owner makes it a playlist request, whatever `target` said. */
export function claimsEditorialOwner(query: string): boolean {
  return splitOwnerHint(query, []).editorial;
}
