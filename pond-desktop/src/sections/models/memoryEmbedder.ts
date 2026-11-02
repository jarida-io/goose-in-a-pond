import type { ContextIndexHealth, ModelActiveRoles, ModelEntry } from "../../api/types";
import { formatSize, modelLabel, rolesFor } from "./modelsView";

// The wording and arithmetic behind the Memory job, read by the classic page, the hub and the Memory screens.

/** The Memory job's models, the pond's recommendation first and the rest as the catalogue lists them. */
export function memoryModels(models: ModelEntry[]): ModelEntry[] {
  const rows = models.filter((m) => rolesFor(m).includes("embedding"));
  const picked = (m: ModelEntry) => (m.recommended ? 0 : 1);
  return rows
    .map((m, i) => ({ m, i }))
    .sort((a, b) => picked(a.m) - picked(b.m) || a.i - b.i)
    .map(({ m }) => m);
}

/** Memory runs only while the provider setting is not "none". */
export function memoryIsOff(roles: ModelActiveRoles | null): boolean {
  return roles?.embedding?.provider === "none";
}

/** The catalogue name Memory runs on; with none chosen the pond runs what its old engine setting implied. */
export function resolvedMemoryName(roles: ModelActiveRoles | null): string | null {
  const slot = roles?.embedding;
  if (!slot || slot.provider === "none") return null;
  const named = slot.model?.trim();
  if (named) return named;
  return slot.provider === "gguf" ? "nomic-embed-text-v1.5" : "all-MiniLM-L6-v2";
}

/** Who holds the Memory job for a card: the model's name, its row where the catalogue has one, and
 *  whether the household never chose it. Null when Memory is off. */
export function memoryHolder(
  models: ModelEntry[],
  roles: ModelActiveRoles | null,
): { name: string; entry: ModelEntry | null; builtIn: boolean } | null {
  const name = resolvedMemoryName(roles);
  if (!name) return null;
  const entry = memoryModels(models).find((m) => m.name === name) ?? null;
  return { name, entry, builtIn: !roles?.embedding?.model?.trim() };
}

/** Whether `m` is the model Memory runs on. */
export function holdsMemory(m: Pick<ModelEntry, "name">, roles: ModelActiveRoles | null): boolean {
  return resolvedMemoryName(roles) === m.name;
}

/** A model's name as the household reads it; EmbeddingGemma 2 stamps its vectors with the 256-wide cut. */
export function embedderTitle(modelId: string | null | undefined): string {
  if (!modelId) return "the Memory model";
  if (modelId.startsWith("embeddinggemma-2")) return "EmbeddingGemma 2";
  if (modelId === "nomic-embed-text-v1.5") return "Nomic Embed Text v1.5";
  return modelId;
}

/** The built-in default, which every surface can name as the lighter choice without the catalogue. */
const LIGHTER_CHOICE = "all-MiniLM-L6-v2";
const LIGHTER_CHOICE_TEXT = `${LIGHTER_CHOICE} (23 MB)`;

/** A load that failed for want of room, which asking again rarely fixes. */
export function isOutOfMemory(reason: string | null | undefined): boolean {
  return /out of memory|\boom\b|failed to allocate|cannot allocate|unable to allocate|insufficient memory/i.test(reason ?? "");
}

/** Shown where a press cannot start the download it would need. */
export const NEEDS_CONNECTION = "Needs a connection to download";

/** What a model costs and where its file is: the size is said before anyone presses Use. */
export function memoryCost(m: Pick<ModelEntry, "size_mb" | "downloaded" | "acquire">): string {
  const size = formatSize(m.size_mb);
  if (m.acquire === "external") {
    return size ? `${size}, fetched the first time it is used` : "Fetched the first time it is used";
  }
  if (m.downloaded) return size ? `On this device, ${size}` : "On this device";
  return size ? `${size} to download` : "Size not listed";
}

/** What a model needs while it runs, from the pond's estimate; null where the catalogue lists none. */
export function memoryNeed(m: Pick<ModelEntry, "ram_estimate_mb">): string | null {
  const size = formatSize(m.ram_estimate_mb);
  return size ? `Needs about ${size} while it runs` : null;
}

export type MemoryFit = "fits" | "tight";

