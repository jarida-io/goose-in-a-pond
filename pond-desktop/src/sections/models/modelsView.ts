import type {
  HfModelFile,
  ModelActiveRoles,
  ModelCompanion,
  ModelEntry,
  ModelMemoryStatus,
  ModelRecommendation,
} from "../../api/types";
import { DEFAULT_HEADROOM_MB, modelResidencyMb, usableBudgetMb } from "../../api/modelFit";
import { engineOf, providerOf } from "../../lib/modelProvider";
import i18n from "../../i18n";

// Pure arithmetic and wording behind the Models surfaces, kept out of the components so it tests
// without a browser. The classic page, the hub screen and the no-model panel all read it.

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

/** What a job's card reads when nothing is named: memory runs on its provider's own model, so
 *  that is not an empty job, and an unfilled one is. */
export function emptyJobText(roles: ModelActiveRoles | null, key: RoleKey): string | null {
  if (key !== "embedding") return null;
  const provider = roles?.embedding?.provider;
  if (provider === "none") return "Switched off";
  return provider ? "Built-in default" : null;
}

// ─── Sizes ─────────────────────────────────────────────────────────────────

/** Bytes as the pond's own download messages write them: decimal GB from 1 GB up, whole MiB below. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || bytes <= 0) return "0 MB";
  if (bytes >= 1_000_000_000) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${Math.floor(bytes / 1_048_576)} MB`;
}

/** A size in MB, as the registry and the memory budget count it, written the same way. */
export function formatSize(mb: number | null | undefined): string {
  if (mb == null || mb <= 0) return "";
  return formatBytes(mb * 1_048_576);
}

/** 131072 → "131,072". */
function thousands(n: number): string {
  return n.toLocaleString("en-US");
}

// ─── Fit ───────────────────────────────────────────────────────────────────

export type FitState = "in_use" | "fits" | "too_big" | "unknown" | "outside" | "not_applicable";

export interface FitReading {
  state: FitState;
  /** Share of the room for one model this would take, 1-100; only when it fits. */
  percent: number | null;
  /** One line in the household's terms, for a tooltip and the screen reader. */
  label: string;
  /** What the row shows in the fit column. */
  text: string;
  /** A way out, when there is one: over budget only because of the add-on. */
  hint: string | null;
}

export interface FitOptions {
  inUse?: boolean;
  /** The person is including picture support, so it counts against the room too. */
  withPictures?: boolean;
}

/** Whether a model fits the room this pond has for one model. The model in use reads "In use";
 *  one over budget reads "Too big for this pond", never a percentage; with no budget reported
 *  (desktop dev machines) it says so rather than guessing. */
export function fitReading(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "companions" | "engine" | "category" | "provider">,
  memory: ModelMemoryStatus | null | undefined,
  opts: FitOptions = {},
): FitReading {
  if (opts.inUse) return { state: "in_use", percent: null, label: "In use", text: "In use", hint: null };

  const engine = engineOf(m);
  // Speech, voices and embeddings are not weighed against the room for a conversation model.
  if (!engine) return { state: "not_applicable", percent: null, label: "", text: "", hint: null };
  if (!engine.in_process) {
    const text = `Runs in ${engine.label}`;
    return {
      state: "outside",
      percent: null,
      label: `${text}, so this pond's memory budget does not apply`,
      text,
      hint: null,
    };
  }

  const usable = usableBudgetMb(memory);
  const size = modelResidencyMb(m, { withPictures: opts.withPictures });
  if (usable === null || !size) {
    return {
      state: "unknown",
      percent: null,
      label: "This machine does not report a memory budget, so fit is not checked",
      text: "—",
      hint: null,
    };
  }
  if (size > usable) {
    const alone = opts.withPictures ? modelResidencyMb(m) : null;
    return {
      state: "too_big",
      percent: null,
      label: `Needs ${formatSize(size)} and this pond has ${formatSize(usable) || "0 MB"} for one model`,
      text: "Too big for this pond",
      hint: alone !== null && alone <= usable ? "It fits without picture support." : null,
    };
  }
  const percent = Math.max(1, Math.min(100, Math.round((size / usable) * 100)));
  return {
    state: "fits",
    percent,
    label: `Uses ${percent}% of the ${formatSize(usable)} this pond has for one model`,
    text: `${percent}%`,
    hint: null,
  };
}

