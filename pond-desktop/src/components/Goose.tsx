/**
 * The goose: a shared illustration for anywhere the Pond needs a face.
 *
 * The drawing and its motion live in `gooseMotion.ts`, which the phone app shares file for file; this
 * draws it once and moves it from `requestAnimationFrame`. The neck and head are rebuilt every frame
 * along a curved spine, so they bend as one body; every other part turns about its own pivot. With
 * `water`, the goose floats on a pond with drifting waves, glints, and a reflection the ripples break
 * up. Colours come from the theme's tokens, so the goose follows the accent.
 *
 * Reduced motion draws one resting frame and leaves it.
 */
import { useEffect, useId, useRef } from "react";
import {
  BACK_SHEEN, BANDS, BELLY, BODY, CHEST_SHEEN, GLINTS, HEAD_DETAIL, PRIMARIES, SCALLOPS, TAIL, WAKE, WAVES, WING,
  WING_LINES, easeListen, neckFrame, neckPose, partsFrame, restingParts, wavePath,
  type GooseMood, type PartsFrame,
} from "./gooseMotion";

export type GooseState = GooseMood;

interface GooseProps {
  /** Idle: afloat. Working: the Pond is answering. Listening: it is hearing speech. */
  state?: GooseState;
  /** Rendered width in px; the height follows the drawing. */
  size?: number;
  /** The pond under the goose; off for inline use, where the drawing crops to the bird. */
  water?: boolean;
  /** Decorative by default; pass a label when it carries meaning. */
  label?: string;
  className?: string;
}

// The theme's colours, and shades of them, so the goose follows the accent.
const C = {
  accent: "var(--color-accent)",
  light: "color-mix(in srgb, var(--color-accent) 58%, #fff)",
  deep: "var(--color-accent-active, color-mix(in srgb, var(--color-accent) 72%, #000))",
  darkest: "color-mix(in srgb, var(--color-accent-active, var(--color-accent)) 62%, #000)",
  beak: "var(--color-warning)",
  beakDeep: "color-mix(in srgb, var(--color-warning) 76%, #000)",
  ground: "var(--color-surface)",
};
const stop = (offset: number, color: string, opacity = 1) => (
  <stop offset={offset} style={{ stopColor: color, stopOpacity: opacity }} />
);