export const FIT_WORD: Record<MemoryFit, string> = { fits: "Fits", tight: "Tight on this pond" };

/** Whether a model's need fits the room the pond reports for a Memory model; null where either is unknown. */
export function memoryFit(
  m: Pick<ModelEntry, "ram_estimate_mb">,
  roomMb: number | null | undefined,
): MemoryFit | null {
  if (roomMb == null || !m.ram_estimate_mb) return null;
  return m.ram_estimate_mb <= roomMb ? "fits" : "tight";
}

export type EmbedderKind =
  | "off" | "waiting" | "loading" | "downloading" | "missing" | "failed" | "reindexing" | "ready";

export interface EmbedderLine {
  kind: EmbedderKind;
  text: string;
  /** The pond's own words for a failed load, for someone who has to look into it. */
  detail: string | null;
  /** Asking again can help; false where the build cannot run the model at all. */
  retry: boolean;
  /** What the button says before and while it asks. */
  retryLabel: string;
  retryingLabel: string;
  /** Reading again only: items that can be found by meaning, of those there are across every store. */
  progress: { done: number; total: number } | null;
  /** What a screen reader hears where `text` changes on every poll; the count stays on the progress bar. */
  spoken: string | null;
  /** The model the line tells the household to choose instead, by catalogue name. */
  wayOut: string | null;
}

/** A failed load that is only the pond waiting on a file it fetches itself, which is no fault. */
export function isFetching(reason: string | null | undefined): boolean {
  return /not on disk|is downloading/i.test(reason ?? "");
}

function line(kind: EmbedderKind, text: string, over: Partial<EmbedderLine> = {}): EmbedderLine {
  return {
    kind, text, detail: null, retry: false, retryLabel: "Try again", retryingLabel: "Trying again",
    progress: null, spoken: null, wayOut: null, ...over,
  };
}

/** What to say about the embedder now; null when the pond cannot say. A missing file is progress only
 *  while `downloading`; otherwise it is a file the household may have stopped and can ask for again. */
export function embedderLine(
  health: ContextIndexHealth | null,
  opts: { downloading?: boolean } = {},
): EmbedderLine | null {
  const embedder = health?.embedder;
  if (!health || !embedder) return null;
  const title = embedderTitle(embedder.model_id);

  switch (embedder.state) {
    case "off":
      return line("off", "Search by meaning is off. Memory searches by keyword.");
    case "chosen":
      return line("waiting", `Searching by keyword until ${title} loads at a quiet moment`);
    case "loading":
      return line("loading", `Searching by keyword while ${title} loads`);
    case "failed": {
      if (opts.downloading) {
        return line("downloading", `Searching by keyword until ${title} finishes downloading`);
      }
      if (isFetching(embedder.reason)) {
        return line("missing", `${title} is not on this device yet, so Memory is searching by keyword.`, {
          retry: true, retryLabel: "Download", retryingLabel: "Starting",
        });
      }
      const reason = embedder.reason?.trim() || null;
      if (reason && /local-inference|leaves out/i.test(reason)) {
        return line("failed", `This build of the pond cannot run ${title}. Choose another Memory model.`, {
          detail: reason,
        });
      }
      if (isOutOfMemory(reason)) {
        const lighter = embedder.model_id !== LIGHTER_CHOICE;
        const next = lighter ? `Choose the lighter ${LIGHTER_CHOICE_TEXT}, or try again.` : "Try again later.";
        return line(
          "failed",
          `There is not enough free memory to load ${title}, so Memory is searching by keyword. ${next}`,
          { detail: reason, retry: true, wayOut: lighter ? LIGHTER_CHOICE : null },
        );
      }
      return line(
        "failed",
        `${title} did not load, so Memory is searching by keyword. Try again, or choose another Memory model.`,
        { detail: reason, retry: true },
      );
    }
    case "ready": {
      if (!health.indexed) return null;
      const total = health.rows ?? 0;
      const done = Math.min(health.matching ?? 0, total);
      if (done < total) {
        return line("reindexing", `Reading memories again: ${done} of ${total} items`, {
          progress: { done, total }, spoken: "Reading memories again",
        });
      }
      return line("ready", `${title} is ready. Every memory can be found by meaning.`);
    }
  }
}

