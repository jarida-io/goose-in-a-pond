// Now Playing. No speaker name or volume slider: the snapshot has no device and
// `MusicControlAction` has no volume. The bar moves only on the 10s poll, never interpolated.

import { useEffect, useState, type CSSProperties, type ReactElement } from "react";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS, pauseEl } from "../../primitives/icons";
import type { NowPlayingData } from "../../data/mockHome";
import { controlNowPlaying, refreshNowPlaying, useHomeData } from "../../state/hubDataStore";
import type { CardSize } from "../../state/dashboardLayout";
import { extractDominantColor } from "./dominantColor";
import "./media-card.css";

export interface MediaCardProps {
  /** The frame's current size (nowPlaying defaults to "s" — see dashboardLayout.ts). */
  size: CardSize;
  /** Where "Connect a music service" goes (Settings). */
  onOpenSettings: () => void;
}

/** Raw ms, absent on the idle snapshot; checked with `typeof` so a real 0 survives. */
type TimedNowPlaying = NowPlayingData & {
  progressMs?: number | null;
  durationMs?: number | null;
};

/** mm:ss. Only ever called with a number the backend actually sent. */
function clock(ms: number): string {
  return Math.floor(ms / 60000) + ":" + String(Math.floor(ms / 1000) % 60).padStart(2, "0");
}

export function MediaCard({ size, onOpenSettings }: MediaCardProps): ReactElement {
  const np = useHomeData().nowPlaying as TimedNowPlaying;
  const art = typeof np.albumArt === "string" && np.albumArt.length > 0 ? np.albumArt : null;

  const [bgColor, setBgColor] = useState<string | null>(null);
  const [liked, setLiked] = useState(false);

  useEffect(() => {
    setLiked(false);
    if (!art) {
      setBgColor(null);
      return;
    }
    let cancelled = false;
    void extractDominantColor(art).then((color) => {
      if (!cancelled) setBgColor(color);
    });
    return () => {
      cancelled = true;
    };
  }, [art]);

  if (!np.connected) {
    return (
      <div className="mcard" data-hook="media" data-state="disconnected" data-size={size}>
        <div className="mcard__glass">
          <div className="mcard__empty">
            <span className="mcard__empty-title">No music service connected</span>
            <span className="mcard__empty-sub">Link an account to see what is playing</span>
            <button type="button" onClick={onOpenSettings}>
              Connect in Settings
            </button>
          </div>
        </div>
      </div>
    );
  }

  if (np.error) {
    return (
      <div className="mcard" data-hook="media" data-state="error" data-size={size}>
        <div className="mcard__glass">
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
      </div>
    );
  }

  const idle = !np.track.trim() || np.track === "Nothing playing";
  const state = idle ? "idle" : np.playing ? "playing" : "paused";

  const durationMs = np.durationMs;
  const progressMs = np.progressMs;
  const showScrub = !idle && typeof durationMs === "number" && durationMs > 0;
  const showTimes = showScrub && typeof progressMs === "number";
  const pct = Math.max(0, Math.min(100, np.elapsed * 100));

  const cardStyle = bgColor
    ? ({ "--mcard-glow": bgColor, "--mcard-play-ico": bgColor } as CSSProperties)
    : undefined;

  // Small is a square album-art tile, not a shrunk player: the photo is the
  // whole point, so everything else floats over it on its own scrim instead
  // of sitting beside a thumbnail. Flat siblings (not nested in `.mcard__glass`)
  // on purpose — each is absolutely positioned, so paint order is just DOM
  // order with no stacking-context fights to referee.
  if (size === "s") {
    return (
      <div className="mcard mcard--tile" data-hook="media" data-state={state} data-size={size} style={cardStyle}>
        <div className="mcard__tile-art">
          {art ? (
            <img className="mcard__tile-img" src={art} alt="" />
          ) : (
            <HubIco d={HP_PATHS.music} size={20} color="rgba(255,255,255,.75)" sw={2} />
          )}
        </div>
        <div className="mcard__tile-scrim" aria-hidden="true" />
        <button
          type="button"
          className="mcard__tile-like"
          aria-label={liked ? "Unlike" : "Like"}
          aria-pressed={liked}
          disabled={idle}
          onClick={() => setLiked((v) => !v)}
        >
          <HubIco d={HP_PATHS.heart} size={16} color="currentColor" fill={liked ? "currentColor" : "none"} sw={2} />
        </button>
        <div className="mcard__tile-meta">
          <span className="mcard__tile-track">{idle ? "Nothing playing" : np.track}</span>
          {np.artist ? <span className="mcard__tile-artist">{np.artist}</span> : null}
        </div>
        <button
          type="button"
          className="mcard__tile-play"
          aria-label={np.playing ? "Pause" : "Play"}
          onClick={() => void controlNowPlaying(np.playing ? "pause" : "play")}
        >
          <HubIco d={np.playing ? pauseEl : HP_PATHS.play} size={14} fill="var(--mcard-play-ico)" color="none" />
        </button>
      </div>
    );
  }

  return (
    <div className="mcard" data-hook="media" data-state={state} data-size={size} style={cardStyle}>
      <div className="mcard__glow" aria-hidden="true" />

      <div className="mcard__glass">
        <div className="mcard__top">
          <div className="mcard__art">
            {art ? (
              <img className="mcard__art-img" src={art} alt="" />
            ) : (
              <HubIco d={HP_PATHS.music} size={16} color="rgba(255,255,255,.75)" sw={2} />
            )}
          </div>

          <div className="mcard__titles">
            <span className="mcard__track">{idle ? "Nothing playing" : np.track}</span>
            {np.artist ? <span className="mcard__artist">{np.artist}</span> : null}
          </div>

          <button
            type="button"
            className="mcard__like"
            aria-label={liked ? "Unlike" : "Like"}
            aria-pressed={liked}
            disabled={idle}
            onClick={() => setLiked((v) => !v)}
          >
            <HubIco d={HP_PATHS.heart} size={18} color="currentColor" fill={liked ? "currentColor" : "none"} sw={2} />
          </button>
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
            <HubIco d={HP_PATHS.skipB} size={18} color="currentColor" sw={2} />
          </button>
          <button
            type="button"
            className="mcard__play"
            aria-label={np.playing ? "Pause" : "Play"}
            onClick={() => void controlNowPlaying(np.playing ? "pause" : "play")}
          >
            {/* Filled glyph, no stroke — HubIco strokes with currentColor by
                default, which would fatten a shape that is already solid. */}
            <HubIco d={np.playing ? pauseEl : HP_PATHS.play} size={16} fill="var(--mcard-play-ico)" color="none" />
          </button>
          <button
            type="button"
            className="mcard__btn"
            aria-label="Next"
            disabled={idle}
            onClick={() => void controlNowPlaying("next")}
          >
            <HubIco d={HP_PATHS.skipF} size={18} color="currentColor" sw={2} />
          </button>
        </div>
      </div>
    </div>
  );
}
