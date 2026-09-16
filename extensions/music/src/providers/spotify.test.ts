import { test } from 'node:test';
import assert from 'node:assert/strict';

import { buildPlayBody, isShortRelease, parseTrack, pickFollowUps } from './spotify.js';
import type { TrackInfo } from './types.js';

/**
 * The pure decisions behind "play a song and keep playing".
 *
 * Only the shaping is tested, deliberately: `request()` is I/O and a mocked
 * `api.spotify.com` proves little, whereas the body sent to
 * `PUT /me/player/play` is the entire bug. This mirrors the pattern the Rust
 * side already uses for the now-playing snapshot — pure functions pinned with
 * plain fixtures.
 */

/** A search hit, which is where the album context actually comes from. */
function track(over: Partial<TrackInfo> = {}): TrackInfo {
  return {
    id: '4uLU6hMCjMI75M1A2tKUQC',
    name: 'Nairobi',
    artist: 'Bensoul',
    album: 'Qwarantunes',
    duration_ms: 210_000,
    uri: 'spotify:track:4uLU6hMCjMI75M1A2tKUQC',
    album_uri: 'spotify:album:1DFixLWuPkv3KT3TnV35m3',
    album_total_tracks: 9,
    album_type: 'album',
    artist_ids: ['7HUEZmpJPTHCbLdF9Wo0Cd'],
    ...over,
  };
}

// ── buildPlayBody ────────────────────────────────────────────────────────────

test('a track plays inside its album', () => {
  const body = buildPlayBody(track());

  assert.deepEqual(body, {
    context_uri: 'spotify:album:1DFixLWuPkv3KT3TnV35m3',
    offset: { uri: 'spotify:track:4uLU6hMCjMI75M1A2tKUQC' },
  });
  // The regression itself: a `uris` list is what Spotify stops after, so its
  // absence is the fix.
  assert.equal('uris' in body, false, 'a uris list would stop after one song');
});

test('a track without an album falls back to uris', () => {
  // The player and queue endpoints return a thinner track object. Losing the
  // continuation is acceptable there; losing the music is not.
  const body = buildPlayBody(track({ album_uri: undefined }));
  assert.deepEqual(body, { uris: ['spotify:track:4uLU6hMCjMI75M1A2tKUQC'] });
});

test('an album or playlist uri is unchanged', () => {
  // These two already worked before the fix. Pinned so it cannot regress them.
  assert.deepEqual(buildPlayBody('spotify:album:1DFixLWuPkv3KT3TnV35m3'), {
    context_uri: 'spotify:album:1DFixLWuPkv3KT3TnV35m3',
  });
  assert.deepEqual(buildPlayBody('spotify:playlist:37i9dQZF1DXcBWIGoYBM5M'), {
    context_uri: 'spotify:playlist:37i9dQZF1DXcBWIGoYBM5M',
  });
});

test('a bare track uri string still goes out as uris', () => {
  // No album is known from a string alone. The caller resolves it first where
  // it can; this is the honest fallback when it cannot.
  assert.deepEqual(buildPlayBody('spotify:track:4uLU6hMCjMI75M1A2tKUQC'), {
    uris: ['spotify:track:4uLU6hMCjMI75M1A2tKUQC'],
  });
});

test('no target means resume, with no body at all', () => {
  assert.deepEqual(buildPlayBody(), {});
  assert.deepEqual(buildPlayBody(undefined), {});
});

test('an offset is never sent with an artist context', () => {
  // Spotify only accepts `offset` for an album or playlist context. This is
  // the constraint that rules out "play this song within the artist" and makes
  // the album the only usable context, so it is worth pinning rather than
  // trusting to memory.
  const body = buildPlayBody('spotify:artist:7HUEZmpJPTHCbLdF9Wo0Cd');
  assert.deepEqual(body, { context_uri: 'spotify:artist:7HUEZmpJPTHCbLdF9Wo0Cd' });
  assert.equal('offset' in body, false);
});

// ── isShortRelease ───────────────────────────────────────────────────────────

test('a single is recognised as short', () => {
  assert.equal(isShortRelease(track({ album_type: 'single', album_total_tracks: 1 })), true);
});

test('a two-track release is short even when typed as an album', () => {
  // Compilations and two-track releases are typed `album` yet run out just as
  // fast, which is why the track count is checked as well as the type.
  assert.equal(isShortRelease(track({ album_type: 'album', album_total_tracks: 2 })), true);
});

test('a full album is not short', () => {
  assert.equal(isShortRelease(track({ album_type: 'album', album_total_tracks: 9 })), false);
});

test('an unknown track count is not treated as short', () => {
  // Guessing "short" here would queue follow-ups behind every track the player
  // endpoints report, which is the wrong default: it would append to a real
  // album the user is happily listening through.
  assert.equal(isShortRelease(track({ album_type: undefined, album_total_tracks: undefined })), false);
});

// ── pickFollowUps ────────────────────────────────────────────────────────────