/** Whether the page should keep asking: the embedder is still moving toward ready. */
export function embedderWatching(health: ContextIndexHealth | null): boolean {
  const embedder = health?.embedder;
  if (!health || !embedder) return false;
  switch (embedder.state) {
    case "chosen":
    case "loading":
      return true;
    case "failed":
      return isFetching(embedder.reason);
    case "ready": {
      if (!health.indexed) return false;
      return (health.matching ?? 0) < (health.rows ?? 0);
    }
    case "off":
      return false;
  }
}

type ChosenModel = Pick<
  ModelEntry,
  "title" | "display_name" | "name" | "category" | "provider" | "downloaded" | "acquire" | "size_mb"
>;

/** What the pond says once Memory has been given a model: a download first where the file is not here,
 *  and while search by meaning is off, that the model waits for the switch. */
export function memoryChosenText(m: ChosenModel, opts: { off?: boolean } = {}): string {
  const title = modelLabel(m);
  const size = formatSize(m.size_mb);
  const needsFile = m.acquire === "download" && !m.downloaded;
  if (opts.off) {
    return needsFile
      ? `${title} is set for memory. Once you turn on search by meaning, the pond downloads it${size ? ` (${size})` : ""} and uses it.`
      : `${title} is set for memory. The pond uses it once you turn on search by meaning.`;
  }
  if (needsFile) {
    return `Using ${title} for memory once it downloads${size ? ` (${size})` : ""}. Memory searches by keyword until then.`;
  }
  return `Now using ${title} for memory. The pond reads memories again at the next quiet moment.`;
}

export type HolderTone = "ok" | "wait" | "busy" | "stopped";

/** What the row of the model Memory is set to may claim, from where the embedder really is. "In use"
 *  only once it runs; before the pond's first answer it claims nothing, and silence after that is
 *  not a fault, so it keeps the household's choice. */
export function holderBadge(
  line: EmbedderLine | null,
  opts: { checking?: boolean } = {},
): { label: string; tone: HolderTone } {
  if (!line && opts.checking) return { label: "Checking", tone: "busy" };
  switch (line?.kind) {
    case "waiting":
      return { label: "Waiting to load", tone: "wait" };
    case "loading":
      return { label: "Loading", tone: "busy" };
    case "downloading":
      return { label: "Downloading", tone: "busy" };
    case "missing":
      return { label: "Chosen, not downloaded", tone: "stopped" };
    case "failed":
      return { label: "Chosen, not loaded", tone: "stopped" };
    default:
      return { label: "In use", tone: "ok" };
  }
}

/** The Jobs card and the hub tile already name the model: they add a word only while it is not running. */
export function jobState(
  line: EmbedderLine | null,
  opts: { checking?: boolean } = {},
): { label: string; tone: HolderTone } | null {
  const badge = holderBadge(line, opts);
  return badge.tone === "ok" ? null : badge;
}

/** The hub's "Model" line: the model by name, and no more than the embedder has done with it. */
export function holderText(line: EmbedderLine | null, modelId: string | null | undefined): string {
  if (!modelId) return "Chosen on the Models screen.";
  const title = embedderTitle(modelId);
  switch (line?.kind) {
    case "waiting":
      return `${title} is chosen and loads at a quiet moment.`;
    case "loading":
      return `${title} is loading.`;
    case "downloading":
      return `${title} is downloading.`;
    case "missing":
      return `${title} is chosen but not downloaded.`;
    case "failed":
      return `${title} is chosen but did not load.`;
    default:
      return `Using ${title}.`;
  }
}

/** What pressing a model's button does: only a press that starts a download says so, and that press
 *  needs a connection. While search is off nothing is fetched, so it stays a plain Use. */
export function memoryPress(
  m: Pick<ModelEntry, "downloaded" | "acquire" | "size_mb">,
  ctx: { off: boolean; offline: boolean },
): { label: string; download: boolean; size: string; blocked: string | null } {
  const download = m.acquire === "download" && !m.downloaded && !ctx.off;
  return {
    label: download ? "Download and use" : "Use",
    download,
    size: formatSize(m.size_mb),
    blocked: download && ctx.offline ? NEEDS_CONNECTION : null,
  };
}
