/**
 * Kokoro voice display metadata, derived from the `<lang><gender>_<name>` id (`af_heart`).
 * An unknown prefix degrades to the raw id, so a new voice stays selectable.
 */

/** Quality tiers, in the order the UI should offer them. */
export const VOICE_QUALITY_TIERS = [
  {
    value: "q8",
    label: "Balanced",
    detail: "92 MB — the default. Fits comfortably beside the language model.",
    sizeMb: 92,
  },
  {
    value: "q8f16",
    label: "Compact",
    detail: "86 MB — smallest. Best on constrained hardware.",
    sizeMb: 86,
  },
  {
    value: "q4f16",
    label: "Small",
    detail: "155 MB — mid-size, four-bit weights.",
    sizeMb: 155,
  },
  {
    value: "fp16",
    label: "High",
    detail: "163 MB — half precision, closer to reference quality.",
    sizeMb: 163,
  },
  {
    value: "fp32",
    label: "Reference",
    detail: "326 MB — full precision. Slowest, and heavy on memory.",
    sizeMb: 326,
  },
] as const;

export type VoiceQuality = (typeof VOICE_QUALITY_TIERS)[number]["value"];

export const DEFAULT_QUALITY: VoiceQuality = "q8";

/** First-run tier: the smallest (Compact); the Models page can suggest a bigger one later. */
export const ONBOARDING_QUALITY: VoiceQuality = "q8f16";
/** The voice shipped by default — Kokoro's own reference voice. */
export const DEFAULT_VOICE = "af_heart";

/** Pace bounds, matching the adapter's clamp. Stored as a multiplier. */
export const MIN_PACE = 0.5;
export const MAX_PACE = 2.0;
export const DEFAULT_PACE = 1.0;

const LANGUAGES: Record<string, string> = {
  a: "American English",
  b: "British English",
  e: "Spanish",
  f: "French",
  h: "Hindi",
  i: "Italian",
  j: "Japanese",
  p: "Portuguese",
  z: "Mandarin",
};

const GENDERS: Record<string, string> = { f: "Female", m: "Male" };

export interface VoiceInfo {
  /** The id the backend uses, e.g. `af_heart`. */
  id: string;
  /** Display name, e.g. "Heart". */
  name: string;
  /** e.g. "American English", or null when the prefix is unknown. */
  language: string | null;
  /** "Female" | "Male", or null when unknown. */
  gender: string | null;
  /** Grouping label for the picker, e.g. "American English · Female". */
  group: string;
}

