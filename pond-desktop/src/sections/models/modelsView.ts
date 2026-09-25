import type {
  DownloadEntry,
  ModelActiveRoles,
  ModelEntry,
  ModelMemoryStatus,
} from "../../api/types";
import { DEFAULT_HEADROOM_MB, modelFitFor, modelResidencyMb } from "../../api/modelFit";

// Pure arithmetic behind the Models page, kept out of the component so it tests without a browser.

/** The four jobs a pond needs filled, in the order they matter to a household. */
export const ROLES = [
  { key: "chat", label: "Conversation", blurb: "Answers you" },
  { key: "asr", label: "Listening", blurb: "Turns speech into words" },
  { key: "tts", label: "Speaking", blurb: "Turns words into speech" },
  { key: "embedding", label: "Memory", blurb: "Finds what it knows" },
] as const;

export type RoleKey = (typeof ROLES)[number]["key"];

export function roleHolder(roles: ModelActiveRoles | null, key: RoleKey): string | null {
  const slot = roles?.[key];
  if (!slot) return null;
  const name = (slot as { model?: string | null }).model;
  return name?.trim() ? name : null;
}

/** A size, in the largest unit that keeps it readable. */
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

/** Will this model fit this device's model budget? `unknown`, never a guess, when the budget
 *  is absent (desktop dev machines, builds without the scheduler). */
export function fitReading(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb">,
  memory: ModelMemoryStatus | null | undefined,
): FitReading {
  const verdict = modelFitFor(m, memory);
  const size = modelResidencyMb(m);
  const raw = memory?.available_for_llm_mb ?? 0;

  if (verdict === "unknown" || !size || raw <= 0) {
    return { verdict: "unknown", percent: null, label: "Size unknown on this device" };
  }

  // Same usable budget as `modelFit`'s verdict: raw minus headroom for KV cache and activations.
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

/** Everything the catalogue knows about that is not here yet. */
export function availableToDownload(models: ModelEntry[]): ModelEntry[] {
  return models.filter((m) => !m.downloaded);
}

/** Roles a model can take, by catalogue category: a name heuristic would copy a backend rule. */
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

/** Descriptions the disk scan writes when it has nothing to say; never shown as a name. */
const PLACEHOLDER_DESCRIPTIONS = ["(detected on disk)"];

/** A model's name as a person would read it. */
export function modelLabel(m: Pick<ModelEntry, "display_name" | "name">): string {
  const shown = m.display_name?.trim();
  if (!shown || PLACEHOLDER_DESCRIPTIONS.includes(shown)) return m.name;
  return shown;
}

/** 131072 → "131,072". */
function thousands(n: number): string {
  return n.toLocaleString("en-US");
}

/** Facts beside a model's name, from the structured (GGUF-header) fields; absent ones are omitted. */
export function modelFacts(
  m: Pick<ModelEntry, "quantization" | "context_length" | "asr_size" | "asr_language">,
): string[] {
  const out: string[] = [];
  if (m.quantization) out.push(m.quantization);
  if (m.context_length && m.context_length > 0) out.push(`${thousands(m.context_length)} ctx`);
  if (m.asr_size) out.push(m.asr_size);
  if (m.asr_language) out.push(m.asr_language === "en" ? "English" : "Multilingual");
  return out;
}

export interface JobGroup {
  key: RoleKey;
  label: string;
  models: ModelEntry[];
}

/** Models grouped by job, in `ROLES` order and wording; empty groups dropped. */
export function groupByJob(models: ModelEntry[]): JobGroup[] {
  return ROLES.map((role) => ({
    key: role.key,
    label: role.label,
    models: models.filter((m) => rolesFor(m).includes(role.key)),
  })).filter((g) => g.models.length > 0);
}
