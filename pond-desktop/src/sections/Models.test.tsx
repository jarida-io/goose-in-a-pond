import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor, within } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn(),
    getActiveRoles: vi.fn(),
    getContextIndexHealth: vi.fn(),
    getMemoryStatus: vi.fn(),
    getDownloadProgress: vi.fn(),
    getDiskUsage: vi.fn(),
    activateModel: vi.fn(),
    deleteModel: vi.fn(),
    downloadModel: vi.fn(),
    addPictures: vi.fn(),
    controlModelDownload: vi.fn(),
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    applyTtsSettings: vi.fn(),
    searchGgufModels: vi.fn(),
    listHfModelFiles: vi.fn(),
    downloadModelFromUrl: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type {
  ContextIndexHealth, DownloadEntry, EmbedderHealth, ModelActiveRoles, ModelEntry, ModelMemoryStatus,
} from "../api/types";
import { ConfirmProvider } from "../components/shared";
import { Models } from "./Models";
import {
  DESKTOP_MEMORY, E4B_ORIN, EG2_ORIN, e2b, e4b, entry, eg2, foundOnDisk, functionGemma, litertE2b, litertE4b,
  llamafile, memoryRoles, miniLm, nomicEmbed, NO_ROLES, ollama, ORIN_MEMORY, rolesWith, whisper,
} from "./models/fixtures";

const NOT_DOWNLOADED = { downloaded: false };
const HERE = { downloaded: true };

function renderModels() {
  return render(
    <ConfirmProvider>
      <Models />
    </ConfirmProvider>,
  );
}

function setup(opts: {
  models?: ModelEntry[];
  roles?: ModelActiveRoles;
  memory?: ModelMemoryStatus;
  downloads?: DownloadEntry[];
} = {}) {
  vi.mocked(api.listModels).mockResolvedValue(
    opts.models ?? [e4b(), e2b(HERE), litertE4b(), litertE2b()],
  );
  vi.mocked(api.getActiveRoles).mockResolvedValue(opts.roles ?? NO_ROLES);
  vi.mocked(api.getMemoryStatus).mockResolvedValue(opts.memory ?? ORIN_MEMORY);
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: opts.downloads ?? [] });
}

/** What the index says with Memory on EmbeddingGemma 2; `embedder` overrides the embedder alone. */
function memoryHealth(embedder: Partial<EmbedderHealth>, over: Partial<ContextIndexHealth> = {}): ContextIndexHealth {
  return {
    indexed: true, model_id: "embeddinggemma-2-256", dims: 256, rows: 95, matching: 95, mismatched: 0,
    missing: 0, coverage: 1, corpora: [],
    embedder: { state: "ready", model_id: "embeddinggemma-2-256", reason: null, ...embedder },
    ...over,
  };
}

/** The band a heading opens: the section it labels. */
async function band(name: string): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { name });
  return heading.closest("section") as HTMLElement;
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getDiskUsage).mockResolvedValue({
    total_bytes: 2_620_370_976, by_category: {}, hf_cache_bytes: 0, incomplete_bytes: 0,
  });
  vi.mocked(api.getSettings).mockResolvedValue({} as never);
  vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({}));
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.deleteModel).mockResolvedValue(undefined as never);
  vi.mocked(api.controlModelDownload).mockResolvedValue({ status: "ok" });
  vi.mocked(api.downloadModel).mockResolvedValue({
    status: "download_started",
    message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)",
  });
  setup();
});

afterEach(cleanup);

describe("the bands", () => {
  it("opens on what is recommended, then Jobs, what is here, and where to get more", async () => {
    renderModels();
    await screen.findByRole("heading", { name: "Recommended for this pond" });
    const bands = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
    expect(bands).toEqual(["Recommended for this pond", "Jobs", "On this device", "Get more"]);
  });

  it("no longer has a band of its own for what is coming down", async () => {
    setup({ downloads: [entry({ filename: "gemma.gguf", model_id: e4b().id, part: "model", total_bytes: 100, downloaded_bytes: 10 })] });
    renderModels();
    await screen.findByRole("heading", { name: "Recommended for this pond" });
    expect(screen.queryByRole("heading", { name: "Coming down" })).toBeNull();
  });
});

