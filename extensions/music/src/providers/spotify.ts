import type { MusicProvider, TrackInfo, PlaylistInfo, AlbumInfo, DeviceInfo, RepeatState, ArtistInfo, TimeRange, PlayTarget, FollowUpResult } from './types.js';
import { describeError, log } from '../log.js';

interface SpotifyTrack {
  id: string;
  name: string;
  /** `id` is the only reliable way to tell this artist's tracks from covers and namesakes. */
  artists: Array<{ id?: string; name: string }>;
  /** All but `name` optional: player and queue endpoints return a thinner track than `/search`. */
  album: {
    name: string;
    id?: string;
    uri?: string;
    total_tracks?: number;
    album_type?: string;
  };
  duration_ms: number;
  uri: string;
}

/**
 * Body for `PUT /v1/me/player/play`. A `uris` list stops when it ends, so a track plays inside
 * its album via `offset`, which Spotify accepts only for album and playlist contexts.
 */
export function buildPlayBody(target?: PlayTarget): Record<string, unknown> {
  if (!target) return {};

  if (typeof target === 'string') {
    // Anything but a track URI is already a context.
    return target.startsWith('spotify:track:')
      ? { uris: [target] }
      : { context_uri: target };
  }

  if (target.album_uri) {
    return { context_uri: target.album_uri, offset: { uri: target.uri } };
  }
  return { uris: [target.uri] };
}

export function parseTrack(track: SpotifyTrack): TrackInfo {
    return {
      id: track.id,
      name: track.name,
      artist: track.artists.map(a => a.name).join(', '),
      album: track.album.name,
      duration_ms: track.duration_ms,
      uri: track.uri,
      // `album_uri` keeps playback going past this track; the rest drive the short-release top-up.
      album_uri: track.album.uri,
      album_total_tracks: track.album.total_tracks,
      album_type: track.album.album_type,
      artist_ids: track.artists.map(a => a.id).filter((id): id is string => !!id),
    };
}

/** Whether album playback would run out almost at once: singles and two-track releases. */
export function isShortRelease(track: TrackInfo): boolean {
  if (track.album_type === 'single') return true;
  return track.album_total_tracks !== undefined && track.album_total_tracks <= SHORT_RELEASE_TRACKS;
}

/** At or below this many tracks, a release gets topped up. */
const SHORT_RELEASE_TRACKS = 2;

/** How many follow-ups to queue behind a short release. */
const FOLLOW_UP_LIMIT = 10;

/** How many candidates to fetch before filtering them down to the artist's own. */
const FOLLOW_UP_SEARCH_LIMIT = 20;

/** The seed artist's own tracks, matched by id (by name if the seed has none), to queue next. */
export function pickFollowUps(
  seed: TrackInfo,
  candidates: TrackInfo[],
  limit: number,
): TrackInfo[] {
  const seedIds = new Set(seed.artist_ids ?? []);
  // Seeded so the requested single's album cut (same song, other id) isn't queued behind it.
  const seen = new Set([seed.name]);
  const out: TrackInfo[] = [];

  for (const c of candidates) {
    if (out.length >= limit) break;
    if (c.uri === seed.uri || c.id === seed.id) continue;
    const sharesArtist = seedIds.size > 0
      ? (c.artist_ids ?? []).some(id => seedIds.has(id))
      : c.artist === seed.artist;
    if (!sharesArtist) continue;
    if (seen.has(c.name)) continue;
    seen.add(c.name);
    out.push(c);
  }

  return out;
}

interface SpotifyAlbum {
  id: string;
  name: string;
  artists: Array<{ name: string }>;
  total_tracks: number;
  uri: string;
  release_date: string;
}

interface SpotifyPlaylist {
  id: string;
  name: string;
  description: string;
  /** Absent on some /me/playlists entries, which carry `items` instead. */
  tracks?: { total: number };
  items?: unknown[];
  uri: string;
  owner?: { id: string; display_name?: string };
}

/**
 * Endpoints Spotify withdrew from apps created after 2024-11-27, GIAP's included, whatever the
 * scopes: /recommendations, /audio-features, /audio-analysis, /artists/{id}/related-artists and
 * /top-tracks, /browse, /me/tracks/contains, PUT/DELETE /me/tracks; `preview_url` is always null.
 */
