// ────────────────────────────────────────────────────────────
// Now Playing — the design's page-2 widget 1, minus the two things the pond
// cannot supply.
//
// CUT: the speaker name. The design's second line reads "Marconi Union ·
// Living Room speaker", but `now_playing_snapshot` emits connected, playing,
// track, artist, album_art, progress_ms and duration_ms and no device object
// at all. There is nothing on the wire a room could be read from, so the
// artist stands alone rather than carrying an invented suffix (DESIGN.md §3).
//
// CUT: the volume slider. `MusicControlAction` is the closed union
// "play" | "pause" | "next" | "previous" — there is no volume read and no
// volume write, in either direction. A slider that moves and changes nothing
// is the worst inert control there is, so it is not drawn.
//
// Five states, exactly one of them rendered:
//
//   disconnected  a fresh install. Says so, and offers Settings. It does NOT
//                 show the mock track the old widget substituted here — that
//                 faked playback on the one screen most likely to be new.
//   error         Spotify answered and refused. The store's own sentence, and
//                 Try again. No transport: asking again is not a playback
//                 command, and the controls would fail the same way the read
//                 just did.
//   idle          connected, nothing playing. Resume is real (Spotify accepts
//                 it), skipping past nothing is not, so prev/next are off.
//   playing/paused  the full card.
//
// The bar steps in 10-second jumps because that is the poll interval. No
// client-side interpolation timer: it would run ahead of the poll and draw a
// position nobody reported.
// ────────────────────────────────────────────────────────────

import type { ReactElement } from "react";
import { HubIco, pauseEl } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import type { NowPlayingData } from "../../data/mockHome";
import { controlNowPlaying, refreshNowPlaying, useHomeData } from "../../state/hubDataStore";
import "./media-card.css";

export interface MediaCardProps {
  /** Where "Connect a music service" sends them. The integrator passes a function that navigates to Settings. */
  onOpenSettings: () => void;
}

/**
 * `progressMs` / `durationMs` are the raw milliseconds the snapshot has always
 * carried and the store used to discard. They are optional here so this file
 * compiles whether it lands before or after the store change, and because they
 * are genuinely absent at runtime on the idle snapshot — which is the same
 * reason every read below goes through `typeof x === "number"` rather than a
 * truthiness check that would also swallow a legitimate 0.
 */
type TimedNowPlaying = NowPlayingData & {
  progressMs?: number | null;
  durationMs?: number | null;
};

/** mm:ss. Only ever called with a number the backend actually sent. */
function clock(ms: number): string {
  return Math.floor(ms / 60000) + ":" + String(Math.floor(ms / 1000) % 60).padStart(2, "0");
}

export function MediaCard({ onOpenSettings }: MediaCardProps): ReactElement {
  const np = useHomeData().nowPlaying as TimedNowPlaying;

  if (!np.connected) {
    return (
      <div className="mcard" data-hook="media" data-state="disconnected">
        <div className="mcard__empty">
          <span className="mcard__empty-title">No music service connected</span>
          <span className="mcard__empty-sub">Link an account to see what is playing</span>
          <button type="button" onClick={onOpenSettings}>
            Connect in Settings
          </button>
        </div>
      </div>
    );
  }

  if (np.error) {
    return (
      <div className="mcard" data-hook="media" data-state="error">
        <div className="mcard__empty">
          {/* The store has already turned the refusal into a sentence a person
              can act on. Repeating it here in our own words would only be a
              second guess at what Spotify said. */}
          <span className="mcard__empty-title">{np.track}</span>
          {np.message ? <span className="mcard__empty-sub">{np.message}</span> : null}
          <button type="button" className="mcard__retry" onClick={() => void refreshNowPlaying(true)}>
            Try again
          </button>
        </div>
      </div>
    );
  }

  const idle = !np.track.trim() || np.track === "Nothing playing";
  const state = idle ? "idle" : np.playing ? "playing" : "paused";

  const durationMs = np.durationMs;
  const progressMs = np.progressMs;
  // A duration is what makes the rail mean anything; without one there is no
  // scale to place the fill against, and idle has nothing to place.
  const showScrub = !idle && typeof durationMs === "number" && durationMs > 0;
  // The labels need both ends. One number alone would have to be paired with a
  // duration read off the bar, which is arithmetic on a decoration.
  const showTimes = showScrub && typeof progressMs === "number";
  const pct = Math.max(0, Math.min(100, np.elapsed * 100));

  const art = typeof np.albumArt === "string" && np.albumArt.length > 0 ? np.albumArt : null;

  return (
    <div className="mcard" data-hook="media" data-state={state}>
      <div className="mcard__art">
        {art ? (
          <img className="mcard__art-img" src={art} alt="" />
        ) : (
          <HubIco d={HP_PATHS.music} size={16} color="rgba(255,255,255,.75)" sw={2} />
        )}
      </div>

      <div className="mcard__col">
        <div className="mcard__titles">
          <span className="mcard__track">{idle ? "Nothing playing" : np.track}</span>
          {np.artist ? <span className="mcard__artist">{np.artist}</span> : null}
        </div>

        {showScrub && (
          <div className="mcard__scrub">
            {showTimes && <span className="mcard__time">{clock(progressMs)}</span>}
            <span className="mcard__rail">
              <span className="mcard__fill" style={{ width: `${pct}%` }} />
              <span className="mcard__thumb" style={{ left: `${pct}%` }} />
            </span>
            {showTimes && <span className="mcard__time">{clock(durationMs)}</span>}
          </div>
        )}

        <div className="mcard__transport">
          <button
            type="button"
            className="mcard__btn"
            aria-label="Previous"
            disabled={idle}
            onClick={() => void controlNowPlaying("previous")}
          >
            <HubIco d={HP_PATHS.skipB} size={18} color="var(--color-text)" sw={2} />
          </button>
          <button
            type="button"
            className="mcard__play"
            aria-label={np.playing ? "Pause" : "Play"}
            onClick={() => void controlNowPlaying(np.playing ? "pause" : "play")}
          >
            {/* Filled glyph, no stroke — HubIco strokes with currentColor by
                default, which would fatten a shape that is already solid. */}
            <HubIco d={np.playing ? pauseEl : HP_PATHS.play} size={16} fill="var(--color-bg)" color="none" />
          </button>
          <button
            type="button"
            className="mcard__btn"
            aria-label="Next"
            disabled={idle}
            onClick={() => void controlNowPlaying("next")}
          >
            <HubIco d={HP_PATHS.skipF} size={18} color="var(--color-text)" sw={2} />
          </button>
        </div>
      </div>
    </div>
  );
}
