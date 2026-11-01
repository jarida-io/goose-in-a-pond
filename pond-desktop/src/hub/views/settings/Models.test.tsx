import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor, within } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn(),
    getActiveRoles: vi.fn(),
    getContextIndexHealth: vi.fn(),
    updateSettings: vi.fn(),
    getMemoryStatus: vi.fn(),
    getDownloadProgress: vi.fn(),
    activateModel: vi.fn(),
    downloadModel: vi.fn(),
    addPictures: vi.fn(),
    controlModelDownload: vi.fn(),
  },
}));

import { ModelsDetail } from "./Models";
import { api } from "../../../api/PondApiClient";
import { ApiError } from "../../../api/types";
import type { ContextIndexHealth, DownloadEntry, EmbedderHealth, ModelEntry, ModelMemoryStatus } from "../../../api/types";
import {
  DESKTOP_MEMORY, E4B_ORIN, e2b, e4b, entry, eg2, foundOnDisk, litertE2b, litertE4b, llamafile, memoryRoles,
  miniLm, nomicEmbed, NO_ROLES, ollama, ORIN_MEMORY, rolesWith, voice, whisper,
} from "../../../sections/models/fixtures";

const HERE = { downloaded: true };

function setup(opts: {
  models?: ModelEntry[];
  roles?: ReturnType<typeof rolesWith>;
  memory?: ModelMemoryStatus;
  downloads?: DownloadEntry[];
} = {}) {
  vi.mocked(api.listModels).mockResolvedValue(opts.models ?? [e4b(), e2b(HERE), litertE4b()]);
  vi.mocked(api.getActiveRoles).mockResolvedValue(opts.roles ?? NO_ROLES);
  vi.mocked(api.getMemoryStatus).mockResolvedValue(opts.memory ?? ORIN_MEMORY);
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: opts.downloads ?? [] });
}

function renderHub(go = vi.fn()) {
  render(<ModelsDetail go={go} />);
  return go;
}

function memoryHealth(embedder: Partial<EmbedderHealth>, over: Partial<ContextIndexHealth> = {}): ContextIndexHealth {
  return {
    indexed: true, model_id: "embeddinggemma-2-256", dims: 256, rows: 95, matching: 95, mismatched: 0,
    missing: 0, coverage: 1, corpora: [],
    embedder: { state: "ready", model_id: "embeddinggemma-2-256", reason: null, ...embedder },
    ...over,
  };
}

/** A card of the screen, by its title. */
async function card(title: string): Promise<HTMLElement> {
  const head = await screen.findByText(title, { selector: ".setcard__title" });
  return head.closest(".setcard") as HTMLElement;
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({}));
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  vi.mocked(api.downloadModel).mockResolvedValue({ status: "download_started", message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)" });
  vi.mocked(api.controlModelDownload).mockResolvedValue({ status: "ok" });
  setup();
});

afterEach(cleanup);

