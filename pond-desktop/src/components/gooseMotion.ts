/**
 * The goose's drawing and motion, as plain data and pure functions of the clock.
 *
 * Shared, file for file, with goose-on-the-go's `components/patterns/goose-motion.ts`, so the
 * dashboard and the phone draw the same bird and move it the same way. Nothing here knows about a
 * framework: the dashboard calls it from `requestAnimationFrame`, the phone from Reanimated on the UI
 * thread, which is what the `'worklet'` directive in each function is for (elsewhere it is an unused
 * string).
 *
 * The drawing is in a 160 x 120 frame with the waterline at y = 98; the pond reaches y = 140.
 *
 * The neck and head are not moved, they are rebuilt each frame. A cubic spine runs from inside the
 * chest to the throat, and the neck's outline is grown along it at a width that slims up the throat
 * and swells into the head; the head is drawn in its own frame at the spine's end and turned by the
 * neck's curve. Both share one gradient in one space, so a bend rolls through the throat with no
 * joint and no seam. Every other part turns or slides about its own pivot.
 *
 * Periods are deliberately not multiples of one another (4.6s bob, 7.1s roll, 5.3s sway, 11s glance,
 * 17s look-down), so the motion drifts in and out of step and the loop never shows.
 */

export type GooseMood = "idle" | "working" | "listening";
export type Point = readonly [number, number];

// ── The drawing ───────────────────────────────────────────────────────────────

/** The body: a boat shape with a full chest at the front and a rear rising into the tail. */
export const BODY =
  "M46 87C45.4 75.6 53.6 67 65.4 64.8 79.6 62.2 98.6 63.6 111.6 67.6 120 70.2 127 72.6 132.6 70.4 134.4 76.8 131 86.6 121.4 92.6 109.6 99.4 88.6 102.6 70 101.8 55.4 101.2 46.6 96.4 46 87z";
export const BELLY = "M44 90c9 9 26 12.6 44 12.6s34-3.6 42-12v20H44z";
export const BACK_SHEEN =
  "M54.6 70.4c10-5.4 21.6-7.4 34-7 11.6.4 21.8 2.4 30 5.8-10-2.4-20.2-3.6-30.6-3.8-11.6-.2-23 1.4-33.4 5z";
export const CHEST_SHEEN = "M49.6 81c1-6 5-10.4 10.8-12.6-3.8 3.6-6.2 8-7.2 13.4-.4 1.4-3.8.9-3.6-.8z";
export const SCALLOPS = ["M58 91q3 2.4 6 0q3 2.4 6 0q3 2.4 6 0", "M62 96.2q3 2.4 6 0q3 2.4 6 0q3 2.4 6 0q3 2.4 6 0"] as const;
/** Three tail feathers, darkest at the back. */
export const TAIL = [
  "M118 76C128 69 139 62 150.6 56.6c1.4-.6 2 .6 1.2 1.7C146 66.6 139 74 130.6 79.8c-4.6.4-8.8-1.2-12.6-3.8z",
  "M119 78.4c11-5 21.2-9.6 30.4-13.2 1.2-.4 1.6.6.8 1.6-4.8 6.4-10.6 11.6-17.4 15-5 0-9.6-1.2-13.8-3.4z",
  "M118.6 80.6c9.4-1.6 17.8-3.6 25.8-5.8 1.1-.3 1.4.6.6 1.4-4.6 3.8-9.4 6.6-14.6 8-4.4-.6-8.4-1.8-11.8-3.6z",
] as const;
export const WING = "M74 79c9-7.6 25-9.8 38-5.4 7.2 2.6 9 8.6 3.4 13-10 7.2-29.6 7-39.4-.6-4.4-3.2-4.6-5.6-2-7z";
/** Flight feathers, outermost first; they ruffle while the goose is working. */
export const PRIMARIES = [
  "M104.4 81.6c8.4.2 16.4-1.4 22.6-5.6.6-.4 1.2 0 .8.6-3.6 6.6-12 9.8-23.4 8.8z",
  "M102.4 84c8.4.8 16 0 22.2-3.2.6-.3 1 .1.7.7-3.8 6-12 8.2-22.9 6.2z",
  "M100.4 86.4c7.4.9 14.2.7 20-1.4.6-.2.9.2.6.7-3.6 5-10.8 6-20.6 4.2z",
] as const;
export const WING_LINES = {
  coverts: ["M80 79.8q7-4.4 16-4.4 9 0 14 3.4", "M82.4 85q8-2.8 16-2.2 7 .6 11.4 2.2"],
  edge: "M78 89.4c7.4 3.6 20.4 4.2 31.6.4",
} as const;

