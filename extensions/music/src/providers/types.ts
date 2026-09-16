export interface TrackInfo {
  id: string;
  name: string;
  artist: string;
  album: string;
  duration_ms: number;
  uri: string;
  is_playing?: boolean;
  progress_ms?: number;
  volume_percent?: number;
  /**
   * The album's own URI, when the source knew it.
   *
   * Load-bearing rather than decorative: playing a track *inside* this context
   * is what stops Spotify falling silent when the track ends. A bare track URI
   * goes out as a one-element `uris` list, which Spotify plays and then stops.
   * Optional because `GET /me/player` and the queue endpoint return a thinner
   * track object than search does.
   */
  album_uri?: string;
  /** Tracks on the album, for deciding whether it is long enough to continue. */
  album_total_tracks?: number;
  /** `album`, `single` or `compilation`. A single needs topping up. */
  album_type?: string;
  /** Artist ids, for telling this artist apart from a same-named cover. */
  artist_ids?: string[];
}

/**
 * What `play` was asked to start.
 *
 * A plain string is a context URI (album, playlist, artist) or a bare track
 * URI, kept for the callers that already pass one. A `TrackInfo` is the richer
 * form: it carries the album, so the track can play inside it.
 */
export type PlayTarget = string | TrackInfo;

/**
 * What a top-up actually managed to queue.
 *
 * `source` is not bookkeeping: `artist` means more by the same artist, while
 * `listener` means the artist search came back empty and the fallback used the
 * listener's own favourites instead. Reporting "5 more by Bensoul" when the
 * queue holds five unrelated songs would be a lie, so the caller needs to know
 * which happened.
 */
export interface FollowUpResult {
  queued: number;
  source: 'artist' | 'listener';
}

/** An artist, as returned by the taste endpoints. */
export interface ArtistInfo {
  id: string;
  name: string;
  genres: string[];
}

/** How far back the taste endpoints look. */
export type TimeRange = "short_term" | "medium_term" | "long_term";

/** Spotify's repeat modes: off, repeat one track, repeat the whole context. */
export type RepeatState = "off" | "track" | "context";

/** A device Spotify can play on — phone, computer, speaker. */
export interface DeviceInfo {
  id: string;
  name: string;
  /** Spotify's own label: "Computer", "Smartphone", "Speaker", "TV"... */
  type: string;
  is_active: boolean;
  volume_percent?: number;
}

export interface PlaylistInfo {
  id: string;
  name: string;
  description: string;
  track_count: number;
  uri: string;
  /** Display name of whoever created it. */
  owner: string;
  /** True when the signed-in user created it, false when they only follow it. */
  is_own: boolean;
}

export interface MusicProvider {
  name: string;
  /**
   * Starts playback, replacing whatever is playing.
   *
   * Pass a `TrackInfo` rather than its `uri` wherever one is to hand: a track
   * played with its album context keeps going afterwards, a bare URI does not.
   */
  play(target?: PlayTarget): Promise<string>;
  pause(): Promise<string>;
  next(): Promise<string>;
  previous(): Promise<string>;
  setVolume(percent: number): Promise<string>;
  setShuffle(enabled: boolean): Promise<string>;
  seek(positionMs: number): Promise<string>;
  setRepeat(state: RepeatState): Promise<string>;
  getDevices(): Promise<DeviceInfo[]>;
  transferPlayback(deviceId: string, deviceName: string): Promise<string>;
  getSavedTracks(limit?: number): Promise<TrackInfo[]>;
  getTopTracks(range: TimeRange, limit?: number): Promise<TrackInfo[]>;
  getTopArtists(range: TimeRange, limit?: number): Promise<ArtistInfo[]>;
  getRecentlyPlayed(limit?: number): Promise<TrackInfo[]>;
  getNowPlaying(): Promise<TrackInfo | null>;
  getQueue(): Promise<TrackInfo[]>;
  /** Appends to the queue without disturbing what is currently playing. */
  addToQueue(uri: string): Promise<string>;
  /**
   * Queues more of the same artist behind a short release, returning how many
   * were queued. A single played inside its album still runs out after one
   * song; this is what keeps it going.
   */
  queueFollowUps(seed: TrackInfo, limit?: number): Promise<FollowUpResult>;
  /** One track by URI, so a bare URI can carry its album context too. */
  getTrack(uri: string): Promise<TrackInfo | null>;
  searchTracks(query: string, limit?: number): Promise<TrackInfo[]>;
  searchAlbums(query: string, limit?: number): Promise<AlbumInfo[]>;
  getPlaylists(limit?: number): Promise<PlaylistInfo[]>;
}

export interface AlbumInfo {
  id: string;
  name: string;
  artist: string;
  total_tracks: number;
  uri: string;
  release_date: string;
}
