#!/usr/bin/env node
/** GIAP Music MCP server for Spotify: a few intent-shaped tools, not one per endpoint. */
import * as readline from "readline";
import { describeError, log } from "./log.js";
import { isShortRelease, SpotifyProvider } from "./providers/spotify.js";
import type { TimeRange } from "./providers/types.js";

const provider = new SpotifyProvider();

const TOOLS = [
  {
    name: "play",
    // Enums, not sibling tools, to save tokens; the description says only what no parameter can.
    description:
      "Play music on Spotify. Music keeps playing afterwards: a song starts inside its album so the album follows on, and a single is topped up with more by the same artist — do not tell the user playback will stop after the song, and do not queue extra songs yourself to keep it going. Search picks the closest match, which is not always what was asked for — tell the user the track name and artist FROM THE RESULT, never the name they asked for.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description:
            "What to play, as the user said it — 'Marvin's Room by Drake', 'Randoms', 'jazz'. Omit to resume what is paused.",
        },
        target: {
          type: "string",
          enum: ["track", "playlist"],
          description:
            "'playlist' matches the user's own playlists loosely by name, preferring ones they created. Default 'track' searches songs, artists and albums.",
        },
        when: {
          type: "string",
          enum: ["now", "next"],
          description:
            "'next' appends to the queue and lets the current track finish; Spotify cannot insert at a chosen position, and cannot queue a whole playlist, so this applies to tracks only. Default 'now' replaces what is playing.",
        },
        uri: {
          type: "string",
          description:
            "A pasted Spotify URI or link. Plays it directly, and is the only way to reach a playlist outside the user's library.",
        },
      },
    },
  },
      {
    name: "playlists",
    description:
      "List every playlist in the user's Spotify library, separated into ones they created and ones they follow from other people. Use this to answer 'what playlists do I have' or 'which of these are mine', and to find the exact name before playing one with the 'play' tool.",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "library",
    description:
      "The user's own Spotify library and listening history: their liked songs, what they listen to most, and what they played recently. Read-only — Spotify does not let this app change what is liked. Use for 'what are my liked songs', 'what do I listen to most', 'what was I playing yesterday'.",
    inputSchema: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["saved", "top_tracks", "top_artists", "recent"],
          description:
            "saved = list liked songs; top_tracks / top_artists = what they listen to most; recent = recently played.",
        },
        time_range: {
          type: "string",
          enum: ["short_term", "medium_term", "long_term"],
          description:
            "How far back top_tracks / top_artists look: short_term is about 4 weeks, medium_term about 6 months, long_term is several years. Defaults to medium_term.",
        },
        limit: {
          type: "number",
          description: "How many to return, 1-50. Defaults to 20.",
        },
      },
      required: ["action"],
    },
  },
  {
    name: "devices",
    description:
      "List the devices Spotify can play on (phone, computer, speaker, TV), or move playback to one of them. Call with no arguments to see what is available; pass transfer_to with a device name to move the music there without interrupting it. Use this for 'play this on the speaker', 'move it to my phone', 'where can I play this'.",
    inputSchema: {
      type: "object",
      properties: {
        transfer_to: {
          type: "string",
          description:
            "Name of the device to move playback to, as the user said it (e.g. 'my phone', 'kitchen speaker'). Matched loosely against the device list. Omit to just list devices.",
        },
      },
    },
  },
  {
    name: "status",
    description:
      "Get what is currently playing on Spotify — track name, artist, album, progress, and upcoming queue.",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "control",
    description:
      "Control Spotify playback: pause, resume, next, previous, set volume, toggle shuffle, jump within the track, or set repeat.",
    inputSchema: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: [
            "pause",
            "resume",
            "next",
            "previous",
            "volume_up",
            "volume_down",
            "set_volume",
            "shuffle_on",
            "shuffle_off",
            "seek",
            "repeat_off",
            "repeat_track",
            "repeat_all",
          ],
          description:
            "The playback action to perform. 'seek' jumps within the current track (give position); 'repeat_track' loops the song, 'repeat_all' loops the album or playlist, 'repeat_off' stops looping.",
        },
        volume: {
          type: "number",
          description: "Exact volume level (0-100). Required when action is 'set_volume'.",
        },
        position: {
          type: "string",
          description:
            "Where to jump to, for action 'seek'. Accepts 'm:ss' like '1:30', or a plain number of seconds like '90'.",
        },
      },
      required: ["action"],
    },
  },
];