/** The head's details, in the head's own frame: x toward the beak, y up, origin at the throat. */
export const HEAD_DETAIL = {
  beakUpper: "M9.5 3.7C14.8 3.3 19.8 2.2 24 .9c.9-.3 1-1 .2-1.2C19.4-.7 14.4-.5 9.5-.3z",
  beakLower: "M9.5-.3c4.8-.1 9.4-.2 13.9-.2.7 0 .7-.6.1-.8C19-2.6 14.3-3.2 9.5-3.4z",
  beakSeam: "M12.6-.4c3.6-.1 7.2-.1 10.6 0",
  nostril: { cx: 17.6, cy: 1.9, rx: 0.85, ry: 0.42 },
  throatShade: "M-6.4-2.2C-2.4-5.6 5.6-6.4 10.8-3.8 6.8-3.4-.4-2.8-6.4-.8z",
  gloss: { cx: 0.6, cy: 7.5, rx: 4.6, ry: 1.45 },
  chinstrap:
    "M-3 1.6C-3.6-1-2.8-3.6-.6-5.2 1.8-6.6 4.8-6.6 7.2-5.8 7.6-5.6 7.4-5 7-5 4.8-4.8 2.6-4.4 1-3.2-.4-2.1-.9-.4-.9 1.4-.9 2.4-2.8 2.5-3 1.6z",
  eye: { cx: 6.2, cy: 4.4, r: 2.25 },
  pupil: { cx: 6.5, cy: 4.3, r: 1.5 },
  catchlight: { cx: 7, cy: 5, r: 0.55 },
} as const;

/** The head's outline in its own frame; the rig maps it into the drawing every frame. */
export const HEAD_OUTLINE: readonly Point[] = [
  [-8, 0.3], [-7.8, 4], [-5.5, 7.6], [-1.5, 9.6], [3.5, 9.8], [8, 8.4], [11, 6], [12.6, 3.6],
  [12.4, 0.6], [11, -2.6], [7.5, -5], [2.5, -6.2], [-2.5, -6.2], [-7, -4],
];

/** The neck's spine at rest: from inside the chest, up the front of the body, to the throat. */
export const SPINE: readonly Point[] = [[60, 80], [58.5, 62], [53, 44], [57, 29]];

/**
 * A ripple ring at rest: [cx, cy, rx, ry]. It hangs from the waterline (y = 98): its top edge sits on
 * the line, and it is scaled about (cx, 98) as it spreads, so the ring always lies on the water and never
 * rises beside the body as one centred on the line did.
 */
export const RIPPLE = [88, 98 + 5.5, 30, 5.5] as const;

/** Waves under the bird, nearest the waterline first: [y, amplitude, wavelength, width, opacity, seconds, reverse]. */
export const WAVES = [
  [101.4, 0.8, 18, 0.6, 0.42, 6, false],
  [105.2, 1.2, 26, 0.85, 0.32, 9, true],
  [110.8, 1.6, 36, 1.15, 0.22, 13, false],
  [119, 2, 48, 1.5, 0.14, 17, true],
] as const;