test('follow-ups exclude the seed track and other artists', () => {
  const seed = track();
  const candidates = [
    // The seed itself, returned by its own artist search.
    track(),
    // A cover by somebody else — the case the artist-id filter exists for.
    track({ id: 'cover', uri: 'spotify:track:cover', name: 'Nairobi', artist: 'Someone Else', artist_ids: ['other'] }),
    track({ id: 'b', uri: 'spotify:track:b', name: 'Lucy', artist_ids: ['7HUEZmpJPTHCbLdF9Wo0Cd'] }),
    track({ id: 'c', uri: 'spotify:track:c', name: 'Peddi', artist_ids: ['7HUEZmpJPTHCbLdF9Wo0Cd'] }),
  ];

  const picks = pickFollowUps(seed, candidates, 10);

  assert.deepEqual(picks.map(t => t.name), ['Lucy', 'Peddi']);
});

test('follow-ups honour the limit', () => {
  const seed = track();
  const candidates = Array.from({ length: 20 }, (_, i) =>
    track({ id: `t${i}`, uri: `spotify:track:t${i}`, name: `Song ${i}` }),
  );

  assert.equal(pickFollowUps(seed, candidates, 3).length, 3);
});

test('the same recording is not queued twice', () => {
  // An artist search returns the single and the album cut of one song. Queuing
  // both would play it back to back.
  const seed = track();
  const candidates = [
    track({ id: 'x1', uri: 'spotify:track:x1', name: 'Lucy', album_type: 'single' }),
    track({ id: 'x2', uri: 'spotify:track:x2', name: 'Lucy', album_type: 'album' }),
  ];

  assert.deepEqual(pickFollowUps(seed, candidates, 10).map(t => t.uri), ['spotify:track:x1']);
});

test('the requested track is not queued behind itself', () => {
  // The case that actually reaches queueFollowUps: the seed is a single, so the
  // artist search also returns the album cut of the same song under a different
  // id. Deduplicating candidates against each other is not enough - the seed
  // has to be in the set too, or the song just asked for plays twice in a row.
  const seed = track({ album_type: 'single', album_total_tracks: 1 });
  const candidates = [
    track({ id: 'alb', uri: 'spotify:track:alb', name: 'Nairobi', album_type: 'album' }),
    track({ id: 'oth', uri: 'spotify:track:oth', name: 'Extravaganza' }),
  ];

  assert.deepEqual(pickFollowUps(seed, candidates, 10).map(t => t.name), ['Extravaganza']);
});

test('without artist ids the artist name is the fallback', () => {
  // The player endpoints omit artist ids, so a seed taken from there has none.
  // Matching on the name is weaker but better than queueing nothing.
  const seed = track({ artist_ids: undefined });
  const candidates = [
    track({ id: 'b', uri: 'spotify:track:b', name: 'Lucy', artist: 'Bensoul', artist_ids: undefined }),
    track({ id: 'c', uri: 'spotify:track:c', name: 'Other', artist: 'Nobody', artist_ids: undefined }),
  ];

  assert.deepEqual(pickFollowUps(seed, candidates, 10).map(t => t.name), ['Lucy']);
});

// ── parseTrack ───────────────────────────────────────────────────────────────

test('parse_track keeps the album context', () => {
  // The guard on the original cause. Every buildPlayBody test above would
  // still pass if this mapping went back to dropping the album, and playback
  // would go silent again — the album fields arrive on the wire and were
  // simply thrown away. So assert on the mapping itself.
  const wire = {
    id: '4uLU6hMCjMI75M1A2tKUQC',
    name: 'Nairobi',
    artists: [{ id: '7HUEZmpJPTHCbLdF9Wo0Cd', name: 'Bensoul' }],
    album: {
      name: 'Qwarantunes',
      id: '1DFixLWuPkv3KT3TnV35m3',
      uri: 'spotify:album:1DFixLWuPkv3KT3TnV35m3',
      total_tracks: 9,
      album_type: 'album',
    },
    duration_ms: 210_000,
    uri: 'spotify:track:4uLU6hMCjMI75M1A2tKUQC',
  };

  const parsed = parseTrack(wire);

  assert.equal(parsed.album_uri, 'spotify:album:1DFixLWuPkv3KT3TnV35m3');
  assert.equal(parsed.album_total_tracks, 9);
  assert.equal(parsed.album_type, 'album');
  assert.deepEqual(parsed.artist_ids, ['7HUEZmpJPTHCbLdF9Wo0Cd']);
  // And the fields that were always there still are.
  assert.equal(parsed.name, 'Nairobi');
  assert.equal(parsed.artist, 'Bensoul');
  assert.equal(parsed.album, 'Qwarantunes');
});

test('parse_track survives the thinner player track object', () => {
  // GET /me/player and the queue endpoint omit the album uri and artist ids.
  // Those must come back undefined rather than throwing, because the same
  // mapping serves both shapes.
  const parsed = parseTrack({
    id: 'x',
    name: 'Something',
    artists: [{ name: 'Someone' }],
    album: { name: 'Some Album' },
    duration_ms: 1000,
    uri: 'spotify:track:x',
  });

  assert.equal(parsed.album_uri, undefined);
  assert.equal(parsed.album_total_tracks, undefined);
  assert.deepEqual(parsed.artist_ids, []);
  assert.equal(parsed.album, 'Some Album');
  // And such a track must not be mistaken for a single needing a top-up.
  assert.equal(isShortRelease(parsed), false);
});