/** One goose, drawn in full; `tag` keeps ids unique when it is drawn twice (the bird and its reflection). */
function Bird({ id, tag }: { id: string; tag: string }) {
  const u = (n: string) => `${id}-${tag}-${n}`;
  const D = HEAD_DETAIL;
  return (
    <>
      <defs>
        <linearGradient id={u("body")} x1="0" y1="0" x2="0" y2="1">{stop(0, C.light)}{stop(0.45, C.accent)}{stop(1, C.darkest)}</linearGradient>
        <linearGradient id={u("wing")} x1="0" y1="0" x2="1" y2="1">{stop(0, C.accent)}{stop(1, C.darkest)}</linearGradient>
        <linearGradient id={u("prim")} x1="0" y1="0" x2="1" y2="0">{stop(0, C.deep)}{stop(1, C.darkest)}</linearGradient>
        <linearGradient id={u("tail")} x1="0" y1="1" x2="1" y2="0">{stop(0, C.deep)}{stop(1, C.darkest)}</linearGradient>
        <linearGradient id={u("nh")} gradientUnits="userSpaceOnUse" x1="42" y1="0" x2="70" y2="0">{stop(0, C.light)}{stop(0.45, C.accent)}{stop(1, C.deep)}</linearGradient>
        <linearGradient id={u("beak")} x1="0" y1="0" x2="0" y2="1">{stop(0, "#ffd27a")}{stop(1, C.beak)}</linearGradient>
        <clipPath id={u("above")}><rect x="0" y="0" width="160" height="99" /></clipPath>
        <clipPath id={u("bodyClip")}><path d={BODY} /></clipPath>
      </defs>
      {/* The neck and head, rebuilt every frame; the neck's root sits behind the chest. */}
      <g data-part="rig">
        <path data-k="neck" fill={`url(#${u("nh")})`} />
        <path data-k="shine" fill="none" stroke="#fff" strokeWidth="1.5" strokeLinecap="round" opacity=".26" />
        {/* The beak's root runs under the head, so no turn opens a notch. */}
        <g data-k="beak">
          <path fill={`url(#${u("beak")})`} d={D.beakUpper} />
          <path style={{ fill: C.beakDeep }} d={D.beakLower} />
          <path fill="none" style={{ stroke: C.beakDeep }} strokeWidth=".45" strokeLinecap="round" opacity=".7" d={D.beakSeam} />
          <ellipse style={{ fill: C.beakDeep }} {...D.nostril} transform={`rotate(6 ${D.nostril.cx} ${D.nostril.cy})`} />
        </g>
        <path data-k="head" fill={`url(#${u("nh")})`} />
        <g data-k="face">
          <path style={{ fill: C.darkest }} opacity=".22" d={D.throatShade} />
          <ellipse fill="#fff" opacity=".42" {...D.gloss} transform={`rotate(-6 ${D.gloss.cx} ${D.gloss.cy})`} />
          <path fill="#fff" opacity=".94" d={D.chinstrap} />
          <g data-k="eye">
            <circle fill="#fff" {...D.eye} />
            <circle fill="#14111c" {...D.pupil} />
            <circle fill="#fff" {...D.catchlight} />
          </g>
        </g>
        {[0, 1, 2].map((i) => <circle key={i} data-k="ring" fill="none" style={{ stroke: C.accent }} strokeWidth="1.3" opacity="0" />)}
      </g>
      <g clipPath={`url(#${u("above")})`}>
        <g data-part="tail">
          <path style={{ fill: C.darkest }} d={TAIL[0]} />
          <path fill={`url(#${u("tail")})`} d={TAIL[1]} />
          <path style={{ fill: C.accent }} d={TAIL[2]} />
        </g>
        <path fill={`url(#${u("body")})`} d={BODY} />
        <g clipPath={`url(#${u("bodyClip")})`}>
          <path style={{ fill: C.darkest }} opacity=".32" d={BELLY} />
          <path fill="#fff" opacity=".34" d={BACK_SHEEN} />
          <path fill="#fff" opacity=".2" d={CHEST_SHEEN} />
          {SCALLOPS.map((d, i) => <path key={d} fill="none" stroke="#fff" strokeWidth="1" strokeLinecap="round" opacity={i ? 0.16 : 0.22} d={d} />)}
        </g>
        <g data-part="wing">
          {PRIMARIES.map((d) => <g key={d} data-part="primary"><path fill={`url(#${u("prim")})`} d={d} /></g>)}
          <path fill={`url(#${u("wing")})`} d={WING} />
          {WING_LINES.coverts.map((d, i) => <path key={d} fill="none" stroke="#fff" strokeWidth={i ? 0.8 : 0.9} strokeLinecap="round" opacity={i ? 0.2 : 0.28} d={d} />)}
          <path fill="none" style={{ stroke: C.darkest }} strokeWidth=".7" strokeLinecap="round" opacity=".45" d={WING_LINES.edge} />
        </g>
      </g>
    </>
  );
}