/** Bands of background colour that break the reflection up: [y, width, dash pattern, seconds, reverse]. */
export const BANDS = [
  [100.8, 0.55, "18 6 30 4 12 8", 10, false],
  [103.6, 0.75, "26 5 14 7 22 4", 13, true],
  [107.2, 1, "12 9 34 6 10 7", 16, false],
  [111.6, 1.3, "30 8 16 10 8 6", 12, true],
  [117, 1.7, "22 8 24 6 12 6", 15, false],
  [123.4, 2.2, "28 8 16 10 10 6", 18, true],
] as const;
/** Every band's dash pattern sums to this, so sliding it one period loops seamlessly. */
export const BAND_PERIOD = 78;

/** Glints of light on the surface: [cx, cy, rx, ry, seconds, delay]. */
export const GLINTS = [
  [34, 100.6, 3.4, 0.38, 3.6, 0],
  [124, 102.2, 2.6, 0.32, 4.4, -1.2],
  [58, 106.4, 2.2, 0.3, 3.9, -2.1],
  [110, 109, 3, 0.34, 5.1, -0.6],
  [70, 116, 3.8, 0.42, 4.8, -2.8],
] as const;

/** The wake's two arms, trailing from the tail while the goose swims hard. */
export const WAKE = ["M132 99.2c8 1.4 15.4 3 22 5.4", "M132 97c8.4-.6 16-2 23.4-4"] as const;

/** One wave as a stroke long enough to slide a wavelength either way. */
export function wavePath(y: number, amplitude: number, wavelength: number): string {
  let d = `M -40 ${y}`;
  for (let i = 0, x = -40; x < 200; i++, x += wavelength / 2) {
    d += ` q ${wavelength / 4} ${i % 2 ? amplitude : -amplitude} ${wavelength / 2} 0`;
  }
  return d;
}

// ── The colours ───────────────────────────────────────────────────────────────

export interface GoosePalette {
  accent: string;
  /** Lit side of the body and neck, and the glints. */
  light: string;
  /** The wing and the shaded side. */
  deep: string;
  /** The belly and the tail's back feather. */
  darkest: string;
  beak: string;
  beakDeep: string;
}

// ── Motion helpers ────────────────────────────────────────────────────────────

const TAU = Math.PI * 2;

/** 0 at the start of each period, 1 at its middle, back to 0: a smooth swell. */
export function swell(t: number, period: number, offset = 0): number {
  "worklet";
  return (1 - Math.cos((TAU * (t + offset)) / period)) / 2;
}

function phaseOf(t: number, period: number): number {
  "worklet";
  return (((t % period) + period) % period) / period;
}

function smoothstep(e0: number, e1: number, x: number): number {
  "worklet";
  const k = Math.min(1, Math.max(0, (x - e0) / (e1 - e0)));
  return k * k * (3 - 2 * k);
}

// ── The neck and head ─────────────────────────────────────────────────────────

function bezier(P: readonly Point[], t: number): Point {
  "worklet";
  const m = 1 - t, a = m * m * m, b = 3 * m * m * t, c = 3 * m * t * t, d = t * t * t;
  return [
    a * P[0][0] + b * P[1][0] + c * P[2][0] + d * P[3][0],
    a * P[0][1] + b * P[1][1] + c * P[2][1] + d * P[3][1],
  ];
}

function tangent(P: readonly Point[], t: number): Point {
  "worklet";
  const m = 1 - t;
  const x = 3 * m * m * (P[1][0] - P[0][0]) + 6 * m * t * (P[2][0] - P[1][0]) + 3 * t * t * (P[3][0] - P[2][0]);
  const y = 3 * m * m * (P[1][1] - P[0][1]) + 6 * m * t * (P[2][1] - P[1][1]) + 3 * t * t * (P[3][1] - P[2][1]);
  const l = Math.hypot(x, y) || 1;
  return [x / l, y / l];
}

