import { test } from 'node:test';
import assert from 'node:assert/strict';

import { buildPlayBody, isShortRelease, parseTrack, pickFollowUps } from './spotify.js';
import type { TrackInfo } from './types.js';

/** Pure play-request shaping only; a mocked api.spotify.com would prove little. */

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
  assert.equal('uris' in body, false, 'a uris list would stop after one song');
});

test('a track without an album falls back to uris', () => {
  // Player/queue endpoints omit the album; playing without continuation beats not playing.
  const body = buildPlayBody(track({ album_uri: undefined }));
  assert.deepEqual(body, { uris: ['spotify:track:4uLU6hMCjMI75M1A2tKUQC'] });
});

test('an album or playlist uri is unchanged', () => {
  assert.deepEqual(buildPlayBody('spotify:album:1DFixLWuPkv3KT3TnV35m3'), {
    context_uri: 'spotify:album:1DFixLWuPkv3KT3TnV35m3',
  });
  assert.deepEqual(buildPlayBody('spotify:playlist:37i9dQZF1DXcBWIGoYBM5M'), {
    context_uri: 'spotify:playlist:37i9dQZF1DXcBWIGoYBM5M',
  });
});

test('a bare track uri string still goes out as uris', () => {
  // A bare string carries no album; callers resolve one first when they can.
  assert.deepEqual(buildPlayBody('spotify:track:4uLU6hMCjMI75M1A2tKUQC'), {
    uris: ['spotify:track:4uLU6hMCjMI75M1A2tKUQC'],
  });
});

test('no target means resume, with no body at all', () => {
  assert.deepEqual(buildPlayBody(), {});
  assert.deepEqual(buildPlayBody(undefined), {});
});

test('an offset is never sent with an artist context', () => {
  // Spotify accepts `offset` only for album or playlist contexts.
  const body = buildPlayBody('spotify:artist:7HUEZmpJPTHCbLdF9Wo0Cd');
  assert.deepEqual(body, { context_uri: 'spotify:artist:7HUEZmpJPTHCbLdF9Wo0Cd' });
  assert.equal('offset' in body, false);
});

// ── isShortRelease ───────────────────────────────────────────────────────────

test('a single is recognised as short', () => {
  assert.equal(isShortRelease(track({ album_type: 'single', album_total_tracks: 1 })), true);
});

test('a two-track release is short even when typed as an album', () => {
  assert.equal(isShortRelease(track({ album_type: 'album', album_total_tracks: 2 })), true);
});

test('a full album is not short', () => {
  assert.equal(isShortRelease(track({ album_type: 'album', album_total_tracks: 9 })), false);
});

test('an unknown track count is not treated as short', () => {
  // Defaulting to "short" would append follow-ups to real albums from the player endpoints.
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
  // Artist search returns both the single and the album cut of a song.
  const seed = track();
  const candidates = [
    track({ id: 'x1', uri: 'spotify:track:x1', name: 'Lucy', album_type: 'single' }),
    track({ id: 'x2', uri: 'spotify:track:x2', name: 'Lucy', album_type: 'album' }),
  ];

  assert.deepEqual(pickFollowUps(seed, candidates, 10).map(t => t.uri), ['spotify:track:x1']);
});

test('the requested track is not queued behind itself', () => {
  // A single seed's album cut has a different id, so dedup must include the seed itself.
  const seed = track({ album_type: 'single', album_total_tracks: 1 });
  const candidates = [
    track({ id: 'alb', uri: 'spotify:track:alb', name: 'Nairobi', album_type: 'album' }),
    track({ id: 'oth', uri: 'spotify:track:oth', name: 'Extravaganza' }),
  ];

  assert.deepEqual(pickFollowUps(seed, candidates, 10).map(t => t.name), ['Extravaganza']);
});

test('without artist ids the artist name is the fallback', () => {
  // Player endpoints omit artist ids; name matching is weaker but beats queueing nothing.
  const seed = track({ artist_ids: undefined });
  const candidates = [
    track({ id: 'b', uri: 'spotify:track:b', name: 'Lucy', artist: 'Bensoul', artist_ids: undefined }),
    track({ id: 'c', uri: 'spotify:track:c', name: 'Other', artist: 'Nobody', artist_ids: undefined }),
  ];

  assert.deepEqual(pickFollowUps(seed, candidates, 10).map(t => t.name), ['Lucy']);
});

// ── parseTrack ───────────────────────────────────────────────────────────────

test('parse_track keeps the album context', () => {
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
  assert.equal(parsed.name, 'Nairobi');
  assert.equal(parsed.artist, 'Bensoul');
  assert.equal(parsed.album, 'Qwarantunes');
});

test('parse_track survives the thinner player track object', () => {
  // GET /me/player and the queue endpoint omit the album uri and artist ids.
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
  assert.equal(isShortRelease(parsed), false);
});