// ── Tool handlers ─────────────────────────────────────────────

async function handlePlay(args: Record<string, unknown>): Promise<string> {
  const query = args.query as string | undefined;
  const uri = args.uri as string | undefined;

  if (uri) {
    // Resolve the album so playback continues; on any failure, play the URI as given.
    if (uri.startsWith("spotify:track:")) {
      try {
        const track = await provider.getTrack(uri);
        if (track) {
          const result = await provider.play(track);
          if (isShortRelease(track)) {
            try {
              await provider.queueFollowUps(track);
            } catch (err) {
              log.warn("follow_up_failed", "could not queue follow-ups", {
                seed: track.uri,
                error: describeError(err),
              });
            }
          }
          return result;
        }
      } catch (err) {
        log.warn("track_lookup_failed", "playing the URI without its album context", {
          uri,
          error: describeError(err),
        });
      }
    }
    return await provider.play(uri);
  }

  // A query saying "playlist" counts even with `type` unset: models often fail to set it.
  const saysPlaylist = !!query && /\bplaylists?\b/i.test(query);
  if (query && ((args.type as string | undefined) === "playlist" || saysPlaylist)) {
    const { uri: playlistUri, name } = await resolvePlaylist(query);
    await provider.play(playlistUri);
    return `Now playing playlist: ${name}`;
  }

  // Album: play the whole record in order, same context_uri mechanism.
  if (query && (args.type as string | undefined) === "album") {
    const albums = await provider.searchAlbums(query, 5);
    if (albums.length === 0) {
      return `No album found for "${query}". Try a different search.`;
    }
    const top = albums[0];
    await provider.play(top.uri);

    let text = `Now playing album: ${top.name} by ${top.artist} (${top.total_tracks} tracks, ${top.release_date})`;
    const others = albums.slice(1, 4);
    if (others.length > 0) {
      text +=
        "\n\nOther matches:\n" +
        others.map((a, i) => `${i + 2}. ${a.name} by ${a.artist}`).join("\n");
    }
    return text;
  }

  if (query) {
    const tracks = await provider.searchTracks(query, 5);
    if (tracks.length === 0) {
      return `No results found for "${query}". Try a different search.`;
    }

    const top = tracks[0];
    // Pass the TrackInfo, not `top.uri`: its album lets playback continue past the track.
    await provider.play(top);

    // Top up a short release; the song has already started, so a failure here is only logged.
    let toppedUp: { queued: number; source: "artist" | "listener" } | null = null;
    if (isShortRelease(top)) {
      try {
        toppedUp = await provider.queueFollowUps(top);
      } catch (err) {
        log.warn("follow_up_failed", "could not queue follow-ups", {
          seed: top.uri,
          error: describeError(err),
        });
      }
    }

    const others = tracks.slice(1, 4);
    // "track" outright, so the model can't pass this off as the playlist it was asked for.
    let text = `Now playing track: ${top.name} by ${top.artist} (${top.album})`;
    // Say what follows, naming the source: the fallback queues the listener's favourites.
    if (toppedUp && toppedUp.queued > 0) {
      text +=
        toppedUp.source === "artist"
          ? `\nThen ${toppedUp.queued} more by ${top.artist}.`
          : `\nThen ${toppedUp.queued} more from your top tracks.`;
    } else if (!isShortRelease(top) && top.album_uri) {
      // Only claim the album follows when a context was actually sent.
      text += `\nThe rest of the album follows.`;
    }
    if (others.length > 0) {
      text +=
        "\n\nOther matches:\n" +
        others.map((t, i) => `${i + 2}. ${t.name} by ${t.artist}`).join("\n");
    }
    return text;
  }

  // No query, no URI — resume
  const result = await provider.play();
  return result;
}

