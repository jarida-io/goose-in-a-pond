import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { SettingsCatalogueView } from "./SettingsCatalogue";
import { summariseRetitle, parseText } from "./catalogueView";
import { api } from "../api/PondApiClient";
import { SETUP_WAITING, makeConnections } from "../network/connectionsFixtures";

// ── Mocks ─────────────────────────────────────────────────────

const SERVER_SETTINGS = {
  user_name: "Jerry",
  assistant_name: "Goose",
  timezone: "Africa/Nairobi",
  mic_enabled: true,
  cameras_enabled: true,
  vision_enabled: false,
  memory_extraction_enabled: true,
  unprompted_speech_enabled: false,
  network_mode: "open",
  quiet_hours_start: "22:00",
  quiet_hours_end: "07:00",
  unprompted_speech_categories: "alert",
  weather_latitude: -1.286,
  weather_longitude: 36.817,
  matter_ws_url: "ws://127.0.0.1:5580/giap",
  chat_model: "gemma-4-E4B",
  agent_max_turns: 50,
  tool_call_validation: true,
};

const MODELS = [
  { id: "1", provider: "gguf", name: "gemma-4-E4B", is_active: true, downloaded: true },
  { id: "2", provider: "gguf", name: "qwen3-1.7b", is_active: false, downloaded: true },
  { id: "3", provider: "whisper", name: "base", is_active: true, downloaded: true },
];

function serverSettings(overrides: Record<string, unknown> = {}) {
  return { ...structuredClone(SERVER_SETTINGS), ...overrides };
}

// The device's zone, set per test: reading the real one couples the suite to the host (CI is UTC).
let deviceZoneValue = "Africa/Nairobi";

vi.mock("../lib/place", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/place")>()),
  deviceZone: () => deviceZoneValue,
}));

vi.mock("../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listModels: vi.fn(),
    retitleSessions: vi.fn(),
    resetOnboarding: vi.fn(),
    listTimeZones: vi.fn(),
    detectLocation: vi.fn(),
    getAudioDevices: vi.fn().mockRejectedValue(new Error("not under test here")),
    listProfiles: vi.fn(),
    testAudioDevices: vi.fn(),
    getConnections: vi.fn(),
    getSetupStatus: vi.fn(),
    getSystemInfo: vi.fn(),
    scanWifi: vi.fn(),
  },
}));

const mockApi = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  updateSettings: ReturnType<typeof vi.fn>;
  listModels: ReturnType<typeof vi.fn>;
  retitleSessions: ReturnType<typeof vi.fn>;
  resetOnboarding: ReturnType<typeof vi.fn>;
  listTimeZones: ReturnType<typeof vi.fn>;
  detectLocation: ReturnType<typeof vi.fn>;
  getConnections: ReturnType<typeof vi.fn>;
  getSetupStatus: ReturnType<typeof vi.fn>;
  getSystemInfo: ReturnType<typeof vi.fn>;
  listProfiles: ReturnType<typeof vi.fn>;
};

const MEMBERS = [
  { id: "7491ea2c", display_name: "Emmanuel Charles Oyugi", avatar_emoji: "*" },
  { id: "2c9c9225", display_name: "Jerry", avatar_emoji: "*" },
];

/** A re-titling reply with the boring fields filled in. */
function retitleReply(over: Record<string, unknown> = {}) {
  return { started: true, ...over };
}

/** Render and wait for the first paint after settings load. */
async function renderPage(overrides: Record<string, unknown> = {}, models: unknown[] = MODELS) {
  mockApi.getSettings.mockResolvedValue(serverSettings(overrides));
  mockApi.listModels.mockResolvedValue(models);
  mockApi.listProfiles.mockResolvedValue({ profiles: MEMBERS });
  mockApi.listTimeZones.mockResolvedValue({
    zones: [
      { zone: "Africa/Nairobi", offset: "+03:00", place: "Nairobi" },
      { zone: "Africa/Kampala", offset: "+03:00", place: "Kampala" },
      { zone: "UTC", offset: "+00:00", place: "" },
    ],
  });
  mockApi.updateSettings.mockImplementation(async (patch: Record<string, unknown>) =>
    serverSettings({ ...overrides, ...patch }));
  const view = render(<SettingsCatalogueView />);
  await screen.findByText("Who lives here");
  return view;
}

/** The row that owns a given control, found by its accessible name. */
function rowFor(label: string): HTMLElement {
  const control = screen.getByLabelText(label);
  const row = control.closest(".scat__row");
  if (!row) throw new Error(`no row for "${label}"`);
  return row as HTMLElement;
}

