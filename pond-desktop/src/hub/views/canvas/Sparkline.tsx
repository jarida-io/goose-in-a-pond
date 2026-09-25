import React from "react";

interface SparklineProps {
  data: number[];
  positive: boolean;
}

/** Minimal SVG polyline sparkline. */
export function Sparkline({ data, positive }: SparklineProps): React.ReactElement | null {
  if (!data || data.length < 2) return null;

  const min = Math.min(...data);
  const max = Math.max(...data);
  const range = max - min || 1;
  const W = 64;
  const H = 26;
  const step = W / (data.length - 1);

  const pts = data
    .map(
      (v, i) =>
        `${(i * step).toFixed(1)},${(H - 2 - ((v - min) / range) * (H - 4)).toFixed(1)}`
    )
    .join(" ");

  const color = positive ? "#16A34A" : "#DC2626";

  return (
    <svg
      width={W}
      height={H}
      viewBox={`0 0 ${W} ${H}`}
      aria-hidden="true"
      style={{ flexShrink: 0 }}
    >
      <polyline
        points={pts}
        fill="none"
        stroke={color}
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