/** The neck's width along the spine: thick at the chest, slim up the throat, swelling into the head. */
export function neckWidth(t: number): number {
  "worklet";
  return t < 0.78 ? 10.6 + 6.4 * (1 - t / 0.78) ** 2 : 10.6 + 1.2 * ((t - 0.78) / 0.22) ** 2;
}

/** A smooth path through the points: Catmull-Rom, written as cubic Béziers. */
export function smoothPath(pts: readonly Point[], closed: boolean): string {
  "worklet";
  const n = pts.length;
  const at = (i: number) => pts[closed ? (i + n) % n : Math.max(0, Math.min(n - 1, i))];
  const f = (v: number) => v.toFixed(2);
  let d = `M${f(pts[0][0])} ${f(pts[0][1])}`;
  for (let i = 0; i < (closed ? n : n - 1); i++) {
    const p0 = at(i - 1), p1 = at(i), p2 = at(i + 1), p3 = at(i + 2);
    d += `C${f(p1[0] + (p2[0] - p0[0]) / 6)} ${f(p1[1] + (p2[1] - p0[1]) / 6)} ${f(p2[0] - (p3[0] - p1[0]) / 6)} ${f(p2[1] - (p3[1] - p1[1]) / 6)} ${f(p2[0])} ${f(p2[1])}`;
  }
  return closed ? `${d}Z` : d;
}

const REST_HEADING = Math.atan2(SPINE[3][1] - SPINE[2][1], SPINE[3][0] - SPINE[2][0]);

/** What the neck and head are doing at time `t`: the spine, the head's tilt, a glance and the eyelid. */
export interface NeckPose {
  spine: Point[];
  /** Radians, added to the turn the neck's curve gives the head. */
  tilt: number;
  /**
   * 0 to 1: how far the head has turned toward the viewer. The face ahead of the throat (eye, beak)
   * foreshortens and the head rounds a little; the back of the head stays the neck's width.
   */
  glance: number;
  /** 1 open, near 0 closed. */
  lid: number;
}

/**
 * The pose at `t` seconds. `listen` is 0 to 1, eased by the caller as the goose starts or stops
 * listening. Idle, it sways with the head a beat behind the neck, glances toward you every 11s and
 * lowers its head to look into the water every 17s; it blinks every 4.3s.
 */
export function neckPose(t: number, mood: GooseMood, listen: number): NeckPose {
  "worklet";
  const tempo = mood === "working" ? 2.4 : 1;
  const sway = Math.sin((TAU * t * tempo) / 5.3);
  const lag = Math.sin((TAU * t * tempo) / 5.3 - 1);
  const idle = mood === "idle";
  const look = phaseOf(t - 4, 17);
  const down = idle ? smoothstep(0.62, 0.69, look) * (1 - smoothstep(0.79, 0.86, look)) : 0;
  const turn = phaseOf(t - 1.5, 11);
  const glance = idle ? smoothstep(0.3, 0.35, turn) * (1 - smoothstep(0.47, 0.52, turn)) : 0;
  const blink = phaseOf(t, 4.3);
  const lid = (blink > 0.945 && blink < 0.965) || (blink > 0.98 && blink < 0.995) ? 0.12 : 1;
  const P: Point[] = SPINE.map((q) => [q[0], q[1]] as Point);
  const settle = 1 - listen;
  const s1: Point = [P[1][0] + 0.4 * sway, P[1][1]];
  const s2: Point = [P[2][0] + 1.1 * sway * settle - 4 * down + 1.4 * listen, P[2][1] + 3 * down - 1.2 * listen];
  const s3: Point = [P[3][0] + 1.7 * sway * settle - 6.5 * down + 0.8 * listen, P[3][1] + 8.5 * down - 2.6 * listen];
  const tiltDeg = 3.5 * lag * settle - 24 * down - 2.4 * down * Math.sin(TAU * t * 1.6) - 13 * listen;
  return { spine: [P[0], s1, s2, s3], tilt: (tiltDeg * Math.PI) / 180, glance, lid };
}