beforeEach(() => {
  vi.clearAllMocks();
  deviceZoneValue = "Africa/Nairobi";
});
afterEach(cleanup);

describe("SettingsCatalogue", () => {
  it("shows what the pond may do as a sentence, from live values", async () => {
    await renderPage();
    const sentence = document.querySelector(".scat__sentence")!;
    expect(sentence.textContent).toContain("listens");
    expect(sentence.textContent).toContain("watches nothing");
    expect(sentence.textContent).toContain("speaks only when spoken to");
    expect(sentence.textContent).toContain("any host on the internet");
  });

  it("rewrites the sentence when the reach changes, without saving", async () => {
    await renderPage({ network_mode: "offline" });
    expect(document.querySelector(".scat__sentence")!.textContent)
      .toContain("Nothing leaves this house.");
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  it("says it watches nothing while camera consent is off, whatever vision says", async () => {
    await renderPage({ vision_enabled: true, cameras_enabled: false });
    expect(document.querySelector(".scat__sentence")!.textContent).toContain("watches nothing");
  });

  it("does not offer a control that nothing reads", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));

    // `cloud_fallback_enabled` is read by nothing, so a household is not shown it.
    expect(screen.queryByLabelText("Fall back to a cloud model")).toBeNull();

    expect((screen.getByLabelText("Microphone") as HTMLInputElement).disabled).toBe(false);
    // The camera consent switch stops capture, so it is offered beside the microphone.
    expect((screen.getByLabelText("Cameras") as HTMLInputElement).disabled).toBe(false);
  });

  it("shows the inert ones, marked, in developer view", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));

    const fallback = screen.getByLabelText("Fall back to a cloud model") as HTMLInputElement;
    expect(fallback.disabled).toBe(true);
    expect(rowFor("Fall back to a cloud model").textContent).toContain("Nothing reads this");
    expect(rowFor("Fall back to a cloud model").textContent).toContain("cloud_fallback_enabled");
  });

  it("keeps field names out of sight until asked for", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));

    // The description is there, so the row says more than its label.
    expect(rowFor("Microphone").textContent).not.toContain("mic_enabled");
    expect(rowFor("Microphone").textContent!.length).toBeGreaterThan("Microphone".length + 20);

    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));
    expect(rowFor("Microphone").textContent).toContain("mic_enabled");
  });

  it("offers a way back into setup, behind developer view and behind a confirm", async () => {
    mockApi.resetOnboarding.mockResolvedValue({
      onboarded: false, current_step: "welcome", steps_completed: 0, total_steps: 5,
    });
    await renderPage();

    expect(screen.queryByRole("button", { name: /Start onboarding/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));
    const start = screen.getByRole("button", { name: /Start onboarding/ });

    // First press only arms it — re-running setup throws away what it collected.
    fireEvent.click(start);
    expect(mockApi.resetOnboarding).not.toHaveBeenCalled();
    await screen.findByRole("button", { name: /Yes, start setup/ });
  });

  it("disarms the setup button when developer view is switched off", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));
    fireEvent.click(screen.getByRole("button", { name: /Start onboarding/ }));
    await screen.findByRole("button", { name: /Yes, start setup/ });

    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));
    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));
    // Never found half-pressed.
    expect(screen.getByRole("button", { name: /Start onboarding/ })).toBeTruthy();
    expect(mockApi.resetOnboarding).not.toHaveBeenCalled();
  });

  it("folds the banner away and keeps answering its question while folded", async () => {
    await renderPage();
    const toggle = screen.getByRole("button", { name: /Right now/ });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");

    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    // Folded, it still says what the pond may do — a summary, not a blank strip.
    expect(document.querySelector(".scat__postureGist")!.textContent).toContain("listens");
  });

  it("contracts the search to its icon and keeps what was typed", async () => {
    await renderPage();
    const box = screen.getByLabelText("Search settings") as HTMLInputElement;
    fireEvent.change(box, { target: { value: "camera" } });
    await screen.findByText(/result/);

    // Pressing the icon while open clears and closes — one control, both ways.
    fireEvent.click(screen.getByRole("button", { name: /Close search/ }));
    expect((screen.getByLabelText("Search settings") as HTMLInputElement).value).toBe("");
    expect(document.querySelector(".scat__search")!.getAttribute("data-open")).toBe("false");
  });

  // With no backend `dev:vite` serves index.html for /api, which `request` casts to `Settings`.
  it("shows an error rather than crashing when the reply is not settings", async () => {
    mockApi.getSettings.mockResolvedValue(undefined);
    render(<SettingsCatalogueView />);
    // `ErrorBanner` rewrites the wording, so assert on the role.
    expect(await screen.findByRole("alert")).toBeTruthy();
  });

  it("offers every zone the server knows, not a hand-picked few", async () => {
    await renderPage();
    const picker = (await screen.findByLabelText("Time zone")) as HTMLSelectElement;
    const values = [...picker.options].map((o) => o.value);
    expect(values).toContain("Africa/Kampala");
    // The offset is shown, so two similarly-named zones can be told apart.
    expect([...picker.options].map((o) => o.textContent).join(" ")).toContain("+03:00");
  });

  it("fills the place and both coordinates from one press", async () => {
    // No browser geolocation: the normal case in the desktop shell's webview.
    vi.stubGlobal("navigator", { ...navigator, geolocation: undefined });
    mockApi.detectLocation.mockResolvedValue({
      name: "Nairobi, Kenya",
      latitude: -1.2864,
      longitude: 36.8172,
      timezone: "Africa/Nairobi",
      source: "geocoded",
      certain: true,
      has_coordinates: true,
      note: null,
    });

    await renderPage({ weather_location_name: "" });
    fireEvent.click(screen.getByRole("button", { name: /Detect/ }));

    await waitFor(() =>
      expect((screen.getByLabelText("Latitude") as HTMLInputElement).value).toBe("-1.2864"));
    expect((screen.getByLabelText("Longitude") as HTMLInputElement).value).toBe("36.8172");
    expect((screen.getByLabelText("Location") as HTMLInputElement).value).toBe("Nairobi, Kenya");
    vi.unstubAllGlobals();
  });

  it("says when the place was inferred rather than found", async () => {
    vi.stubGlobal("navigator", { ...navigator, geolocation: undefined });
    mockApi.detectLocation.mockResolvedValue({
      name: "Nairobi",
      latitude: 0,
      longitude: 0,
      timezone: "Africa/Nairobi",
      source: "timezone",
      certain: false,
      has_coordinates: false,
      note: "Worked out the time zone, but not the exact spot.",
    });

    await renderPage({ weather_location_name: "" });
    fireEvent.click(screen.getByRole("button", { name: /Detect/ }));

    await screen.findByText(/not the exact spot/);
    // Null Island must never be written as if it were a fix.
    expect((screen.getByLabelText("Latitude") as HTMLInputElement).value).toBe("-1.286");
    vi.unstubAllGlobals();
  });

  it("looks up the name already in the box", async () => {
    vi.stubGlobal("navigator", { ...navigator, geolocation: undefined });
    mockApi.detectLocation.mockResolvedValue({
      name: "Kisumu, Kenya",
      latitude: -0.1022,
      longitude: 34.7617,
      timezone: "Africa/Nairobi",
      source: "geocoded",
      certain: true,
      has_coordinates: true,
      note: null,
    });

    await renderPage({ weather_location_name: "Kisumu" });
    fireEvent.click(screen.getByRole("button", { name: /Detect/ }));

    await waitFor(() => expect(mockApi.detectLocation).toHaveBeenCalled());
    expect(mockApi.detectLocation.mock.calls[0][0]).toMatchObject({ typed_name: "Kisumu" });
    vi.unstubAllGlobals();
  });

  it("still offers appearance, which the pond has no say in", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /^Appearance/ }));
    expect(document.querySelector(".scat__panel--appearance")).toBeTruthy();
  });

  it("still offers wake-word training beside the phrase", async () => {
    await renderPage({ voice_wake_word: "hey goose" });
    fireEvent.click(screen.getByRole("button", { name: /^Voice/ }));
    const train = screen.getByRole("button", { name: /Train/ }) as HTMLButtonElement;
    expect(train.disabled).toBe(false);
  });

  it("will not train a phrase that has not been typed yet", async () => {
    await renderPage({ voice_wake_word: "" });
    fireEvent.click(screen.getByRole("button", { name: /^Voice/ }));
    expect((screen.getByRole("button", { name: /Train/ }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("leaves the model-role mirrors to the Models page", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    // Mirrors of `model_role_assignments`: catalogued, not offered.
    expect(screen.queryByLabelText("Chat model")).toBeNull();
    expect(screen.queryByLabelText("Embedding model")).toBeNull();
  });

  it("counts the inert settings per category in the rail", async () => {
    await renderPage();
    const privacy = screen.getByRole("button", { name: /Privacy & Security/ });
    // cloud_fallback_enabled
    expect(within(privacy).getByTitle(/1 setting here that nothing reads/)).toBeTruthy();
  });

  it("blocks the save while a field is invalid, and says which", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Automations/ }));

    const quiet = screen.getByLabelText("Quiet from");
    fireEvent.change(quiet, { target: { value: "10pm" } });

    // The message names the fix rather than the rule.
    await screen.findByText(/Use a 24-hour time like 22:00/);
    const save = screen.getByRole("button", { name: /Fix 1 field/ }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.click(save);
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  it("sends only what changed, and adopts the value the server answers with", async () => {
    await renderPage();
    fireEvent.change(screen.getByLabelText("Your name"), { target: { value: "Anyumba" } });

    const save = await screen.findByRole("button", { name: /Save 1 change/ });
    fireEvent.click(save);

    await waitFor(() => expect(mockApi.updateSettings).toHaveBeenCalledTimes(1));
    // A patch, not the whole object.
    expect(mockApi.updateSettings).toHaveBeenCalledWith({ user_name: "Anyumba" });
    // "Save" becomes "Saved" and the panel settles clean.
    await screen.findByRole("button", { name: /^Saved$/ });
  });

  it("shows the server's own words when a save is refused", async () => {
    await renderPage();
    mockApi.updateSettings.mockRejectedValueOnce(
      new Error('network_mode "opn" is not one of ["open", "allowlist", "offline"]'));

    fireEvent.change(screen.getByLabelText("Your name"), { target: { value: "X" } });
    fireEvent.click(await screen.findByRole("button", { name: /Save 1 change/ }));

    await screen.findByText(/is not one of/);
  });

  it("offers the household profile as a pick of members by name, and saves the id", async () => {
    await renderPage({ primary_profile_id: null });
    const picker = (await waitFor(() => {
      const el = screen.getByLabelText("Household profile");
      if (el.tagName !== "SELECT") throw new Error("still a text box");
      return el;
    })) as HTMLSelectElement;
    expect([...picker.options].map((o) => o.text)).toEqual([
      "Not set",
      "Emmanuel Charles Oyugi",
      "Jerry",
    ]);

    fireEvent.change(picker, { target: { value: "7491ea2c" } });
    fireEvent.click(await screen.findByRole("button", { name: /Save 1 change/ }));
    await waitFor(() =>
      expect(mockApi.updateSettings).toHaveBeenCalledWith({ primary_profile_id: "7491ea2c" }),
    );
  });

  it("says a stored household profile names nobody rather than hiding it", async () => {
    // What one pond held on 2026-10-28: a display name typed into the old text box.
    await renderPage({ primary_profile_id: "Emmanuel Charles Oyugi" });
    const picker = (await waitFor(() => {
      const el = screen.getByLabelText("Household profile");
      if (el.tagName !== "SELECT") throw new Error("still a text box");
      return el;
    })) as HTMLSelectElement;
    expect(picker.value).toBe("Emmanuel Charles Oyugi");
    expect([...picker.options].map((o) => o.text)).toContain(
      "Emmanuel Charles Oyugi — not a household member",
    );
  });

  it("fills pickers from the model registry", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    const model = screen.getByLabelText("Model") as HTMLSelectElement;
    const values = [...model.options].map((o) => o.value);
    expect(values).toContain("gemma-4-E4B");
    expect(values).toContain("qwen3-1.7b");
    // Whisper models belong to the ASR picker, not this one.
    expect(values).not.toContain("base");
  });

  // Commented out with the guess-ahead switch (speculative decoding left the engine); restore with it.
  // it("offers the guess-ahead switch under Models, drawn from the server value", async () => {
  //   // Passed explicitly both ways -- the catalogue reads an ABSENT key as
  //   // OFF (`Boolean(value)`), so a test relying on the default-true rather
  //   // than passing the key would pass for the wrong reason.
  //   await renderPage({ speculative_decoding_enabled: false });
  //   fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
  //   const off = screen.getByLabelText("Guess ahead with a helper model") as HTMLInputElement;
  //   expect(off.checked).toBe(false);
  //   cleanup();
  //
  //   await renderPage({ speculative_decoding_enabled: true });
  //   fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
  //   const on = screen.getByLabelText("Guess ahead with a helper model") as HTMLInputElement;
  //   expect(on.checked).toBe(true);
  // });
  //
  // it("saves only the guess-ahead key", async () => {
  //   await renderPage({ speculative_decoding_enabled: true });
  //   fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
  //   fireEvent.click(screen.getByLabelText("Guess ahead with a helper model"));
  //
  //   const save = await screen.findByRole("button", { name: /Save 1 change/ });
  //   fireEvent.click(save);
  //
  //   await waitFor(() => expect(mockApi.updateSettings).toHaveBeenCalledTimes(1));
  //   expect(mockApi.updateSettings).toHaveBeenCalledWith({ speculative_decoding_enabled: false });
  // });

  it("keeps a configured model the registry does not list", async () => {
    await renderPage({ chat_model: "some-model-i-removed" });
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    const model = screen.getByLabelText("Model") as HTMLSelectElement;
    expect(model.value).toBe("some-model-i-removed");
    expect([...model.options].map((o) => o.text)).toContain("some-model-i-removed — not installed");
  });

  describe("a model on this device's own engine", () => {
    const WITH_LITERT = [
      ...MODELS,
      { id: "4", provider: "litert", name: "gemma-4-E2B-it.litertlm", is_active: true, downloaded: true },
    ];
    const texts = (select: HTMLSelectElement) => [...select.options].map((o) => o.text);

    it("offers a LiteRT-LM model for conversation, under the provider the server stores", async () => {
      await renderPage({ chat_provider: "local", chat_model: "gemma-4-E2B-it.litertlm" }, WITH_LITERT);
      fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
      const model = screen.getByLabelText("Model") as HTMLSelectElement;
      expect(model.value).toBe("gemma-4-E2B-it.litertlm");
      expect(texts(model)).not.toContain("gemma-4-E2B-it.litertlm — not installed");
      const provider = screen.getByLabelText("Provider") as HTMLSelectElement;
      expect(provider.value).toBe("local");
      expect(texts(provider)).toContain("local");
      expect(texts(provider).some((t) => t.includes("not installed"))).toBe(false);
      expect(texts(provider)).not.toContain("gguf");
    });

    it("keeps a LiteRT-LM model off the tool-call helper, which cannot run one", async () => {
      await renderPage({}, WITH_LITERT);
      fireEvent.click(screen.getByRole("button", { name: /^Extensions/ }));
      const helper = screen.getByLabelText("Tool-call helper model") as HTMLSelectElement;
      expect(texts(helper)).toContain("qwen3-1.7b · llama.cpp");
      expect(texts(helper).join("|")).not.toMatch(/litertlm|LiteRT-LM/);
    });

    it("shows a provider saved as gguf as local, without counting it as a change", async () => {
      await renderPage({ chat_provider: "gguf" });
      fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
      expect((screen.getByLabelText("Provider") as HTMLSelectElement).value).toBe("local");
      expect(screen.queryByRole("button", { name: /Save \d+ change/ })).toBeNull();
    });
  });

  describe("model pickers read Title · engine", () => {
    const texts = (select: HTMLSelectElement) => [...select.options].map((o) => o.text);
    const values = (select: HTMLSelectElement) => [...select.options].map((o) => o.value);
    const row = (over: Record<string, unknown>) => ({
      id: "x", provider: "gguf", category: "gguf", is_active: false, downloaded: true,
      kind: "conversation", recommended_role: "chat",
      engine: { id: "llama_cpp", label: "llama.cpp", file_format: ".gguf", in_process: true },
      ...over,
    });
    const picker = async (models: unknown[], overrides: Record<string, unknown> = {}) => {
      await renderPage(overrides, models);
      fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
      return screen.getByLabelText("Model") as HTMLSelectElement;
    };

    it("names a model by its title and the engine that runs it, whatever the file is called", async () => {
      const model = await picker([
        row({ name: "gemma-4-E4B-it-qat-UD-Q4_K_XL", title: "Gemma 4 E4B", display_name: "Gemma 4 E4B Instruct, quantisation-aware 4-bit (~4.2 GB, ...)" }),
        row({
          name: "gemma-4-E2B-it.litertlm", title: "Gemma 4 E2B", provider: "litert", category: "litert",
          engine: { id: "litert_lm", label: "LiteRT-LM", file_format: ".litertlm", in_process: true },
        }),
        row({
          name: "qwen3:4b", title: "qwen3:4b", provider: "ollama", category: "ollama",
          engine: { id: "ollama", label: "Ollama", file_format: null, in_process: false },
        }),
      ]);
      expect(texts(model)).toEqual(expect.arrayContaining([
        "Gemma 4 E4B · llama.cpp", "Gemma 4 E2B · LiteRT-LM", "qwen3:4b · Ollama",
      ]));
      // The value stays what the server reads.
      expect(values(model)).toEqual(expect.arrayContaining(["gemma-4-E4B-it-qat-UD-Q4_K_XL", "gemma-4-E2B-it.litertlm", "qwen3:4b"]));
      expect(texts(model).join("|")).not.toMatch(/quantisation-aware/);
    });

    it("never shows the placeholder the disk scan writes, not even for the model in use", async () => {
      const model = await picker(
        [row({ name: "Llama-3.2-3B-Instruct-Q4_K_M", title: "Llama-3.2-3B-Instruct-Q4_K_M", display_name: "(detected on disk)" }),
         row({ name: "bare-file", display_name: "(detected on disk)" })],
        { chat_model: "bare-file" },
      );
      expect(texts(model).join("|")).not.toMatch(/detected on disk/);
      expect(texts(model)).toEqual(expect.arrayContaining(["Llama-3.2-3B-Instruct-Q4_K_M · llama.cpp", "bare-file · llama.cpp"]));
      expect(model.value).toBe("bare-file");
      expect(screen.queryByText(/detected on disk/)).toBeNull();
    });

    it("tells two rows that read alike apart, and lists a model once", async () => {
      const model = await picker([
        row({ name: "gemma-a", title: "Gemma 4 E4B", quantization: "Q4_K_M" }),
        row({ name: "gemma-b", title: "Gemma 4 E4B", quantization: "Q8_0" }),
        row({ name: "twin-a", title: "Twin", filename: "twin-a.gguf" }),
        row({ name: "twin-b", title: "Twin", filename: "twin-b.gguf" }),
        row({ name: "gemma-a", title: "Gemma 4 E4B", quantization: "Q4_K_M" }),
      ]);
      const labels = texts(model).filter((t) => /Gemma|Twin/.test(t));
      expect(labels).toEqual([
        "Gemma 4 E4B · llama.cpp (Q4_K_M)",
        "Gemma 4 E4B · llama.cpp (Q8_0)",
        "Twin · llama.cpp (twin-a.gguf)",
        "Twin · llama.cpp (twin-b.gguf)",
      ]);
      expect(new Set(labels).size).toBe(labels.length);
      expect(values(model).filter((v) => v === "gemma-a")).toHaveLength(1);
    });

    it("offers only models that can hold a conversation, but every one to the tool-call helper", async () => {
      const models = [
        row({ name: "gemma-4-E2B-it-qat-UD-Q4_K_XL", title: "Gemma 4 E2B" }),
        row({ name: "functiongemma-270m", title: "FunctionGemma 270M", kind: "helper", recommended_role: "tool" }),
      ];
      const model = await picker(models);
      expect(texts(model)).toContain("Gemma 4 E2B · llama.cpp");
      expect(texts(model).join("|")).not.toMatch(/FunctionGemma/);
      cleanup();

      await renderPage({}, models);
      fireEvent.click(screen.getByRole("button", { name: /^Extensions/ }));
      const helper = screen.getByLabelText("Tool-call helper model") as HTMLSelectElement;
      expect(texts(helper)).toEqual(expect.arrayContaining(["FunctionGemma 270M · llama.cpp", "Gemma 4 E2B · llama.cpp"]));
    });

    it("still keeps a stored model the registry no longer lists", async () => {
      const model = await picker([row({ name: "here", title: "Here" })], { chat_model: "removed-model" });
      expect(model.value).toBe("removed-model");
      expect(texts(model)).toContain("removed-model — not installed");
    });
  });

  it("offers the device's own time zone when it differs from the saved one", async () => {
    deviceZoneValue = "Africa/Kampala";
    await renderPage({ timezone: "UTC" });
    const detect = screen.getByRole("button", { name: /Use Africa\/Kampala/ });
    fireEvent.click(detect);
    await waitFor(() =>
      expect((screen.getByLabelText("Time zone") as HTMLSelectElement).value)
        .toBe("Africa/Kampala"));
  });

  it("offers nothing when the device already agrees with the saved zone", async () => {
    deviceZoneValue = "UTC";
    await renderPage({ timezone: "UTC" });
    expect(screen.queryByRole("button", { name: /^Use / })).toBeNull();
  });

  it("searches across every category, not just the open one", async () => {
    await renderPage();
    // "Camera address" is not on the category the page opens to.
    fireEvent.change(screen.getByLabelText("Search settings"), { target: { value: "camera" } });
    await screen.findByText(/results/);
    expect(screen.getByLabelText("Camera address")).toBeTruthy();

    // `cameras_enabled` matches too, and it is the consent switch, so it is offered.
    expect(screen.getByLabelText("Cameras")).toBeTruthy();
  });

  it("finds a setting by what it does, not only by its name", async () => {
    await renderPage();
    // "greetings" is only in "Home name"'s description.
    fireEvent.change(screen.getByLabelText("Search settings"), { target: { value: "greetings" } });
    // One match, so the heading reads "result" — not "results".
    await screen.findByText(/result/);
    expect(screen.getByLabelText("Home name")).toBeTruthy();
  });

  it("renders consequential choices as radios", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    const group = screen.getByRole("radiogroup", { name: "Network reach" });
    const radios = within(group).getAllByRole("radio");
    expect(radios).toHaveLength(3);
    expect((within(group).getByRole("radio", { name: /Open/ }) as HTMLInputElement).checked).toBe(true);

    fireEvent.click(within(group).getByRole("radio", { name: /Offline/ }));
    // The sentence at the top answers immediately — before any save.
    await waitFor(() =>
      expect(document.querySelector(".scat__sentence")!.textContent)
        .toContain("Nothing leaves this house."));
  });

  it("degrades a picker to free text when the registry cannot be reached", async () => {
    mockApi.getSettings.mockResolvedValue(serverSettings());
    mockApi.listModels.mockRejectedValue(new Error("offline"));
    render(<SettingsCatalogueView />);
    await screen.findByText("Who lives here");
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    // A text box keeps the configured value visible; an empty dropdown hides it.
    const model = await screen.findByLabelText("Model");
    expect(model.tagName).toBe("INPUT");
    expect((model as HTMLInputElement).value).toBe("gemma-4-E4B");
  });

  it("offers a retry when settings cannot be loaded", async () => {
    mockApi.getSettings.mockRejectedValueOnce(new Error("ECONNREFUSED"));
    mockApi.listModels.mockResolvedValue([]);
    render(<SettingsCatalogueView />);
    const retry = await screen.findByRole("button", { name: /Try again/ });

    mockApi.getSettings.mockResolvedValue(serverSettings());
    fireEvent.click(retry);
    await screen.findByText("Who lives here");
  });
});

