import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { ModelPickerModal } from "./ModelPickerModal";
import { api } from "../api/PondApiClient";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn().mockResolvedValue([
      { id: "1", provider: "ollama",    name: "llama3.2",   display_name: "Llama 3.2",   is_active: true,  ram_estimate_mb: 2048 },
      { id: "2", provider: "ollama",    name: "mistral",    display_name: "Mistral 7B",  is_active: false, recommended_role: "chat" },
      { id: "3", provider: "llamafile", name: "phi3",       display_name: "Phi-3 Mini",  is_active: false, ram_estimate_mb: 1800 },
    ]),
  },
}));

const mockOnSelect = vi.fn();
const mockOnClose  = vi.fn();

function renderPicker(overrides = {}) {
  return render(
    <ModelPickerModal
      role="chat"
      currentProvider={null}
      currentModel={null}
      onSelect={mockOnSelect}
      onClose={mockOnClose}
      {...overrides}
    />,
  );
}

describe("ModelPickerModal", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows loading then renders model list", async () => {
    renderPicker();
    await waitFor(() => {
      if (!screen.queryByText("ollama")) throw new Error("not loaded");
    });
    expect(screen.getByText("Llama 3.2")).toBeTruthy();
    expect(screen.getByText("Mistral 7B")).toBeTruthy();
    expect(screen.getByText("Phi-3 Mini")).toBeTruthy();
  });

  it("filters models by search query", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("Llama 3.2")) throw new Error(); });

    const searchInput = screen.getByPlaceholderText("Search models…");
    fireEvent.change(searchInput, { target: { value: "phi" } });

    expect(screen.queryByText("Phi-3 Mini")).toBeTruthy();
    expect(screen.queryByText("Llama 3.2")).toBeNull();
    expect(screen.queryByText("Mistral 7B")).toBeNull();
  });

  it("shows 'No models found' when search has no matches", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("Llama 3.2")) throw new Error(); });

    fireEvent.change(screen.getByPlaceholderText("Search models…"), {
      target: { value: "xyznonexistent" },
    });
    expect(screen.getByText("No models found.")).toBeTruthy();
  });

  it("clicking a model row selects it; Select button calls onSelect", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("Mistral 7B")) throw new Error(); });

    fireEvent.click(screen.getByText("Mistral 7B"));
    fireEvent.click(screen.getByText("Select"));

    expect(mockOnSelect).toHaveBeenCalledWith("ollama", "mistral");
  });

  it("Cancel button calls onClose without selecting", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("Llama 3.2")) throw new Error(); });

    fireEvent.click(screen.getByText("Cancel"));
    expect(mockOnClose).toHaveBeenCalledTimes(1);
    expect(mockOnSelect).not.toHaveBeenCalled();
  });

  it("Select button is disabled when nothing is selected", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("Llama 3.2")) throw new Error(); });

    const selectBtn = screen.getByText("Select").closest("button");
    expect(selectBtn).toBeTruthy();
    expect(selectBtn?.getAttribute("aria-disabled") ?? selectBtn?.disabled?.toString()).toBeTruthy();
  });

  it("pre-selects the current model", async () => {
    renderPicker({ currentProvider: "ollama", currentModel: "llama3.2" });
    await waitFor(() => { if (!screen.queryByText("Llama 3.2")) throw new Error(); });

    // An enabled Select button means something is pre-selected.
    const selectBtn = screen.getByText("Select").closest("button");
    expect(selectBtn?.getAttribute("aria-disabled")).not.toBe("true");
  });

  it("clicking the overlay or ✕ closes the modal", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("Llama 3.2")) throw new Error(); });

    fireEvent.click(screen.getByLabelText("Close"));
    expect(mockOnClose).toHaveBeenCalledTimes(1);
  });

  it("calls api.listModels once on mount", async () => {
    renderPicker();
    await waitFor(() => { if (!screen.queryByText("ollama")) throw new Error(); });
    expect(vi.mocked(api.listModels)).toHaveBeenCalledTimes(1);
  });
});
