/** The goose: a shared illustration for anywhere the pond needs a face. Built from parts so
 *  `state` can animate each; all motion is in `styles/goose.css`, keyed off `data-state`. */

export type GooseState =
  /** Afloat and waiting. Slow bob, occasional head dip. */
  | "idle"
  /** The agent is running. Faster paddle, ripples quicken. */
  | "working"
  /** Listening for speech. Head lifts and holds, alert. */
  | "listening";

interface GooseProps {
  state?: GooseState;
  /** Rendered width in px; height follows the viewBox. */
  size?: number;
  /** Draw the waterline and ripples; off for inline use, where the viewBox crops to the bird. */
  water?: boolean;
  /** Decorative by default — pass a label when it carries meaning. */
  label?: string;
  className?: string;
}

export function Goose({ state = "idle", size = 132, water = true, label, className }: GooseProps) {
  const viewBox = water ? "0 0 160 120" : "38 6 108 96";
  const ratio = water ? 120 / 160 : 96 / 108;
  return (
    <svg
      className={`goose${className ? ` ${className}` : ""}`}
      data-state={state}
      data-water={water ? "true" : "false"}
      viewBox={viewBox}
      width={size}
      height={size * ratio}
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {/* Water. The ripples open from the waterline; the line itself is still. */}
      {water && (
      <g className="goose__water">
        <ellipse className="goose__ripple goose__ripple--1" cx="80" cy="98" rx="30" ry="5.5" />
        <ellipse className="goose__ripple goose__ripple--2" cx="80" cy="98" rx="30" ry="5.5" />
        <path className="goose__waterline" d="M14 98h132" />
      </g>
      )}

      {/* Everything above the water rides the same bob. */}
      <g className="goose__float">
        {/* Body: elongated rather than round. A circle reads as a duck; the
            length is what makes the silhouette a goose. */}
        <path
          className="goose__body"
          d="M40 84c0-14 16-23 39-23s41 8 41 22-17 20-41 20-39-5-39-19z"
        />

        {/* Tail. Its base sits INSIDE the body outline — drawn flush to the
            edge it reads as a separate shard floating alongside the bird. */}
        <path className="goose__tail" d="M106 80l34-19-8 25z" />

        {/* Wing, a shade down so the bird has a shoulder. */}
        <path
          className="goose__wing"
          d="M64 78c8-7 22-9 33-4 6 3 7 9 2 13-9 6-26 6-34-1-4-3-4-6-1-8z"
        />

        {/* Neck and head, on their own slower phase. The neck is long, slender
            and tapered — the first version was a short wedge, which is exactly
            what made it read as a duck. */}
        <g className="goose__head">
          <path className="goose__neck" d="M70 66c-7-15-7-31 0-41l10 2c-5 9-5 25 0 39z" />
          <ellipse className="goose__skull" cx="75" cy="22" rx="10.5" ry="8.5" />
          {/* The chinstrap. Without it a purple head is just a blob. */}
          <path className="goose__strap" d="M67 27q8 6 16 0" />
          <path className="goose__beak" d="M65 18l-16 5 16 5z" />
          <circle className="goose__eye" cx="79" cy="19" r="2" />
        </g>
      </g>
    </svg>
  );
}
