import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { SettingsCatalogueView, summariseRetitle, parseText } from "./SettingsCatalogue";
import { api } from "../api/PondApiClient";

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

// The zone the device reports, held in a variable so a test can choose it.
// Reading the real one couples the suite to the machine: a CI runner is UTC,
// so a test that saves "UTC" and expects to be offered something else is
// asking whether the two differ on THIS host, not whether the component does
// the right thing when they do.
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
};

/** A re-titling reply with the boring fields filled in. */
function retitleReply(over: Record<string, unknown> = {}) {
  return { started: true, ...over };
}

/** Render and wait for the first paint after settings load. */
async function renderPage(overrides: Record<string, unknown> = {}) {
  mockApi.getSettings.mockResolvedValue(serverSettings(overrides));
  mockApi.listModels.mockResolvedValue(MODELS);
  // The zone picker asks the server for the IANA catalogue; a couple of rows
  // is enough to prove it renders what it is given rather than a hand list.
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

  it("does not offer a control that nothing reads", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));

    // `cameras_enabled` persists and renders, but no code reads it. It used to
    // show disabled with a note saying why; the note is a maintenance fact, so
    // it moved behind developer view — and a disabled control whose reason is
    // hidden is worse than either half. So the household is not shown it at all.
    expect(screen.queryByLabelText("Cameras")).toBeNull();

    // Its live neighbour is untouched.
    expect((screen.getByLabelText("Microphone") as HTMLInputElement).disabled).toBe(false);
  });

  it("shows the inert ones, marked, in developer view", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    fireEvent.click(screen.getByRole("button", { name: /Developer view/ }));

    const cameras = screen.getByLabelText("Cameras") as HTMLInputElement;
    expect(cameras.disabled).toBe(true);
    expect(rowFor("Cameras").textContent).toContain("Nothing reads this");
    // The field name appears here and only here.
    expect(rowFor("Cameras").textContent).toContain("cameras_enabled");
  });

  it("keeps field names out of sight until asked for", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));

    // A household sees what the setting does, never what it is called — but the
    // description is there, so the row says more than its label.
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

    // Not on offer to a household.
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

  /// Found while wiring the zone picker: with no backend, `dev:vite` serves
  /// index.html for /api, `request` casts it to `Settings`, and the `errors`
  /// memo indexes `undefined` by the first catalogue key — so the page died
  /// with "Cannot read properties of undefined (reading 'user_name')" where
  /// the docs promise an error state.
  it("shows an error rather than crashing when the reply is not settings", async () => {
    mockApi.getSettings.mockResolvedValue(undefined);
    render(<SettingsCatalogueView />);
    // The banner, not a blank page and not a thrown render. `ErrorBanner`
    // rewrites the wording, so the role is what this asserts on.
    expect(await screen.findByRole("alert")).toBeTruthy();
  });

  /// The three hand-maintained lists held 16, 18 and 13 zones and none of them
  /// held Kampala, so a household there could not say where it was.
  it("offers every zone the server knows, not a hand-picked few", async () => {
    await renderPage();
    const picker = (await screen.findByLabelText("Time zone")) as HTMLSelectElement;
    const values = [...picker.options].map((o) => o.value);
    expect(values).toContain("Africa/Kampala");
    // The offset is shown, so two similarly-named zones can be told apart.
    expect([...picker.options].map((o) => o.textContent).join(" ")).toContain("+03:00");
  });

  it("fills the place and both coordinates from one press", async () => {
    // No browser geolocation, which is the normal case inside Tauri. The old
    // button depended on it and so produced a name and no coordinates.
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

  /// A guess must not be reported as a fact.
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

  /// What is already typed beats what the time zone implies.
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

  // The two things the deleted classic Settings view owned. Losing either while
  // keeping the settings around them would look like they still worked.
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
    // pond-server syncs these FROM model_role_assignments, so a control here
    // would lose to the next sync. They stay catalogued, they are not offered.
    expect(screen.queryByLabelText("Chat model")).toBeNull();
    expect(screen.queryByLabelText("Embedding model")).toBeNull();
  });

  it("counts the inert settings per category in the rail", async () => {
    await renderPage();
    const privacy = screen.getByRole("button", { name: /Privacy & Security/ });
    // cameras_enabled + cloud_fallback_enabled
    expect(within(privacy).getByTitle(/2 settings here that nothing reads/)).toBeTruthy();
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
    // The action keeps its name through the flow — "Save" becomes "Saved" —
    // and the panel settles clean, so the next save does not resend it.
    await screen.findByRole("button", { name: /^Saved$/ });
  });

  it("shows the server's own words when a save is refused", async () => {
    await renderPage();
    mockApi.updateSettings.mockRejectedValueOnce(
      new Error('network_mode "opn" is not one of ["open", "allowlist", "offline"]'));

    fireEvent.change(screen.getByLabelText("Your name"), { target: { value: "X" } });
    fireEvent.click(await screen.findByRole("button", { name: /Save 1 change/ }));

    // Verbatim, not routed through friendlyMessage — the 422 names the field
    // and the accepted values, and that sentence is the entire reason it failed.
    await screen.findByText(/is not one of/);
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

  it("keeps a configured model the registry does not list", async () => {
    await renderPage({ chat_model: "some-model-i-removed" });
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    const model = screen.getByLabelText("Model") as HTMLSelectElement;
    expect(model.value).toBe("some-model-i-removed");
    expect([...model.options].map((o) => o.text)).toContain("some-model-i-removed — not installed");
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

  // The other half of "only when it differs", which nothing asserted before:
  // the offer has to be absent, not merely correct when present.
  it("offers nothing when the device already agrees with the saved zone", async () => {
    deviceZoneValue = "UTC";
    await renderPage({ timezone: "UTC" });
    expect(screen.queryByRole("button", { name: /^Use / })).toBeNull();
  });

  it("searches across every category, not just the open one", async () => {
    await renderPage();
    // "Motion sensitivity" lives under Vision; "Camera address" beside it — and
    // neither is on the category the page opens to.
    fireEvent.change(screen.getByLabelText("Search settings"), { target: { value: "camera" } });
    await screen.findByText(/results/);
    expect(screen.getByLabelText("Camera address")).toBeTruthy();

    // `cameras_enabled` also matches "camera", but nothing reads it, so it is
    // not among the results a household is offered.
    expect(screen.queryByLabelText("Cameras")).toBeNull();
  });

  it("finds a setting by what it does, not only by its name", async () => {
    await renderPage();
    // "greetings" appears nowhere in the label "Home name" — only in its
    // description. Before descriptions were searchable this found nothing.
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

describe("summariseRetitle", () => {
  it("says a pass has started", () => {
    expect(summariseRetitle(retitleReply())).toBe("Renaming — names appear as it goes");
  });

  /// The distinction the copy exists for: a pond whose titling loop never
  /// spawned has nothing to wake, and a person told "it did not work" would
  /// press the button again expecting a different answer.
  it("separates nothing-here-to-run from a failure", () => {
    expect(summariseRetitle(retitleReply({
      started: false,
      reason: "the titling job has no loop in this process",
    }))).toBe("the titling job has no loop in this process");
    expect(summariseRetitle(retitleReply({ started: false })))
      .toBe("Nothing here to run");
  });

  /// The button used to hold the request open through every model call so it
  /// could report a count. On the Orin that was minutes against a 30 s client
  /// timeout, so the count it promised arrived as an error. Claiming a result
  /// this reply cannot contain is the specific regression to guard.
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

  /// The request is short now, but it is still a request, and a double press
  /// would ask the lane twice for a pass it is already going to run.
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

  /// The toggle governs what happens unattended. A button that silently did
  /// nothing because of a switch elsewhere on the same page is the worse
  /// surprise, so it is offered either way.
  it("is offered even when the automatic pass is switched off", async () => {
    mockApi.retitleSessions.mockResolvedValue(retitleReply());
    await renderPage({ session_titling_enabled: false });
    fireEvent.click(screen.getByRole("button", { name: /Automations/ }));

    const button = screen.getByRole("button", { name: /Rename now/ }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
  });
});

// The server's `suggestions_muted` is a `Vec<String>`. Sent as the raw string it
// was refused with a 422, and that refusal took every other edit in the same
// Save with it -- so touching this one box made Settings unsavable.
describe("parseText: list-valued text boxes", () => {
  it("sends the hidden-suggestions box as a list, not the string it was typed as", () => {
    expect(parseText("suggestions_muted", "weather_today, devices_online")).toEqual([
      "weather_today",
      "devices_online",
    ]);
  });

  it("reads an emptied box as the empty list, which is what 'clear to unmute' means", () => {
    // `""` against a baseline of `[]` is what made the page dirty with a change
    // it could never save.
    expect(parseText("suggestions_muted", "")).toEqual([]);
    expect(parseText("suggestions_muted", " , ")).toEqual([]);
  });

  it("leaves an ordinary text field a string -- the control for the two above", () => {
    // Without this, a parseText that split EVERY field would pass both tests
    // and send the assistant's name as an array.
    expect(parseText("assistant_name", "Goose, the pond")).toBe("Goose, the pond");
  });
});