async function handleQueue(args: Record<string, unknown>): Promise<string> {
  const uri = args.uri as string | undefined;
  const query = args.query as string | undefined;

  if (uri) {
    await provider.addToQueue(uri);
    return `Queued ${uri}`;
  }

  if (!query) {
    return "Tell me what to queue — a song name, optionally with the artist.";
  }

  const tracks = await provider.searchTracks(query, 5);
  if (tracks.length === 0) {
    return `No results found for "${query}". Try a different search.`;
  }

  const top = tracks[0];
  await provider.addToQueue(top.uri);

  // Name the pick so a wrong one can be spotted and skipped.
  let text = `Queued: ${top.name} by ${top.artist} (${top.album}). Current track keeps playing.`;
  const others = tracks.slice(1, 4);
  if (others.length > 0) {
    text +=
      "\n\nOther matches:\n" +
      others.map((t, i) => `${i + 2}. ${t.name} by ${t.artist}`).join("\n");
  }
  return text;
}

/** Lowercase, drop emoji and punctuation, collapse runs of whitespace. */
function normalizeName(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^\p{Letter}\p{Number}]+/gu, " ")
    .trim()
    .replace(/\s+/g, " ");
}

/** Filler around a spoken playlist name, stripped from the query only (it dilutes the match). */
const QUERY_FILLER = new Set([
  "the", "a", "an", "my", "our", "from", "in", "on", "of", "please", "playlist",
  "playlists", "list", "library", "spotify", "called", "named", "one",
]);

/** Drops filler, keeping the original if that would leave nothing to match on. */
function contentWords(normalized: string): string {
  const kept = normalized.split(" ").filter(w => w && !QUERY_FILLER.has(w));
  return kept.length > 0 ? kept.join(" ") : normalized;
}