describe("Recommended for this pond", () => {
  it("shows GIAP's picks, best first, each with its reason and the engine that runs it", async () => {
    renderModels();
    const recommended = await band("Recommended for this pond");
    const cards = within(recommended).getAllByRole("article");
    expect(cards.map((c) => c.getAttribute("aria-label"))).toEqual([
      "Gemma 4 E4B, llama.cpp", "Gemma 4 E2B, llama.cpp", "Gemma 4 E4B, LiteRT-LM",
    ]);
    expect(within(cards[0]).getByText("Best answers this pond can run")).toBeTruthy();
    expect(within(cards[0]).getByText(".gguf")).toBeTruthy();
    expect(within(cards[2]).getByText(".litertlm")).toBeTruthy();
    expect(within(cards[2]).getByText("Text only")).toBeTruthy();
    expect(within(recommended).getByText(/Nothing is downloaded or switched on until you choose it/)).toBeTruthy();
  });

  it("shows measured numbers only where the server sent them", async () => {
    const measured = { ...e4b().recommended!, measured: E4B_ORIN };
    setup({ models: [e4b({ recommended: measured }), e2b(), litertE4b()] });
    renderModels();
    const recommended = await band("Recommended for this pond");
    expect(within(recommended).getByText("First reply in about 1 s, 15-16 tokens a second, 16k window")).toBeTruthy();
    expect(within(recommended).getByText("Measured on this kind of device, 5 Oct 2026")).toBeTruthy();
    // The other two carry no numbers, so the card says none.
    expect(within(recommended).getAllByText(/tokens a second/)).toHaveLength(1);
  });

  it("raises one card, and asks its one question with one button, while nothing converses", async () => {
    renderModels();
    const recommended = await band("Recommended for this pond");
    const raised = recommended.querySelectorAll(".mm-pick[data-raised='true']");
    expect(raised).toHaveLength(1);
    expect(raised[0].getAttribute("aria-label")).toBe("Gemma 4 E4B, llama.cpp");
    expect(document.querySelectorAll(".mm-btn--ask")).toHaveLength(1);
    expect(raised[0].querySelector(".mm-btn--ask")).toBeTruthy();
  });

  it("raises the pick in use instead, and asks nothing, once something converses", async () => {
    setup({ roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const raised = recommended.querySelectorAll(".mm-pick[data-raised='true']");
    expect(raised).toHaveLength(1);
    expect(raised[0].getAttribute("aria-label")).toBe("Gemma 4 E2B, llama.cpp");
    expect(document.querySelectorAll(".mm-btn--ask")).toHaveLength(0);
    expect(within(raised[0] as HTMLElement).getByText("In use")).toBeTruthy();
  });

  it("says the number before it is spent, and drops the add-on when it is unticked", async () => {
    setup({ memory: DESKTOP_MEMORY });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    expect(within(card).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    const tick = within(card).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(true);

    fireEvent.click(tick);
    expect(within(card).getByText("4.2 GB")).toBeTruthy();
    expect(within(card).queryByText(/for pictures/)).toBeNull();

    fireEvent.click(within(card).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() =>
      expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false }),
    );
  });

  it("downloads with the add-on by default when it fits, and says what the pond will fetch", async () => {
    setup({ memory: DESKTOP_MEMORY });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    fireEvent.click(within(card).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() =>
      expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: true }),
    );
    expect(await screen.findByText("Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)")).toBeTruthy();
  });

  it("says a file already coming down is already on its way, and not as a failure", async () => {
    vi.mocked(api.downloadModel).mockResolvedValue({
      status: "already_downloading",
      message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)",
    });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    fireEvent.click(within(card).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    const note = await screen.findByText("Gemma 4 E4B is already on its way.");
    expect(note.getAttribute("role")).toBe("status");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("offers no add-on for a text-only engine, and sends no body for it", async () => {
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, LiteRT-LM" });
    expect(within(card).queryByRole("checkbox")).toBeNull();
    fireEvent.click(within(card).getByRole("button", { name: "Download Gemma 4 E4B, LiteRT-LM" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("litert", litertE4b().name, undefined));
  });
});

describe("On this device", () => {
  it("splits conversation by engine, and opens each with one plain sentence", async () => {
    setup({
      models: [
        e2b(HERE), foundOnDisk(), litertE4b(HERE), ollama(), llamafile(),
        e4b(NOT_DOWNLOADED), litertE2b(NOT_DOWNLOADED),
      ],
    });
    renderModels();
    const device = await band("On this device");
    const conversation = within(device).getByRole("region", { name: "Conversation" });
    expect(within(conversation).getByText("llama.cpp runs .gguf files and can read pictures with an add-on.")).toBeTruthy();
    expect(within(conversation).getByText("LiteRT-LM runs .litertlm files on the GPU, text only.")).toBeTruthy();
    expect(within(conversation).getByText("Ollama is your own Ollama server; llamafile runs a model packed into one file.")).toBeTruthy();
    expect(within(conversation).getByText("Other engines")).toBeTruthy();
    expect(within(conversation).getByText("qwen3:4b")).toBeTruthy();
    // Under their own engines, in order, and not the ones that are not here yet.
    const titles = Array.from(conversation.querySelectorAll(".mdl-row__name")).map((n) => n.textContent);
    expect(titles).toEqual([
      "Gemma 4 E2B", "Llama-3.2-3B-Instruct-Q4_K_M", "Gemma 4 E4B", "mistral-7b-instruct", "qwen3:4b",
    ]);
  });

  it("chips where a row came from, and only where that informs", async () => {
    setup({ models: [e2b(HERE), foundOnDisk(), ollama(), llamafile(), litertE2b(HERE)] });
    renderModels();
    const device = await band("On this device");
    const chips = new Map<string, (string | null)[]>();
    device.querySelectorAll(".mdl-row").forEach((row) => {
      const key = `${row.querySelector(".mdl-row__name")?.textContent} ${row.querySelector(".mdl-row__facts")?.textContent ?? ""}`;
      chips.set(key, Array.from(row.querySelectorAll(".mm-chip")).map((c) => c.textContent));
    });
    expect(chips.get("Gemma 4 E2B UD-Q4_K_XL · 131,072-token window")).toEqual(["Recommended"]);
    expect(chips.get("Llama-3.2-3B-Instruct-Q4_K_M Q4_K_M")).toEqual(["Found on disk"]);
    expect(chips.get("qwen3:4b ")).toEqual(["Ollama"]);
    expect(chips.get("mistral-7b-instruct ")).toEqual(["Added"]);
    // The catalogue earns no chip: the LiteRT-LM E2B is one of our picks that is not recommended.
    expect(chips.get("Gemma 4 E2B 32,768-token window")).toEqual([]);
  });

  it("never offers a helper as conversation, and never shows the scan's placeholder as a name", async () => {
    setup({ models: [e2b(HERE), functionGemma(HERE), foundOnDisk()] });
    renderModels();
    const device = await band("On this device");
    const conversation = within(device).getByRole("region", { name: "Conversation" });
    expect(within(conversation).queryByText("FunctionGemma 270M")).toBeNull();
    // It is on the device, so it is listed, under Other, with nothing to use it for.
    const other = within(device).getByRole("region", { name: "Other" });
    expect(within(other).getByText("FunctionGemma 270M")).toBeTruthy();
    expect(within(other).queryByRole("button", { name: /^Use / })).toBeNull();
    expect(screen.queryByText("(detected on disk)")).toBeNull();
    expect(within(device).getByText("Llama-3.2-3B-Instruct-Q4_K_M")).toBeTruthy();
  });

  it("says on every row which jobs it is doing, and folds what serves a row into that row", async () => {
    const minilm = {
      id: "embedding/all-MiniLM-L6-v2", provider: "embedding", category: "embedding",
      name: "all-MiniLM-L6-v2", title: "all-MiniLM-L6-v2", is_active: false, downloaded: false,
      acquire: "external", roles: ["embedding"], section: "memory",
    } as ModelEntry;
    setup({
      models: [
        e2b({
          ...HERE, roles: ["chat", "tool"], section: "conversation",
          companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "installed" }],
        }),
        minilm,
      ],
      roles: {
        ...rolesWith(null),
        helpers: [
          { id: "gguf/e2b#pictures", title: "Gemma 4 E2B pictures", uses: ["pictures"], section: "pictures", serves: e2b().id },
          { id: "camera_detector", title: "YOLOX-Nano", uses: ["camera"], section: "pictures", serves: null },
          { id: "mtp-gemma-4-E2B-it", title: null, uses: ["drafter"], section: "other", serves: e2b().id },
        ],
      },
    });
    renderModels();
    const device = await band("On this device");
    // Memory has its own panel under Jobs; the drafter and encoder are part of the Gemma row.
    const regions = within(device).getAllByRole("region").map((r) => r.getAttribute("aria-label"));
    expect(regions).toEqual(["Conversation", "Camera and faces"]);

    const conversation = within(device).getByRole("region", { name: "Conversation" });
    expect(within(conversation).getByText("Conversation", { selector: ".mm-chip--use" })).toBeTruthy();
    expect(within(conversation).getByText("Tool calls", { selector: ".mm-chip--use" })).toBeTruthy();
    expect(within(conversation).getByText("Faster answers")).toBeTruthy();
    expect(within(conversation).getByText("Pictures included")).toBeTruthy();
    expect(within(device).queryByText("Gemma 4 E2B pictures")).toBeNull();

    // The camera's detector serves no row, so it stands alone, saying what it does, with nothing to press.
    const camera = within(device).getByRole("region", { name: "Camera and faces" });
    expect(within(camera).getByText("YOLOX-Nano")).toBeTruthy();
    expect(within(camera).getByText("Spots people, animals and things in your cameras' pictures.")).toBeTruthy();
    expect(within(camera).queryByRole("button")).toBeNull();
  });

  it("reads each row's picture support in the household's words", async () => {
    const installed = e2b({ ...HERE, companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "installed" }] });
    const refused = foundOnDisk({ companions: [{ kind: "pictures", label: "Llama", size_bytes: 1, state: "not_on_this_device" }] });
    setup({ models: [installed, refused, litertE4b(HERE), foundOnDisk({ id: "gguf/plain", name: "plain" })] });
    renderModels();
    const device = await band("On this device");
    expect(within(device).getByText("Pictures included")).toBeTruthy();
    expect(within(device).getByText("Pictures aren't available on this device")).toBeTruthy();
    expect(within(device).getAllByText("Text only").length).toBeGreaterThanOrEqual(2);
  });

  it("says honestly that picture support is being checked, and offers nothing to press meanwhile", async () => {
    const checking = e2b({ ...HERE, companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "verifying" }] });
    setup({ models: [checking] });
    renderModels();
    const device = await band("On this device");
    expect(within(device).getByText("Checking picture support")).toBeTruthy();
    expect(within(device).queryByRole("button", { name: /Add pictures/ })).toBeNull();
  });

  it("adds picture support only when asked, and says what it will fetch", async () => {
    vi.mocked(api.addPictures).mockResolvedValue({
      status: "download_started",
      message: "Downloading picture support for Gemma 4 E2B (941 MB)",
    });
    setup({ models: [e2b(HERE)] });
    renderModels();
    const device = await band("On this device");
    expect(api.addPictures).not.toHaveBeenCalled();
    fireEvent.click(within(device).getByRole("button", { name: "Add pictures · 941 MB" }));
    await waitFor(() => expect(api.addPictures).toHaveBeenCalledWith("gguf", e2b().name));
    expect(await screen.findByText("Downloading picture support for Gemma 4 E2B (941 MB)")).toBeTruthy();
  });

  it("uses a model on the person's word, and says so", async () => {
    setup({ models: [e2b(HERE)] });
    renderModels();
    const device = await band("On this device");
    fireEvent.click(within(device).getByRole("button", { name: "Use Gemma 4 E2B, llama.cpp for conversation" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("gguf", e2b().name, "chat"));
    expect(await screen.findByText("Now using Gemma 4 E2B for conversation.")).toBeTruthy();
  });

  it("says In use for the model in use, once, and offers it no Use", async () => {
    setup({ models: [e2b(HERE), foundOnDisk()], roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderModels();
    const device = await band("On this device");
    const row = within(device).getByText("Gemma 4 E2B").closest(".mdl-row") as HTMLElement;
    expect(within(row).getByText("In use")).toBeTruthy();
    expect(within(row).queryByRole("button", { name: /^Use / })).toBeNull();
    expect(within(row).queryByText(/\d%/)).toBeNull();
  });

  it("asks first like any delete, and shows the pond's own sentence for the model in use", async () => {
    const refusal = "Gemma 4 E2B is doing a job right now (Conversation). Give that job to another model first.";
    vi.mocked(api.deleteModel).mockRejectedValue(new ApiError(409, refusal));
    setup({ models: [e2b(HERE), foundOnDisk()], roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderModels();
    const device = await band("On this device");
    const row = within(device).getByText("Gemma 4 E2B").closest(".mdl-row") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "Delete Gemma 4 E2B, llama.cpp" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await waitFor(() => expect(api.deleteModel).toHaveBeenCalledWith("gguf", e2b().name));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(refusal);
    expect(screen.queryByText(/freed/)).toBeNull();
  });

  it("says what was freed when a model is deleted", async () => {
    setup({ models: [e2b(HERE), foundOnDisk()] });
    renderModels();
    const device = await band("On this device");
    fireEvent.click(within(device).getByRole("button", { name: "Delete Llama-3.2-3B-Instruct-Q4_K_M, llama.cpp" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await waitFor(() => expect(api.deleteModel).toHaveBeenCalledWith("gguf", "Llama-3.2-3B-Instruct-Q4_K_M"));
    expect(await screen.findByText("Deleted Llama-3.2-3B-Instruct-Q4_K_M. 2.0 GB freed.")).toBeTruthy();
  });

  it("offers no Delete for a model another program manages", async () => {
    setup({ models: [ollama()] });
    renderModels();
    const device = await band("On this device");
    expect(within(device).queryByRole("button", { name: /Delete/ })).toBeNull();
    expect(within(device).getByText("Runs in Ollama")).toBeTruthy();
  });

  it("asks before deleting, and shows a refusal in the pond's own words", async () => {
    // Another row names the same file: the pond keeps it, and names that model and its job.
    const refusal = "Gemma 4 E2B uses the same file and is doing a job right now (Conversation). Give that job to another model first.";
    vi.mocked(api.deleteModel).mockRejectedValue(new ApiError(409, refusal));
    setup({ models: [e2b(HERE), foundOnDisk()] });
    renderModels();
    const device = await band("On this device");
    fireEvent.click(within(device).getByRole("button", { name: "Delete Llama-3.2-3B-Instruct-Q4_K_M, llama.cpp" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await waitFor(() => expect(api.deleteModel).toHaveBeenCalledWith("gguf", "Llama-3.2-3B-Instruct-Q4_K_M"));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(refusal);
    // Still on the device, and nothing is claimed freed.
    expect(screen.queryByText(/freed/)).toBeNull();
  });
});

describe("fit", () => {
  it("never reads a percentage into the thousands: over budget is words", async () => {
    // 1 GB of budget left: the old reading was 300,000%.
    setup({
      models: [e4b(), e2b(HERE)],
      memory: { total_mb: 7620, available_for_llm_mb: 1030, loaded_model: null, reclaimable_mb: 0 },
    });
    renderModels();
    await screen.findByRole("heading", { name: "On this device" });
    expect(screen.getAllByText("Too big for this pond").length).toBeGreaterThanOrEqual(2);
    expect(document.body.textContent).not.toMatch(/\d{3,}%/);
  });

  it("starts the add-on unticked where only the model fits, says why, and lets it be ticked anyway", async () => {
    // 4020 MB of weights fit the 4800 MB left; with 945 MB of add-on they do not.
    setup({
      models: [e4b()],
      memory: { total_mb: 7620, available_for_llm_mb: 5824, loaded_model: null, reclaimable_mb: 0 },
    });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const tick = within(recommended).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(false);
    const why = within(recommended).getByText("Left out: with pictures it would not fit this pond. Tick to include it anyway.");
    expect(tick.getAttribute("aria-describedby")).toBe(why.id);
    // What it costs without the add-on, and that it fits.
    expect(within(recommended).getByText("4.2 GB")).toBeTruthy();
    expect(within(recommended).getByText("84%")).toBeTruthy();
    expect(within(recommended).queryByText("Too big for this pond")).toBeNull();

    // Ticked anyway: the number, the verdict and the way out are said, and the reason is gone.
    fireEvent.click(tick);
    expect(within(recommended).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(within(recommended).getByText("Too big for this pond")).toBeTruthy();
    expect(within(recommended).getByText("It fits without picture support.")).toBeTruthy();
    expect(within(recommended).queryByText(/^Left out:/)).toBeNull();
    fireEvent.click(within(recommended).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() =>
      expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: true }),
    );
  });

  it("downloads without the add-on, as the box says, when only the model fits", async () => {
    setup({ models: [e4b()] });
    renderModels();
    const recommended = await band("Recommended for this pond");
    fireEvent.click(within(recommended).getByRole("button", { name: "Download Gemma 4 E4B, llama.cpp" }));
    await waitFor(() =>
      expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false }),
    );
  });

  it("never drops the add-on silently: it stays ticked when it fits, when nothing fits, and when the pond cannot say", async () => {
    for (const memory of [
      DESKTOP_MEMORY,
      { total_mb: 7620, available_for_llm_mb: 1030, loaded_model: null, reclaimable_mb: 0 },
      { total_mb: 0, available_for_llm_mb: 0, loaded_model: null },
    ]) {
      setup({ models: [e4b()], memory });
      const { unmount } = renderModels();
      const recommended = await band("Recommended for this pond");
      const tick = within(recommended).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
      expect(tick.checked).toBe(true);
      expect(within(recommended).queryByText(/^Left out:/)).toBeNull();
      unmount();
    }
  });

  it("keeps what the household chose when the room changes under it", async () => {
    setup({ models: [e4b()] });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const tick = within(recommended).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(false);
    fireEvent.click(tick);
    expect(tick.checked).toBe(true);
    fireEvent.click(tick);
    expect(tick.checked).toBe(false);
    expect(within(recommended).getByText(/^Left out:/)).toBeTruthy();
  });

  it("counts what switching away from the model in use frees", async () => {
    // Only 1000 MB free beside the model in use, which returns 2400 MB when replaced.
    setup({
      models: [e2b(HERE), e4b(), litertE4b()],
      roles: rolesWith({ provider: "local", model: e2b().name }),
      memory: { total_mb: 7620, available_for_llm_mb: 1000, loaded_model: null, reclaimable_mb: 4800 },
    });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const lite = within(recommended).getByRole("article", { name: "Gemma 4 E4B, LiteRT-LM" });
    expect(within(lite).queryByText("Too big for this pond")).toBeNull();
    expect(within(lite).getByText(/%$/)).toBeTruthy();
  });
});

describe("the header", () => {
  it("shows the room for one model, the same figure the rows are judged against", async () => {
    setup({ memory: { total_mb: 7620, available_for_llm_mb: 5820, loaded_model: null, reclaimable_mb: 0 } });
    renderModels();
    const stat = (await screen.findByText("for one model")).closest(".mdl-stat") as HTMLElement;
    await waitFor(() => expect(within(stat).getByText("5.0 GB")).toBeTruthy());
  });

  it("never renders blank, however empty the budget, and names the disk", async () => {
    setup({ memory: { total_mb: 0, available_for_llm_mb: 0, loaded_model: null } });
    renderModels();
    const stat = (await screen.findByText("for one model")).closest(".mdl-stat") as HTMLElement;
    const disk = screen.getByText("on disk").closest(".mdl-stat") as HTMLElement;
    await waitFor(() => expect(within(disk).getByText("2.6 GB")).toBeTruthy());
    expect(within(stat).getByText("—")).toBeTruthy();
    expect(stat.getAttribute("title")).toMatch(/does not report a memory budget/);
  });

  it("shows a dash while the budget is still being read, not an empty place", async () => {
    vi.mocked(api.getMemoryStatus).mockReturnValue(new Promise(() => {}));
    renderModels();
    const stat = (await screen.findByText("for one model")).closest(".mdl-stat") as HTMLElement;
    expect(within(stat).getByText("—")).toBeTruthy();
  });
});

describe("what is coming down", () => {
  const MODEL = "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf";
  const PICTURES = "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf";
  const both = (over: Partial<DownloadEntry> = {}) => [
    entry({ filename: MODEL, model_id: e4b().id, part: "model", total_bytes: 4_215_695_776, downloaded_bytes: 1_200_000_000, ...over }),
    entry({ filename: PICTURES, category: "mmproj", model_id: e4b().id, part: "pictures", total_bytes: 991_552_320, downloaded_bytes: 500_000_000, ...over }),
  ];

  it("sits on the row it belongs to, one bar per file", async () => {
    setup({ downloads: both() });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    const bars = within(card).getAllByRole("progressbar");
    expect(bars.map((b) => b.getAttribute("aria-label"))).toEqual([
      "Gemma 4 E4B, llama.cpp, model", "Gemma 4 E4B, llama.cpp, pictures",
    ]);
    expect(within(card).getByText("1.2 GB of 4.2 GB")).toBeTruthy();
    expect(within(card).getByText("476 MB of 945 MB")).toBeTruthy();
    expect(within(card).queryByRole("button", { name: /^Download/ })).toBeNull();
  });

  it("has one set of controls per model, and Stop is told to the model, which moves every part", async () => {
    setup({ downloads: both() });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    expect(within(card).getAllByRole("button", { name: "Pause" })).toHaveLength(1);
    expect(within(card).getAllByRole("button", { name: "Stop" })).toHaveLength(1);

    fireEvent.click(within(card).getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(api.controlModelDownload).toHaveBeenCalledTimes(1));
    expect(api.controlModelDownload).toHaveBeenCalledWith(e4b().id, "cancel");
    expect(await screen.findByText("Stopped Gemma 4 E4B. Nothing was kept.")).toBeTruthy();
  });

  it("pauses the model, and says what a pause keeps where the pond can resume it", async () => {
    setup({ downloads: both({ resumable: true }) });
    renderModels();
    const recommended = await band("Recommended for this pond");
    fireEvent.click(within(recommended).getByRole("button", { name: "Pause" }));
    await waitFor(() => expect(api.controlModelDownload).toHaveBeenCalledWith(e4b().id, "pause"));
    expect(await screen.findByText("Paused Gemma 4 E4B. What has arrived so far is kept.")).toBeTruthy();
  });

  it("says it will start again from the beginning when the pond cannot resume it", async () => {
    setup({ downloads: both({ resumable: false }) });
    renderModels();
    const recommended = await band("Recommended for this pond");
    fireEvent.click(within(recommended).getByRole("button", { name: "Pause" }));
    expect(await screen.findByText("Paused Gemma 4 E4B. It will start again from the beginning.")).toBeTruthy();
    expect(screen.queryByText(/is kept/)).toBeNull();
  });

  it("offers Resume and Stop for a paused model, and says what the pause kept", async () => {
    setup({ downloads: both({ status: "paused", resumable: true }) });
    renderModels();
    const recommended = await band("Recommended for this pond");
    expect(within(recommended).getByText("Paused. What has arrived so far is kept.")).toBeTruthy();
    expect(within(recommended).getByRole("button", { name: "Resume" })).toBeTruthy();
    expect(within(recommended).getByRole("button", { name: "Stop" })).toBeTruthy();
  });

  it("says a paused model that cannot resume will start again from the beginning", async () => {
    setup({ downloads: both({ status: "paused", resumable: false }) });
    renderModels();
    const recommended = await band("Recommended for this pond");
    expect(within(recommended).getByText("Paused. It will start again from the beginning.")).toBeTruthy();
  });

  it("shows why one part failed in the pond's own sentence, and offers to try it again", async () => {
    const reason = "The download site is having trouble (error 503). Try again later.";
    setup({ downloads: [both()[0], { ...both()[1], status: "error", error: reason }] });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const alert = within(recommended).getByRole("alert");
    expect(alert.textContent).toBe(reason);
    fireEvent.click(within(recommended).getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(api.controlModelDownload).toHaveBeenCalledWith(e4b().id, "resume"));
  });

  it("says which reconnect a dropped download is on, and keeps its controls", async () => {
    setup({ downloads: [{ ...both()[0], reconnecting: { attempt: 2, max_attempts: 8 } }, both()[1]] });
    renderModels();
    const recommended = await band("Recommended for this pond");
    const card = within(recommended).getByRole("article", { name: "Gemma 4 E4B, llama.cpp" });
    expect(within(card).getByText("The connection dropped. Reconnecting (attempt 2 of 8)")).toBeTruthy();
    expect(within(card).getByRole("button", { name: "Pause" })).toBeTruthy();
    expect(within(card).getByRole("button", { name: "Stop" })).toBeTruthy();
  });

  it("offers Stop beside Try again on a failed download, which throws away what it kept", async () => {
    const reason = "The connection dropped before the file finished. Resume to carry on from where it stopped.";
    setup({ downloads: [{ ...both()[0], status: "error", error: reason }, both()[1]] });
    renderModels();
    const recommended = await band("Recommended for this pond");
    expect(within(recommended).getByRole("alert").textContent).toBe(reason);
    expect(within(recommended).getByRole("button", { name: "Try again" })).toBeTruthy();
    fireEvent.click(within(recommended).getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(api.controlModelDownload).toHaveBeenCalledWith(e4b().id, "cancel"));
  });

  it("shows a pictures part arriving on an installed model's own row", async () => {
    setup({
      models: [e2b({ ...HERE, companions: [{ kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state: "downloading" }] })],
      downloads: [entry({ filename: "mmproj/gemma-4-e2b-it-qat/mmproj-BF16.gguf", category: "mmproj", model_id: e2b().id, part: "pictures", total_bytes: 986_833_728, downloaded_bytes: 100_000_000 })],
    });
    renderModels();
    const device = await band("On this device");
    expect(within(device).getByRole("progressbar", { name: "Gemma 4 E2B, llama.cpp, pictures" })).toBeTruthy();
  });
});

describe("Jobs", () => {
  it("names the model and the engine that holds a job, not its file stem", async () => {
    setup({ models: [e2b(HERE)], roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderModels();
    const jobs = await band("Jobs");
    await waitFor(() => expect(within(jobs).getByText("Gemma 4 E2B")).toBeTruthy());
    expect(within(jobs).getByText("llama.cpp")).toBeTruthy();
    expect(within(jobs).queryByText(e2b().name)).toBeNull();
  });

  it("says nothing is chosen, in the household's words, for an unfilled job", async () => {
    renderModels();
    const jobs = await band("Jobs");
    expect(within(jobs).getAllByText("Nothing chosen yet").length).toBeGreaterThanOrEqual(1);
    expect(screen.queryByText("Nothing assigned")).toBeNull();
  });

  it("does not call memory empty when it runs on its built-in model", async () => {
    vi.mocked(api.getActiveRoles).mockResolvedValue({
      ...NO_ROLES, embedding: { provider: "fastembed", model: "" },
    } as never);
    renderModels();
    const jobs = await band("Jobs");
    await waitFor(() => expect(within(jobs).getByText("Built-in default")).toBeTruthy());
  });

  it("points Change at a control that exists", async () => {
    setup({ models: [e2b(HERE)], roles: rolesWith({ provider: "local", model: e2b().name }) });
    renderModels();
    const jobs = await band("Jobs");
    fireEvent.click(await within(jobs).findByRole("button", { name: "Change the model for conversation" }));
    const note = await screen.findByText("Press Use on the model you want for conversation.");
    expect(note).toBeTruthy();
    // ... and a Use button is on the page for it to name.
    expect(screen.queryByText(/Use for conversation/)).toBeNull();
  });
});

describe("Get more", () => {
  it("lists what the pond knows and is not here, by engine, not repeating the picks", async () => {
    setup({ models: [e4b(), e2b(HERE), litertE4b(), litertE2b(), whisper("base.en")] });
    renderModels();
    const more = await band("Get more");
    expect(within(more).getByText("Gemma 4 E2B")).toBeTruthy();
    expect(within(more).getByText("LiteRT-LM runs .litertlm files on the GPU, text only.")).toBeTruthy();
    expect(within(more).queryByText("Gemma 4 E4B")).toBeNull();
    expect(within(more).getByRole("region", { name: "More listening models" })).toBeTruthy();
    expect(within(more).getByText("Whisper base.en (en)")).toBeTruthy();
  });

  it("offers no download for a row that has nowhere to download from", async () => {
    setup({ models: [foundOnDisk({ downloaded: false, acquire: "unavailable" }), e2b(HERE)] });
    renderModels();
    const more = await band("Get more");
    expect(within(more).queryByRole("button", { name: /Download Llama/ })).toBeNull();
  });

  it("searches Hugging Face, and says a file's add-on size before it is spent", async () => {
    vi.mocked(api.searchGgufModels).mockResolvedValue({
      models: [{ id: "unsloth/gemma-4-E4B-it-qat-GGUF", downloads: 1200, likes: 5, tags: [], url: "x" }],
    });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [
        { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", size_mb: 4020, url: "https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/main/gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", pictures: { size_bytes: 991_552_320, label: "Gemma 4 E4B" } },
        { filename: "other.gguf", size_mb: 1000, url: "https://huggingface.co/a/b/resolve/main/other.gguf", pictures: null },
      ],
    });
    vi.mocked(api.downloadModelFromUrl).mockResolvedValue({ status: "downloading", message: "Downloading gemma (4.2 GB) and picture support (945 MB)" });
    setup({ memory: DESKTOP_MEMORY });
    renderModels();
    const more = await band("Get more");
    fireEvent.change(within(more).getByRole("searchbox", { name: "Search Hugging Face for a model" }), { target: { value: "gemma" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /unsloth\/gemma-4-E4B-it-qat-GGUF/ }));

    expect(await within(more).findByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(within(more).getByText("1.0 GB")).toBeTruthy();

    // The add-on is the file's own choice: only a file that has one carries a box.
    const gemma = within(more).getByRole("group", { name: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf" });
    expect(within(within(more).getByRole("group", { name: "other.gguf" })).queryByRole("checkbox")).toBeNull();
    const tick = within(gemma).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(true);
    fireEvent.click(tick);
    expect(within(more).getByText("4.2 GB")).toBeTruthy();

    fireEvent.click(within(gemma).getByRole("button", { name: "Download gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf" }));
    await waitFor(() =>
      expect(api.downloadModelFromUrl).toHaveBeenCalledWith(
        expect.stringContaining("gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"), "gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", { pictures: false },
      ),
    );
  });

  it("starts a file's add-on unticked where only the file fits this pond, and says why", async () => {
    // 4020 MB fits the 4800 MB this pond has for one model; with 945 MB of add-on it does not.
    vi.mocked(api.searchGgufModels).mockResolvedValue({
      models: [{ id: "unsloth/gemma-4-E4B-it-qat-GGUF", downloads: 1200, likes: 5, tags: [], url: "x" }],
    });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [
        { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", size_mb: 4020, url: "https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/main/gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", pictures: { size_bytes: 991_552_320, label: "Gemma 4 E4B" } },
        { filename: "small.gguf", size_mb: 1000, url: "https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/main/small.gguf", pictures: { size_bytes: 991_552_320, label: "Gemma 4 E4B" } },
      ],
    });
    vi.mocked(api.downloadModelFromUrl).mockResolvedValue({ status: "downloading", message: "Downloading gemma (4.2 GB)" });
    renderModels();
    const more = await band("Get more");
    fireEvent.change(within(more).getByRole("searchbox", { name: "Search Hugging Face for a model" }), { target: { value: "gemma" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /unsloth\/gemma-4-E4B-it-qat-GGUF/ }));

    const big = await within(more).findByRole("group", { name: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf" });
    const bigTick = within(big).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(bigTick.checked).toBe(false);
    expect(within(big).getByText("Left out: with pictures it would not fit this pond. Tick to include it anyway.")).toBeTruthy();
    expect(within(big).getByText("4.2 GB")).toBeTruthy();

    // The small file fits with its add-on, so it keeps it, and says nothing is left out.
    const small = within(more).getByRole("group", { name: "small.gguf" });
    expect((within(small).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement).checked).toBe(true);
    expect(within(small).queryByText(/^Left out:/)).toBeNull();
    expect(within(small).getByText("1.0 GB + 945 MB for pictures")).toBeTruthy();

    fireEvent.click(within(big).getByRole("button", { name: "Download gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf" }));
    await waitFor(() =>
      expect(api.downloadModelFromUrl).toHaveBeenCalledWith(
        expect.stringContaining("gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"), "gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", { pictures: false },
      ),
    );

    // Ticked anyway, the person's word stands.
    fireEvent.click(bigTick);
    expect(within(big).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    fireEvent.click(within(big).getByRole("button", { name: "Download gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf" }));
    await waitFor(() =>
      expect(api.downloadModelFromUrl).toHaveBeenLastCalledWith(
        expect.stringContaining("gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"), "gguf", "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", { pictures: true },
      ),
    );
  });

  it("says so when the listing gives a file no size, and still names its add-on's", async () => {
    vi.mocked(api.searchGgufModels).mockResolvedValue({ models: [{ id: "ggml-org/SmolVLM-256M-Instruct-GGUF", downloads: 9, likes: 1, tags: [], url: "x" }] });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [{ filename: "SmolVLM-256M-Instruct-Q8_0.gguf", url: "https://huggingface.co/ggml-org/SmolVLM-256M-Instruct-GGUF/resolve/main/SmolVLM-256M-Instruct-Q8_0.gguf", pictures: { size_bytes: 190_031_616, label: "SmolVLM 256M" } }],
    });
    renderModels();
    const more = await band("Get more");
    fireEvent.change(within(more).getByRole("searchbox"), { target: { value: "smol" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /SmolVLM-256M/ }));
    expect(await within(more).findByText("Size not listed + 181 MB for pictures")).toBeTruthy();
  });

  it("shows the download of a file named by URL on the row it becomes, as soon as it starts", async () => {
    const smol = e2b({
      id: "gguf/SmolVLM-256M-Instruct-Q8_0", name: "SmolVLM-256M-Instruct-Q8_0", title: "SmolVLM 256M",
      filename: "SmolVLM-256M-Instruct-Q8_0.gguf", downloaded: false, provenance: "added", recommended: undefined,
      size_mb: 166, quantization: "Q8_0", context_length: undefined, acquire: "download",
      companions: [{ kind: "pictures", label: "SmolVLM 256M", size_bytes: 190_031_616, state: "downloading" }],
    });
    vi.mocked(api.searchGgufModels).mockResolvedValue({ models: [{ id: "ggml-org/SmolVLM-256M-Instruct-GGUF", downloads: 9, likes: 1, tags: [], url: "x" }] });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [{ filename: "SmolVLM-256M-Instruct-Q8_0.gguf", url: "https://huggingface.co/ggml-org/SmolVLM-256M-Instruct-GGUF/resolve/main/SmolVLM-256M-Instruct-Q8_0.gguf", pictures: { size_bytes: 190_031_616, label: "SmolVLM 256M" } }],
    });
    vi.mocked(api.downloadModelFromUrl).mockResolvedValue({
      status: "downloading", message: "Downloading SmolVLM 256M (166 MB) and picture support (181 MB)",
    });
    renderModels();
    const more = await band("Get more");
    expect(screen.queryByText("SmolVLM 256M")).toBeNull();

    // Once it starts, the pond has the row and the transfers.
    vi.mocked(api.listModels).mockResolvedValue([e2b(HERE), smol]);
    vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: [
      entry({ filename: "SmolVLM-256M-Instruct-Q8_0.gguf", model_id: smol.id, part: "model", total_bytes: 175_054_528, downloaded_bytes: 50_000_000 }),
      entry({ filename: "mmproj/smolvlm-256m-instruct/mmproj-SmolVLM-256M-Instruct-f16.gguf", category: "mmproj", model_id: smol.id, part: "pictures", total_bytes: 190_031_616, downloaded_bytes: 60_000_000 }),
    ] });
    fireEvent.change(within(more).getByRole("searchbox"), { target: { value: "smol" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /SmolVLM-256M/ }));
    fireEvent.click(await within(more).findByRole("button", { name: "Download SmolVLM-256M-Instruct-Q8_0.gguf" }));

    expect(await screen.findByText("Downloading SmolVLM 256M (166 MB) and picture support (181 MB)")).toBeTruthy();
    const row = (await screen.findAllByText("SmolVLM 256M")).map((el) => el.closest(".mdl-row")).find(Boolean) as HTMLElement;
    expect(within(row).getAllByRole("progressbar")).toHaveLength(2);
    expect(within(row).getByText("Added")).toBeTruthy();
    expect(within(row).getByRole("button", { name: "Pause" })).toBeTruthy();
  });

  it("lists no add-on where the pond sends none, as it does on a device with a memory budget", async () => {
    vi.mocked(api.searchGgufModels).mockResolvedValue({ models: [{ id: "u/r", downloads: 1, likes: 0, tags: [], url: "x" }] });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [{ filename: "m.gguf", size_mb: 2000, url: "https://huggingface.co/u/r/resolve/main/m.gguf", pictures: null }],
    });
    vi.mocked(api.downloadModelFromUrl).mockResolvedValue({ status: "download_started", message: "Downloading m (2.1 GB)" });
    renderModels();
    const more = await band("Get more");
    fireEvent.change(within(more).getByRole("searchbox"), { target: { value: "r" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /u\/r/ }));
    expect(await within(more).findByText("2.1 GB")).toBeTruthy();
    expect(within(more).queryByText(/for pictures/)).toBeNull();
    expect(within(more).queryByRole("checkbox")).toBeNull();

    // And the request does not ask about an add-on the file does not have.
    fireEvent.click(within(more).getByRole("button", { name: "Download m.gguf" }));
    await waitFor(() =>
      expect(api.downloadModelFromUrl).toHaveBeenCalledWith("https://huggingface.co/u/r/resolve/main/m.gguf", "gguf", "m.gguf", undefined),
    );
  });

  it("says a file named by URL is already on its way when the pond is already fetching it", async () => {
    vi.mocked(api.searchGgufModels).mockResolvedValue({ models: [{ id: "u/r", downloads: 1, likes: 0, tags: [], url: "x" }] });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [{ filename: "m.gguf", size_mb: 2000, url: "https://huggingface.co/u/r/resolve/main/m.gguf", pictures: null }],
    });
    vi.mocked(api.downloadModelFromUrl).mockResolvedValue({ status: "already_downloading", message: "Downloading m (2.1 GB)" });
    renderModels();
    const more = await band("Get more");
    fireEvent.change(within(more).getByRole("searchbox"), { target: { value: "r" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /u\/r/ }));
    fireEvent.click(await within(more).findByRole("button", { name: "Download m.gguf" }));
    expect(await screen.findByText("m.gguf is already on its way.")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("does not promise picture support on a device that will not carry it", async () => {
    const refusing = e2b({ ...HERE, companions: [{ kind: "pictures", label: "E2B", size_bytes: 986_833_728, state: "not_on_this_device" }] });
    setup({ models: [refusing] });
    vi.mocked(api.searchGgufModels).mockResolvedValue({ models: [{ id: "u/r", downloads: 1, likes: 0, tags: [], url: "x" }] });
    vi.mocked(api.listHfModelFiles).mockResolvedValue({
      files: [{ filename: "m.gguf", size_mb: 2000, url: "https://huggingface.co/u/r/resolve/main/m.gguf", pictures: { size_bytes: 986_833_728, label: "E2B" } }],
    });
    renderModels();
    const more = await band("Get more");
    fireEvent.change(within(more).getByRole("searchbox"), { target: { value: "r" } });
    fireEvent.click(within(more).getByRole("button", { name: "Search" }));
    fireEvent.click(await within(more).findByRole("button", { name: /u\/r/ }));
    expect(await within(more).findByText("2.1 GB")).toBeTruthy();
    expect(within(more).queryByText(/for pictures/)).toBeNull();
    expect(within(more).queryByRole("checkbox")).toBeNull();
  });
});

describe("a pond with nothing on it", () => {
  it("invites the first download rather than showing an empty list", async () => {
    setup({ models: [e4b(), e2b(), litertE4b()] });
    renderModels();
    expect(await screen.findByText("Nothing downloaded yet. Pick one above, or add one below.")).toBeTruthy();
  });

  it("says when the catalogue cannot be read, with a way to try again", async () => {
    vi.mocked(api.listModels).mockRejectedValue(new Error("connection refused"));
    renderModels();
    expect(await screen.findByText("Could not connect to the server. Make sure it is running.")).toBeTruthy();
  });
});

describe("Memory", () => {
  const MEMORY_ROWS = () => [e4b(), e2b(HERE), miniLm(), nomicEmbed(), eg2()];

  async function memoryModels(): Promise<HTMLElement> {
    return (await screen.findByRole("region", { name: "Memory models" })) as HTMLElement;
  }

  it("keeps the page's four bands, and puts the Memory models inside Jobs", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "fastembed") });
    renderModels();
    const jobs = await band("Jobs");
    expect(within(jobs).getByRole("region", { name: "Memory models" })).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
      "Recommended for this pond", "Jobs", "On this device", "Get more",
    ]);
    // Memory is listed once, in its panel, not again as a section of the device list.
    const device = await band("On this device");
    expect(within(device).queryByRole("region", { name: "Memory" })).toBeNull();
  });

  it("recommends EmbeddingGemma 2 first, with what it buys and its size, and one action to use it", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "fastembed") });
    renderModels();
    const memory = await memoryModels();
    const rows = within(memory).getAllByRole("listitem");
    expect(within(rows[0]).getByText("EmbeddingGemma 2")).toBeTruthy();
    expect(within(rows[0]).getByText("Recommended")).toBeTruthy();
    expect(within(rows[0]).getByText("Finds facts in Swahili, Sheng or English, whichever language you ask in")).toBeTruthy();
    expect(within(rows[0]).getByText("295 MB to download")).toBeTruthy();
    expect(within(rows[0]).getAllByRole("button")).toHaveLength(1);
  });

  it("never switches by itself: the default stays in use until the household presses Use", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "fastembed") });
    renderModels();
    const memory = await memoryModels();
    const miniRow = within(memory).getByText("all-MiniLM-L6-v2").closest("li") as HTMLElement;
    expect(within(miniRow).getByText("In use")).toBeTruthy();
    expect(api.activateModel).not.toHaveBeenCalled();
  });

  it("says Download and use, with the size, on a press that starts a download, and plain Use where none is needed", async () => {
    setup({ models: [e2b(HERE), miniLm(), nomicEmbed({ downloaded: true }), eg2()], roles: memoryRoles("", "fastembed") });
    renderModels();
    const memory = await memoryModels();
    const press = within(memory).getByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" });
    expect(press.textContent).toBe("Download and use");
    expect((press as HTMLButtonElement).disabled).toBe(false);
    expect(within(memory).getByRole("button", { name: "Use Nomic Embed Text v1.5 for memory" }).textContent).toBe("Use");
  });

  it("rests Download and use, and says why, when the pond reaches nothing outside the house", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ network_mode: "offline" } as never);
    setup({ models: [e2b(HERE), miniLm(), nomicEmbed({ downloaded: true }), eg2()], roles: memoryRoles("", "fastembed") });
    renderModels();
    const memory = await memoryModels();
    const press = await within(memory).findByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" });
    await waitFor(() => expect((press as HTMLButtonElement).disabled).toBe(true));
    expect(within(memory).getByText("Needs a connection to download")).toBeTruthy();
    expect(press.getAttribute("aria-describedby")).toBe(within(memory).getByText("Needs a connection to download").id);
    // A model already here needs no connection, so it stays pressable.
    expect((within(memory).getByRole("button", { name: "Use Nomic Embed Text v1.5 for memory" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("assigns the Memory role on Use, says a download comes first, and starts watching for it", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "fastembed") });
    renderModels();
    const memory = await memoryModels();
    fireEvent.click(within(memory).getByRole("button", { name: "Download and use EmbeddingGemma 2 for memory, 295 MB" }));
    await waitFor(() =>
      expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"),
    );
    expect(
      await screen.findByText(
        "Using EmbeddingGemma 2 for memory once it downloads (295 MB). Memory searches by keyword until then.",
      ),
    ).toBeTruthy();
    // The download is a separate call, so nothing was fetched through the Download route.
    expect(api.downloadModel).not.toHaveBeenCalled();
  });

  it("shows the download with the same bar, Pause and Stop every model has", async () => {
    setup({
      models: MEMORY_ROWS(),
      roles: memoryRoles("embeddinggemma-2"),
      downloads: [
        entry({ filename: "embeddinggemma-2-Q8_0.gguf", category: "embedding", model_id: eg2().id, part: "model", total_bytes: 309_855_456, downloaded_bytes: 123_000_000 }),
      ],
    });
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "failed", reason: "embeddinggemma-2-Q8_0.gguf is not on disk; the models pipeline is downloading it" }, { matching: 0 }),
    );
    renderModels();
    const memory = await memoryModels();
    expect(await within(memory).findByRole("progressbar", { name: /EmbeddingGemma 2.*model/i })).toBeTruthy();
    expect(within(memory).getByRole("button", { name: "Pause" })).toBeTruthy();
    expect(within(memory).getByRole("button", { name: "Stop" })).toBeTruthy();
    // A file on its way is not a failure, and "In use" waits until it is ready to be.
    expect(await within(memory).findByText("Searching by keyword until EmbeddingGemma 2 finishes downloading")).toBeTruthy();
    expect(within(memory).queryByRole("alert")).toBeNull();
    expect(within(memory).queryByText("In use")).toBeNull();
    expect(within(memory).queryByRole("button", { name: "Use EmbeddingGemma 2 for memory" })).toBeNull();
  });

  it("keeps MiniLM selectable once another model is in use", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
    renderModels();
    const memory = await memoryModels();
    const eg2Row = within(memory).getByText("EmbeddingGemma 2").closest("li") as HTMLElement;
    expect(within(eg2Row).getByText("In use")).toBeTruthy();
    fireEvent.click(within(memory).getByRole("button", { name: "Use all-MiniLM-L6-v2 for memory" }));
    await waitFor(() =>
      expect(api.activateModel).toHaveBeenCalledWith("embedding", "all-MiniLM-L6-v2", "embedding"),
    );
    expect(await screen.findByText(/^Now using all-MiniLM-L6-v2 for memory/)).toBeTruthy();
  });

  it("shows the measured numbers only where the server sent them", async () => {
    const measured = { ...eg2().recommended!, measured: EG2_ORIN };
    setup({ models: [e2b(HERE), miniLm(), eg2({ recommended: measured })], roles: memoryRoles("", "fastembed") });
    renderModels();
    const memory = await memoryModels();
    expect(within(memory).getByText(/top five 73% of the time, against 39% for the default/)).toBeTruthy();
    expect(within(memory).getByText("Measured on this kind of device, 31 Oct 2026")).toBeTruthy();
  });

  it("names the model the Memory card holds, never its whole description", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
    renderModels();
    const jobs = await band("Jobs");
    const card = jobs.querySelector(".mdl-role:nth-child(4)") as HTMLElement;
    await waitFor(() => expect(within(card).getByText("EmbeddingGemma 2")).toBeTruthy());
    expect(card.textContent).not.toContain("Over 100 languages");
    expect(within(card).queryByText("Built-in default")).toBeNull();
  });

  it("points Change at the Memory models", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "fastembed") });
    renderModels();
    const jobs = await band("Jobs");
    fireEvent.click(await within(jobs).findByRole("button", { name: "Change the model for memory" }));
    expect(await screen.findByText("Press Use on the model you want for memory.")).toBeTruthy();
    expect(document.getElementById("mdl-memory")).toBeTruthy();
  });

  describe("what the pond says while it switches", () => {
    it("says it searches by keyword while the model loads", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({ state: "loading" }, { matching: 0 }));
      renderModels();
      const memory = await memoryModels();
      expect(await within(memory).findByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeTruthy();
    });

    it("counts the re-index", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({}, { matching: 40 }));
      renderModels();
      const memory = await memoryModels();
      expect(await within(memory).findByText("Reading memories again: 40 of 95 items")).toBeTruthy();
      expect(within(memory).getByRole("progressbar", { name: "Items read again" }).getAttribute("aria-valuenow")).toBe("42");
    });

    it("says a failed load plainly, names the lighter model, keeps the pond's words behind Details, and asks again without clearing", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      vi.mocked(api.getContextIndexHealth).mockResolvedValue(
        memoryHealth({ state: "failed", reason: "loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory" }, { matching: 0 }),
      );
      renderModels();
      const memory = await memoryModels();
      const alert = await within(memory).findByRole("alert");
      expect(alert.textContent).toContain("There is not enough free memory to load EmbeddingGemma 2, so Memory is searching by keyword.");
      expect(alert.textContent).toContain("Choose the lighter all-MiniLM-L6-v2 (23 MB), or try again.");
      expect(alert.textContent).not.toContain("Gpu");
      const details = within(alert).getByRole("button", { name: "Details" });
      expect(details.getAttribute("aria-expanded")).toBe("false");
      fireEvent.click(details);
      expect(details.getAttribute("aria-expanded")).toBe("true");
      expect(within(alert).getByText("loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory")).toBeTruthy();
      fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
      await waitFor(() =>
        expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"),
      );
    });

    // The household chose it; the pond is the one that knows whether it runs.
    it("does not call a chosen model that failed, or is still loading, in use", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      vi.mocked(api.getContextIndexHealth).mockResolvedValue(
        memoryHealth({ state: "failed", reason: "loading embeddinggemma-2-Q8_0.gguf on the Gpu: out of memory" }, { matching: 0 }),
      );
      renderModels();
      const memory = await memoryModels();
      const row = (await within(memory).findByText("EmbeddingGemma 2", { selector: ".mm-mem__title" })).closest("li") as HTMLElement;
      await waitFor(() => expect(within(row).getByText("Chosen, not loaded")).toBeTruthy());
      expect(within(memory).queryByText("In use")).toBeNull();
    });

    it("says Loading, not In use, while the chosen model loads", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({ state: "loading" }, { matching: 0 }));
      renderModels();
      const memory = await memoryModels();
      expect(await within(memory).findByText("Loading")).toBeTruthy();
      expect(within(memory).queryByText("In use")).toBeNull();
    });

    it("says In use once the chosen model is ready", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      renderModels();
      const memory = await memoryModels();
      const row = (await within(memory).findByText("EmbeddingGemma 2", { selector: ".mm-mem__title" })).closest("li") as HTMLElement;
      await waitFor(() => expect(within(row).getByText("In use")).toBeTruthy());
    });

    it("says it is ready once everything is re-read, and asks nothing more of the pond", async () => {
      setup({ models: MEMORY_ROWS(), roles: memoryRoles("embeddinggemma-2") });
      renderModels();
      const memory = await memoryModels();
      expect(await within(memory).findByText("EmbeddingGemma 2 is ready. Every memory can be found by meaning.")).toBeTruthy();
      expect(within(memory).queryByRole("alert")).toBeNull();
      expect(within(memory).queryByRole("progressbar")).toBeNull();
    });
  });

  it("says search by meaning is off, and turns it on with one press", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "none") });
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "off", model_id: null }, { indexed: false }),
    );
    renderModels();
    const memory = await memoryModels();
    expect(within(memory).getByText("Search by meaning is off. Memory searches by keyword.")).toBeTruthy();
    // Nothing is in use while it is off.
    expect(within(memory).queryByText("In use")).toBeNull();
    fireEvent.click(within(memory).getByRole("button", { name: "Turn on" }));
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ embedding_provider: "auto" }));
    expect(await screen.findByText(/Search by meaning is on/)).toBeTruthy();
  });

  // Nothing loads, fetches or reads again while it is off, so Use may only say the model is set.
  it("says a model chosen while search by meaning is off is used once it is on, and promises no re-read", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "none") });
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "off", model_id: null }, { indexed: false }),
    );
    renderModels();
    const memory = await memoryModels();
    // No download starts while it is off, so the press is a plain Use, with no size to spend.
    fireEvent.click(within(memory).getByRole("button", { name: "Use EmbeddingGemma 2 for memory" }));
    await waitFor(() =>
      expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"),
    );
    const said = await screen.findByText(/EmbeddingGemma 2 is set for memory/);
    expect(said.textContent).toBe(
      "EmbeddingGemma 2 is set for memory. Once you turn on search by meaning, the pond downloads it (295 MB) and uses it.",
    );
    expect(screen.queryByText(/reads memories again|Now using/)).toBeNull();
  });

  it("works on a pond that predates the embedder report", async () => {
    setup({ models: MEMORY_ROWS(), roles: memoryRoles("", "fastembed") });
    const older = memoryHealth({});
    delete older.embedder;
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(older);
    renderModels();
    const memory = await memoryModels();
    expect(within(memory).getAllByRole("listitem")).toHaveLength(3);
    expect(within(memory).queryByRole("status")).toBeNull();
  });
});