/** Title-case a voice's name segment: `van_dyke` → "Van Dyke". */
function titleCase(segment: string): string {
  return segment
    .split(/[_-]/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** Parse one Kokoro voice id into what the UI shows. */
export function describeVoice(id: string): VoiceInfo {
  const match = /^([a-z])([fm])_(.+)$/.exec(id);
  if (!match) {
    // Unknown shape — still selectable, just ungrouped.
    return { id, name: titleCase(id), language: null, gender: null, group: "Other" };
  }
  const [, langKey, genderKey, rest] = match;
  const language = LANGUAGES[langKey] ?? null;
  const gender = GENDERS[genderKey] ?? null;
  if (!language || !gender) {
    return { id, name: titleCase(rest), language, gender, group: "Other" };
  }
  return { id, name: titleCase(rest), language, gender, group: `${language} · ${gender}` };
}

/** Group voices for a `<select>`: English first, then alphabetical, "Other" last. */
export function groupVoices(ids: string[]): { group: string; voices: VoiceInfo[] }[] {
  const groups = new Map<string, VoiceInfo[]>();
  for (const info of ids.map(describeVoice)) {
    const list = groups.get(info.group) ?? [];
    list.push(info);
    groups.set(info.group, list);
  }

  const rank = (g: string) => {
    if (g === "Other") return 3;
    if (g.startsWith("American English")) return 0;
    if (g.startsWith("British English")) return 1;
    return 2;
  };

  return [...groups.entries()]
    .map(([group, voices]) => ({
      group,
      voices: voices.sort((a, b) => a.name.localeCompare(b.name)),
    }))
    .sort((a, b) => rank(a.group) - rank(b.group) || a.group.localeCompare(b.group));
}

/**
 * Catalogue title, `af_heart` → `Af_Heart`, keeping the prefix the picker drops. Derived, since
 * `ModelRecord.name` must stay the lowercase id the engine loads `<name>.bin` from.
 */
export function voiceTitle(id: string): string {
  return id
    .split("_")
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join("_");
}


/**
 * Overall grades, verbatim from Kokoro's `VOICES.md` (training-data quality and quantity).
 * English only, the voices the catalogue seeds; an unlisted voice shows no grade.
 */
const VOICE_GRADES: Record<string, string> = {
  // American English — female
  af_heart: "A", af_bella: "A-", af_nicole: "B-", af_aoede: "C+", af_kore: "C+",
  af_sarah: "C+", af_alloy: "C", af_nova: "C", af_sky: "C-", af_jessica: "D",
  af_river: "D",
  // American English — male
  am_fenrir: "C+", am_michael: "C+", am_puck: "C+", am_echo: "D", am_eric: "D",
  am_liam: "D", am_onyx: "D", am_santa: "D-", am_adam: "F+",
  // British English — female
  bf_emma: "B-", bf_isabella: "C", bf_alice: "D", bf_lily: "D",
  // British English — male
  bm_fable: "C", bm_george: "C", bm_lewis: "D+", bm_daniel: "D",
};

/** Notes Kokoro's own table records as traits, for the few voices that carry one. */
const VOICE_NOTES: Record<string, string> = {
  af_heart: "Reference voice",
  af_bella: "Most training data",
  af_nicole: "Close-mic",
};

/** Kokoro's published grade for a voice, or null when it publishes none. */
export function gradeFor(id: string): string | null {
  return VOICE_GRADES[id] ?? null;
}

/** The trait Kokoro's table records, if any. */
export function noteFor(id: string): string | null {
  return VOICE_NOTES[id] ?? null;
}

/** Sort key: A before F, ungraded voices last. */
export function gradeRank(id: string): number {
  const g = VOICE_GRADES[id];
  if (!g) return 99;
  const letter = "ABCDF".indexOf(g[0]);
  const modifier = g[1] === "+" ? -0.3 : g[1] === "-" ? 0.3 : 0;
  return letter + modifier;
}

/** Preview lines, varied so a few plays cover statement, number, time and a soft close. */
export const PREVIEW_STATEMENTS: string[] = [
  "Your four o'clock moved to Thursday. I've left the morning open.",
  "It's sixty-eight inside, and clear until about four.",
  "The front door locked itself at eleven, same as it always does.",
  "I've turned the patio lights down to forty percent.",
  "You asked me to mention the water filter. It's been three months.",
  "Nothing needs you right now.",
  "I live here on your shelf, I think on my own, and nothing you say to me leaves this room.",
];

/** Cycles, not random: comparing two voices is only fair if they say the same line. */
export function statementAt(playCount: number): string {
  return PREVIEW_STATEMENTS[playCount % PREVIEW_STATEMENTS.length];
}

/** Human label for a pace multiplier, for the slider readout. */
export function paceLabel(pace: number): string {
  if (pace < 0.7) return "Much slower";
  if (pace < 0.9) return "Slower";
  if (pace <= 1.1) return "Natural";
  if (pace <= 1.35) return "Faster";
  return "Much faster";
}

/** Clamp a pace value to what the engine accepts. */
export function clampPace(pace: number): number {
  if (!Number.isFinite(pace)) return DEFAULT_PACE;
  return Math.min(MAX_PACE, Math.max(MIN_PACE, pace));
}

/** Weights plus ONNX Runtime's arena; overestimates on purpose so the LLM isn't starved. */
export function tierCostMb(value: string): number {
  return Math.round(describeQuality(value).sizeMb * 1.6);
}

/** Largest tier that fits `availableMb` (by size, not list order); the default when unknown. */
export function recommendedQuality(availableMb: number | null): VoiceQuality {
  if (availableMb == null || availableMb <= 0) return DEFAULT_QUALITY;
  const affordable = [...VOICE_QUALITY_TIERS]
    .filter((t) => tierCostMb(t.value) < availableMb)
    .sort((a, b) => b.sizeMb - a.sizeMb)[0];
  return (affordable?.value ?? DEFAULT_QUALITY) as VoiceQuality;
}

/** One sentence on whether to change tier, naming the memory trade, never "higher is better". */
export function qualityAdvice(current: string, availableMb: number | null): string {
  const now = describeQuality(current);
  const best = describeQuality(recommendedQuality(availableMb));

  if (availableMb == null || availableMb <= 0) {
    return `${now.label} is the default and fits nearly anything. Higher tiers smooth out long sentences but cost memory the language model also wants.`;
  }
  if (tierCostMb(now.value) >= availableMb) {
    return `${now.label} wants about ${tierCostMb(now.value)} MB and this device has ${Math.round(availableMb)} MB free. Expect slower replies — ${best.label} is the one that fits.`;
  }
  if (best.sizeMb > now.sizeMb) {
    return `${best.label} would also fit here (${best.sizeMb} MB). It smooths out long sentences; ${now.label} is fine for short ones.`;
  }
  return `${now.label} is the best fit for this device — ${Math.round(availableMb)} MB free after the language model.`;
}

/** The tier's descriptor, falling back to the default rather than undefined. */
export function describeQuality(value: string | undefined) {
  return (
    VOICE_QUALITY_TIERS.find((t) => t.value === value) ??
    VOICE_QUALITY_TIERS.find((t) => t.value === DEFAULT_QUALITY)!
  );
}
