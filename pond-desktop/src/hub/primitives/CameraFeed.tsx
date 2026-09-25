import { HubIco, cameraEl } from "./HubIco";
import type { CameraData } from "../data/mockHome";

interface CameraFeedProps {
  cam: CameraData;
  h?: number;
  big?: boolean;
  interactive?: boolean;
}

export function CameraFeed({ cam, h = 120, big = false, interactive = true }: CameraFeedProps) {
  function open() {
    if (interactive) {
      window.dispatchEvent(new CustomEvent("hub:camera", { detail: cam.id }));
    }
  }

  // `big` has its own Expand button, so the tile must not be a nested role="button" (axe flags it).
  const tileIsButton = interactive && !big;

  return (
    <div
      className="cam"
      style={{ height: h, cursor: interactive ? "pointer" : "default" }}
      onClick={open}
      role={tileIsButton ? "button" : undefined}
      tabIndex={tileIsButton ? 0 : undefined}
      onKeyDown={(e) => { if (tileIsButton && (e.key === "Enter" || e.key === " ")) open(); }}
    >
      <div
        className="cam__img"
        style={{
          background: `linear-gradient(${cam.hue + 20}deg, hsl(${cam.hue},35%,42%), hsl(${cam.hue + 30},40%,28%))`,
        }}
      >
        {/* faux scene shapes */}
        <svg
          width="100%"
          height="100%"
          viewBox="0 0 200 120"
          preserveAspectRatio="xMidYMid slice"
          style={{ position: "absolute", inset: 0, opacity: 0.5 }}
        >
          <rect x="0" y="78" width="200" height="42" fill="rgba(0,0,0,.25)" />
          <path d="M0 80 L60 60 L130 70 L200 52 L200 120 L0 120Z" fill="rgba(255,255,255,.06)" />
          <rect x="20" y="40" width="34" height="42" rx="2" fill="rgba(255,255,255,.10)" />
          <circle cx="150" cy="34" r="14" fill="hsla(50,90%,75%,.30)" />
        </svg>
        <span className="cam__live">
          <span className="cam__live-dot" />
          LIVE
        </span>
        <div className="cam__label">
          <span className="cam__name">{cam.name}</span>
          <span className="cam__time">{cam.time}</span>
        </div>
        {big && (
          <button
            className="cam__expand"
            aria-label="Expand camera"
            onClick={(e) => {
              e.stopPropagation();
              window.dispatchEvent(new CustomEvent("hub:camera", { detail: cam.id }));
            }}
          >
            <HubIco d={cameraEl} size={16} color="#fff" />
          </button>
        )}
      </div>
    </div>
  );
}