/** The header's figure: the same room for one model the rows are judged against. Never blank. */
export function budgetReading(memory: ModelMemoryStatus | null | undefined): {
  text: string;
  label: string;
} {
  const usable = usableBudgetMb(memory);
  if (usable === null) {
    return {
      text: "—",
      label: "This machine does not report a memory budget, so fit is not checked",
    };
  }
  return {
    text: formatSize(usable) || "0 MB",
    label: `The most one model can weigh here, with ${formatSize(DEFAULT_HEADROOM_MB)} kept back for its working memory`,
  };
}

// ─── What a row says ───────────────────────────────────────────────────────

/** Models whose catalogue row says they are on disk. */
export function downloadedOnly(models: ModelEntry[]): ModelEntry[] {
  return models.filter((m) => m.downloaded);
}

/** Roles a model can take, by catalogue category: a name heuristic would copy a backend rule. */
export function rolesFor(
  m: Pick<ModelEntry, "provider" | "recommended_role" | "kind">,
): RoleKey[] {
  const llm = ["gguf", "litert", "llamafile", "ollama"].includes(m.provider);
  // A helper (the tool-call model, a companion) is never offered as conversation.
  if (llm && m.kind === "helper") return [];
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
      return llm || m.kind === undefined ? ["chat"] : [];
  }
}

/** Descriptions the disk scan writes when it has nothing to say; never shown as a name. */
const PLACEHOLDER_DESCRIPTIONS = ["(detected on disk)"];

/** A model's name as a person would read it: its title, else its description unless that is a
 *  placeholder, else its file name. Voices keep their own titles. */
export function modelLabel(
  m: Pick<ModelEntry, "title" | "display_name" | "name" | "category" | "provider">,
): string {
  const kind = m.category ?? m.provider;
  const voice = kind.startsWith("tts");
  const shown = (voice ? m.display_name : m.title)?.trim();
  if (shown && !PLACEHOLDER_DESCRIPTIONS.includes(shown)) return shown;
  const described = m.display_name?.trim();
  if (described && !PLACEHOLDER_DESCRIPTIONS.includes(described)) return described;
  return m.name;
}

/** A name that tells two rows with one title apart, for a screen reader: "Gemma 4 E4B, LiteRT-LM". */
export function modelFullName(
  m: Pick<ModelEntry, "title" | "display_name" | "name" | "category" | "provider" | "engine">,
): string {
  const engine = engineOf(m);
  return engine ? `${modelLabel(m)}, ${engine.label}` : modelLabel(m);
}

export type SourceChip = "Recommended" | "Added" | "Found on disk" | "Ollama";

/** Where a row came from, only where that informs: the catalogue earns no chip. */
export function sourceChip(
  m: Pick<ModelEntry, "recommended" | "provenance">,
): SourceChip | null {
  if (m.recommended) return "Recommended";
  switch (m.provenance) {
    case "added":
      return "Added";
    case "on_disk":
      return "Found on disk";
    case "ollama":
      return "Ollama";
    default:
      return null;
  }
}

/** Facts beside a model's name, from the structured (GGUF-header) fields; absent ones are omitted. */
export function modelFacts(
  m: Pick<ModelEntry, "quantization" | "context_length" | "asr_size" | "asr_language">,
): string[] {
  const out: string[] = [];
  if (m.quantization) out.push(m.quantization);
  if (m.context_length && m.context_length > 0) out.push(`${thousands(m.context_length)}-token window`);
  if (m.asr_size) out.push(m.asr_size);
  if (m.asr_language) out.push(m.asr_language === "en" ? "English" : "Multilingual");
  return out;
}

// ─── Picture support ───────────────────────────────────────────────────────

export type AddOnKind =
  | "none" | "included" | "add" | "adding" | "checking" | "text_only" | "unavailable";

export interface AddOn {
  kind: AddOnKind;
  /** What the row reads. */
  text: string;
  bytes: number | null;
}

export function picturesOf(m: Pick<ModelEntry, "companions">): ModelCompanion | undefined {
  return m.companions?.find((c) => c.kind === "pictures");
}

