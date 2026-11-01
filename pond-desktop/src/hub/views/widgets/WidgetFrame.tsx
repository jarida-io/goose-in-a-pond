// Home's arrange-mode chrome. Unlike the design: move buttons, not a grip (there's no drag-to-reorder,
// DESIGN.md §3), and 44px controls for the panel's touch floor, so the toolbar lane is 48px.

import type { ReactElement, ReactNode } from "react";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import "./widget-frame.css";

export type WidgetSize = "s" | "m" | "l";

export interface WidgetFrameProps {
  /** What this widget is called in the arrange controls' accessible names, e.g. "Weather". */
  title: string;
  size: WidgetSize;
  /** When false, only the children render. */
  arranging: boolean;
  onSize: (size: WidgetSize) => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  canMoveUp: boolean;
  canMoveDown: boolean;
  /** False for Home's last card: the store won't remove it (an empty Home has no way back to the sheet). */
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
    // data-size stays when not arranging: WidgetTrack's grid rule reads it.
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
            <HubIco d={HP_PATHS.chevD} size={18} sw={2.4} className="wframe__up" />
          </button>

          <button
            type="button"
            className="wframe__btn"
            aria-label={`Move ${title} down`}
            disabled={!canMoveDown}
            onClick={onMoveDown}
          >
            <HubIco d={HP_PATHS.chevD} size={18} sw={2.4} />
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
            <HubIco d={HP_PATHS.x} size={16} sw={2.4} />
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

/** A bare button, not a framed widget, so it sits in the page column's 12px gap without arrange chrome. */
export function AddWidgetButton({ label, onClick }: AddWidgetButtonProps): ReactElement {
  return (
    <button type="button" className="wframe-add" onClick={onClick}>
      <HubIco d={HP_PATHS.plus} size={16} color="var(--pp)" sw={2} />
      {label}
    </button>
  );
}
