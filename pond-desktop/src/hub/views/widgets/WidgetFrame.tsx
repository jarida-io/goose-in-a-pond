// ────────────────────────────────────────────────────────────
// WidgetFrame — the chrome the household sees only while arranging Home.
//
// Two departures from the design, both forced by rules this repo holds:
//
//   NO GRIP  The design draws a six-dot grip with `cursor: grab` and nothing
//            behind it. Drag-to-reorder is not in this build, and a control
//            labelled with a verb it cannot perform is what DESIGN.md §3
//            forbids. Two move buttons stand in: plainer, and they move.
//   44px     The design's toolbar controls are 26px on their short axis. This
//            ships on a 1024×600 finger-driven panel with a 44px touch floor,
//            so every control here is 44px and the toolbar lane grows from
//            30px to 48px. The shapes and the token colours are the design's.
//
// Pure chrome. It holds no state and knows nothing about what it wraps —
// every action is a callback the caller supplies, and when `arranging` is
// false it renders the children and no chrome at all.
// ────────────────────────────────────────────────────────────

import type { ReactElement, ReactNode } from "react";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import "./widget-frame.css";

export type WidgetSize = "s" | "m" | "l";

export interface WidgetFrameProps {
  /** What this widget is called in the arrange controls' accessible names, e.g. "Weather". */
  title: string;
  size: WidgetSize;
  /** True while the household is arranging Home. When false the frame renders children and no chrome at all. */
  arranging: boolean;
  onSize: (size: WidgetSize) => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  canMoveUp: boolean;
  canMoveDown: boolean;
  /**
   * False when this is the last card left on Home anywhere.
   *
   * The store refuses that removal — a Home with nothing on it has no card to
   * arrange and so no visible route back to the sheet — and the sheet's own
   * remove is already disabled for it. Without the same answer here the frame's
   * x renders lit, 44px, and does nothing at all when a thumb finds it.
   */
  canRemove: boolean;
  children: ReactNode;
}

const SIZES: readonly WidgetSize[] = ["s", "m", "l"];

/** Spoken size names — a lone "S" read aloud tells nobody what it does. */
const SIZE_NAMES: Record<WidgetSize, string> = { s: "small", m: "medium", l: "large" };

export function WidgetFrame({
  title,
  size,
  arranging,
  onSize,
  onRemove,
  onMoveUp,
  onMoveDown,
  canMoveUp,
  canMoveDown,
  canRemove,
  children,
}: WidgetFrameProps): ReactElement {
  return (
    // data-size is present arranging or not: WidgetTrack's grid rule reads it,
    // and the column layout must not change when the toolbar goes away.
    <section className="wframe" data-size={size} data-arranging={arranging || undefined}>
      {arranging && (
        <div className="wframe__bar">
          <button
            type="button"
            className="wframe__btn"
            aria-label={`Move ${title} up`}
            disabled={!canMoveUp}
            onClick={onMoveUp}
          >
            <HubIco d={HP_PATHS.chevD} size={18} color="var(--color-text)" sw={2.4} className="wframe__up" />
          </button>

          <button
            type="button"
            className="wframe__btn"
            aria-label={`Move ${title} down`}
            disabled={!canMoveDown}
            onClick={onMoveDown}
          >
            <HubIco d={HP_PATHS.chevD} size={18} color="var(--color-text)" sw={2.4} />
          </button>

          <div className="wframe__seg">
            {SIZES.map((value) => (
              <button
                key={value}
                type="button"
                className="wframe__seg-btn"
                aria-pressed={size === value}
                aria-label={`Show ${title} ${SIZE_NAMES[value]}`}
                onClick={() => onSize(value)}
              >
                {value.toUpperCase()}
              </button>
            ))}
          </div>

          <button
            type="button"
            className="wframe__btn"
            aria-label={`Remove ${title} from Home`}
            disabled={!canRemove}
            onClick={onRemove}
          >
            <HubIco d={HP_PATHS.x} size={16} color="var(--color-text)" sw={2.4} />
          </button>
        </div>
      )}

      <div className="wframe__body">{children}</div>
    </section>
  );
}

export interface AddWidgetButtonProps {
  /** What it offers, e.g. "Add a widget". Never name widgets the catalogue does not hold. */
  label: string;
  onClick: () => void;
}

/**
 * The add affordance. A bare button the page column holds directly, so it
 * takes part in the page's 12px gap rather than sitting inside a frame of its
 * own — it is not a widget and carries no arrange chrome.
 */
export function AddWidgetButton({ label, onClick }: AddWidgetButtonProps): ReactElement {
  return (
    <button type="button" className="wframe-add" onClick={onClick}>
      <HubIco d={HP_PATHS.plus} size={16} color="var(--pp)" sw={2} />
      {label}
    </button>
  );
}