/** The neck's outline, its highlight, the head's outline and the head's frame, for one pose. */
export interface NeckFrame {
  neck: string;
  highlight: string;
  head: string;
  /** SVG `matrix(a b c d e f)` values placing the head's own frame (x to the beak, y up) in the drawing. */
  headMatrix: [number, number, number, number, number, number];
  /** Where the listening rings centre, in the drawing. */
  ringCentre: Point;
}

export function neckFrame(pose: NeckPose): NeckFrame {
  "worklet";
  const { spine: P, tilt, glance } = pose;
  const N = 20;
  const left: Point[] = [], right: Point[] = [], shine: Point[] = [];
  for (let i = 0; i <= N; i++) {
    const t = i / N, p = bezier(P, t), d = tangent(P, t), w = neckWidth(t) / 2;
    left.push([p[0] - d[1] * w, p[1] + d[0] * w]);
    right.push([p[0] + d[1] * w, p[1] - d[0] * w]);
    if (t > 0.1 && t < 0.72) shine.push([p[0] + d[1] * (w - 2.3), p[1] - d[0] * (w - 2.3)]);
  }
  const end = tangent(P, 1), J = P[3];
  // The neck's curve turns the head 60% of the way; the rest is the bird keeping its head level.
  const heading = Math.PI + 0.6 * (Math.atan2(end[1], end[0]) - REST_HEADING) + tilt;
  // A turn toward the viewer is about the head's own upright axis, which passes through the throat:
  // only what lies ahead of it comes toward you and shortens, so the back of the head never pinches
  // where it meets the neck, and the head fills out a touch as its round front shows.
  const squeeze = 1 - 0.26 * glance;
  const round = 1 + 0.06 * glance;
  const cx = Math.cos(heading), cy = Math.sin(heading);
  const ux = -Math.sin(heading) * round, uy = Math.cos(heading) * round;
  const place = (q: Point): Point => {
    const x = q[0] > 0 ? q[0] * squeeze : q[0];
    return [J[0] + x * cx + q[1] * ux, J[1] + x * cy + q[1] * uy];
  };
  return {
    neck: smoothPath(left.concat(right.reverse()), true),
    highlight: smoothPath(shine, false),
    head: smoothPath(HEAD_OUTLINE.map(place), true),
    // The face's details (eye, beak, gloss) sit ahead of the throat, so they take the full squeeze.
    headMatrix: [cx * squeeze, cy * squeeze, ux, uy, J[0], J[1]],
    ringCentre: place([3, 2]),
  };
}

/** Eases `listen` toward 1 while listening and back to 0 after, over about 0.2s. */
export function easeListen(listen: number, mood: GooseMood, dt: number): number {
  "worklet";
  const target = mood === "listening" ? 1 : 0;
  return listen + (target - listen) * (1 - Math.exp(-dt * 5));
}

// ── Every other part ──────────────────────────────────────────────────────────

/** Where each moving part is at time `t`. Angles in degrees; translations in the drawing's units. */
export interface PartsFrame {
  bob: { y: number; sx: number; sy: number };
  roll: number;
  wing: number;
  primaries: [number, number, number];
  tail: number;
  shadow: { scale: number; opacity: number };
  reflection: { y: number; skew: number; sy: number };
  waves: number[];
  bands: number[];
  glints: { opacity: number; x: number; sx: number }[];
  ripples: { scale: number; opacity: number }[];
  wake: { offset: number; opacity: number }[];
  rings: { r: number; opacity: number }[];
}

/** The tail's flick: still most of the time, then a quick flick and settle every 6.7s. */
function flick(t: number): number {
  "worklet";
  const p = phaseOf(t, 6.7);
  if (p < 0.78 || p >= 0.92) return 0;
  if (p < 0.82) return -10 * smoothstep(0.78, 0.82, p);
  if (p < 0.86) return -10 + 16 * smoothstep(0.82, 0.86, p);
  if (p < 0.89) return 6 - 9 * smoothstep(0.86, 0.89, p);
  return -3 + 3 * smoothstep(0.89, 0.92, p);
}