describe("Connections category", () => {
  async function openConnections(overrides: Record<string, unknown> = {}) {
    await renderPage(overrides);
    fireEvent.click(screen.getByRole("button", { name: /^Connections/ }));
    await screen.findByRole("heading", { name: "Wi-Fi" });
  }

  beforeEach(() => {
    mockApi.getConnections.mockResolvedValue(makeConnections());
    mockApi.getSetupStatus.mockResolvedValue(SETUP_WAITING);
    mockApi.getSystemInfo.mockResolvedValue({ platform: "linux" });
  });

  it("shows Wi-Fi, Ethernet, Bluetooth and Pairing, each with what the pond says about it", async () => {
    await openConnections();
    expect(screen.getByRole("heading", { level: 2, name: "Connections" })).toBeTruthy();
    expect(Array.from(document.querySelectorAll(".scat__groupHead h3"), (n) => n.textContent)).toEqual([
      "Wi-Fi",
      "Ethernet",
      "Bluetooth",
      "Pairing",
    ]);
    expect(await screen.findByText("192.168.1.11")).toBeTruthy();
    expect(screen.getByText("eth0")).toBeTruthy();
    expect(screen.getByText("Visible to phones")).toBeTruthy();
    expect(await screen.findByText("Waiting to be claimed (card 4e81)")).toBeTruthy();
  });

  it("holds the two switches, which no longer sit under Privacy and Security", async () => {
    await openConnections();
    expect(screen.getByLabelText("Set up over Bluetooth")).toBeTruthy();
    expect(screen.getByLabelText("Say pairing codes aloud")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    expect(screen.queryByLabelText("Set up over Bluetooth")).toBeNull();
    expect(screen.queryByLabelText("Say pairing codes aloud")).toBeNull();
    expect(screen.getByText(/are under Connections/)).toBeTruthy();
  });

  it("opens Connections from the pointer under Privacy and Security", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    fireEvent.click(screen.getByRole("button", { name: "Open Connections" }));
    await screen.findByRole("heading", { name: "Wi-Fi" });
  });

  it("stages a switch for the page's Save like any other, and reads the radio again once saved", async () => {
    await openConnections({ ble_setup_enabled: true });
    const callsBefore = mockApi.getConnections.mock.calls.length;

    fireEvent.click(screen.getByLabelText("Set up over Bluetooth"));
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
    fireEvent.click(await screen.findByRole("button", { name: /Save 1 change/ }));
    await waitFor(() => expect(mockApi.updateSettings).toHaveBeenCalledWith({ ble_setup_enabled: false }));
    await waitFor(() => expect(mockApi.getConnections.mock.calls.length).toBeGreaterThan(callsBefore), { timeout: 3_000 });
  });

  it("does not ask the pond about its links while another category is showing", async () => {
    await renderPage();
    expect(mockApi.getConnections).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    expect(mockApi.getConnections).not.toHaveBeenCalled();
  });

  it("finds the switches by search, without the live panels or a read of the pond", async () => {
    await renderPage();
    fireEvent.change(screen.getByLabelText("Search settings"), { target: { value: "bluetooth" } });
    await screen.findByText(/result/);
    expect(screen.getByLabelText("Set up over Bluetooth")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Wi-Fi" })).toBeNull();
    expect(mockApi.getConnections).not.toHaveBeenCalled();
  });

  it("offers Find networks only on request, never scanning on its own", async () => {
    await openConnections();
    expect(await screen.findByRole("button", { name: "Find networks" })).toBeTruthy();
    expect((api as unknown as { scanWifi: ReturnType<typeof vi.fn> }).scanWifi).not.toHaveBeenCalled();
  });
});

describe("summariseRetitle", () => {
  it("says a pass has started", () => {
    expect(summariseRetitle(retitleReply())).toBe("Renaming — names appear as it goes");
  });

  // "Failed" would invite a retry that can't help: there's nothing here to wake.
  it("separates nothing-here-to-run from a failure", () => {
    expect(summariseRetitle(retitleReply({
      started: false,
      reason: "the titling job has no loop in this process",
    }))).toBe("the titling job has no loop in this process");
    expect(summariseRetitle(retitleReply({ started: false })))
      .toBe("Nothing here to run");
  });

  // The reply returns before any model call (minutes on the Orin, vs a 30 s timeout), so has no count.
  it("never claims a count it could not have", () => {
    for (const reply of [
      retitleReply(),
      retitleReply({ started: false }),
      retitleReply({ started: false, reason: "this process has no inference lane" }),
    ]) {
      expect(summariseRetitle(reply)).not.toMatch(/\bRenamed\b/);
      expect(summariseRetitle(reply)).not.toMatch(/\d/);
    }
  });
});

describe("the rename-now button", () => {
  async function openAutomation() {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Automations/ }));
  }

  it("asks for a pass and reports that it started", async () => {
    mockApi.retitleSessions.mockResolvedValue(retitleReply());
    await openAutomation();

    fireEvent.click(screen.getByRole("button", { name: /Rename now/ }));

    await screen.findByText("Renaming — names appear as it goes");
    expect(mockApi.retitleSessions).toHaveBeenCalledTimes(1);
    // Renaming is not a settings change; it must not dirty the save button.
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  // A double press would ask the lane twice for one pass.
  it("says it is asking and cannot be pressed again mid-request", async () => {
    let release!: (v: unknown) => void;
    mockApi.retitleSessions.mockReturnValue(new Promise((r) => { release = r; }));
    await openAutomation();

    fireEvent.click(screen.getByRole("button", { name: /Rename now/ }));

    const busy = await screen.findByRole("button", { name: /Asking/ });
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(busy);
    expect(mockApi.retitleSessions).toHaveBeenCalledTimes(1);

    release(retitleReply());
    await screen.findByText("Renaming — names appear as it goes");
  });

  it("shows the failure rather than a silent no-op", async () => {
    mockApi.retitleSessions.mockRejectedValue(new Error("No language model is configured"));
    await openAutomation();

    fireEvent.click(screen.getByRole("button", { name: /Rename now/ }));

    await screen.findByText("No language model is configured");
    // Recoverable: the button comes back rather than staying stuck on "Asking".
    await waitFor(() =>
      expect((screen.getByRole("button", { name: /Rename now/ }) as HTMLButtonElement).disabled)
        .toBe(false));
  });

  it("is offered even when the automatic pass is switched off", async () => {
    mockApi.retitleSessions.mockResolvedValue(retitleReply());
    await renderPage({ session_titling_enabled: false });
    fireEvent.click(screen.getByRole("button", { name: /Automations/ }));

    const button = screen.getByRole("button", { name: /Rename now/ }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
  });
});

// `suggestions_muted` is a `Vec<String>`: a raw string gets a 422 that fails the whole Save.
describe("parseText: list-valued text boxes", () => {
  it("sends the hidden-suggestions box as a list, not the string it was typed as", () => {
    expect(parseText("suggestions_muted", "weather_today, devices_online")).toEqual([
      "weather_today",
      "devices_online",
    ]);
  });

  it("reads an emptied box as the empty list, which is what 'clear to unmute' means", () => {
    // `""` against a `[]` baseline would dirty the page with an unsavable change.
    expect(parseText("suggestions_muted", "")).toEqual([]);
    expect(parseText("suggestions_muted", " , ")).toEqual([]);
  });

  it("sends the camera event kinds as a list; emptied means every kind", () => {
    expect(parseText("vision_event_types", "person, face_recognized")).toEqual([
      "person",
      "face_recognized",
    ]);
    expect(parseText("vision_event_types", "")).toEqual([]);
  });

  it("leaves an ordinary text field a string -- the control for the lists above", () => {
    // Catches a parseText that splits every field.
    expect(parseText("assistant_name", "Goose, the pond")).toBe("Goose, the pond");
  });
});
