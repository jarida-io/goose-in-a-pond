import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor, within } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listMemories: vi.fn(),
    getContextIndexHealth: vi.fn(),
    getActiveRoles: vi.fn(),
    activateModel: vi.fn(),
  },
}));

import { MemoryDetail } from "./Memory";
import { api } from "../../../api/PondApiClient";
import { memoryRoles } from "../../../sections/models/fixtures";
import type { ContextIndexHealth, EmbedderHealth } from "../../../api/types";

function memoryHealth(embedder: Partial<EmbedderHealth>, over: Partial<ContextIndexHealth> = {}): ContextIndexHealth {
  return {
    indexed: true, model_id: "embeddinggemma-2-256", dims: 256, rows: 95, matching: 95, mismatched: 0,
    missing: 0, coverage: 1, corpora: [],
    embedder: { state: "ready", model_id: "embeddinggemma-2-256", reason: null, ...embedder },
    ...over,
  };
}

async function renderMemory(go: (route: string) => void = () => {}) {
  render(<MemoryDetail go={go} />);
  await screen.findByText("Search by meaning", { selector: ".setcard__title" });
  await waitFor(() => expect(api.getSettings).toHaveBeenCalled());
}

/** The card the screen titles `Search by meaning`, as the classic setting is named. */
function semanticCard(): HTMLElement {
  return screen.getByText("Search by meaning", { selector: ".setcard__title" }).closest(".setcard") as HTMLElement;
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listMemories).mockResolvedValue([]);
  vi.mocked(api.getSettings).mockResolvedValue({ embedding_provider: "auto" } as never);
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({}));
  vi.mocked(api.getActiveRoles).mockResolvedValue(memoryRoles("embeddinggemma-2"));
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
});
afterEach(cleanup);

describe("Hub Memory: search by meaning", () => {
  it("is one switch, with the model chosen on the Models screen", async () => {
    await renderMemory();
    const card = semanticCard();
    expect(within(card).getByRole("button", { name: "Search by meaning" }).getAttribute("aria-pressed")).toBe("true");
    expect(within(card).queryByRole("combobox")).toBeNull();
    expect(card.textContent).not.toMatch(/FastEmbed|GGUF|Embedding provider|Orin/);
  });

  it("opens the Models screen to choose the model", async () => {
    const go = vi.fn();
    await renderMemory(go);
    fireEvent.click(within(semanticCard()).getByRole("button", { name: "Choose a model" }));
    expect(go).toHaveBeenCalledWith("models");
  });

  it("names the model in use, from what the pond reports", async () => {
    await renderMemory();
    expect(await within(semanticCard()).findByText("Using EmbeddingGemma 2.")).toBeTruthy();
  });

  it("says a chosen model that did not load is not in use", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
    );
    await renderMemory();
    expect(await within(semanticCard()).findByText("EmbeddingGemma 2 is chosen but did not load.")).toBeTruthy();
    expect(within(semanticCard()).queryByText(/^Using /)).toBeNull();
  });

  it("says a model still loading is not in use yet", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({ state: "loading" }, { matching: 0 }));
    await renderMemory();
    expect(await within(semanticCard()).findByText("EmbeddingGemma 2 is loading.")).toBeTruthy();
    expect(within(semanticCard()).queryByText(/^Using /)).toBeNull();
  });

  it("does not claim the Swahili example for whatever model is in use", async () => {
    await renderMemory();
    expect(semanticCard().textContent).not.toMatch(/Swahili/);
  });

  it("reads the older engine names as on, so the switch shows what the pond does", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ embedding_provider: "fastembed" } as never);
    await renderMemory();
    await waitFor(() =>
      expect(within(semanticCard()).getByRole("button", { name: "Search by meaning" }).getAttribute("aria-pressed")).toBe("true"),
    );
  });

  it("shows the switch off when the pond has it off, once the settings arrive", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ embedding_provider: "none" } as never);
    await renderMemory();
    await waitFor(() =>
      expect(within(semanticCard()).getByRole("button", { name: "Search by meaning" }).getAttribute("aria-pressed")).toBe("false"),
    );
  });

  it("writes \"none\" when switched off, and says what Memory does instead", async () => {
    await renderMemory();
    fireEvent.click(within(semanticCard()).getByRole("button", { name: "Search by meaning" }));
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ embedding_provider: "none" }));
    expect(await screen.findByText("Search by meaning is off. Memory searches by keyword.")).toBeTruthy();
  });

  it("writes \"auto\" when switched on, so the Memory model decides the engine", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ embedding_provider: "none" } as never);
    await renderMemory();
    await waitFor(() =>
      expect(within(semanticCard()).getByRole("button", { name: "Search by meaning" }).getAttribute("aria-pressed")).toBe("false"),
    );
    fireEvent.click(within(semanticCard()).getByRole("button", { name: "Search by meaning" }));
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ embedding_provider: "auto" }));
    expect(await screen.findByText("Search by meaning is on. The pond reads memories again at the next quiet moment.")).toBeTruthy();
  });

  it("puts the switch back and says why when the pond refuses", async () => {
    vi.mocked(api.updateSettings).mockRejectedValue(new Error("settings are locked"));
    await renderMemory();
    fireEvent.click(within(semanticCard()).getByRole("button", { name: "Search by meaning" }));
    expect(await screen.findByText(/settings are locked/)).toBeTruthy();
    await waitFor(() =>
      expect(within(semanticCard()).getByRole("button", { name: "Search by meaning" }).getAttribute("aria-pressed")).toBe("true"),
    );
  });
});

describe("Hub Memory: what the pond says while it switches", () => {
  it("says it is ready once the store is re-read", async () => {
    await renderMemory();
    expect(await within(semanticCard()).findByText("EmbeddingGemma 2 is ready. Every memory can be found by meaning.")).toBeTruthy();
  });

  it("says it searches by keyword while the model loads", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({ state: "loading" }, { matching: 0 }));
    await renderMemory();
    expect(await within(semanticCard()).findByText("Searching by keyword while EmbeddingGemma 2 loads")).toBeTruthy();
  });

  it("counts the re-index", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(memoryHealth({}, { matching: 40 }));
    await renderMemory();
    expect(await within(semanticCard()).findByText("Reading memories again: 40 of 95 items")).toBeTruthy();
  });

  it("says a failed load plainly, keeps the pond's words behind Details, and asks again without clearing anything", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "failed", reason: "loading x.gguf on the Gpu: out of memory" }, { matching: 0 }),
    );
    await renderMemory();
    const alert = await within(semanticCard()).findByRole("alert");
    expect(alert.textContent).toContain("There is not enough free memory to load EmbeddingGemma 2");
    expect(alert.textContent).toContain("all-MiniLM-L6-v2 (23 MB)");
    expect(within(alert).queryByText("loading x.gguf on the Gpu: out of memory")).toBeNull();
    fireEvent.click(within(alert).getByRole("button", { name: "Details" }));
    expect(within(alert).getByText("loading x.gguf on the Gpu: out of memory")).toBeTruthy();
    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("embedding", "embeddinggemma-2", "embedding"));
  });

  it("says search by meaning is off when the pond reports it off", async () => {
    vi.mocked(api.getContextIndexHealth).mockResolvedValue(
      memoryHealth({ state: "off", model_id: null }, { indexed: false, model_id: null }),
    );
    await renderMemory();
    expect(await within(semanticCard()).findByText("Search by meaning is off. Memory searches by keyword.")).toBeTruthy();
    expect(within(semanticCard()).getByText("Chosen on the Models screen.")).toBeTruthy();
  });
});
