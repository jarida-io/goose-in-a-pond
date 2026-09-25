import { useState } from "react";

// ─── Toggle (pill switch) ─────────────────────────────────────
interface ToggleProps {
  on?: boolean;
  onChange?: (on: boolean) => void;
  /** Accessible name — the hub button otherwise carries none beyond
   *  aria-pressed. Backward-compatible: every existing caller omits it. */
  label?: string;
}
export function Toggle({ on: initial = false, onChange, label }: ToggleProps) {
  const [on, setOn] = useState(initial);
  function handleClick() {
    const next = !on;
    setOn(next);
    onChange?.(next);
  }
  return (
    <button
      className="htoggle"
      data-on={on}
      onClick={handleClick}
      aria-pressed={on}
      aria-label={label}
      type="button"
    >
      <span className="htoggle__knob" />
    </button>
  );
}

// ─── Segment (segmented control) ─────────────────────────────
interface SegmentProps {
  options: string[];
  value?: string;
  onChange?: (v: string) => void;
}
export function Segment({ options, value, onChange }: SegmentProps) {
  const [v, setV] = useState(value ?? options[0]);
  function pick(o: string) {
    setV(o);
    onChange?.(o);
  }
  return (
    <div className="hseg">
      {options.map((o) => (
        <button
          key={o}
          className="hseg__opt"
          data-active={v === o}
          onClick={() => pick(o)}
          type="button"
        >
          {o}
        </button>
      ))}
    </div>
  );
}

// ─── Slider ───────────────────────────────────────────────────
interface SliderProps {
  min?: number;
  max?: number;
  value?: number;
  suffix?: string;
  onChange?: (v: number) => void;
}
export function Slider({ min = 0, max = 100, value = 50, suffix = "", onChange }: SliderProps) {
  const [v, setV] = useState(value);
  const pct = ((v - min) / (max - min)) * 100;
  function handleChange(e: React.ChangeEvent<HTMLInputElement>) {
    const next = Number(e.target.value);
    setV(next);
    onChange?.(next);
  }
  return (
    <div className="hrange">
      <input
        type="range"
        min={min}
        max={max}
        value={v}
        onChange={handleChange}
        style={{
          background: `linear-gradient(90deg,var(--pp) ${pct}%,#E6E3F0 ${pct}%)`,
        }}
      />
      <span className="hrange__val">
        {v}
        {suffix}
      </span>
    </div>
  );
}

// ─── Card (setcard wrapper) ───────────────────────────────────
interface CardProps {
  title?: React.ReactNode;
  right?: React.ReactNode;
  children: React.ReactNode;
}
export function Card({ title, right, children }: CardProps) {
  return (
    <div className="setcard">
      {(title != null || right != null) && (
        <div className="setcard__head">
          {title != null && <span className="setcard__title">{title}</span>}
          {right}
        </div>
      )}
      {children}
    </div>
  );
}

// ─── Row (list row — div not button to avoid nested-button a11y violations) ──
interface RowProps {
  label: string;
  sub?: string;
  control?: React.ReactNode;
  onClick?: () => void;
}
export function Row({ label, sub, control, onClick }: RowProps) {
  function handleKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (onClick && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      onClick();
    }
  }
  return (
    <div
      className="srow"
      role={onClick ? "button" : undefined}
      tabIndex={onClick ? 0 : undefined}
      onClick={onClick}
      onKeyDown={onClick ? handleKeyDown : undefined}
      style={{ cursor: onClick ? "pointer" : "default" }}
    >
      <span className="srow__text">
        <span className="srow__label">{label}</span>
        {sub && <span className="srow__sub">{sub}</span>}
      </span>
      <span
        className="srow__control"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.stopPropagation()}
      >
        {control}
      </span>
    </div>
  );
}

// ─── Chip ─────────────────────────────────────────────────────
interface ChipProps {
  children: React.ReactNode;
  active?: boolean;
  onClick?: () => void;
}
export function Chip({ children, active = false, onClick }: ChipProps) {
  return (
    <button
      className="preset-chip"
      data-active={active}
      onClick={onClick}
      type="button"
    >
      {children}
    </button>
  );
}