/** The row's line about picture support. Only llama.cpp and LiteRT-LM rows carry one: Ollama and
 *  llamafile read pictures their own way, so claiming "text only" for them would be wrong. */
export function addOnOf(
  m: Pick<ModelEntry, "companions" | "engine" | "category" | "provider">,
): AddOn {
  const id = engineOf(m)?.id;
  if (id !== "llama_cpp" && id !== "litert_lm") return { kind: "none", text: "", bytes: null };
  const pictures = picturesOf(m);
  if (!pictures) return { kind: "text_only", text: "Text only", bytes: null };
  switch (pictures.state) {
    case "installed":
      return { kind: "included", text: "Pictures included", bytes: pictures.size_bytes };
    case "downloading":
      return { kind: "adding", text: "Adding pictures", bytes: pictures.size_bytes };
    case "verifying":
      return { kind: "checking", text: "Checking picture support", bytes: pictures.size_bytes };
    case "not_on_this_device":
      return { kind: "unavailable", text: "Pictures aren't available on this device", bytes: null };
    default:
      return {
        kind: "add",
        text: `Add pictures · ${formatBytes(pictures.size_bytes)}`,
        bytes: pictures.size_bytes,
      };
  }
}

/** Whether this device carries picture add-ons, read from the rows that have one: false only when
 *  every one of them says it does not, null when none says anything. */
export function carriesPictures(models: Pick<ModelEntry, "companions">[]): boolean | null {
  const states = models.flatMap((m) => {
    const pictures = picturesOf(m);
    return pictures ? [pictures.state] : [];
  });
  if (states.length === 0) return null;
  return states.some((state) => state !== "not_on_this_device");
}

/** Whether a download of this row can bring picture support along, for the tick box. */
export function offersPictures(m: Pick<ModelEntry, "companions" | "downloaded">): boolean {
  return !m.downloaded && picturesOf(m)?.state === "available";
}

/** Said beside a picture box that starts unticked because only the model fits. */
export const PICTURES_LEFT_OUT = "Left out: with pictures it would not fit this pond. Tick to include it anyway.";

/** The model fits the room this pond has for one model, but not together with the picture support its
 *  download would bring. That is the one case the box starts unticked: when the add-on fits, or the
 *  pond cannot say, it starts ticked and nothing drops it silently. The same readings the card shows. */
export function fitsOnlyWithoutPictures(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "companions" | "downloaded" | "engine" | "category" | "provider">,
  memory: ModelMemoryStatus | null | undefined,
): boolean {
  if (!offersPictures(m)) return false;
  return (
    fitReading(m, memory).state === "fits" &&
    fitReading(m, memory, { withPictures: true }).state === "too_big"
  );
}

/** The same question for a file Hugging Face lists, from the sizes its listing gives. A file whose
 *  size is not listed cannot be weighed, so its add-on stays in. */
export function fileFitsOnlyWithoutPictures(
  file: Pick<HfModelFile, "size_mb" | "pictures">,
  memory: ModelMemoryStatus | null | undefined,
): boolean {
  if (!file.pictures || !file.size_mb || file.size_mb <= 0) return false;
  return fitsOnlyWithoutPictures(
    {
      size_mb: file.size_mb,
      category: "gguf",
      provider: "gguf",
      downloaded: false,
      companions: [
        { kind: "pictures", label: file.pictures.label, size_bytes: file.pictures.size_bytes, state: "available" },
      ],
    },
    memory,
  );
}

/** The size a person is about to spend, said before they spend it: "4.2 GB + 945 MB for pictures". */
export function downloadSummary(
  m: Pick<ModelEntry, "size_mb" | "companions">,
  includePictures: boolean,
): string {
  const size = formatSize(m.size_mb) || "Size unknown";
  const pictures = picturesOf(m);
  return pictures?.state === "available" && includePictures
    ? `${size} + ${formatBytes(pictures.size_bytes)} for pictures`
    : size;
}

// ─── Recommended picks ─────────────────────────────────────────────────────

const RANK_ORDER: Record<ModelRecommendation["rank"], number> = {
  primary: 0,
  lighter: 1,
  alternative: 2,
};