/** 0–1 match score; compares without spaces too, since people say "sautisol" for "Sauti sol". */
function playlistMatchScore(query: string, playlistName: string): number {
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

/** Splits off a trailing "by X" owner, only when X owns something here: names contain "by" too. */
function splitOwnerHint(
  query: string,
  playlists: { owner: string }[]
): { name: string; owner: string | null } {
  const m = query.match(/^(.*?)\s+by\s+([^,]+)$/i);
  if (!m) return { name: query, owner: null };

  const candidate = normalizeName(m[2]);
  const known = playlists.some(p => {
    const o = normalizeName(p.owner);
    return o === candidate || o.includes(candidate) || candidate.includes(o);
  });

  return known && m[1].trim()
    ? { name: m[1].trim(), owner: m[2].trim() }
    : { name: query, owner: null };
}

/** Finds a library playlist by name; the model has names, not ids. */
async function resolvePlaylist(query: string): Promise<{ uri: string; name: string }> {
  const playlists = await provider.getPlaylists();
  const { name, owner } = splitOwnerHint(query, playlists);

  // Narrow to the named owner, or the own-playlist tie-break picks the user's lookalike.
  let pool = playlists;
  if (owner) {
    const wanted = normalizeName(owner);
    pool = playlists.filter(p => {
      const o = normalizeName(p.owner);
      return o === wanted || o.includes(wanted) || wanted.includes(o);
    });
    if (pool.length === 0) {
      throw new Error(`Nobody called "${owner}" owns a playlist in this library.`);
    }
  }

  const ranked = pool
    .map(p => ({ p, score: playlistMatchScore(name, p.name) }))
    // On a tie only, prefer the user's own playlist over a followed one.
    .sort((a, b) => b.score - a.score || Number(b.p.is_own) - Number(a.p.is_own));

  const best = ranked[0];
  if (best && best.score >= 0.5) return { uri: best.p.uri, name: best.p.name };

  const suggestions = ranked
    .slice(0, 8)
    .map(r => r.p.name)
    .join(", ");
  const scope = owner ? ` from ${owner}` : "";
  // Other users' playlists are 403 for this app, so explain the way out.
  throw new Error(
    `No playlist matching "${name}"${scope} in this library. ` +
      `Closest${scope}: ${suggestions || "(none)"}. ` +
      `Only playlists the user created or follows are visible — if it belongs to someone else, ` +
      `they can follow it in Spotify, or paste its link to play it directly.`
  );
}

async function handlePlayPlaylist(args: Record<string, unknown>): Promise<string> {
  // A link or URI is the only way to reach a playlist outside the library.
  const given = (args.uri ?? args.url) as string | undefined;
  if (given) {
    const id = given.match(/playlist[/:]([A-Za-z0-9]+)/)?.[1];
    if (!id) return `That does not look like a Spotify playlist link: ${given}`;
    await provider.play(`spotify:playlist:${id}`);
    return `Now playing playlist from the link provided.`;
  }

  const name = (args.name ?? args.query) as string | undefined;
  if (!name) return "Which playlist? Give me its name, or a Spotify playlist link.";

  const { uri, name: actual } = await resolvePlaylist(name);
  await provider.play(uri);
  return `Now playing playlist: ${actual}`;
}


async function handleLibrary(args: Record<string, unknown>): Promise<string> {
  const action = args.action as string;
  const range = (args.time_range as TimeRange) || "medium_term";
  const limit = typeof args.limit === "number" ? args.limit : 20;
  const query = args.query as string | undefined;

  switch (action) {
    case "saved": {
      const tracks = await provider.getSavedTracks(limit);
      if (tracks.length === 0) return "No liked songs in this Spotify account.";
      return (
        `${tracks.length} liked song(s):\n` +
        tracks.map((t, i) => `${i + 1}. ${t.name} by ${t.artist}`).join("\n")
      );
    }


    case "top_tracks": {
      const tracks = await provider.getTopTracks(range, limit);
      if (tracks.length === 0) return "Spotify has no top tracks for this period yet.";
      return (
        `Top ${tracks.length} track(s) (${describeRange(range)}):\n` +
        tracks.map((t, i) => `${i + 1}. ${t.name} by ${t.artist}`).join("\n")
      );
    }

    case "top_artists": {
      const artists = await provider.getTopArtists(range, limit);
      if (artists.length === 0) return "Spotify has no top artists for this period yet.";
      return (
        `Top ${artists.length} artist(s) (${describeRange(range)}):\n` +
        artists
          .map((a, i) => `${i + 1}. ${a.name}${a.genres.length ? ` - ${a.genres.slice(0, 3).join(", ")}` : ""}`)
          .join("\n")
      );
    }

    case "recent": {
      const tracks = await provider.getRecentlyPlayed(limit);
      if (tracks.length === 0) return "No recent listening history.";
      return (
        `${tracks.length} recently played:\n` +
        tracks.map((t, i) => `${i + 1}. ${t.name} by ${t.artist}`).join("\n")
      );
    }

    default:
      return `Unknown action: ${action}`;
  }
}

function describeRange(range: TimeRange): string {
  return range === "short_term"
    ? "last 4 weeks"
    : range === "long_term"
      ? "several years"
      : "last 6 months";
}

async function handleDevices(args: Record<string, unknown>): Promise<string> {
  const devices = await provider.getDevices();
  if (devices.length === 0) {
    return "No Spotify devices are available. Open Spotify on a phone, computer or speaker first.";
  }

  const target = args.transfer_to as string | undefined;
  if (!target) {
    return (
      `${devices.length} device(s) available:\n` +
      devices
        .map(d => `- ${d.name} (${d.type})${d.is_active ? " - currently playing here" : ""}`)
        .join("\n")
    );
  }

  // Also match on device type: people say "the speaker" more than a device's name.
  const ranked = devices
    .map(d => ({ d, score: Math.max(playlistMatchScore(target, d.name), playlistMatchScore(target, d.type)) }))
    .sort((a, b) => b.score - a.score);

  const best = ranked[0];
  if (!best || best.score < 0.5) {
    return `No device matching "${target}". Available: ${devices.map(d => `${d.name} (${d.type})`).join(", ")}`;
  }
  if (best.d.is_active) {
    return `${best.d.name} is already the one playing.`;
  }

  return provider.transferPlayback(best.d.id, best.d.name);
}

async function handlePlaylists(): Promise<string> {
  const playlists = await provider.getPlaylists();
  if (playlists.length === 0) return "No playlists found on this Spotify account.";

  const mine = playlists.filter(p => p.is_own);
  const followed = playlists.filter(p => !p.is_own);

  let text = `${playlists.length} playlist(s) in the library: ${mine.length} created by the user, ${followed.length} followed from others.`;
  if (mine.length > 0) {
    text += `\n\nCreated by the user (${mine.length}):\n` + mine.map(p => `- ${p.name}`).join("\n");
  }
  if (followed.length > 0) {
    text +=
      `\n\nFollowed from other people (${followed.length}):\n` +
      followed.map(p => `- ${p.name} (by ${p.owner})`).join("\n");
  }
  return text;
}

async function handleStatus(): Promise<string> {
  const now = await provider.getNowPlaying();
  if (!now) {
    return "Nothing is currently playing on Spotify.";
  }

  const progress = now.progress_ms
    ? `${Math.floor(now.progress_ms / 60000)}:${String(Math.floor((now.progress_ms % 60000) / 1000)).padStart(2, "0")}`
    : "0:00";
  const duration = `${Math.floor(now.duration_ms / 60000)}:${String(Math.floor((now.duration_ms % 60000) / 1000)).padStart(2, "0")}`;

  let text = `${now.is_playing ? "Playing" : "Paused"}: ${now.name} by ${now.artist}\nAlbum: ${now.album}\nProgress: ${progress} / ${duration}`;

  try {
    const queue = await provider.getQueue();
    const upcoming = queue.slice(1, 4);
    if (upcoming.length > 0) {
      text +=
        "\n\nUp next:\n" +
        upcoming.map((t, i) => `${i + 1}. ${t.name} by ${t.artist}`).join("\n");
    }
  } catch {
    // Queue not available — that's fine
  }

  return text;
}

async function handleControl(args: Record<string, unknown>): Promise<string> {
  const action = args.action as string;
  const volume = args.volume as number | undefined;

  switch (action) {
    case "pause":
      return provider.pause();
    case "resume":
      return provider.play();
    case "next":
      return provider.next();
    case "previous":
      return provider.previous();
    case "volume_up": {
      const now = await provider.getNowPlaying();
      const cur = now?.volume_percent ?? 50;
      return provider.setVolume(Math.min(100, cur + 10));
    }
    case "volume_down": {
      const now = await provider.getNowPlaying();
      const cur = now?.volume_percent ?? 50;
      return provider.setVolume(Math.max(0, cur - 10));
    }
    case "set_volume":
      return provider.setVolume(volume ?? 50);
    case "shuffle_on":
      return provider.setShuffle(true);
    case "shuffle_off":
      return provider.setShuffle(false);
    case "seek": {
      const ms = parsePosition(args.position);
      if (ms === null) {
        return "Where should I jump to? Give a time like '1:30' or a number of seconds.";
      }
      return provider.seek(ms);
    }
    case "repeat_off":
      return provider.setRepeat("off");
    case "repeat_track":
      return provider.setRepeat("track");
    case "repeat_all":
      return provider.setRepeat("context");
    default:
      return `Unknown action: ${action}`;
  }
}

/** Reads "1:30", "90" or 90 as milliseconds. Returns null if it is neither. */
function parsePosition(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return Math.max(0, value * 1000);
  if (typeof value !== "string") return null;

  const text = value.trim();
  const clock = text.match(/^(\d+):([0-5]?\d)$/);
  if (clock) return (Number(clock[1]) * 60 + Number(clock[2])) * 1000;

  const seconds = Number(text);
  return Number.isFinite(seconds) ? Math.max(0, seconds * 1000) : null;
}

// ── Logging ───────────────────────────────────────────────────
function debug(...args: unknown[]) {
  const [first, ...rest] = args;
  log.debug(
    "jsonrpc",
    typeof first === "string" ? first : JSON.stringify(first),
    rest.length > 0 ? { detail: rest.map(a => (typeof a === "string" ? a : JSON.stringify(a))).join(" ") } : undefined,
  );
}

// ── MCP JSON-RPC server ───────────────────────────────────────

interface JsonRpcRequest {
  jsonrpc: string;
  id?: number | string;
  method: string;
  params?: Record<string, unknown>;
}

async function handleRequest(
  request: JsonRpcRequest
): Promise<Record<string, unknown> | null> {
  const { method, id, params } = request;

  debug(`<-- ${method}`, params ? JSON.stringify(params).slice(0, 200) : "");

  switch (method) {
    case "initialize":
      debug("initializing");
      return {
        jsonrpc: "2.0",
        id,
        result: {
          protocolVersion: "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "giap-music", version: "0.2.0" },
        },
      };

    case "notifications/initialized":
      debug("initialized OK");
      return null;

    case "tools/list":
      debug(`listing ${TOOLS.length} tools`);
      return { jsonrpc: "2.0", id, result: { tools: TOOLS } };

    case "tools/call": {
      const toolName = (params as Record<string, unknown>)?.name as string;
      const args =
        ((params as Record<string, unknown>)?.arguments as Record<
          string,
          unknown
        >) ?? {};

      const started = Date.now();
      log.info("tool_call", `handling ${toolName}`, { tool: toolName });

      try {
        let text: string;
        switch (toolName) {
          case "play": {
            const target = (args.target as string | undefined) ?? "track";
            const when = (args.when as string | undefined) ?? "now";
            debug(`play → ${target}/${when}`, String(args.query ?? args.uri ?? "(resume)"));
            if (target === "playlist") {
              text = await handlePlayPlaylist({ ...args, name: args.name ?? args.query });
              if (when === "next") {
                text += "\n\n(Played now — Spotify cannot add a whole playlist to the queue.)";
              }
            } else if (when === "next") {
              text = await handleQueue(args);
            } else {
              text = await handlePlay(args);
            }
            break;
          }
          case "library":
            debug("library →", args.action, args.query ?? "");
            text = await handleLibrary(args);
            break;
          case "devices":
            debug("devices →", args.transfer_to ?? "(list)");
            text = await handleDevices(args);
            break;
          case "playlists":
            debug("playlists → listing");
            text = await handlePlaylists();
            break;
          case "status":
            debug("status → checking now playing");
            text = await handleStatus();
            break;
          case "control":
            debug("control →", args.action, args.volume ?? "");
            text = await handleControl(args);
            break;
          default:
            log.warn("unknown_tool", "the model asked for a tool this extension does not have", {
              tool: toolName,
            });
            return {
              jsonrpc: "2.0",
              id,
              error: { code: -32601, message: `Unknown tool: ${toolName}` },
            };
        }

        log.info("tool_ok", `${toolName} succeeded`, {
          tool: toolName,
          chars: text.length,
          duration_ms: Date.now() - started,
        });
        return {
          jsonrpc: "2.0",
          id,
          result: { content: [{ type: "text", text }] },
        };
      } catch (err) {
        const msg = describeError(err);
        log.warn("tool_failed", `${toolName} failed`, {
          tool: toolName,
          error: msg,
          duration_ms: Date.now() - started,
        });
        return {
          jsonrpc: "2.0",
          id,
          result: {
            content: [{ type: "text", text: `Error: ${msg}` }],
            isError: true,
          },
        };
      }
    }

    default:
      return {
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: `Method not found: ${method}` },
      };
  }
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line: string) => {
  try {
    const request = JSON.parse(line) as JsonRpcRequest;
    const response = await handleRequest(request);
    if (response) {
      process.stdout.write(JSON.stringify(response) + "\n");
    }
  } catch {
    // ignore malformed JSON
  }
});