/** The pond, back to front: the reflection, the water, the bands that break the reflection, the waves, the glints. */
function Pond({ id }: { id: string }) {
  return (
    <>
      <defs>
        <linearGradient id={`${id}-deep`} gradientUnits="userSpaceOnUse" x1="0" x2="0" y1="98" y2="138">{stop(0, C.accent, 0.24)}{stop(0.45, C.accent, 0.1)}{stop(1, C.accent, 0)}</linearGradient>
        <radialGradient id={`${id}-shadow`} cx=".5" cy=".5" r=".5"><stop offset="0" stopColor="#000" stopOpacity=".26" /><stop offset="1" stopColor="#000" stopOpacity="0" /></radialGradient>
        <radialGradient id={`${id}-oval`} gradientUnits="userSpaceOnUse" cx="84" cy="100" r="76" gradientTransform="translate(84 100) scale(1 .42) translate(-84 -100)">
          <stop offset="0" stopColor="#fff" /><stop offset=".6" stopColor="#fff" /><stop offset="1" stopColor="#fff" stopOpacity="0" />
        </radialGradient>
        <mask id={`${id}-fade`} maskUnits="userSpaceOnUse" x="0" y="94" width="160" height="46"><rect x="0" y="94" width="160" height="46" fill={`url(#${id}-oval)`} /></mask>
        <linearGradient id={`${id}-v`} gradientUnits="userSpaceOnUse" x1="0" x2="0" y1="98" y2="136"><stop offset="0" stopColor="#fff" /><stop offset=".45" stopColor="#fff" stopOpacity=".55" /><stop offset="1" stopColor="#fff" stopOpacity="0" /></linearGradient>
        <mask id={`${id}-reflMask`} maskUnits="userSpaceOnUse" x="0" y="98" width="160" height="42"><rect x="0" y="98" width="160" height="42" fill={`url(#${id}-v)`} /></mask>
      </defs>
      <g mask={`url(#${id}-fade)`}>
        <g data-part="reflection" mask={`url(#${id}-reflMask)`}>
          <g opacity=".42" transform="translate(0 196) scale(1 -1)"><Bird id={id} tag="r" /></g>
        </g>
      </g>
      <rect x="0" y="98" width="160" height="42" fill={`url(#${id}-deep)`} mask={`url(#${id}-fade)`} />
      <g mask={`url(#${id}-fade)`}>
        {BANDS.map(([y, width, dash]) => (
          <path key={y} data-part="band" d={`M-80 ${y}h320`} fill="none" style={{ stroke: C.ground }} strokeWidth={width} strokeDasharray={dash} opacity=".7" />
        ))}
      </g>
      <g mask={`url(#${id}-fade)`}>
        {WAVES.map(([y, amplitude, wavelength, width, opacity]) => (
          <g key={y} data-part="wave" opacity={opacity}>
            <path d={wavePath(y, amplitude, wavelength)} fill="none" style={{ stroke: C.accent }} strokeWidth={width} strokeLinecap="round" />
          </g>
        ))}
      </g>
      {GLINTS.map(([cx, cy, rx, ry]) => <ellipse key={cx} data-part="glint" style={{ fill: C.light }} cx={cx} cy={cy} rx={rx} ry={ry} opacity="0" />)}
      <g data-part="shadow"><ellipse fill={`url(#${id}-shadow)`} cx="90" cy="99" rx="46" ry="4.2" /></g>
      {WAKE.map((d) => (
        <path key={d} data-part="wake" d={d} fill="none" style={{ stroke: C.accent }} strokeWidth="1.3" strokeLinecap="round" strokeDasharray="14 60" opacity="0" />
      ))}
      {[0, 1].map((i) => <ellipse key={i} data-part="ripple" cx="88" cy="98" rx="30" ry="5.5" fill="none" style={{ stroke: C.accent }} strokeWidth="1.3" opacity="0" />)}
    </>
  );
}