/** GIAP's suggestions for conversation, best first. */
export function recommendedPicks(models: ModelEntry[]): ModelEntry[] {
  return models
    .filter((m) => m.recommended && rolesFor(m).includes("chat"))
    .sort((a, b) => RANK_ORDER[a.recommended!.rank] - RANK_ORDER[b.recommended!.rank]);
}

/** "5 Oct 2026" from "2026-10-05". */
function plainDate(iso: string): string {
  const d = new Date(`${iso}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" });
}

/** The measured numbers for a pick, only where the server sent them for this kind of machine. */
export function measuredOf(
  r: ModelRecommendation | undefined,
): { text: string; caption: string } | null {
  const measured = r?.measured;
  if (!measured) return null;
  return {
    text: measured.summary,
    caption: `Measured on this kind of device, ${plainDate(measured.measured_on)}`,
  };
}

/** The pick that carries the page's one raised card: the one in use, else the one GIAP puts first. */
export function raisedPick(picks: ModelEntry[], roles: ModelActiveRoles | null): ModelEntry | null {
  return picks.find((m) => isInUse(m, roles)) ?? picks[0] ?? null;
}

/** The pick whose button is the page's one question: with no conversation model in use, the
 *  first suggestion, unless it is already coming down. */
export function askingPick(
  picks: ModelEntry[],
  roles: ModelActiveRoles | null,
  comingDown: (m: ModelEntry) => boolean,
): ModelEntry | null {
  if (roleHolder(roles, "chat")) return null;
  const first = picks[0];
  return first && !comingDown(first) ? first : null;
}

// ─── Who holds a job ───────────────────────────────────────────────────────

/** Whether the job's holder is this row. Conversation also compares the provider, through
 *  `providerOf`: the role records `local` for a GGUF or LiteRT-LM file. */
function holds(
  roles: ModelActiveRoles | null,
  key: RoleKey,
  m: Pick<ModelEntry, "name" | "category" | "provider">,
): boolean {
  const slot = roles?.[key] as { provider?: string; model?: string | null } | null | undefined;
  const name = slot?.model?.trim();
  if (!name || name !== m.name) return false;
  if (key !== "chat" || !slot?.provider) return true;
  return providerOf(slot.provider) === providerOf(m.category ?? m.provider);
}

export function isInUse(
  m: Pick<ModelEntry, "name" | "category" | "provider" | "recommended_role" | "kind">,
  roles: ModelActiveRoles | null,
): boolean {
  return rolesFor(m).some((key) => holds(roles, key, m));
}

/** The row a job's holder is, so its card can say a title and an engine, not a file stem. */
export function holderEntry(
  models: ModelEntry[],
  roles: ModelActiveRoles | null,
  key: RoleKey,
): ModelEntry | null {
  return models.find((m) => rolesFor(m).includes(key) && holds(roles, key, m)) ?? null;
}

// ─── Grouping ──────────────────────────────────────────────────────────────

export interface JobGroup {
  key: RoleKey;
  label: string;
  models: ModelEntry[];
}

/** Recommended picks first, best first, then by title. */
export function sortModels(models: ModelEntry[]): ModelEntry[] {
  return [...models].sort((a, b) => {
    const rank = (m: ModelEntry) => (m.recommended ? RANK_ORDER[m.recommended.rank] : 9);
    return rank(a) - rank(b) || modelLabel(a).localeCompare(modelLabel(b));
  });
}

/** Models grouped by job, in `ROLES` order and wording; empty groups dropped. */
export function groupByJob(models: ModelEntry[]): JobGroup[] {
  return ROLES.map((role) => ({
    key: role.key,
    label: role.label,
    models: models.filter((m) => rolesFor(m).includes(role.key)),
  })).filter((g) => g.models.length > 0);
}

/** Why this pond cannot run `m`, in the household's words; null when it can. The pond refuses to
 *  download or use such a model, so every surface shows this instead of those buttons. */
export function unavailableNote(
  m: Pick<ModelEntry, "available" | "unavailable_reason">,
): string | null {
  if (m.available !== false) return null;
  return i18n.t(`models.unavailable.${m.unavailable_reason ?? "other"}`, {
    defaultValue: i18n.t("models.unavailable.other"),
  });
}
