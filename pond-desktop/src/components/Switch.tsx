import type { ButtonHTMLAttributes } from "react";
import "../styles/switch.css";

type NativeButton = Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "onChange" | "role" | "type" | "aria-checked" | "aria-label"
>;

export interface SwitchProps extends NativeButton {
  checked: boolean;
  onChange?: (next: boolean) => void;
  /** Accessible name when no visible label is tied in with `aria-labelledby`. */
  label?: string;
  size?: "sm" | "md";
}

/** The one on/off switch in the app: the household's accent when on, a round knob, `role="switch"`. */
export function Switch({
  checked,
  onChange,
  label,
  size = "md",
  className,
  onClick,
  ...rest
}: SwitchProps) {
  return (
    <button
      {...rest}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      data-on={checked}
      className={["giap-switch", size === "sm" ? "giap-switch--sm" : "", className ?? ""]
        .filter(Boolean)
        .join(" ")}
      onClick={(event) => {
        onClick?.(event);
        if (event.defaultPrevented || rest["aria-disabled"] === true || rest["aria-disabled"] === "true") {
          return;
        }
        onChange?.(!checked);
      }}
    >
      <span className="giap-switch__knob" aria-hidden="true" />
    </button>
  );
}
