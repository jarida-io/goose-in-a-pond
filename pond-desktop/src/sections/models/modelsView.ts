import type {
  DownloadEntry,
  ModelActiveRoles,
  ModelEntry,
  ModelMemoryStatus,
} from "../../api/types";
import { DEFAULT_HEADROOM_MB, modelFitFor, modelResidencyMb } from "../../api/modelFit";

/**
 * The arithmetic behind the Models page, kept out of the component.
 *
 * Everything here is a pure function of data the pond already reports, so the
 * numbers on screen can be tested without a browser — and the page can be read
 * without working out what a percentage is measuring.
 */

/** The four jobs a pond needs filled, in the order they matter to a household. */
export const ROLES = [
  { key: "chat", label: "Conversation", blurb: "Answers you" },
  { key: "asr", label: "Listening", blurb: "Turns speech into words" },
  { key: "tts", label: "Speaking", blurb: "Turns words into speech" },
  { key: "embedding", label: "Memory", blurb: "Finds what it knows" },
] as const;

export type RoleKey = (typeof ROLES)[number]["key"];

/** Which model currently holds a job, or `null` when nothing does. */
export function roleHolder(roles: ModelActiveRoles | null, key: RoleKey): string | null {
  const slot = roles?.[key];
  if (!slot) return null;
  const name = (slot as { model?: string | null }).model;
  return name?.trim() ? name : null;
}

/**
 * A size, in the largest unit that keeps it readable.
 *
 * Megabytes past a few thousand stop being a quantity anybody pictures — "4.1
 * GB" is a size, "4198 MB" is a number you have to divide.
 */
