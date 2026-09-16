import type { MusicProvider, TrackInfo, PlaylistInfo, AlbumInfo, DeviceInfo, RepeatState, ArtistInfo, TimeRange, PlayTarget, FollowUpResult } from './types.js';
import { describeError, log } from '../log.js';

interface SpotifyTrack {
  id: string;
  name: string;
  /**
   * `id` matters as much as `name`: it is the only reliable way to tell this
   * artist's tracks from covers and same-titled songs when following up a
   * search by artist name.
   */
  artists: Array<{ id?: string; name: string }>;
  /**
   * Everything but `name` was previously declared away, which is how the bug
   * happened: the fields arrive on every search response, but a narrowed type
   * made them invisible and `parseTrack` dropped them. `uri` is what lets a
   * track play inside its album instead of alone.
   *
   * All optional because `GET /me/player` and `GET /me/player/queue` return a
   * thinner track object than `/search` does.
   */
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
 * The body for `PUT /v1/me/player/play`.
 *
 * Pure, and exported, so the one decision that caused the "Spotify goes
 * silent" bug can be pinned by tests without a fake Spotify. The rest of this
 * file is I/O; this is the part worth asserting on.
 *
 * Spotify's play endpoint takes **either** shape, never both:
 *
 * - `uris` — an ad-hoc list. Spotify plays exactly those tracks and then
 *   STOPS. A one-element list is a playlist of one song, which is why asking
 *   for a single track used to end in silence with nothing left in the queue.
 * - `context_uri` — an album, playlist or artist. Playback runs through the
 *   context and, on Premium, Spotify's own autoplay carries on past the end.
 *
 * So a track is played *inside* its album, positioned with `offset`.
 *
 * The constraint that shapes all of this: **`offset` is only valid when the
 * context is an album or a playlist.** Spotify rejects it for an artist
 * context, so "play this song within the artist" cannot be expressed — the
 * album is the only context that both starts on the requested track and
 * continues afterwards. `an_offset_is_never_sent_with_an_artist_context` pins
 * that.
 *
 * A track whose album is unknown still falls back to `uris`. That is the old
 * behaviour, kept deliberately: a missing field should cost the continuation,
 * not the music.
 */
export function buildPlayBody(target?: PlayTarget): Record<string, unknown> {
  if (!target) return {};

  if (typeof target === 'string') {
    // A bare track URI has no album to play inside; anything else already is
    // a context.
    return target.startsWith('spotify:track:')
      ? { uris: [target] }
      : { context_uri: target };
  }

  if (target.album_uri) {
    return { context_uri: target.album_uri, offset: { uri: target.uri } };
  }
  return { uris: [target.uri] };
}

/**
 * A Spotify track object mapped to our own shape.
 *
 * Exported and module-level because this mapping is where the "Spotify goes
 * silent" bug actually lived: the album fields arrive on every search
 * response, but a narrowed type hid them and this function dropped them, so
 * the album context never reached the point where playback was started.
 * Keeping it testable is the guard against that happening again.
 */
export function parseTrack(track: SpotifyTrack): TrackInfo {
    return {
      id: track.id,
      name: track.name,
      artist: track.artists.map(a => a.name).join(', '),
      album: track.album.name,
      duration_ms: track.duration_ms,
      uri: track.uri,
      // The album context, carried rather than dropped. `album_uri` is what
      // `buildPlayBody` needs to keep playback going after the requested
      // track; the other three decide whether a release is too short to be
      // worth continuing. None costs an extra request -- they are already in
      // the response we just parsed.
      album_uri: track.album.uri,
      album_total_tracks: track.album.total_tracks,
      album_type: track.album.album_type,
      artist_ids: track.artists.map(a => a.id).filter((id): id is string => !!id),
    };
}

/**
 * Whether a release is too short for "the rest of the album" to mean anything.
 *
 * A single is the case that defeats an album context: playing track 1 of 1 and
 * continuing through the album still leaves silence one song later. Both
 * signals come free with the search response, so asking costs no request.
 *
 * `total_tracks` is checked as well as `album_type` because compilations and
 * two-track releases are typed `album` yet run out just as fast, and because
 * `album_type` is absent from the thinner track object the player endpoints
 * return.
 */
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

/**
 * Follow-up tracks to queue behind a short release, newest search first.
 *
 * Pure so the filtering can be tested: it is the part that goes wrong. Keeps
 * only tracks that genuinely share an artist id with the seed, which is what
 * stops covers, tributes and same-titled songs by other artists from being
 * queued as though they were the artist's own work. Falls back to matching on
 * the artist *name* only when the seed carried no ids, since the player
 * endpoints omit them.
 */
export function pickFollowUps(
  seed: TrackInfo,
  candidates: TrackInfo[],
  limit: number,
): TrackInfo[] {
  const seedIds = new Set(seed.artist_ids ?? []);
  // Seeded with the requested track: a single and its album cut are the same
  // recording under two ids, and the single is precisely the case that reaches
  // this function, so without the seed in here the song the user asked for gets
  // queued behind itself and plays twice.
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
 * Endpoints Spotify withdrew from apps created after 2024-11-27, which includes
 * GIAP's. Verified against a live token — these are not a scope problem and
 * asking for more permissions will not bring them back:
 *
 *   GET /recommendations                     404
 *   GET /recommendations/available-genre-seeds  404
 *   GET /audio-features/{id}                 403
 *   GET /audio-analysis/{id}                 403
 *   GET /artists/{id}/related-artists        403
 *   GET /artists/{id}/top-tracks             403
 *   GET /browse/featured-playlists           403
 *   GET /browse/new-releases                 403
 *   GET /me/tracks/contains                  403
 *   PUT / DELETE /me/tracks                  403  (library writes, even
 *                                                 with user-library-modify)
 *   track.preview_url                        always null
 *
 * So there is no "play me something like this", no mood or tempo matching, and
 * no 30-second previews. Do not build features that depend on them.
 */
export class SpotifyProvider implements MusicProvider {
  name = 'Spotify';
  private baseUrl = 'https://api.spotify.com/v1';

  /** Current access token — initialized from env, updated on refresh. */
  private accessToken: string | null = process.env.SPOTIFY_ACCESS_TOKEN ?? null;

  private get token(): string {
    if (!this.accessToken) {
      // The one failure a user can fix in ten seconds, and the one that looked
      // exactly like a broken extension when nothing reported it.
      log.warn('no_token', 'no Spotify token — the extension has never been signed in', {
        hint: 'sign in to Spotify from the Extensions tab',
      });
      throw new Error('SPOTIFY_ACCESS_TOKEN not set. Sign in via GIAP Extensions.');
    }
    return this.accessToken;
  }

  /** GIAP server URL for OAuth refresh requests. */
  private readonly giapUrl = process.env.GIAP_SERVER_URL || 'http://127.0.0.1:4000';

  /**
   * Ask GIAP to refresh the Spotify token, then update the in-memory
   * token from the response so we can retry without a process restart.
   */
  private async refreshToken(): Promise<boolean> {
    // Every arm below says which one it was. This function used to end in
    // `catch { /* GIAP may be unreachable */ }` and three silent `return false`s,
    // so an expired token, a GIAP that could not be reached, a rejected refresh
    // and a malformed response were one symptom: music stopped working and
    // nothing anywhere said why. They need different fixes from the user.
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
          // 401 here is GIAP's own internal token, not Spotify's — a different
          // fault entirely from the one that sent us here.
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

  /**
   * Issues a request, refreshing the token and retrying once on a 401.
   *
   * Returns the raw `Response` and reads nothing from it — whether there is a
   * body, and what it means, is the caller's business.
   */
  private async request(method: string, path: string, body?: unknown): Promise<Response> {
    // Re-read the `token` getter on each attempt: a refresh replaces the token
    // in place, and the retry has to send the new one.
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
        // The refresh path has already said which way it failed; this is the
        // consequence, and the sentence the user reads.
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
        // Bounded: an error body can be long, and the first line carries the
        // reason. Redaction happens in the logger.
        body: body.slice(0, 300),
        duration_ms: Date.now() - started,
      });

      // A scope the token was never granted. Distinct from the withdrawn
      // endpoints above, which answer 403 with a bare "Forbidden" and stay
      // broken however many times the user signs in — this one is fixed by
      // re-authorising, so say that instead of surfacing a raw 403.
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

  /**
   * Issues a request whose response body is of no interest.
   *
   * The player-control endpoints are documented to answer 204, but Spotify
   * actually answers `POST /me/player/next` with a 200 that carries no
   * content-type and a 27-byte opaque token. Parsing that as JSON is what made
   * every skip fail with `Unexpected token ... is not valid JSON` — on a body
   * no caller has ever read.
   */
  private async command(method: string, path: string, body?: unknown): Promise<void> {
    await this.request(method, path, body);
  }

  /**
   * Issues a request and parses a JSON body, tolerating a bodyless success.
   *
   * Reads the body as text first: `Response.json()` throws on an empty body,
   * and an empty 200 or a 204 is a legitimate answer to several of these calls.
   * A non-empty body that is not JSON is still an error worth surfacing.
   */
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
      // `tracks` is not always present. /me/playlists returns some entries with
      // an `items` array and no `tracks` object at all, so reading
      // `playlist.tracks.total` outright throws on a perfectly ordinary account.
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
      // A device with no id cannot be targeted for transfer, so it is not worth
      // offering as somewhere to send playback.
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
    // `play: true` keeps it playing across the move; without it Spotify can
    // hand the device the track in a paused state, which reads as a failure.
    await this.command('PUT', '/me/player', { device_ids: [deviceId], play: true });
    return `Playback moved to ${deviceName}`;
  }

  // ── Library and listening history ──────────────────────────
  // All of these need scopes added after the extension first shipped, so on an
  // install that has not re-authorised they fail with the re-sign-in message
  // from `request`, not a bare 403.

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

    // Errors deliberately propagate. `null` here means one thing only —
    // Spotify answered, and nothing is playing — because that is exactly how
    // the caller reports it ("Nothing is currently playing on Spotify"). A
    // `catch` returning null made an expired token, a failed refresh and an
    // unreachable Spotify all indistinguishable from an idle player, which is
    // the most misleading answer available.
    const data = await this.api<PlayerState>('GET', '/me/player');
    if (!data || !data.item) return null;

    const track = parseTrack(data.item);
    track.is_playing = data.is_playing;
    track.progress_ms = data.progress_ms;
    track.volume_percent = data.device?.volume_percent;
    return track;
  }

