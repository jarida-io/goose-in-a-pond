// ────────────────────────────────────────────────────────────
// Home's status strip.
//
// The design draws a single 60px header carrying the hamburger, the bell, the
// arrange control, the temperature, the clock and the avatar. This is only its
// right-hand half, because the left-hand half already exists: `ShellBar` is
// rendered above this view with both of those 44px pills already wired -- the
// hamburger to the drawer, the bell to the notifications route. Drawing them
// again here would give the panel two menus and two bells.
//
// So the strip carries exactly what the design's header has and the shell does
// not, and it belongs to Home rather than to the shell because every item on it
// is about this screen: the arrange control edits this screen's layout, and the
// weather, clock and avatar are the screen's own content. Chrome above Home on
// the 600px panel comes to 60 + 44 = 104px.
//
// Everything sits in ONE cluster, flush right, in the design's own order --
// arrange pill, temperature, date/time, avatar. The strip has no left-hand
// slot on purpose: the shell's hamburger already starts at x=20 on the row
// above, so anything this file put at its own left edge would stack a second
// left-aligned control directly under the first, in the one position the
// design keeps empty.
//
// It is 44px rather than 60 for that reason -- the second bar has to pay for
// itself in the height it takes from the widgets below it.
// ────────────────────────────────────────────────────────────

import React from "react";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { useNow } from "../state/useNow";
import "./home-status-bar.css";

export interface HomeStatusBarProps {
  /** True while the household is arranging Home. */
  arranging: boolean;
  onToggleArrange: () => void;
  /** Outdoor temperature as the pond reports it, or null when weather is off, unset, or unanswered. Null hides the slot. */
  temp: number | null;
  /** The name the pond knows, for the monogram. Empty string when it has none. */
  userName: string;
}

export function HomeStatusBar({
  arranging,
  onToggleArrange,
  temp,
  userName,
}: HomeStatusBarProps): React.ReactElement {
  // A panel on a shelf is never reloaded, so a clock read once at mount would
  // be wrong within the minute and stale by morning. `useNow` re-ticks.
  const now = useNow();

  const name = userName.trim();
  const monogram = name.charAt(0).toUpperCase();

  return (
    <div className="hbar" data-hook="home-status">
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
        {temp !== null && <span className="hbar__temp">{temp}°</span>}

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