describe("Hub Models: the jobs", () => {
  it("shows the four jobs a pond fills, in the household's words, and no fake Think or Task", async () => {
    renderHub();
    const tiles = await screen.findByRole("list", { name: "Which model does each job" });
    const jobs = within(tiles).getAllByRole("listitem").map((t) => t.querySelector(".hm-tile__job")?.textContent);
    expect(jobs).toEqual(["Conversation", "Listening", "Speaking", "Memory"]);
    expect(screen.queryByText("Think")).toBeNull();
    expect(screen.queryByText("Task")).toBeNull();
  });

  it("names the model holding a job and the engine that runs it", async () => {
    setup({ roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderHub();
    const tiles = await screen.findByRole("list", { name: "Which model does each job" });
    await waitFor(() => expect(within(tiles).getByText("Gemma 4 E2B")).toBeTruthy());
    expect(within(tiles).getByText("llama.cpp")).toBeTruthy();
    expect(within(tiles).getByText(".gguf")).toBeTruthy();
  });

  it("says nothing is chosen for an unfilled job, and that memory runs on its built-in model", async () => {
    vi.mocked(api.getActiveRoles).mockResolvedValue({ ...NO_ROLES, embedding: { provider: "fastembed", model: "" } } as never);
    renderHub();
    const tiles = await screen.findByRole("list", { name: "Which model does each job" });
    await waitFor(() => expect(within(tiles).getByText("Built-in default")).toBeTruthy());
    expect(within(tiles).getAllByText("Nothing chosen yet")).toHaveLength(3);
  });

  it("says the room for one model, the same figure the rows use", async () => {
    renderHub();
    expect(await screen.findByText("Everything runs on this pond. Room for one model: 5.0 GB.")).toBeTruthy();
  });

  it("says nothing of room when the machine reports no budget", async () => {
    setup({ memory: { total_mb: 0, available_for_llm_mb: 0, loaded_model: null } });
    renderHub();
    expect(await screen.findByText("Everything runs on this pond.")).toBeTruthy();
  });
});

describe("Hub Models: what to get", () => {
  it("offers the picks that are not here yet, with their size, and nothing else of the catalogue", async () => {
    setup({
      models: [e4b(), e2b(HERE), litertE4b(), litertE2b(), whisper("base.en"), whisper("small"), voice("af_heart", { downloaded: false }), ollama()],
      memory: DESKTOP_MEMORY,
    });
    renderHub();
    const recommended = await card("Recommended for this pond");
    const names = within(recommended).getAllByRole("article").map((a) => a.getAttribute("aria-label"));
    expect(names).toEqual(["Gemma 4 E4B, llama.cpp", "Gemma 4 E4B, LiteRT-LM"]);
    expect(within(recommended).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    // Not the whole catalogue: no speech models, no voices, nothing the pond does not recommend.
    expect(screen.queryByText("Whisper small (en)")).toBeNull();
    expect(screen.queryByText("Whisper base.en (en)")).toBeNull();
    expect(screen.queryByText("Litert")).toBeNull();
  });

  it("has no Download placeholder that can never be pressed", async () => {
    renderHub();
    await card("Recommended for this pond");
    expect(screen.queryByTitle(/coming in Phase/i)).toBeNull();
    const disabled = screen.queryAllByRole("button").filter((b) => (b as HTMLButtonElement).disabled);
    expect(disabled).toEqual([]);
  });

  it("downloads with the add-on ticked where it fits, and can be unticked, saying the number each way", async () => {
    setup({ memory: DESKTOP_MEMORY });
    renderHub();
    const recommended = await card("Recommended for this pond");
    const pick = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    const tick = within(pick).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(true);
    fireEvent.click(tick);
    expect(within(pick).getByText("4.2 GB")).toBeTruthy();
    fireEvent.click(within(pick).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false }));
    expect(await screen.findByText("Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)")).toBeTruthy();
  });

  it("starts the add-on unticked where only the model fits, says why, and lets it be ticked anyway", async () => {
    renderHub();
    const recommended = await card("Recommended for this pond");
    const pick = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    const tick = within(pick).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(false);
    const why = within(pick).getByText("Left out: with pictures it would not fit this pond. Tick to include it anyway.");
    expect(tick.getAttribute("aria-describedby")).toBe(why.id);
    expect(within(pick).getByText("4.2 GB")).toBeTruthy();
    expect(within(pick).queryByText("Too big for this pond")).toBeNull();

    fireEvent.click(within(pick).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false }));

    fireEvent.click(tick);
    expect(within(pick).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(within(pick).getByText("Too big for this pond")).toBeTruthy();
    expect(within(pick).queryByText(/^Left out:/)).toBeNull();
    fireEvent.click(within(pick).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenLastCalledWith("gguf", e4b().name, { pictures: true }));
  });

  it("raises one card and asks with one button while nothing converses; after that it raises the model in use", async () => {
    renderHub();
    const recommended = await card("Recommended for this pond");
    expect(recommended.querySelectorAll(".mm-pick[data-raised='true']")).toHaveLength(1);
    expect(document.querySelectorAll(".mm-btn--ask")).toHaveLength(1);
    cleanup();

    setup({ roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderHub();
    await card("Recommended for this pond");
    await waitFor(() => expect(document.querySelectorAll(".hm-row[data-raised='true']")).toHaveLength(1));
    expect(document.querySelectorAll(".mm-pick[data-raised='true']")).toHaveLength(0);
    expect(document.querySelectorAll(".mm-btn--ask")).toHaveLength(0);
  });

  it("shows a download on its own card, a bar per file and one set of controls for the model", async () => {
    const downloads = [
      entry({ filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", model_id: e4b().id, part: "model", total_bytes: 4_215_695_776, downloaded_bytes: 2_000_000_000 }),
      entry({ filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf", category: "mmproj", model_id: e4b().id, part: "pictures", total_bytes: 991_552_320, downloaded_bytes: 991_552_320, status: "done" }),
    ];
    setup({ downloads });
    renderHub();
    const recommended = await card("Recommended for this pond");
    const pick = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    expect(within(pick).getAllByRole("progressbar")).toHaveLength(2);
    expect(within(pick).getByText("2.0 GB of 4.2 GB")).toBeTruthy();
    expect(within(pick).getByText("Arrived")).toBeTruthy();
    fireEvent.click(within(pick).getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(api.controlModelDownload).toHaveBeenCalledWith(e4b().id, "cancel"));
    expect(await screen.findByText("Stopped Gemma 4 E4B. Nothing was kept.")).toBeTruthy();
  });

  it("shows why a part failed in the pond's own sentence, with a way to try again", async () => {
    const reason = "There is no room left on this device to save the download. Free some space, then try again.";
    const downloads = [
      entry({ filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", model_id: e4b().id, part: "model", total_bytes: 4_215_695_776, downloaded_bytes: 2_000_000_000, status: "error", error: reason }),
    ];
    setup({ downloads });
    renderHub();
    const recommended = await card("Recommended for this pond");
    expect(within(recommended).getByRole("alert").textContent).toBe(reason);
    expect(within(recommended).getByRole("button", { name: "Try again" })).toBeTruthy();
  });

  it("says a pick already coming down is already on its way, and not as a failure", async () => {
    vi.mocked(api.downloadModel).mockResolvedValue({
      status: "already_downloading",
      message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)",
    });
    renderHub();
    const recommended = await card("Recommended for this pond");
    const pick = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    fireEvent.click(within(pick).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    expect((await screen.findByRole("status")).textContent).toBe("Gemma 4 E4B is already on its way.");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows a refused download in the pond's words, and nothing of its own", async () => {
    const refusal = "This model has no file name, so the pond cannot save it.";
    vi.mocked(api.downloadModel).mockRejectedValue(new ApiError(400, refusal, "no_file"));
    renderHub();
    const recommended = await card("Recommended for this pond");
    const pick = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    fireEvent.click(within(pick).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    expect((await screen.findByRole("alert")).textContent).toBe(refusal);
  });
});

describe("Hub Models: what is here", () => {
  it("groups the installed models by engine, each opened by one plain sentence", async () => {
    setup({ models: [e2b(HERE), litertE4b(HERE), foundOnDisk(), ollama(), llamafile(), e4b()] });
    renderHub();
    const conversation = await card("Conversation");
    expect(within(conversation).getByText("llama.cpp runs .gguf files and can read pictures with an add-on.")).toBeTruthy();
    expect(within(conversation).getByText("LiteRT-LM runs .litertlm files on the GPU, text only.")).toBeTruthy();
    expect(within(conversation).getByText("Ollama is your own Ollama server; llamafile runs a model packed into one file.")).toBeTruthy();
    const titles = Array.from(conversation.querySelectorAll(".hm-row__title")).map((t) => t.textContent);
    expect(titles).toEqual(["Gemma 4 E2B", "Llama-3.2-3B-Instruct-Q4_K_M", "Gemma 4 E4B", "mistral-7b-instruct", "qwen3:4b"]);
  });

  it("says why a pick is a pick, with measured numbers only where the server sent them", async () => {
    const measured = { ...e2b().recommended!, measured: E4B_ORIN };
    setup({ models: [e2b({ ...HERE, recommended: measured })] });
    renderHub();
    const conversation = await card("Conversation");
    expect(within(conversation).getByText("Recommended")).toBeTruthy();
    expect(within(conversation).getByText(e2b().recommended!.reason)).toBeTruthy();
    expect(within(conversation).getByText(E4B_ORIN.summary)).toBeTruthy();
  });

  it("says In use for the model in use, once, though the role reads local", async () => {
    setup({ models: [litertE4b(HERE), e2b(HERE)], roles: rolesWith({ provider: "local", model: litertE4b().name }) });
    renderHub();
    const conversation = await card("Conversation");
    await waitFor(() => expect(within(conversation).getAllByText("In use")).toHaveLength(1));
    const rows = Array.from(conversation.querySelectorAll(".hm-row"));
    const inUse = rows.find((r) => r.getAttribute("data-inuse") === "true")!;
    expect(inUse.querySelector(".hm-row__title")?.textContent).toBe("Gemma 4 E4B");
    expect(within(inUse as HTMLElement).queryByRole("button", { name: /^Use / })).toBeNull();
  });

  it("uses a model on the person's word, and says so", async () => {
    setup({ models: [e2b(HERE)] });
    renderHub();
    const conversation = await card("Conversation");
    fireEvent.click(within(conversation).getByRole("button", { name: "Use Gemma 4 E2B, llama.cpp for conversation" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("gguf", e2b().name, "chat"));
    expect(await screen.findByText("Now using Gemma 4 E2B for conversation.")).toBeTruthy();
  });

  it("says a model is too big for this pond rather than a percentage", async () => {
    setup({
      models: [e2b(HERE)],
      memory: { total_mb: 7620, available_for_llm_mb: 1500, loaded_model: null, reclaimable_mb: 0 },
    });
    renderHub();
    const conversation = await card("Conversation");
    expect(within(conversation).getByText("Too big for this pond")).toBeTruthy();
    expect(document.body.textContent).not.toMatch(/\d{3,}%/);
  });

  it("reads picture support, and adds it only when pressed", async () => {
    vi.mocked(api.addPictures).mockResolvedValue({ status: "download_started", message: "Downloading picture support for Gemma 4 E2B (941 MB)" });
    setup({ models: [e2b(HERE), litertE4b(HERE)] });
    renderHub();
    const conversation = await card("Conversation");
    expect(within(conversation).getByText("Text only")).toBeTruthy();
    expect(api.addPictures).not.toHaveBeenCalled();
    fireEvent.click(within(conversation).getByRole("button", { name: "Add pictures · 941 MB" }));
    await waitFor(() => expect(api.addPictures).toHaveBeenCalledWith("gguf", e2b().name));
    expect(await screen.findByText("Downloading picture support for Gemma 4 E2B (941 MB)")).toBeTruthy();
  });

  it("says when nothing is on the device to talk with, and points at the picks", async () => {
    setup({ models: [e4b(), litertE4b()] });
    renderHub();
    const conversation = await card("Conversation");
    expect(within(conversation).getByText(/Nothing is on this device to talk with yet/)).toBeTruthy();
    expect(within(conversation).getByText(/Download one of the picks above/)).toBeTruthy();
  });

  it("keeps a helper out of conversation", async () => {
    setup({ models: [e2b(HERE), { ...e2b(HERE), id: "gguf/functiongemma", name: "functiongemma", title: "FunctionGemma 270M", kind: "helper", recommended_role: "tool", recommended: undefined }] });
    renderHub();
    await card("Conversation");
    expect(screen.queryByText("FunctionGemma 270M")).toBeNull();
  });
});

describe("Hub Models: speech", () => {
  it("lists only the listening models that are on the device, and the voice, not 54 of them", async () => {
    const voices = ["af_heart", "af_bella", "af_nicole"].map((n, i) => voice(n, { downloaded: i === 0 }));
    setup({
      models: [e2b(HERE), whisper("base.en", HERE), whisper("small"), ...voices],
      roles: { ...NO_ROLES, tts: { provider: "tts_kokoro", model: "af_heart" } },
    });
    renderHub();
    const speech = await card("Speech");
    expect(within(speech).getByText("Whisper base.en (en)")).toBeTruthy();
    expect(within(speech).queryByText("Whisper small (en)")).toBeNull();
    await waitFor(() => expect(within(speech).getByText("Af_Heart")).toBeTruthy());
    expect(within(speech).queryByText("Af_Bella")).toBeNull();
  });

  it("sends a household to Voice to choose one, and to the computer for a listening model", async () => {
    setup({ models: [e2b(HERE)] });
    const go = renderHub();
    const speech = await card("Speech");
    expect(within(speech).getByText(/No listening model is on this device yet/)).toBeTruthy();
    fireEvent.click(within(speech).getByRole("button", { name: "Choose a voice" }));
    expect(go).toHaveBeenCalledWith("voice");
  });

  it("uses a listening model on the person's word", async () => {
    setup({ models: [e2b(HERE), whisper("base.en", HERE)] });
    renderHub();
    const speech = await card("Speech");
    fireEvent.click(within(speech).getByRole("button", { name: /Use Whisper base.en/ }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("whisper", "base.en", "asr"));
  });
});

describe("Hub Models: when the pond cannot be reached", () => {
  it("says so, and still draws the screen", async () => {
    vi.mocked(api.listModels).mockRejectedValue(new Error("connection refused"));
    renderHub();
    expect(await screen.findByText(/Could not reach the server/)).toBeTruthy();
    expect(screen.getByText("Models")).toBeTruthy();
  });

  it("says a failure as an alert, and a success as a status", async () => {
    vi.mocked(api.activateModel).mockRejectedValueOnce(new Error("The file is not on this device"));
    setup({ models: [e2b(HERE)] });
    renderHub();
    fireEvent.click(await screen.findByRole("button", { name: "Use Gemma 4 E2B, llama.cpp for conversation" }));
    expect((await screen.findByRole("alert")).textContent).toBe("The file is not on this device");
    fireEvent.click(screen.getByRole("button", { name: "Use Gemma 4 E2B, llama.cpp for conversation" }));
    expect((await screen.findByRole("status")).textContent).toBe("Now using Gemma 4 E2B for conversation.");
  });

  it("goes back to Settings", async () => {
    const go = renderHub();
    fireEvent.click(await screen.findByRole("button", { name: "Back to Settings" }));
    expect(go).toHaveBeenCalledWith("settings");
  });
});

describe("Hub Models: Memory", () => {
  const ROWS = () => [e4b(), e2b(HERE), miniLm(), nomicEmbed(), eg2()];

  it("names the model Memory runs on, never its whole description, and says when nobody chose it", async () => {
    setup({ models: ROWS(), roles: memoryRoles("", "fastembed") });
    renderHub();
    const tiles = await screen.findByRole("list", { name: "Which model does each job" });
    await waitFor(() => expect(within(tiles).getByText("all-MiniLM-L6-v2")).toBeTruthy());
    expect(within(tiles).getByText("Built-in default")).toBeTruthy();
    expect(tiles.textContent).not.toContain("fast, lightweight");
  });

  it("recommends EmbeddingGemma 2 first, with what it buys, its size and one Use", async () => {
    setup({ models: ROWS(), roles: memoryRoles("", "fastembed") });
    renderHub();
    const memory = await card("Memory");
    const rows = within(memory).getAllByRole("listitem");
    // Folded: the recommendation and the model in use; the rest on request.
    expect(rows).toHaveLength(2);
    expect(within(rows[0]).getByText("EmbeddingGemma 2")).toBeTruthy();
    expect(within(rows[0]).getByText("Recommended")).toBeTruthy();
    expect(within(rows[0]).getByText("Finds facts in Swahili, Sheng or English, whichever language you ask in")).toBeTruthy();
    expect(within(rows[0]).getByText("295 MB to download")).toBeTruthy();
    expect(within(rows[1]).getByText("In use")).toBeTruthy();
  });

  it("keeps the other models a press away, and opens them on request", async () => {
    setup({ models: ROWS(), roles: memoryRoles("", "fastembed") });
    renderHub();
    const memory = await card("Memory");
    fireEvent.click(within(memory).getByRole("button", { name: "Other Memory models (1)" }));
    expect(within(memory).getAllByRole("listitem")).toHaveLength(3);
    expect(within(memory).getByRole("button", { name: "Show fewer Memory models" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("assigns the Memory role on Use, and says a download comes first", async () => {
    setup({ models: ROWS(), roles: memoryRoles("", "fastembed") });
    renderHub();
    const memory = await card("Memory");
    fireEvent.click(within(memory).getByRole("button", { name: "Use EmbeddingGemma 2 for memory" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"));
    expect(
      await screen.findByText("Using EmbeddingGemma 2 for memory once it downloads (295 MB). Memory searches by keyword until then."),
    ).toBeTruthy();
    expect(api.downloadModel).not.toHaveBeenCalled();
  });

  it("shows the download on its row with the bar and the controls", async () => {
    setup({
      models: ROWS(),
      roles: memoryRoles("embeddinggemma-2"),
      downloads: [entry({ filename: "embeddinggemma-2-Q8_0.gguf", category: "embedding", model_id: eg2().id, part: "model", total_bytes: 309_855_456, downloaded_bytes: 100_000_000 })],
    });
    renderHub();
    const memory = await card("Memory");
    expect(await within(memory).findByRole("progressbar", { name: /EmbeddingGemma 2.*model/i })).toBeTruthy();
    expect(within(memory).getByRole("button", { name: "Pause" })).toBeTruthy();
  });

  it("says it searches by keyword while the model loads", async () => {
    setup({ models: ROWS(), roles: memoryRoles("embeddinggemma-2") });
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({ state: "loading" }, { matching: 0 }));
    renderHub();
    const memory = await card("Memory");
    expect(await within(memory).findByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeTruthy();
  });

  it("counts the re-index", async () => {
    setup({ models: ROWS(), roles: memoryRoles("embeddinggemma-2") });
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({}, { matching: 40 }));
    renderHub();
    const memory = await card("Memory");
    expect(await within(memory).findByText("Re-indexing: 40 of 95")).toBeTruthy();
  });

  it("says a failed load in plain words, with a way to try again", async () => {
    setup({ models: ROWS(), roles: memoryRoles("embeddinggemma-2") });
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "failed", reason: "loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory" }, { matching: 0 }),
    );
    renderHub();
    const memory = await card("Memory");
    const alert = await within(memory).findByRole("alert");
    expect(alert.textContent).toContain("EmbeddingGemma 2 did not load");
    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"));
  });
});

describe("Hub Models: made of tokens", () => {
  const HERE_DIR = dirname(fileURLToPath(import.meta.url));
  const HEX = /#[0-9a-fA-F]{3,8}\b/;

  it("writes no colour of its own, so dark mode and the chosen accent reach it", () => {
    const source = readFileSync(resolve(HERE_DIR, "Models.tsx"), "utf8");
    expect(source.match(HEX)).toBeNull();

    const css = readFileSync(resolve(HERE_DIR, "../../hub.css"), "utf8");
    const start = css.indexOf("/* ─── Models ───");
    const end = css.indexOf(".mrow__btn {");
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const block = css.slice(start, end);
    expect(block.match(HEX)).toBeNull();
    expect(block).not.toMatch(/rgba?\(/);
  });
});
