import React from "react";

// ─── Compound SVG elements (multi-path icons from the HP dictionary) ──────────

export const lockEl = (
  <>
    <rect x="5" y="11" width="14" height="10" rx="2" />
    <path d="M8 11V7a4 4 0 0 1 8 0v4" />
  </>
);

export const unlockEl = (
  <>
    <rect x="5" y="11" width="14" height="10" rx="2" />
    <path d="M8 11V7a4 4 0 0 1 7.5-2" />
  </>
);

export const sunEl = (
  <>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </>
);

export const cameraEl = (
  <>
    <path d="M2 8a2 2 0 0 1 2-2h2l1.5-2h5L14 6h2a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2z" transform="translate(1 0)" />
    <circle cx="11" cy="13" r="3.5" />
  </>
);

export const micEl = (
  <>
    <rect x="9" y="2" width="6" height="12" rx="3" />
    <path d="M5 10a7 7 0 0 0 14 0M12 17v4M8 21h8" />
  </>
);

export const pauseEl = (
  <>
    <rect x="6" y="4" width="4" height="16" rx="1" />
    <rect x="14" y="4" width="4" height="16" rx="1" />
  </>
);

export const dotsEl = (
  <>
    <circle cx="5" cy="12" r="1.6" />
    <circle cx="12" cy="12" r="1.6" />
    <circle cx="19" cy="12" r="1.6" />
  </>
);

export const briefcaseEl = (
  <>
    <rect x="2" y="7" width="20" height="14" rx="2" />
    <path d="M8 7V5a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M2 13h20" />
  </>
);

export const filmEl = (
  <>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M7 3v18M17 3v18M3 8h4M17 8h4M3 16h4M17 16h4M3 12h18" />
  </>
);

export const focusEl = (
  <>
    <circle cx="12" cy="12" r="3" />
    <circle cx="12" cy="12" r="8" />
  </>
);

// ─── HubIco component ─────────────────────────────────────────

interface HubIcoProps {
  /** Either a plain SVG path string (d attribute) or a ReactNode for multi-path icons */
  d: string | React.ReactNode;
  size?: number;
  color?: string;
  fill?: string;
  sw?: number;
  className?: string;
}

/** Thin SVG icon wrapper (the design's HIco). */
export function HubIco({
  d,
  size = 20,
  color = "currentColor",
  fill = "none",
  sw = 1.9,
  className,
}: HubIcoProps): React.ReactElement {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={fill}
      stroke={color}
      strokeWidth={sw}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0, display: "block" }}
      className={className}
    >
      {typeof d === "string" ? <path d={d} /> : d}
    </svg>
  );
}
