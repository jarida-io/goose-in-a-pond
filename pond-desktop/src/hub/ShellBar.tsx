// ────────────────────────────────────────────────────────────
// The bar that opens the drawer.
//
// Two controls, both 44px pills, both in the design's header on all eight
// screens: the hamburger and the bell. Nothing else from that header is here
// -- the clock, the weather and the avatar belong to the Home redesign, not to
// navigation.
//
// The bell earns its place rather than inheriting it. When the sidebar became
// a drawer its pinned Notifications row had nowhere to go, and a notification
// you have to open a menu to discover is one you find late.
// ────────────────────────────────────────────────────────────

import { HubIco } from "./primitives/HubIco";
import { HP_PATHS } from "./primitives/icons";
import "./hubDrawer.css";

export interface ShellBarProps {
  onMenu: () => void;
  onBell: () => void;
  /** Unread schedule runs. Real count -- the rail used to hardcode 3. */
  unread: number;
}

export function ShellBar({ onMenu, onBell, unread }: ShellBarProps) {
  return (
    <div className="shellbar">
      <button
        type="button"
        className="shellbar__btn"
        onClick={onMenu}
        aria-label="Open menu"
      >
        <i className="shellbar__bar" />
        <i className="shellbar__bar" />
        <i className="shellbar__bar" />
      </button>
      <button
        type="button"
        className="shellbar__btn"
        onClick={onBell}
        aria-label={unread > 0 ? `Notifications, ${unread} unread` : "Notifications"}
      >
        <HubIco d={HP_PATHS.bell} size={20} color="var(--color-text)" sw={1.9} />
        {unread > 0 && (
          <span className="shellbar__count" aria-hidden="true">
            {unread > 99 ? "99+" : unread}
          </span>
        )}
      </button>
    </div>
  );
}
