# GIAP Music Extension

MCP extension for music playback and playlist management via the Spotify Web API.

## Installation

Install from the GIAP Extensions marketplace with one click. Click **Sign in with Spotify** to authorize playback control -- GIAP handles the entire OAuth flow.

## Playback behaviour

`play` starts music **and keeps it going**. A song is played *inside its album*
— `PUT /me/player/play` with `context_uri` set to the album and `offset` set to
the track — so the requested song starts and the album follows on. When the
release is a single or a two-track EP, more by the same artist is queued behind
it, because an album context is no help when the album is one song long.

This matters because Spotify's play endpoint takes either `uris` or
`context_uri`, never both, and a `uris` list is an ad-hoc queue that Spotify
plays and then **stops**. Passing a single track URI as `uris: [track]` is
therefore a playlist of exactly one song, which is why playback used to fall
silent with nothing left in the queue.

Two constraints worth knowing before changing any of this:

- `offset` is only valid when the context is an **album or playlist**. Spotify
  rejects it for an artist context, so "play this song within the artist"
  cannot be expressed.
- The obvious source for follow-ups, `GET /artists/{id}/top-tracks`, answers
  **403** for this app — see the withdrawn-endpoint list in
  `src/providers/spotify.ts`. Follow-ups come from a field-filtered `/search`
  instead, narrowed to the seed track's artist id.

`queue` is unchanged and still appends without interrupting.

## Available Tools

> **This table is out of date** and is kept only until it is rewritten. The
> tools actually served are `play`, `queue`, `play_playlist`, `playlists`,
> `library`, `devices`, `status` and `control` — see `TOOLS` in `src/server.ts`,
> which is the only authority. Several rows below name provider methods that
> were deleted (playlist writes and library writes are refused by Spotify for
> this app).

| Tool | Description |
|------|-------------|
| `now_playing` | Get info about the currently playing track |
| `play` | Start or resume playback, optionally by Spotify URI |
| `pause` | Pause the current playback |
| `next` | Skip to the next track |
| `previous` | Go back to the previous track |
| `search_tracks` | Search for tracks by name, artist, or keyword |
| `search_albums` | Search for albums by name, artist, or keyword |
| `get_playlists` | List saved playlists |
| `get_playlist_tracks` | Get all tracks in a playlist |
| `create_playlist` | Create a new playlist |
| `add_to_playlist` | Add tracks to an existing playlist |
| `get_queue` | View the current playback queue |
| `set_volume` | Set playback volume (0-100) |
| `set_shuffle` | Enable or disable shuffle mode |

## Manual Setup (Development)

For local development and testing without the GIAP OAuth flow:

1. Create a Spotify Developer app at https://developer.spotify.com/dashboard
2. Generate an access token with the required scopes:
   - `user-read-playback-state`
   - `user-modify-playback-state`
   - `user-read-currently-playing`
   - `playlist-read-private`
   - `playlist-modify-private`
   - `playlist-modify-public`
3. Set the token as an environment variable:

```bash
export SPOTIFY_ACCESS_TOKEN="your-token-here"
```

4. Run the server:

```bash
cd extensions/music
npm install
npm start
```

## Testing

Test the MCP protocol handshake:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | npx tsx src/server.ts
```

List available tools:

```bash
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | npx tsx src/server.ts
```

## Requirements

- Node.js 18+ (for native fetch)
- Spotify Premium account (required for playback control)
- An active Spotify device (phone, desktop app, or web player)