export function formatSize(mb: number | null | undefined): string {
  if (mb == null || mb <= 0) return "";
  if (mb < 1024) return `${Math.round(mb)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

/** How far a transfer has got, or `null` when the total is not yet known. */
export function downloadPercent(d: Pick<DownloadEntry, "downloaded_bytes" | "total_bytes">): number | null {
  if (!d.total_bytes || d.total_bytes <= 0) return null;
  return Math.min(100, Math.round((d.downloaded_bytes / d.total_bytes) * 100));
}

/** Bytes, for the line under a progress bar. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || bytes <= 0) return "0 MB";
  const mb = bytes / 1_048_576;
  return formatSize(mb) || "0 MB";
}

/** A transfer that has finished, one way or another, is not in flight. */
export function isInFlight(d: Pick<DownloadEntry, "status">): boolean {
  return d.status === "downloading" || d.status === "paused";
}

export interface FitReading {
  verdict: "fits" | "spills" | "unknown";
  /** Share of the device's model budget this would occupy, 0-100+. */
  percent: number | null;
  /** One line, in the household's terms rather than the machine's. */
  label: string;
}

/**
 * Will this model run on this device, and how much of the budget does it take?
 *
 * The page's one real claim. A model list that does not answer it makes you
 * download several gigabytes to find out, which on a home device is the whole
 * evening.
 *
 * `unknown` is reported honestly rather than guessed: the memory budget is
 * absent on desktop dev machines and on any build without the scheduler, and a
 * confident bar drawn from nothing is worse than no bar.
 */
export function fitReading(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "reads_images" | "image_support_bytes">,
  memory: ModelMemoryStatus | null | undefined,
): FitReading {
  const verdict = modelFitFor(m, memory);
  const size = modelResidencyMb(m);
  const raw = memory?.available_for_llm_mb ?? 0;

  if (verdict === "unknown" || !size || raw <= 0) {
    return { verdict: "unknown", percent: null, label: "Size unknown on this device" };
  }

  // Measured against what a model can ACTUALLY have, not the raw figure.
  //
  // `modelFit` reserves headroom on top of the weights for the KV cache and
  // activation buffers, so a model at 88% of the raw budget is already over.
  // Drawing the bar against the raw figure put a bar at 88% under the words
  // "larger than" — the picture and the sentence disagreeing about the same
  // model. Both now use the number that decides the verdict.
  const usable = Math.max(1, raw - DEFAULT_HEADROOM_MB);
  const percent = Math.round((size / usable) * 100);

  return {
    verdict,
    percent,
    label:
      verdict === "fits"
        ? `Uses ${percent}% of the ${formatSize(usable)} this device can give one model`
        : `Bigger than the ${formatSize(usable)} this device can give one model — it would run, slowly`,
  };
}

/** Models already on disk, newest catalog order preserved. */
export function downloadedOnly(models: ModelEntry[]): ModelEntry[] {
  return models.filter((m) => m.downloaded);
}

/**
 * Everything the catalogue knows about that is not here yet.
 *
 * This is where speech models come from. The catalogue ships whisper builds
 * and piper voices with download URLs already attached, and the page was
 * dropping every one of them on the floor by showing only what was already
 * downloaded — so a pond could add a chat model from Hugging Face but had no
 * route at all to a second voice or a better transcriber.
 */
export function availableToDownload(models: ModelEntry[]): ModelEntry[] {
  return models.filter((m) => !m.downloaded);
}

/**
 * Which roles a model can be given.
 *
 * Driven by the category the catalog put it in rather than its name: a name
 * heuristic is a copy of a backend rule that has already moved on.
 */
export function rolesFor(m: Pick<ModelEntry, "provider" | "recommended_role">): RoleKey[] {
  if (m.recommended_role && ROLES.some((r) => r.key === m.recommended_role)) {
    return [m.recommended_role as RoleKey];
  }
  switch (m.provider) {
    case "whisper":
      return ["asr"];
    case "tts":
    case "piper":
      return ["tts"];
    case "embedding":
      return ["embedding"];
    default:
      return ["chat"];
  }
}

/**
 * Descriptions the catalogue writes when it has nothing to say.
 *
 * The filesystem scan stamps every file it discovers with "(detected on disk)".
 * The client maps a model's description to its display name, so those arrived
 * on screen as a card called "(detected on disk)" — a name that identifies
 * nothing, on the very models a person is least likely to recognise, while the
 * filename that WOULD identify them sat unused.
 */
const PLACEHOLDER_DESCRIPTIONS = ["(detected on disk)"];

/** A model's name as a person would read it. */
export function modelLabel(m: Pick<ModelEntry, "display_name" | "name">): string {
  const shown = m.display_name?.trim();
  if (!shown || PLACEHOLDER_DESCRIPTIONS.includes(shown)) return m.name;
  return shown;
}

/** 131072 → "131,072". A context window is long enough that the grouping is
 *  what makes it readable at a glance. */
function thousands(n: number): string {
  return n.toLocaleString("en-US");
}

/**
 * The facts worth showing beside a model's name.
 *
 * Read from the structured fields the catalogue persists — which, for a model
 * discovered on disk, the server now fills from the file's own GGUF header
 * rather than leaving empty. Built here from those fields rather than from the
 * description string, so the page lays them out instead of re-parsing a
 * sentence somebody else formatted.
 *
 * Absent facts are simply absent. A row reading "unknown · unknown" is worse
 * than a row with a name and a size.
 */
export function modelFacts(
  m: Pick<
    ModelEntry,
    "quantization" | "context_length" | "asr_size" | "asr_language" | "reads_images"
  >,
): string[] {
  const out: string[] = [];
  if (m.quantization) out.push(m.quantization);
  if (m.context_length && m.context_length > 0) out.push(`${thousands(m.context_length)} ctx`);
  if (m.asr_size) out.push(m.asr_size);
  if (m.asr_language) out.push(m.asr_language === "en" ? "English" : "Multilingual");
  // `=== true`, never truthy: `reads_images` is device-aware and absent for
  // anything the server has not classified, and absent must read as "no fact
  // to show" rather than "yes".
  if (m.reads_images === true) out.push("Reads pictures");
  return out;
}

export interface JobGroup {
  key: RoleKey;
  label: string;
  models: ModelEntry[];
}

/**
 * Downloaded models, gathered under the job each one can do.
 *
 * The same four words the Jobs band uses, so "Listening" means one thing on
 * this page rather than "ASR" up top and "Whisper" further down. Empty groups
 * are dropped — a heading over nothing is a question the page cannot answer.
 */
export function groupByJob(models: ModelEntry[]): JobGroup[] {
  return ROLES.map((role) => ({
    key: role.key,
    label: role.label,
    models: models.filter((m) => rolesFor(m).includes(role.key)),
  })).filter((g) => g.models.length > 0);
}
