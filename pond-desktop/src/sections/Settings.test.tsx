import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Settings } from "./Settings";
import { api } from "../api/PondApiClient";

// ── Mocks ─────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn().mockResolvedValue({
      assistant_name: "Pond",
      user_name: "Jerry",
      prompt_style: "balanced",
      agent_memory_inject: false,
      weather_enabled: false,
      chat_provider: "llamafile",
      chat_model: "llama3.2",
      llm_provider: "llamafile",
      llm_temperature: 0.7,
      llm_max_tokens: 1024,
      agent_goose_mode: "auto",
      agent_max_turns: 10,
      agent_memory_limit: 5,
      weather_latitude: 0,
      weather_longitude: 0,
      retention_event_log_days: 30,
      retention_sensor_days: 7,
      retention_session_messages_keep: 100,
      voice_wake_word: "goose",
      voice_wake_word_transcriptions: [],
    }),
    updateSettings: vi.fn().mockResolvedValue({}),
    resetWakeWordCalibration: vi.fn().mockResolvedValue(undefined),
    listModels: vi.fn().mockResolvedValue([]),
    getActiveRoles: vi.fn().mockResolvedValue({ chat: null, think: null, task: null, asr: null, tts: null }),
  },
}));

vi.mock("../state/AppContext", () => ({
  useAppState: () => ({ serverUrl: "http://127.0.0.1:4000" }),
  useAppDispatch: () => vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

// ── Helpers ───────────────────────────────────────────────────

async function renderSettings() {
  render(<Settings />);
  // Wait for getSettings to resolve — Identity tab content appears
  await waitFor(() => {
    const input = screen.queryByPlaceholderText("Friend");
    if (!input) throw new Error("not loaded");
  });
}

function clickTab(label: string) {
  // HeroUI Tabs renders tabs with role="tab" — try that first, fall back to text
  const byRole = screen.queryAllByRole("tab", { name: new RegExp(label, "i") });
  if (byRole.length > 0) {
    fireEvent.click(byRole[0]);
  } else {
    fireEvent.click(screen.getAllByText(label)[0]);
  }
}

// ── Tests ─────────────────────────────────────────────────────

describe("Settings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders Identity tab by default with user_name field present", async () => {
    await renderSettings();
    const input = screen.getByPlaceholderText("Friend") as HTMLInputElement;
    expect(input.value).toBe("Jerry");
  });

  it("Voice tab shows voice_wake_word field", async () => {
    await renderSettings();
    clickTab("Voice");
    // voice_wake_word field has placeholder "goose"
    await waitFor(() => {
      if (!screen.queryByPlaceholderText("goose")) throw new Error("not rendered");
    });
    expect(screen.getByPlaceholderText("goose")).toBeTruthy();
  });

  it("Voice tab shows 'Not calibrated' and Calibrate button when transcriptions empty", async () => {
    await renderSettings();
    clickTab("Voice");
    await waitFor(() => {
      if (!screen.queryByText("Not calibrated")) throw new Error("not rendered");
    });
    expect(screen.getByText("Not calibrated")).toBeTruthy();
    expect(screen.getByText("Calibrate")).toBeTruthy();
  });

  it("Models tab shows Main LLM and Tool Caller sections", async () => {
    await renderSettings();
    clickTab("Models");
    await waitFor(() => {
      if (!screen.queryByText("Main LLM")) throw new Error("not rendered");
    });
    expect(screen.getByText("Main LLM")).toBeTruthy();
    expect(screen.getByText("Tool Caller")).toBeTruthy();
    // The main LLM role row shows a "Change…" button
    const changeBtns = screen.getAllByText("Change…");
    expect(changeBtns.length).toBeGreaterThanOrEqual(1);
    // Current chat model is shown in the display row
    expect(screen.getByText("llamafile / llama3.2")).toBeTruthy();
  });

  it("Prompts tab renders 4 prompt_style radio options", async () => {
    await renderSettings();
    clickTab("Prompts");
    await waitFor(() => {
      if (!screen.queryByText("Balanced")) throw new Error("not rendered");
    });
    expect(screen.getByText("Balanced")).toBeTruthy();
    expect(screen.getByText("Concise")).toBeTruthy();
    expect(screen.getByText("Technical")).toBeTruthy();
    expect(screen.getByText("Warm")).toBeTruthy();
  });

  it("Location tab shows weather_enabled toggle and lat/lon disabled when off", async () => {
    await renderSettings();
    clickTab("Location");
    await waitFor(() => {
      if (screen.queryAllByText("Enable weather").length === 0) throw new Error("not rendered");
    });
    expect(screen.queryAllByText("Enable weather").length).toBeGreaterThan(0);
    // lat input is disabled when weather_enabled is false
    const latInput = screen.getByPlaceholderText("-1.2921") as HTMLInputElement;
    expect(latInput.disabled).toBe(true);
  });

  it("Agent tab shows 3 agent_goose_mode radio options and memory_limit disabled when inject off", async () => {
    await renderSettings();
    clickTab("Agent");
    await waitFor(() => {
      if (!screen.queryByText("Smart (recommended)")) throw new Error("not rendered");
    });
    // New friendly labels
    expect(screen.getByText("Smart (recommended)")).toBeTruthy();
    expect(screen.getByText("Chat only")).toBeTruthy();
    expect(screen.getByText("Proactive")).toBeTruthy();
    // memory_limit number input is disabled when agent_memory_inject is false
    // It's a spinbutton with value "5" from mock data
    const spinbtns = screen.getAllByRole("spinbutton") as HTMLInputElement[];
    const memLimit = spinbtns.find((el) => el.disabled && el.value === "5");
    expect(memLimit).toBeTruthy();
  });

  it("Data tab shows retention_event_log_days field", async () => {
    await renderSettings();
    clickTab("Data");
    await waitFor(() => {
      if (!screen.queryByText("Event logs")) throw new Error("not rendered");
    });
    expect(screen.getByText("Event logs")).toBeTruthy();
    const spinbtns = screen.getAllByRole("spinbutton") as HTMLInputElement[];
    expect(spinbtns.length).toBeGreaterThan(0);
  });

  it("Save button calls api.updateSettings with current state", async () => {
    await renderSettings();
    // Save button text is "Save Settings"
    fireEvent.click(screen.getByText("Save Settings"));
    await waitFor(() => {
      if (vi.mocked(api.updateSettings).mock.calls.length === 0) {
        throw new Error("updateSettings not called yet");
      }
    });
    expect(vi.mocked(api.updateSettings)).toHaveBeenCalledTimes(1);
    const arg = vi.mocked(api.updateSettings).mock.calls[0][0];
    expect(arg).toMatchObject({ assistant_name: "Pond", user_name: "Jerry" });
  });
});