/** A ripple opening from the body: rising fast, then thinning as it spreads. */
function ripple(t: number, period: number, offset: number): { scale: number; opacity: number } {
  "worklet";
  const p = phaseOf(t + offset, period);
  const out = Math.min(1, p / 0.72);
  const opacity = p < 0.14 ? (0.55 * p) / 0.14 : p < 0.72 ? 0.55 * (1 - (p - 0.14) / 0.58) : 0;
  return { scale: 0.35 + 1.2 * (1 - (1 - out) * (1 - out)), opacity };
}

function slide(t: number, seconds: number, distance: number, reverse: boolean): number {
  "worklet";
  const p = phaseOf(t, seconds);
  return reverse ? -distance * (1 - p) : -distance * p;
}

export function partsFrame(t: number, mood: GooseMood, listen: number): PartsFrame {
  "worklet";
  const working = mood === "working";
  const bobPeriod = working ? 1.4 : 4.6;
  const b = swell(t, bobPeriod);
  const water = mood === "listening" ? 40 : 0;
  const pace = (seconds: number, fast: number) => (water ? water : working ? fast : seconds);
  const bob = mood === "listening"
    ? { y: -1.5 - 1.5 * swell(t, 2.4), sx: 1, sy: 1 }
    : { y: -2.6 * b, sx: 1.012 - 0.017 * b, sy: 0.99 + 0.016 * b };
  const ruffle = (offset: number) => (working ? -7 * swell(t, 0.7, offset) : 0);
  return {
    bob,
    roll: -1.4 + 3 * swell(t, working ? 2.6 : 7.1),
    wing: 1 + 0.03 * swell(t, working ? 1.2 : 3.9),
    primaries: [ruffle(-0.24), ruffle(-0.12), ruffle(0)],
    tail: flick(t),
    shadow: { scale: 1 - 0.1 * b, opacity: 0.9 - 0.3 * b },
    reflection: { y: 2.2 * b, skew: 3.5 * b, sy: 1 + 0.06 * b },
    waves: WAVES.map(([, , wavelength, , , seconds, reverse], i) =>
      slide(t, pace(seconds, [2.4, 3.6, 5.2, 7][i]), wavelength, reverse)),
    bands: BANDS.map(([, , , seconds, reverse]) => slide(t, pace(seconds, 4), BAND_PERIOD, reverse)),
    glints: GLINTS.map(([, , , , seconds, delay]) => {
      const g = swell(t, seconds, delay);
      return { opacity: 0.95 * g, x: 2 * g, sx: 0.6 + 0.4 * g };
    }),
    ripples: mood === "listening"
      ? [{ scale: 1, opacity: 0 }, { scale: 1, opacity: 0 }]
      : [ripple(t, bobPeriod, 0), ripple(t, bobPeriod, bobPeriod / 2)],
    wake: [0, 0.5].map((o) => ({ offset: -74 * phaseOf(t + o, 1), opacity: working ? 0.7 : 0 })),
    rings: [0, 1, 2].map((i) => {
      const q = phaseOf(t / 2.4 + i / 3, 1);
      return { r: 6 + 22 * q, opacity: listen * (q < 0.15 ? q / 0.15 : 1 - q) * 0.85 };
    }),
  };
}

/** The goose at rest, for reduced motion: every part where it belongs, the glints showing. */
export function restingParts(mood: GooseMood): PartsFrame {
  "worklet";
  const frame = partsFrame(0, mood, mood === "listening" ? 1 : 0);
  return { ...frame, glints: frame.glints.map(() => ({ opacity: 0.6, x: 0, sx: 1 })), ripples: frame.ripples.map(() => ({ scale: 1, opacity: 0 })) };
}