/** Puts every part of one goose where the clock says. */
function paint(svg: SVGSVGElement, t: number, mood: GooseMood, listen: number, parts: PartsFrame) {
  const set = (el: Element | null | undefined, name: string, value: string | number) => el?.setAttribute(name, String(value));
  const all = (part: string) => svg.querySelectorAll(`[data-part="${part}"]`);
  const pose = neckPose(t, mood, listen);
  const neck = neckFrame(pose);
  const head = `matrix(${neck.headMatrix.join(" ")})`;
  const { cx, cy } = HEAD_DETAIL.eye;
  all("rig").forEach((rig) => {
    set(rig.querySelector('[data-k="neck"]'), "d", neck.neck);
    set(rig.querySelector('[data-k="shine"]'), "d", neck.highlight);
    set(rig.querySelector('[data-k="head"]'), "d", neck.head);
    set(rig.querySelector('[data-k="beak"]'), "transform", head);
    set(rig.querySelector('[data-k="face"]'), "transform", head);
    set(rig.querySelector('[data-k="eye"]'), "transform", `translate(${cx} ${cy}) scale(1 ${pose.lid}) translate(${-cx} ${-cy})`);
    rig.querySelectorAll('[data-k="ring"]').forEach((ring, i) => {
      set(ring, "cx", neck.ringCentre[0].toFixed(2));
      set(ring, "cy", neck.ringCentre[1].toFixed(2));
      set(ring, "r", parts.rings[i].r.toFixed(2));
      set(ring, "opacity", parts.rings[i].opacity.toFixed(3));
    });
  });
  const { bob } = parts;
  set(svg.querySelector('[data-part="glide"]'), "transform", `translate(0 ${bob.y}) translate(88 98) scale(${bob.sx} ${bob.sy}) translate(-88 -98)`);
  set(svg.querySelector('[data-part="roll"]'), "transform", `rotate(${parts.roll} 88 98)`);
  all("tail").forEach((el) => set(el, "transform", `rotate(${parts.tail} 120 78)`));
  all("wing").forEach((wing) => {
    set(wing, "transform", `translate(76 82) scale(${parts.wing}) translate(-76 -82)`);
    wing.querySelectorAll('[data-part="primary"]').forEach((el, i) => set(el, "transform", `rotate(${parts.primaries[i]} 104 84)`));
  });
  const r = parts.reflection;
  set(svg.querySelector('[data-part="reflection"]'), "transform", `translate(0 ${r.y}) translate(88 98) skewX(${r.skew}) scale(1 ${r.sy}) translate(-88 -98)`);
  all("wave").forEach((el, i) => set(el, "transform", `translate(${parts.waves[i]} 0)`));
  all("band").forEach((el, i) => set(el, "transform", `translate(${parts.bands[i]} 0)`));
  all("glint").forEach((el, i) => {
    const g = parts.glints[i], [gx, gy] = GLINTS[i];
    set(el, "transform", `translate(${g.x} 0) translate(${gx} ${gy}) scale(${g.sx} 1) translate(${-gx} ${-gy})`);
    set(el, "opacity", g.opacity.toFixed(3));
  });
  const shadow = svg.querySelector('[data-part="shadow"]');
  set(shadow, "transform", `translate(90 99) scale(${parts.shadow.scale}) translate(-90 -99)`);
  set(shadow, "opacity", parts.shadow.opacity.toFixed(3));
  all("ripple").forEach((el, i) => {
    set(el, "transform", `translate(88 98) scale(${parts.ripples[i].scale}) translate(-88 -98)`);
    set(el, "opacity", parts.ripples[i].opacity.toFixed(3));
  });
  all("wake").forEach((el, i) => {
    set(el, "stroke-dashoffset", parts.wake[i].offset.toFixed(2));
    set(el, "opacity", parts.wake[i].opacity);
  });
}

const prefersStill = () => typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;

export function Goose({ state = "idle", size = 132, water = true, label, className }: GooseProps) {
  const id = `goose-${useId().replace(/[^a-zA-Z0-9]/g, "")}`;
  const ref = useRef<SVGSVGElement>(null);
  // The loop reads the mood through this, so a change of mood does not restart the clock.
  const mood = useRef<GooseMood>(state);
  mood.current = state;

  useEffect(() => {
    const svg = ref.current;
    if (!svg || prefersStill()) return;
    let listen = mood.current === "listening" ? 1 : 0;
    // A first frame now, so the neck and head are there before the loop's first tick.
    const start = performance.now() / 1000;
    paint(svg, start, mood.current, listen, partsFrame(start, mood.current, listen));
    if (typeof requestAnimationFrame !== "function") return;
    let raf = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = Math.min(0.1, (now - last) / 1000);
      last = now;
      const t = now / 1000;
      listen = easeListen(listen, mood.current, dt);
      paint(svg, t, mood.current, listen, partsFrame(t, mood.current, listen));
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [water]);

  // Reduced motion runs no loop: each mood gets one resting frame.
  useEffect(() => {
    if (ref.current && prefersStill()) paint(ref.current, 0, state, state === "listening" ? 1 : 0, restingParts(state));
  }, [state, water]);

  return (
    <svg
      ref={ref}
      className={`goose${className ? ` ${className}` : ""}`}
      data-state={state}
      viewBox={water ? "0 0 160 140" : "22 6 136 96"}
      width={size}
      height={size * (water ? 140 / 160 : 96 / 136)}
      style={{ overflow: "visible", flex: "0 0 auto" }}
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {water && <Pond id={id} />}
      <g data-part="glide"><g data-part="roll"><Bird id={id} tag="b" /></g></g>
      {water && <path d="M10 98.4h140" fill="none" style={{ stroke: C.light }} strokeWidth=".9" strokeLinecap="round" opacity=".7" />}
    </svg>
  );
}
