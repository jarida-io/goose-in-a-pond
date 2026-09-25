import { useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { pauseEl } from "./HubIco";
import { useHomeData, controlNowPlaying, refreshNowPlaying } from "../state/hubDataStore";

type NowPlayingVariant = "bar" | "tile";

interface NowPlayingProps {
  variant?: NowPlayingVariant;
}

export function NowPlaying({ variant = "bar" }: NowPlayingProps) {
  const np = useHomeData().nowPlaying;
  // Cosmetic toggle so the demo widget stays interactive while Spotify isn't connected.
  const [demoPlaying, setDemoPlaying] = useState(true);
  const playing = np.connected ? np.playing : demoPlaying;
  // Spotify is linked but refusing requests: the controls would fail too, so disable them.
  const errored = Boolean(np.error);

  function handlePlayPause() {
    if (errored) return;
    if (np.connected) void controlNowPlaying(playing ? "pause" : "play");
    else setDemoPlaying((p) => !p);
  }

  function handleSkip(action: "next" | "previous") {
    if (errored) return;
    if (np.connected) void controlNowPlaying(action);
  }

  // Real cover art becomes the scrimmed card background; the swatch-and-icon is the no-art fallback.
  const hasArt = !errored && Boolean(np.albumArt);

  return (
    <div
      className={`np np--${variant}${errored ? " np--error" : ""}${hasArt ? " np--has-art" : ""}`}
      style={
        hasArt
          ? {
              backgroundImage: `linear-gradient(180deg, var(--np-scrim-top), var(--np-scrim-bottom)), url(${np.albumArt})`,
              backgroundSize: "cover",
              backgroundPosition: "center",
            }
          : undefined
      }
    >
      {!hasArt && (
        <div
          className="np__art"
          style={{
            background: errored
              ? "linear-gradient(135deg,#94A3B8,#64748B)"
              : `linear-gradient(135deg,hsl(${np.hue},60%,58%),hsl(${np.hue + 40},55%,42%))`,
          }}
        >
          <HubIco
            d={errored ? HP_PATHS.alert : HP_PATHS.music}
            size={variant === "tile" ? 26 : 18}
            color="rgba(255,255,255,.9)"
          />
        </div>
      )}
      <div className="np__info">
        <span className="np__track">{np.track}</span>
        <span className="np__artist" title={np.message}>
          {np.artist}
        </span>
        {variant === "tile" && !errored && (
          <div className="np__bar">
            <span style={{ width: `${np.elapsed * 100}%` }} />
          </div>
        )}
      </div>
      <div className="np__ctrls">
        {errored ? (
          /* Repeated refusals stop the poll; a person asking again is what resumes it. */
          <button className="np__retry" onClick={() => void refreshNowPlaying(true)}>
            Try again
          </button>
        ) : (
          <>
            <button aria-label="Previous" onClick={() => handleSkip("previous")}>
              <HubIco d={HP_PATHS.skipB} size={16} color={hasArt ? "rgba(255,255,255,.85)" : "#64748B"} />
            </button>
            <button
              className="np__play"
              onClick={handlePlayPause}
              aria-label={playing ? "Pause" : "Play"}
            >
              <HubIco d={playing ? pauseEl : HP_PATHS.play} size={16} color="#fff" />
            </button>
            <button aria-label="Next" onClick={() => handleSkip("next")}>
              <HubIco d={HP_PATHS.skipF} size={16} color={hasArt ? "rgba(255,255,255,.85)" : "#64748B"} />
            </button>
          </>
        )}
      </div>
    </div>
  );
}
