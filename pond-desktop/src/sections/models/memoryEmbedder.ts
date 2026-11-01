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
  return modelId;
}

/** What a model costs and where its file is: the size is said before anyone presses Use. */
export function memoryCost(m: Pick<ModelEntry, "size_mb" | "downloaded" | "acquire">): string {
  const size = formatSize(m.size_mb);
  if (m.acquire === "external") {
    return size ? `${size}, fetched the first time it is used` : "Fetched the first time it is used";
  }
  if (m.downloaded) return size ? `On this device, ${size}` : "On this device";
  return size ? `${size} to download` : "Size not listed";
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
  /** Re-indexing only: memories that can be found by meaning, of those there are. */
  progress: { done: number; total: number } | null;
}

/** A failed load that is only the pond waiting on a file it fetches itself, which is no fault. */
export function isFetching(reason: string | null | undefined): boolean {
  return /not on disk|is downloading/i.test(reason ?? "");
}

function line(kind: EmbedderKind, text: string, over: Partial<EmbedderLine> = {}): EmbedderLine {
  return {
    kind, text, detail: null, retry: false, retryLabel: "Try again", retryingLabel: "Trying again",
    progress: null, ...over,
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
        return line("reindexing", `Re-indexing: ${done} of ${total}`, { progress: { done, total } });
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

/** What the pond says once Memory has been given a model: a download first where the file is not here. */
export function memoryChosenText(
  m: Pick<ModelEntry, "title" | "display_name" | "name" | "category" | "provider" | "downloaded" | "acquire" | "size_mb">,
): string {
  const title = modelLabel(m);
  if (m.acquire === "download" && !m.downloaded) {
    const size = formatSize(m.size_mb);
    return `Using ${title} for memory once it downloads${size ? ` (${size})` : ""}. Memory searches by keyword until then.`;
  }
  return `Now using ${title} for memory. The pond reads your memories again at the next quiet moment.`;
}