  /**
   * Looks up one track, so a bare URI can be played inside its album too.
   *
   * `GET /tracks/{id}` is a catalog read: unscoped, and not one of the
   * endpoints Spotify withdrew. Only used on the URI-given path, where there
   * is no search response to take the album from — the common path already has
   * it and spends no request here.
   */
  async getTrack(uri: string): Promise<TrackInfo | null> {
    const id = uri.startsWith('spotify:track:') ? uri.slice('spotify:track:'.length) : uri;
    if (!id) return null;
    const track = await this.api<SpotifyTrack>('GET', `/tracks/${encodeURIComponent(id)}`);
    // An empty body parses to `{}`, which has no uri to play.
    if (!track || !track.uri) return null;
    return parseTrack(track);
  }

  /**
   * Queues more of the same artist behind a short release.
   *
   * An album context is enough for an album, but not for a single: playing
   * track 1 of 1 and running to the end of the album still leaves silence one
   * song later. This fills that gap.
   *
   * Deliberately NOT `GET /artists/{id}/top-tracks`, which would be the
   * obvious source — see the withdrawn-endpoint list above. It answers 403 for
   * this app, permanently, and no amount of re-consenting changes that. Plain
   * `/search` is unscoped and unaffected, so the artist's catalogue is reached
   * with a field-filtered query instead. `fieldFilteredQuery` only rewrites
   * "title by artist", so an `artist:"…"` filter passes through to Spotify
   * untouched.
   *
   * Returns how many tracks were queued, so the caller can say so — or say
   * nothing, if none were.
   */
  async queueFollowUps(seed: TrackInfo, limit: number = FOLLOW_UP_LIMIT): Promise<FollowUpResult> {
    // The joined `artist` string can hold several names; Spotify indexes the
    // primary one, and the id filter in `pickFollowUps` does the real work of
    // rejecting wrong matches.
    const primaryArtist = seed.artist.split(',')[0].trim();
    let picks: TrackInfo[] = [];
    let source: FollowUpResult['source'] = 'artist';

    if (primaryArtist) {
      const byArtist = await this.searchTracks(`artist:"${primaryArtist}"`, FOLLOW_UP_SEARCH_LIMIT);
      picks = pickFollowUps(seed, byArtist, limit);
    }

    if (picks.length === 0) {
      source = 'listener';
      // Nothing found for the artist -- fall back to what this listener
      // actually likes. `/me/top/tracks` is granted (`user-top-read`) and is
      // not one of the withdrawn endpoints.
      const top = await this.getTopTracks('medium_term', FOLLOW_UP_SEARCH_LIMIT);
      picks = top.filter(t => t.uri !== seed.uri).slice(0, limit);
    }

    let queued = 0;
    for (const track of picks) {
      try {
        await this.addToQueue(track.uri);
        queued += 1;
      } catch (err) {
        // Stop at the first refusal rather than hammering. `request()` has no
        // 429 handling and no Retry-After respect, and this loop is the
        // largest burst this extension makes, so a rate limit or a device
        // going away must end the loop, not repeat into it.
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

  /**
   * Appends a track to the queue, leaving current playback untouched.
   *
   * This is the only insert Spotify offers: the endpoint takes a `uri` and an
   * optional `device_id` but no position, and there is no reorder endpoint, so
   * "play next" cannot be built on it. Do not let a caller imply otherwise.
   */
  async addToQueue(uri: string): Promise<string> {
    try {
      await this.command('POST', `/me/player/queue?uri=${encodeURIComponent(uri)}`);
    } catch (err) {
      // Spotify answers 404 when no device is active, which reads as "not
      // found" but means "nothing is open to queue onto" — the common case.
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

  /**
   * Turns "Nairobi by Bensoul" into `track:"Nairobi" artist:"Bensoul"`.
   *
   * Spotify's search has no notion of natural language: every word in `q` is
   * matched as a term, so "by" and a featured artist are scored as if the user
   * had asked for them. "nairobi by bensoul" returns Extravaganza by Sauti Sol;
   * "Intro by quality control ft gucci mane" returns Easy by Nicki Minaj. The
   * field-filtered form returns the right track first in both cases.
   *
   * Returns `null` when the query has no "by", leaving it to be sent as-is.
   */
  private fieldFilteredQuery(query: string): string | null {
    const split = query.match(/^(.*?)\s+by\s+(.*)$/i);
    if (!split) return null;

    const title = split[1].trim();
    // Drop a featured-artist tail: the primary artist is what Spotify indexes
    // under artist:, and the guest usually appears in the track title anyway.
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

    // Try the precise form first, but never let it lose results: a strict
    // filter finds nothing when the user misremembers a title, and the loose
    // query still would.
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

  /**
   * Every playlist in the user's library, followed ones included.
   *
   * Spotify caps a page at 50, so a library larger than that has to be paged
   * through: asking for one page silently hid 19 of this account's 69, which
   * meant "which playlists do I have" was wrong and a playlist past the first
   * page could never be found by name.
   */
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
