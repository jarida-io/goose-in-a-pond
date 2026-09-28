import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  claimsEditorialOwner,
  contentWords,
  normalizeName,
  playlistMatchScore,
  splitOwnerHint,
} from './match.js';

/**
 * Whose playlist was meant. Asked for one Spotify owns, the extension used to
 * match the user's library and play one of theirs — answering the wrong
 * person's request looks identical to answering it.
 */

const LIBRARY = [{ owner: 'Emmanuel' }, { owner: 'Arlene' }, { owner: 'Spotify' }];

test('the possessive names Spotify as the owner', () => {
  const hint = splitOwnerHint("spotify's Today's Top Hits", LIBRARY);
  assert.equal(hint.editorial, true);
  assert.equal(hint.name, "Today's Top Hits");
});

test('a trailing "by spotify" names Spotify as the owner', () => {
  const hint = splitOwnerHint("Today's Top Hits by spotify", LIBRARY);
  assert.equal(hint.editorial, true);
  assert.equal(hint.name, "Today's Top Hits");
});

/** Believed even when Spotify owns nothing here — otherwise it falls back to a silent match. */
test('Spotify is an owner even when absent from the library', () => {
  assert.equal(splitOwnerHint("spotify's Discover Weekly", [{ owner: 'Emmanuel' }]).editorial, true);
});

test('a real person is only an owner when they own something here', () => {
  const known = splitOwnerHint('RnB by Arlene', LIBRARY);
  assert.equal(known.owner, 'Arlene');
  assert.equal(known.editorial, false);
  assert.equal(known.name, 'RnB');

  // "Songs by Sauti Sol" is a playlist NAME; reading it as an owner loses it.
  const stranger = splitOwnerHint('Songs by Sauti Sol', LIBRARY);
  assert.equal(stranger.owner, null);
  assert.equal(stranger.name, 'Songs by Sauti Sol');
});

/** "The Spotify playlist Randoms" names the platform, not the owner. */
test('a bare "spotify" is the platform, not the owner', () => {
  const hint = splitOwnerHint('the spotify playlist Randoms', LIBRARY);
  assert.equal(hint.editorial, false);
  assert.equal(contentWords(normalizeName('the spotify playlist Randoms')), 'randoms');
});

/**
 * Routing. "play daily mix 1 by spotify" has no word "playlist", so it reached
 * the track search, which rewrote it to track:"daily mix 1" artist:"spotify",
 * found nothing, and played an unrelated Tory Lanez track reporting success.
 */
test('naming Spotify as owner routes to the playlist path', () => {
  assert.equal(claimsEditorialOwner('daily mix 1 by spotify'), true);
  assert.equal(claimsEditorialOwner("spotify's Discover Weekly"), true);
  assert.equal(claimsEditorialOwner("Marvin's Room by Drake"), false);
  assert.equal(claimsEditorialOwner('Randoms'), false);
  assert.equal(claimsEditorialOwner('the spotify playlist Randoms'), false);
});

/** Regression guard on moving the scorer out of server.ts. */
test('scoring survives the move', () => {
  assert.equal(playlistMatchScore('Randoms', 'Randoms'), 1);
  assert.equal(playlistMatchScore('sautisol', 'sauti sol'), 0.95);
  assert.equal(playlistMatchScore('sautisol', 'Sauti sol/Kenyan gold'), 0.92);
  assert.equal(playlistMatchScore('R&B Classics', 'R&B Classics 90s & 2000s - Best Old School'), 0.92);
  assert.ok(playlistMatchScore('Classics', 'R&B Classics 90s & 2000s - Best Old School') < 0.92);
  assert.equal(playlistMatchScore('nothing alike', 'Randoms'), 0);
});