export class SpotifyProvider implements MusicProvider {
  name = 'Spotify';
  private baseUrl = 'https://api.spotify.com/v1';

  /** Current access token — initialized from env, updated on refresh. */
  private accessToken: string | null = process.env.SPOTIFY_ACCESS_TOKEN ?? null;

  private get token(): string {
    if (!this.accessToken) {
      log.warn('no_token', 'no Spotify token — the extension has never been signed in', {
        hint: 'sign in to Spotify from the Extensions tab',
      });
      throw new Error('SPOTIFY_ACCESS_TOKEN not set. Sign in via GIAP Extensions.');
    }
    return this.accessToken;
  }

  /** GIAP server URL for OAuth refresh requests. */
  private readonly giapUrl = process.env.GIAP_SERVER_URL || 'http://127.0.0.1:4000';

  /** Asks GIAP to refresh the token and updates it in memory, so retries need no restart. */
  private async refreshToken(): Promise<boolean> {
    // Each failure arm logs distinctly: they need different fixes from the user.
    const started = Date.now();
    log.debug('token_refresh_started', 'asking GIAP to refresh the Spotify token');

    try {
      const refreshResp = await fetch(`${this.giapUrl}/api/v1/oauth/refresh`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${process.env.GIAP_INTERNAL_TOKEN ?? ''}`,
        },
        body: JSON.stringify({ provider: 'spotify' }),
      });

      if (!refreshResp.ok) {
        log.warn('token_refresh_rejected', 'GIAP refused to refresh the Spotify token', {
          status: refreshResp.status,
          // A 401 here rejects GIAP's internal token, not Spotify's.
          hint: refreshResp.status === 401
            ? 'the extension\'s internal token was rejected'
            : 'sign in to Spotify again from the Extensions tab',
          duration_ms: Date.now() - started,
        });
        return false;
      }

      const data = await refreshResp.json() as { refreshed?: boolean; access_token?: string };
      if (data.access_token) {
        this.accessToken = data.access_token;
        log.info('token_refreshed', 'Spotify token refreshed', {
          duration_ms: Date.now() - started,
        });
        return true;
      }

      log.warn('token_refresh_empty', 'GIAP accepted the refresh but returned no token', {
        refreshed: data.refreshed ?? false,
        duration_ms: Date.now() - started,
      });
    } catch (error) {
      log.warn('token_refresh_unreachable', 'could not reach GIAP to refresh the token', {
        url: this.giapUrl,
        error: describeError(error),
        duration_ms: Date.now() - started,
      });
    }
    return false;
  }

  /** Sends a request, refreshing the token and retrying once on 401; the body is left unread. */
  private async request(method: string, path: string, body?: unknown): Promise<Response> {
    // `this.token` is read per attempt so the retry sends the refreshed token.
    const send = () => fetch(`${this.baseUrl}${path}`, {
      method,
      headers: {
        'Authorization': `Bearer ${this.token}`,
        'Content-Type': 'application/json',
      },
      body: body ? JSON.stringify(body) : undefined,
    });

    const started = Date.now();
    let resp = await send();
    let afterRefresh = '';

    if (resp.status === 401) {
      log.debug('token_expired', 'Spotify rejected the token; refreshing', { method, path });
      if (!await this.refreshToken()) {
        // refreshToken() has logged the cause; this logs the consequence.
        log.warn('request_unauthorised', 'a Spotify request could not be authorised', {
          method,
          path,
        });
        throw new Error(
          'Spotify token expired and refresh failed. Re-authenticate via GIAP Extensions.'
        );
      }
      resp = await send();
      afterRefresh = ' (after refresh)';
    }

    if (!resp.ok) {
      const body = await resp.text();
      log.warn('spotify_api_failed', 'Spotify refused a request', {
        method,
        path,
        status: resp.status,
        after_refresh: afterRefresh !== '',
        // The reason comes first in an error body; the logger redacts secrets.
        body: body.slice(0, 300),
        duration_ms: Date.now() - started,
      });

      // A missing scope, fixed by re-authorising; withdrawn endpoints give a bare 403 "Forbidden".
      if (resp.status === 403 && body.includes('Insufficient client scope')) {
        throw new Error(
          'Spotify has not granted GIAP this permission yet. Sign in to Spotify again ' +
            'from the Extensions tab to authorise it.'
        );
      }

      throw new Error(`Spotify API ${resp.status}${afterRefresh}: ${body}`);
    }

    log.debug('spotify_api_ok', 'Spotify answered', {
      method,
      path,
      status: resp.status,
      duration_ms: Date.now() - started,
    });
    return resp;
  }

  /** Ignores the body: `POST /me/player/next` answers 200 with a non-JSON token, not 204. */
  private async command(method: string, path: string, body?: unknown): Promise<void> {
    await this.request(method, path, body);
  }

  /** Parses a JSON body; an empty 200 or 204 yields `{}`, while non-JSON text throws. */
  private async api<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
    const resp = await this.request(method, path, body);
    const text = await resp.text();
    if (!text.trim()) return {} as T;
    return JSON.parse(text) as T;
  }


  private parseAlbum(album: SpotifyAlbum): AlbumInfo {
    return {
      id: album.id,
      name: album.name,
      artist: album.artists.map(a => a.name).join(', '),
      total_tracks: album.total_tracks,
      uri: album.uri,
      release_date: album.release_date,
    };
  }

  private parsePlaylist(playlist: SpotifyPlaylist, currentUserId?: string): PlaylistInfo {
    const owner = playlist.owner;
    return {
      id: playlist.id,
      name: playlist.name,
      description: playlist.description || '',
      track_count: playlist.tracks?.total ?? playlist.items?.length ?? 0,
      uri: playlist.uri,
      owner: owner?.display_name || owner?.id || 'unknown',
      is_own: currentUserId !== undefined && owner?.id === currentUserId,
    };
  }

  async play(target?: PlayTarget): Promise<string> {
    const body = buildPlayBody(target);

    await this.command('PUT', '/me/player/play', Object.keys(body).length > 0 ? body : undefined);

    if (!target) return 'Resumed playback';
    const uri = typeof target === 'string' ? target : target.uri;
    return `Playing ${uri}`;
  }

  async pause(): Promise<string> {
    await this.command('PUT', '/me/player/pause');
    return 'Playback paused';
  }

  async next(): Promise<string> {
    await this.command('POST', '/me/player/next');
    return 'Skipped to next track';
  }

  async previous(): Promise<string> {
    await this.command('POST', '/me/player/previous');
    return 'Went to previous track';
  }

  async setVolume(percent: number): Promise<string> {
    const clamped = Math.max(0, Math.min(100, Math.round(percent)));
    await this.command('PUT', `/me/player/volume?volume_percent=${clamped}`);
    return `Volume set to ${clamped}%`;
  }

  async setShuffle(enabled: boolean): Promise<string> {
    await this.command('PUT', `/me/player/shuffle?state=${enabled}`);
    return `Shuffle ${enabled ? 'enabled' : 'disabled'}`;
  }

  async seek(positionMs: number): Promise<string> {
    const clamped = Math.max(0, Math.round(positionMs));
    await this.command('PUT', `/me/player/seek?position_ms=${clamped}`);
    const mins = Math.floor(clamped / 60000);
    const secs = String(Math.floor((clamped % 60000) / 1000)).padStart(2, '0');
    return `Jumped to ${mins}:${secs}`;
  }

  async setRepeat(state: RepeatState): Promise<string> {
    await this.command('PUT', `/me/player/repeat?state=${state}`);
    const label =
      state === 'off'
        ? 'Repeat off'
        : state === 'track'
          ? 'Repeating this track'
          : 'Repeating the album or playlist';
    return label;
  }

  async getDevices(): Promise<DeviceInfo[]> {
    interface DevicesResponse {
      devices: Array<{
        id: string | null;
        name: string;
        type: string;
        is_active: boolean;
        volume_percent?: number;
      }>;
    }

    const data = await this.api<DevicesResponse>('GET', '/me/player/devices');
    return (data.devices || [])
      // Spotify can list devices without an id; those can't be targeted.
      .filter(d => d.id)
      .map(d => ({
        id: d.id as string,
        name: d.name,
        type: d.type,
        is_active: d.is_active,
        volume_percent: d.volume_percent,
      }));
  }

  async transferPlayback(deviceId: string, deviceName: string): Promise<string> {
    // Without `play: true` Spotify may transfer in a paused state.
    await this.command('PUT', '/me/player', { device_ids: [deviceId], play: true });
    return `Playback moved to ${deviceName}`;
  }

  // ── Library and listening history ──────────────────────────
  // These need scopes older sign-ins lack; `request` then asks the user to sign in again.

  async getSavedTracks(limit: number = 20): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface SavedResponse {
      items: Array<{ track: SpotifyTrack | null }>;
    }
    const data = await this.api<SavedResponse>('GET', `/me/tracks?limit=${clamped}`);
    return (data.items || [])
      .map(i => i.track)
      .filter((t): t is SpotifyTrack => !!t)
      .map(t => parseTrack(t));
  }




  async getTopTracks(range: TimeRange, limit: number = 20): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface TopTracks {
      items: SpotifyTrack[];
    }
    const data = await this.api<TopTracks>(
      'GET',
      `/me/top/tracks?time_range=${range}&limit=${clamped}`
    );
    return (data.items || []).map(t => parseTrack(t));
  }

  async getTopArtists(range: TimeRange, limit: number = 20): Promise<ArtistInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface TopArtists {
      items: Array<{ id: string; name: string; genres?: string[] }>;
    }
    const data = await this.api<TopArtists>(
      'GET',
      `/me/top/artists?time_range=${range}&limit=${clamped}`
    );
    return (data.items || []).map(a => ({
      id: a.id,
      name: a.name,
      genres: a.genres || [],
    }));
  }

  async getRecentlyPlayed(limit: number = 20): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface RecentResponse {
      items: Array<{ track: SpotifyTrack | null }>;
    }
    const data = await this.api<RecentResponse>(
      'GET',
      `/me/player/recently-played?limit=${clamped}`
    );
    return (data.items || [])
      .map(i => i.track)
      .filter((t): t is SpotifyTrack => !!t)
      .map(t => parseTrack(t));
  }

  async getNowPlaying(): Promise<TrackInfo | null> {
    interface PlayerState {
      item: SpotifyTrack | null;
      is_playing: boolean;
      progress_ms: number;
      device?: { volume_percent: number };
    }

    // Errors propagate: `null` must mean only "nothing is playing", as the caller reports it.
    const data = await this.api<PlayerState>('GET', '/me/player');
    if (!data || !data.item) return null;

    const track = parseTrack(data.item);
    track.is_playing = data.is_playing;
    track.progress_ms = data.progress_ms;
    track.volume_percent = data.device?.volume_percent;
    return track;
  }

  /** Fetches a track so a bare URI can play inside its album (`/tracks/{id}` is unscoped). */
  async getTrack(uri: string): Promise<TrackInfo | null> {
    const id = uri.startsWith('spotify:track:') ? uri.slice('spotify:track:'.length) : uri;
    if (!id) return null;
    const track = await this.api<SpotifyTrack>('GET', `/tracks/${encodeURIComponent(id)}`);
    // An empty body parses to `{}`, which has no uri to play.
    if (!track || !track.uri) return null;
    return parseTrack(track);
  }

  /** Tops up a short release with the artist's tracks, via `/search` (`top-tracks` is withdrawn). */
  async queueFollowUps(seed: TrackInfo, limit: number = FOLLOW_UP_LIMIT): Promise<FollowUpResult> {
    // `artist` may join several names; search the primary and let `pickFollowUps` filter by id.
    const primaryArtist = seed.artist.split(',')[0].trim();
    let picks: TrackInfo[] = [];
    let source: FollowUpResult['source'] = 'artist';

    if (primaryArtist) {
      const byArtist = await this.searchTracks(`artist:"${primaryArtist}"`, FOLLOW_UP_SEARCH_LIMIT);
      picks = pickFollowUps(seed, byArtist, limit);
    }

    if (picks.length === 0) {
      source = 'listener';
      // Fall back to the listener's top tracks (`user-top-read`, not withdrawn).
      const top = await this.getTopTracks('medium_term', FOLLOW_UP_SEARCH_LIMIT);
      picks = top.filter(t => t.uri !== seed.uri).slice(0, limit);
    }

    let queued = 0;
    for (const track of picks) {
      try {
        await this.addToQueue(track.uri);
        queued += 1;
      } catch (err) {
        // Stop at the first failure: `request()` has no 429 handling and this is the biggest burst.
        log.warn('follow_up_queue_stopped', 'stopped queueing follow-ups', {
          queued,
          remaining: picks.length - queued,
          source,
          error: describeError(err),
        });
        break;
      }
    }

    log.debug('follow_ups_queued', 'queued follow-ups behind a short release', {
      seed: seed.uri,
      queued,
      source,
    });
    return { queued, source };
  }

  /** Appends to the queue; Spotify has no positioned insert or reorder, so no "play next". */
  async addToQueue(uri: string): Promise<string> {
    try {
      await this.command('POST', `/me/player/queue?uri=${encodeURIComponent(uri)}`);
    } catch (err) {
      // Spotify answers 404 when no device is active.
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes('Spotify API 404')) {
        throw new Error('No active Spotify device. Open Spotify on a device first.');
      }
      throw err;
    }
    return 'Added to queue';
  }

  async getQueue(): Promise<TrackInfo[]> {
    interface QueueResponse {
      currently_playing: SpotifyTrack | null;
      queue: SpotifyTrack[];
    }

    const data = await this.api<QueueResponse>('GET', '/me/player/queue');
    const tracks: TrackInfo[] = [];

    if (data.currently_playing) {
      const current = parseTrack(data.currently_playing);
      current.is_playing = true;
      tracks.push(current);
    }

    for (const item of data.queue || []) {
      tracks.push(parseTrack(item));
    }

    return tracks;
  }

  /** "X by Y" → `track:"X" artist:"Y"` (else null); Spotify scores every word, "by" included. */
  private fieldFilteredQuery(query: string): string | null {
    const split = query.match(/^(.*?)\s+by\s+(.*)$/i);
    if (!split) return null;

    const title = split[1].trim();
    // Drop "feat. X": Spotify indexes only the primary artist under `artist:`.
    const artist = split[2]
      .replace(/\s+(feat\.?|ft\.?|featuring|with)\s+.*$/i, '')
      .trim();

    if (!title || !artist) return null;
    // Quote both so multi-word values stay one term.
    return `track:"${title}" artist:"${artist}"`;
  }

  async searchTracks(query: string, limit: number = 10): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));

    interface SearchResponse {
      tracks: { items: SpotifyTrack[] };
    }

    const run = async (q: string) => {
      const data = await this.api<SearchResponse>(
        'GET',
        `/search?type=track&q=${encodeURIComponent(q)}&limit=${clamped}`
      );
      return (data.tracks?.items || []).map(t => parseTrack(t));
    };

    // Precise form first, then the loose query: a misremembered title defeats the filter.
    const filtered = this.fieldFilteredQuery(query);
    if (filtered) {
      const hits = await run(filtered);
      if (hits.length > 0) return hits;
    }

    return run(query);
  }

  async searchAlbums(query: string, limit: number = 10): Promise<AlbumInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    const encoded = encodeURIComponent(query);

    interface SearchResponse {
      albums: { items: SpotifyAlbum[] };
    }

    const data = await this.api<SearchResponse>(
      'GET',
      `/search?type=album&q=${encoded}&limit=${clamped}`
    );

    return (data.albums?.items || []).map(a => this.parseAlbum(a));
  }

  /** Every playlist in the library, followed ones included, paged at Spotify's 50-per-page cap. */
  async getPlaylists(limit: number = 200): Promise<PlaylistInfo[]> {
    interface PlaylistsResponse {
      items: SpotifyPlaylist[];
      total: number;
    }

    const wanted = Math.max(1, limit);
    const out: PlaylistInfo[] = [];
    let offset = 0;

    // Resolve the owner once so each playlist can be marked as theirs or not.
    const me = await this.api<{ id: string }>('GET', '/me');

    while (out.length < wanted) {
      const page = await this.api<PlaylistsResponse>(
        'GET',
        `/me/playlists?limit=50&offset=${offset}`
      );
      const items = (page.items || []).filter(Boolean);
      out.push(...items.map(p => this.parsePlaylist(p, me.id)));

      offset += 50;
      if (items.length === 0 || offset >= (page.total ?? 0)) break;
    }

    return out.slice(0, wanted);
  }



}
