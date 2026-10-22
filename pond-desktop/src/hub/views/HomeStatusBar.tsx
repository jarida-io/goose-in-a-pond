// Home's status strip: the right half of the design's header. ShellBar above already has the
// hamburger and bell, so nothing goes on the left, and it is 44px, not 60, to spare the widgets.

import React from "react";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { greetingForHour, useNow } from "../state/useNow";
import "./home-status-bar.css";

export interface HomeStatusBarProps {
  arranging: boolean;
  onToggleArrange: () => void;
  /** Outdoor temperature; null (weather off, unset or unanswered) hides the slot. */
  temp: number | null;
  /** True when the weather widget is already placed on the page -- its own card
      already states the temperature, so the header copy would be a duplicate. */
  hideTemp?: boolean;
  /** The name the pond knows, for the monogram. Empty string when it has none. */
  userName: string;
  /** The one-sentence line under the greeting -- weather and what's due, or quiet. */
  subtitle: string;
}

export function HomeStatusBar({
  arranging,
  onToggleArrange,
  temp,
  hideTemp = false,
  userName,
  subtitle,
}: HomeStatusBarProps): React.ReactElement {
  // `useNow` re-ticks: a shelf panel is never reloaded, so a mount-time clock goes stale.
  const now = useNow();

  const name = userName.trim();
  const monogram = name.charAt(0).toUpperCase();

  return (
    <div className="hbar" data-hook="home-status">
      <div className="hbar__left">
        <h1 className="hbar__greeting">
          {greetingForHour(now.getHours())}
          {name && <>, <span className="hbar__greeting-name">{name}</span></>}
        </h1>
        {subtitle && <p className="hbar__subtitle">{subtitle}</p>}
      </div>

      <div className="hbar__right">
        {/* The design's caption reads "drag a grip to reorder". There is no drag
            in this build and the frame's controls are arrows, so the caption
            names the control the household actually has.

            Its home in the design is the widget track column's own header row,
            directly above the frames it describes -- and that row belongs to
            DashboardGrid/WidgetTrack, not to this file. Of the homes this strip
            can offer it, the left edge is the one to refuse: x=20 under the
            shell's hamburger is exactly the collision the arrange pill was
            moved out of. So the caption travels with the control it explains,
            immediately left of the pill that leaves the mode, and it is the
            item that gives up width first when the cluster runs out. */}
        {arranging && (
          <p className="hbar__caption">Arranging · use the arrows to reorder</p>
        )}

        {/* First member of the right-hand cluster, as the design has it: a
            quiet check-mark pill reading "Done arranging" while arranging, a
            pencil reading "Arrange" otherwise. */}
        <button type="button" className="hbar__arrange" onClick={onToggleArrange}>
          {arranging ? (
            <HubIco d={HP_PATHS.check} size={16} color="var(--pp)" sw={2} />
          ) : (
            <HubIco d={HP_PATHS.pencil} size={16} color="var(--color-text)" sw={2} />
          )}
          {arranging ? "Done arranging" : "Arrange"}
        </button>

        {/* A bare degree, never a unit letter. The backend pins Open-Meteo to
            celsius and there is no unit setting anywhere in Settings, so either
            letter would be an assertion the pond cannot make -- the design's
            64°F is a mockup literal. */}
        {temp !== null && !hideTemp && <span className="hbar__temp">{temp}°</span>}

        <div className="hbar__stack">
          <span className="hbar__date">
            {now
              .toLocaleDateString(undefined, {
                weekday: "short",
                day: "numeric",
                month: "short",
              })
              .toUpperCase()}
          </span>
          <span className="hbar__time">
            {now.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}
          </span>
        </div>

        {/* Not a button: there is no profile screen behind it. And no user photo
            exists anywhere in the pond -- a profile carries a display name and
            an emoji, and emoji fail this package's build gate -- so a monogram
            off the real name is the only honest avatar. The pattern already
            ships in settings/Account.tsx. */}
        <span
          className="hbar__avatar"
          role="img"
          aria-label={name ? `Signed in as ${name}` : "No name set"}
        >
          {monogram || (
            <HubIco
              d={HP_PATHS.person}
              size={20}
              color="var(--color-text-secondary)"
              sw={1.9}
            />
          )}
        </span>
      </div>
    </div>
  );
}
